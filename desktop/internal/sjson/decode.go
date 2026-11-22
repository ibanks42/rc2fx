package sjson

import (
	"bytes"
	"encoding"
	"encoding/json"
	"reflect"
	"strconv"
	"strings"
	"sync"
)

// Strict typed JSON decoding.
//
// Struct fields use their `json` tag name (or the Go field name if untagged;
// `json:"-"` skips the field). Key differences from encoding/json:
//
//   - Fields are required unless they are pointers (missing or null -> nil)
//     or tagged `default` (missing -> zero value).
//   - A pointer tagged `nonnull` may be omitted, but explicit null is rejected.
//   - Unknown fields are rejected unless the struct implements UnknownJSONFieldsAllowed.
//   - Integers require integer literals in range; floats accept any number;
//     fixed-size arrays require an exact length match.
//   - Types whose pointer implements encoding.TextUnmarshaler decode from a JSON
//     string (used by enums to reject unknown variants).
//   - Types whose pointer implements StrictUnmarshaler decode themselves.
//   - Tree fields accept any JSON value.
//
// The `omitempty` option applies to encoding only: a field is omitted when nil,
// zero, or when IsEmpty() returns true.

// StrictUnmarshaler allows a type to decode itself from a parsed Tree.
type StrictUnmarshaler interface {
	UnmarshalStrictJSON(Tree) error
}

// UnknownJSONFieldsAllowed marks a struct whose unknown JSON fields are ignored
// instead of rejected.
type UnknownJSONFieldsAllowed interface {
	AllowUnknownJSONFields()
}

// DecodeStrict parses data with the default limits and strictly decodes it into
// out (a non-nil pointer).
func DecodeStrict(data []byte, out any) error {
	return DecodeStrictLimited(data, DefaultMaxJSONBytes, DefaultMaxJSONNodes, out)
}

// DecodeStrictLimited parses data with explicit size/node limits and strictly
// decodes it into out.
func DecodeStrictLimited(data []byte, maximumBytes, maximumNodes int, out any) error {
	tree, err := parseStrict(data, maximumBytes, maximumNodes)
	if err != nil {
		return err
	}
	return DecodeTree(tree, out)
}

// FromJSON strictly parses text with the default limits (rejecting duplicate
// keys, oversized input, and excessive nesting) and decodes it into T.
func FromJSON[T any](text string) (T, error) {
	return FromJSONLimited[T](text, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
}

// FromJSONLimited is like FromJSON with explicit byte and node bounds.
func FromJSONLimited[T any](text string, maximumBytes, maximumNodes int) (T, error) {
	var out T
	tree, err := parseStrict([]byte(text), maximumBytes, maximumNodes)
	if err != nil {
		return out, err
	}
	err = DecodeTree(tree, &out)
	if err != nil {
		var zero T
		return zero, err
	}
	return out, nil
}

// DecodeTree decodes a parsed Tree into out using strict rules.
func DecodeTree(tree Tree, out any) error {
	target := reflect.ValueOf(out)
	if target.Kind() != reflect.Pointer || target.IsNil() {
		return Error("JSON decode target must be a non-nil pointer")
	}
	return decodeInto(tree, target.Elem(), true)
}

var (
	treeType              = reflect.TypeFor[Tree]()
	strictUnmarshalerType = reflect.TypeFor[StrictUnmarshaler]()
	jsonUnmarshalerType   = reflect.TypeFor[json.Unmarshaler]()
	textUnmarshalerType   = reflect.TypeFor[encoding.TextUnmarshaler]()
	allowUnknownType      = reflect.TypeFor[UnknownJSONFieldsAllowed]()
)

func decodeInto(tree Tree, target reflect.Value, strictMode bool) error {
	t := target.Type()
	if t == treeType {
		target.Set(reflect.ValueOf(tree))
		return nil
	}
	if t == rawMessageType {
		if len(tree.raw) > 0 {
			target.SetBytes(bytes.Clone(tree.raw))
		} else {
			target.SetBytes(encodeTree(tree, false))
		}
		return nil
	}
	if reflect.PointerTo(t).Implements(strictUnmarshalerType) {
		return target.Addr().Interface().(StrictUnmarshaler).UnmarshalStrictJSON(tree)
	}
	if !strictMode && target.CanAddr() && reflect.PointerTo(t).Implements(jsonUnmarshalerType) {
		raw := tree.raw
		if len(raw) == 0 {
			raw = encodeTree(tree, false)
		}
		return target.Addr().Interface().(json.Unmarshaler).UnmarshalJSON(raw)
	}
	if t.Kind() != reflect.Pointer && reflect.PointerTo(t).Implements(textUnmarshalerType) {
		text, ok := tree.AsString()
		if !ok {
			return invalidType(tree, "a string")
		}
		return target.Addr().Interface().(encoding.TextUnmarshaler).UnmarshalText([]byte(text))
	}
	switch t.Kind() {
	case reflect.Pointer:
		if tree.kind == KindNull {
			target.SetZero()
			return nil
		}
		fresh := reflect.New(t.Elem())
		if err := decodeInto(tree, fresh.Elem(), strictMode); err != nil {
			return err
		}
		target.Set(fresh)
		return nil
	case reflect.Interface:
		if t.NumMethod() != 0 {
			break
		}
		if strictMode {
			target.Set(reflect.ValueOf(tree))
		} else {
			g := toGeneric(tree)
			if g == nil {
				target.SetZero()
			} else {
				target.Set(reflect.ValueOf(g))
			}
		}
		return nil
	case reflect.Bool:
		value, ok := tree.AsBool()
		if !ok {
			return invalidType(tree, "a boolean")
		}
		target.SetBool(value)
		return nil
	case reflect.String:
		value, ok := tree.AsString()
		if !ok {
			return invalidType(tree, "a string")
		}
		target.SetString(value)
		return nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return decodeSigned(tree, target)
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return decodeUnsigned(tree, target)
	case reflect.Float32, reflect.Float64:
		if tree.kind != KindNumber {
			return invalidType(tree, t.String())
		}
		target.SetFloat(tree.number.AsF64())
		return nil
	case reflect.Slice:
		items, ok := tree.AsArray()
		if !ok {
			return invalidType(tree, "a sequence")
		}
		slice := reflect.MakeSlice(t, len(items), len(items))
		for i, item := range items {
			if err := decodeInto(item, slice.Index(i), strictMode); err != nil {
				return err
			}
		}
		target.Set(slice)
		return nil
	case reflect.Array:
		items, ok := tree.AsArray()
		if !ok {
			return invalidType(tree, "an array of length "+strconv.Itoa(t.Len()))
		}
		if len(items) < t.Len() {
			return Error("invalid length " + strconv.Itoa(len(items)) + ", expected an array of length " + strconv.Itoa(t.Len()))
		}
		if len(items) > t.Len() {
			return Error("invalid length " + strconv.Itoa(len(items)) + ", expected fewer elements in array")
		}
		for i, item := range items {
			if err := decodeInto(item, target.Index(i), strictMode); err != nil {
				return err
			}
		}
		return nil
	case reflect.Map:
		if t.Key().Kind() != reflect.String {
			break
		}
		members, ok := tree.AsObject()
		if !ok {
			return invalidType(tree, "a map")
		}
		mapping := reflect.MakeMapWithSize(t, len(members))
		for _, member := range members {
			item := reflect.New(t.Elem()).Elem()
			if err := decodeInto(member.Value, item, strictMode); err != nil {
				return err
			}
			key := reflect.New(t.Key()).Elem()
			key.SetString(member.Key)
			mapping.SetMapIndex(key, item)
		}
		target.Set(mapping)
		return nil
	case reflect.Struct:
		return decodeStruct(tree, target, strictMode)
	}
	return Error("JSON cannot be decoded into " + t.String())
}

func decodeSigned(tree Tree, target reflect.Value) error {
	t := target.Type()
	if tree.kind != KindNumber {
		return invalidType(tree, t.String())
	}
	bits := t.Bits()
	low, high := int64(-1)<<(bits-1), int64(1)<<(bits-1)-1
	switch tree.number.kind {
	case numberU64:
		if tree.number.bits > uint64(high) {
			return invalidValue(tree, t.String())
		}
		target.SetInt(int64(tree.number.bits))
	case numberI64:
		value := int64(tree.number.bits)
		if value < low {
			return invalidValue(tree, t.String())
		}
		target.SetInt(value)
	default:
		return invalidType(tree, t.String())
	}
	return nil
}

func decodeUnsigned(tree Tree, target reflect.Value) error {
	t := target.Type()
	if tree.kind != KindNumber {
		return invalidType(tree, t.String())
	}
	switch tree.number.kind {
	case numberU64:
		bits := t.Bits()
		if bits < 64 && tree.number.bits > uint64(1)<<bits-1 {
			return invalidValue(tree, t.String())
		}
		target.SetUint(tree.number.bits)
	case numberI64:
		return invalidValue(tree, t.String())
	default:
		return invalidType(tree, t.String())
	}
	return nil
}

type fieldInfo struct {
	name      string
	index     int
	optional  bool // pointer or `default`: may be missing
	nonNull   bool
	omitEmpty bool
}

type structInfo struct {
	name         string
	fields       []fieldInfo
	byName       map[string]int
	allowUnknown bool
	denyUnknown  bool
	defaults     bool
}

var structInfos sync.Map // reflect.Type -> *structInfo

func structInfoFor(t reflect.Type) *structInfo {
	if cached, ok := structInfos.Load(t); ok {
		return cached.(*structInfo)
	}
	info := &structInfo{
		name:         t.Name(),
		byName:       map[string]int{},
		allowUnknown: t.Implements(allowUnknownType) || reflect.PointerTo(t).Implements(allowUnknownType),
	}
	for i := 0; i < t.NumField(); i++ {
		field := t.Field(i)
		if field.Name == "_" {
			for option := range strings.SplitSeq(field.Tag.Get("serde"), ",") {
				switch option {
				case "default":
					info.defaults = true
				case "deny_unknown_fields":
					info.denyUnknown = true
				}
			}
			continue
		}
		if !field.IsExported() {
			continue
		}
		tag, hasTag := field.Tag.Lookup("json")
		if tag == "-" {
			continue
		}
		name, options, _ := strings.Cut(tag, ",")
		if name == "" || !hasTag {
			name = field.Name
		}
		f := fieldInfo{name: name, index: i, optional: field.Type.Kind() == reflect.Pointer}
		for option := range strings.SplitSeq(options, ",") {
			switch option {
			case "default":
				f.optional = true
			case "nonnull":
				f.nonNull = true
			case "omitempty":
				f.omitEmpty = true
			}
		}
		info.byName[name] = len(info.fields)
		info.fields = append(info.fields, f)
	}
	actual, _ := structInfos.LoadOrStore(t, info)
	return actual.(*structInfo)
}

func decodeStruct(tree Tree, target reflect.Value, strictMode bool) error {
	t := target.Type()
	info := structInfoFor(t)
	members, ok := tree.AsObject()
	if !ok {
		return invalidType(tree, "struct "+info.name)
	}
	var seenSmall [64]bool
	var seen []bool
	if len(info.fields) <= len(seenSmall) {
		seen = seenSmall[:len(info.fields)]
	} else {
		seen = make([]bool, len(info.fields))
	}
	deny := info.denyUnknown || (strictMode && !info.allowUnknown)
	for _, member := range members {
		index, known := info.byName[member.Key]
		if !known {
			if !deny {
				continue
			}
			return unknownField(member.Key, info)
		}
		if seen[index] {
			return Error("duplicate field `" + member.Key + "`")
		}
		field := info.fields[index]
		if field.nonNull && member.Value.kind == KindNull {
			return invalidType(member.Value, "a sequence")
		}
		if err := decodeInto(member.Value, target.Field(field.index), strictMode); err != nil {
			return err
		}
		seen[index] = true
	}
	for index, field := range info.fields {
		if seen[index] {
			continue
		}
		if !field.optional && !info.defaults {
			return Error("missing field `" + field.name + "`")
		}
		target.Field(field.index).SetZero()
	}
	return nil
}

func unknownField(name string, info *structInfo) error {
	message := "unknown field `" + name + "`, "
	switch len(info.fields) {
	case 0:
		message += "there are no fields"
	case 1:
		message += "expected `" + info.fields[0].name + "`"
	case 2:
		message += "expected `" + info.fields[0].name + "` or `" + info.fields[1].name + "`"
	default:
		message += "expected one of "
		for i, field := range info.fields {
			if i > 0 {
				message += ", "
			}
			message += "`" + field.name + "`"
		}
	}
	return Error(message)
}

// unexpected formats a short description of tree for decode errors.
func unexpected(tree Tree) string {
	switch tree.kind {
	case KindNull:
		return "null"
	case KindBool:
		if tree.boolean {
			return "boolean `true`"
		}
		return "boolean `false`"
	case KindNumber:
		switch tree.number.kind {
		case numberF64:
			return "floating point `" + tree.number.String() + "`"
		default:
			return "integer `" + tree.number.String() + "`"
		}
	case KindString:
		return "string " + strconv.Quote(tree.text)
	case KindArray:
		return "sequence"
	}
	return "map"
}

func invalidType(tree Tree, expected string) error {
	return Error("invalid type: " + unexpected(tree) + ", expected " + expected)
}

func invalidValue(tree Tree, expected string) error {
	return Error("invalid value: " + unexpected(tree) + ", expected " + expected)
}
