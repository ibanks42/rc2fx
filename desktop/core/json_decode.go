package core

import (
	"encoding"
	"reflect"
	"strconv"
	"strings"
	"sync"
)

// The strict typed decoder: serde's derive semantics over the strict parser.
//
// Struct fields are named by their `json` tag (the field name when untagged;
// `json:"-"` skips a field). Unlike encoding/json:
//
//   - every field is required unless it is a pointer (Rust `Option<T>`:
//     missing or null is nil) or tagged `default` (missing is the zero
//     value, Rust `#[serde(default)]`);
//   - a pointer tagged `nonnull` may be missing but not null (a present
//     Option that must hold a value);
//   - unknown fields are rejected, unless the struct type has an
//     AllowUnknownJSONFields method;
//   - integers must be integer literals within the field's range; floats
//     accept any number; a fixed-size array needs exactly its length;
//   - a type whose pointer implements encoding.TextUnmarshaler is read from a
//     JSON string (the enums use this to reject unknown variants);
//   - a type whose pointer implements StrictUnmarshaler decodes itself;
//   - Value fields take any JSON.
//
// The `omitempty` option affects encoding only: the field is left out when it
// is a nil pointer, its zero value, or has an IsEmpty() method returning true
// (Rust `skip_serializing_if`).

// StrictUnmarshaler lets a type decode itself from a strictly parsed tree.
type StrictUnmarshaler interface {
	UnmarshalStrictJSON(Value) error
}

// UnknownJSONFieldsAllowed marks a struct whose unknown fields are ignored
// rather than rejected (a Rust struct without deny_unknown_fields).
type UnknownJSONFieldsAllowed interface {
	AllowUnknownJSONFields()
}

// DecodeStrict is from_json for bytes: default limits, then typed decoding
// into out (a non-nil pointer).
func DecodeStrict(data []byte, out any) error {
	return DecodeStrictLimited(data, DefaultMaxJSONBytes, DefaultMaxJSONNodes, out)
}

// DecodeStrictLimited is from_json_limited for bytes.
func DecodeStrictLimited(data []byte, maximumBytes, maximumNodes int, out any) error {
	tree, err := parseStrict(data, maximumBytes, maximumNodes)
	if err != nil {
		return err
	}
	return DecodeValue(tree, out)
}

// FromJSON is rc2fx_core::from_json: rejects duplicate keys at every depth,
// oversized input and excessive nesting, then decodes strictly.
func FromJSON[T any](text string) (T, error) {
	return FromJSONLimited[T](text, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
}

// FromJSONLimited is rc2fx_core::from_json_limited: strict parsing with
// explicit storage bounds. Callers should prefer FromJSON and its default
// limits.
func FromJSONLimited[T any](text string, maximumBytes, maximumNodes int) (T, error) {
	var out T
	tree, err := parseStrict([]byte(text), maximumBytes, maximumNodes)
	if err != nil {
		return out, err
	}
	err = DecodeValue(tree, &out)
	if err != nil {
		var zero T
		return zero, err
	}
	return out, nil
}

// DecodeValue is serde_json::from_value with the strict rules above.
func DecodeValue(tree Value, out any) error {
	target := reflect.ValueOf(out)
	if target.Kind() != reflect.Pointer || target.IsNil() {
		return CoreError("JSON decode target must be a non-nil pointer")
	}
	return decodeInto(tree, target.Elem())
}

var (
	valueType             = reflect.TypeFor[Value]()
	strictUnmarshalerType = reflect.TypeFor[StrictUnmarshaler]()
	textUnmarshalerType   = reflect.TypeFor[encoding.TextUnmarshaler]()
	allowUnknownType      = reflect.TypeFor[UnknownJSONFieldsAllowed]()
)

func decodeInto(tree Value, target reflect.Value) error {
	t := target.Type()
	if t == valueType {
		target.Set(reflect.ValueOf(tree))
		return nil
	}
	if reflect.PointerTo(t).Implements(strictUnmarshalerType) {
		return target.Addr().Interface().(StrictUnmarshaler).UnmarshalStrictJSON(tree)
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
		if tree.kind == ValueNull {
			target.SetZero()
			return nil
		}
		fresh := reflect.New(t.Elem())
		if err := decodeInto(tree, fresh.Elem()); err != nil {
			return err
		}
		target.Set(fresh)
		return nil
	case reflect.Interface:
		if t.NumMethod() != 0 {
			break
		}
		target.Set(reflect.ValueOf(tree))
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
		if tree.kind != ValueNumber {
			return invalidType(tree, rustTypeName(t))
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
			if err := decodeInto(item, slice.Index(i)); err != nil {
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
			return CoreError("invalid length " + strconv.Itoa(len(items)) + ", expected an array of length " + strconv.Itoa(t.Len()))
		}
		if len(items) > t.Len() {
			return CoreError("invalid length " + strconv.Itoa(len(items)) + ", expected fewer elements in array")
		}
		for i, item := range items {
			if err := decodeInto(item, target.Index(i)); err != nil {
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
			if err := decodeInto(member.Value, item); err != nil {
				return err
			}
			key := reflect.New(t.Key()).Elem()
			key.SetString(member.Key)
			mapping.SetMapIndex(key, item)
		}
		target.Set(mapping)
		return nil
	case reflect.Struct:
		return decodeStruct(tree, target)
	}
	return CoreError("JSON cannot be decoded into " + t.String())
}

func decodeSigned(tree Value, target reflect.Value) error {
	t := target.Type()
	if tree.kind != ValueNumber {
		return invalidType(tree, rustTypeName(t))
	}
	bits := t.Bits()
	low, high := int64(-1)<<(bits-1), int64(1)<<(bits-1)-1
	switch tree.number.kind {
	case numberU64:
		if tree.number.bits > uint64(high) {
			return invalidValue(tree, rustTypeName(t))
		}
		target.SetInt(int64(tree.number.bits))
	case numberI64:
		value := int64(tree.number.bits)
		if value < low {
			return invalidValue(tree, rustTypeName(t))
		}
		target.SetInt(value)
	default:
		return invalidType(tree, rustTypeName(t))
	}
	return nil
}

func decodeUnsigned(tree Value, target reflect.Value) error {
	t := target.Type()
	if tree.kind != ValueNumber {
		return invalidType(tree, rustTypeName(t))
	}
	switch tree.number.kind {
	case numberU64:
		bits := t.Bits()
		if bits < 64 && tree.number.bits > uint64(1)<<bits-1 {
			return invalidValue(tree, rustTypeName(t))
		}
		target.SetUint(tree.number.bits)
	case numberI64:
		return invalidValue(tree, rustTypeName(t))
	default:
		return invalidType(tree, rustTypeName(t))
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

func decodeStruct(tree Value, target reflect.Value) error {
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
	for _, member := range members {
		index, known := info.byName[member.Key]
		if !known {
			if info.allowUnknown {
				continue
			}
			return unknownField(member.Key, info)
		}
		field := info.fields[index]
		if field.nonNull && member.Value.kind == ValueNull {
			return invalidType(member.Value, "a sequence")
		}
		if err := decodeInto(member.Value, target.Field(field.index)); err != nil {
			return err
		}
		seen[index] = true
	}
	for index, field := range info.fields {
		if seen[index] {
			continue
		}
		if !field.optional {
			return CoreError("missing field `" + field.name + "`")
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
	return CoreError(message)
}

// unexpected is serde's description of the value that did not fit.
func unexpected(tree Value) string {
	switch tree.kind {
	case ValueNull:
		return "null"
	case ValueBool:
		if tree.boolean {
			return "boolean `true`"
		}
		return "boolean `false`"
	case ValueNumber:
		switch tree.number.kind {
		case numberF64:
			return "floating point `" + tree.number.String() + "`"
		default:
			return "integer `" + tree.number.String() + "`"
		}
	case ValueString:
		return "string " + strconv.Quote(tree.text)
	case ValueArray:
		return "sequence"
	}
	return "map"
}

func invalidType(tree Value, expected string) error {
	return CoreError("invalid type: " + unexpected(tree) + ", expected " + expected)
}

func invalidValue(tree Value, expected string) error {
	return CoreError("invalid value: " + unexpected(tree) + ", expected " + expected)
}

func rustTypeName(t reflect.Type) string {
	switch t.Kind() {
	case reflect.Int8:
		return "i8"
	case reflect.Int16:
		return "i16"
	case reflect.Int32:
		return "i32"
	case reflect.Int64:
		return "i64"
	case reflect.Int:
		return "isize"
	case reflect.Uint8:
		return "u8"
	case reflect.Uint16:
		return "u16"
	case reflect.Uint32:
		return "u32"
	case reflect.Uint64:
		return "u64"
	case reflect.Uint, reflect.Uintptr:
		return "usize"
	case reflect.Float32:
		return "f32"
	case reflect.Float64:
		return "f64"
	}
	return t.String()
}

// enumUnmarshal is the shared UnmarshalText of the string enums: the text
// must be one of the variants.
func enumUnmarshal[T ~string](target *T, text []byte, variants []T) error {
	for _, variant := range variants {
		if string(variant) == string(text) {
			*target = variant
			return nil
		}
	}
	message := "unknown variant `" + string(text) + "`, expected "
	switch len(variants) {
	case 1:
		message += "`" + string(variants[0]) + "`"
	case 2:
		message += "`" + string(variants[0]) + "` or `" + string(variants[1]) + "`"
	default:
		message += "one of "
		for i, variant := range variants {
			if i > 0 {
				message += ", "
			}
			message += "`" + string(variant) + "`"
		}
	}
	return CoreError(message)
}
