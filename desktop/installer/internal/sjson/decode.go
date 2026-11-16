package sjson

import (
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strconv"
	"strings"
	"sync"
)

// Unmarshal decodes data into v, a non-nil pointer, with serde's rules.
func Unmarshal(data []byte, v any) error {
	rv := reflect.ValueOf(v)
	if rv.Kind() != reflect.Pointer || rv.IsNil() {
		return errors.New("sjson: Unmarshal needs a non-nil pointer")
	}
	n, err := parse(data)
	if err != nil {
		return err
	}
	return decode(n, rv.Elem())
}

// Valid reports whether data is one JSON value by serde_json's grammar.
func Valid(data []byte) error {
	_, err := parse(data)
	return err
}

// Value parses data into a generic tree: nil, bool, json.Number, string,
// []any and map[string]any (a repeated key keeps its last value, as
// serde_json's Value does).
func Value(data []byte) (any, error) {
	n, err := parse(data)
	if err != nil {
		return nil, err
	}
	return generic(n), nil
}

var unmarshalerType = reflect.TypeFor[json.Unmarshaler]()

func describe(n *node) string {
	switch n.kind {
	case kindNull:
		return "null"
	case kindBool:
		return fmt.Sprintf("boolean `%t`", n.b)
	case kindNumber:
		if strings.ContainsAny(n.s, ".eE") {
			return fmt.Sprintf("floating point `%s`", n.s)
		}
		return fmt.Sprintf("integer `%s`", n.s)
	case kindString:
		return fmt.Sprintf("string %q", n.s)
	case kindArray:
		return "sequence"
	default:
		return "map"
	}
}

func invalidType(n *node, expected string) error {
	return fmt.Errorf("invalid type: %s, expected %s", describe(n), expected)
}

func decode(n *node, rv reflect.Value) error {
	if rv.CanAddr() && rv.Addr().Type().Implements(unmarshalerType) {
		return rv.Addr().Interface().(json.Unmarshaler).UnmarshalJSON(n.raw)
	}
	switch rv.Kind() {
	case reflect.Pointer:
		if n.kind == kindNull {
			rv.SetZero()
			return nil
		}
		elem := reflect.New(rv.Type().Elem())
		if err := decode(n, elem.Elem()); err != nil {
			return err
		}
		rv.Set(elem)
		return nil
	case reflect.Interface:
		if rv.NumMethod() != 0 {
			return fmt.Errorf("sjson: cannot decode into %s", rv.Type())
		}
		g := generic(n)
		if g == nil {
			rv.SetZero()
		} else {
			rv.Set(reflect.ValueOf(g))
		}
		return nil
	case reflect.String:
		if n.kind != kindString {
			return invalidType(n, "a string")
		}
		rv.SetString(n.s)
		return nil
	case reflect.Bool:
		if n.kind != kindBool {
			return invalidType(n, "a boolean")
		}
		rv.SetBool(n.b)
		return nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		if n.kind != kindNumber || strings.ContainsAny(n.s, ".eE") {
			return invalidType(n, rv.Type().String())
		}
		v, err := strconv.ParseInt(n.s, 10, rv.Type().Bits())
		if err != nil {
			return fmt.Errorf("invalid value: integer `%s`, expected %s", n.s, rv.Type())
		}
		rv.SetInt(v)
		return nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		if n.kind != kindNumber || strings.ContainsAny(n.s, ".eE") {
			return invalidType(n, rv.Type().String())
		}
		v, err := strconv.ParseUint(n.s, 10, rv.Type().Bits())
		if err != nil {
			return fmt.Errorf("invalid value: integer `%s`, expected %s", n.s, rv.Type())
		}
		rv.SetUint(v)
		return nil
	case reflect.Float32, reflect.Float64:
		if n.kind != kindNumber {
			return invalidType(n, rv.Type().String())
		}
		v, err := strconv.ParseFloat(n.s, rv.Type().Bits())
		if err != nil {
			return fmt.Errorf("invalid number `%s`", n.s)
		}
		rv.SetFloat(v)
		return nil
	case reflect.Slice:
		if n.kind != kindArray {
			return invalidType(n, "a sequence")
		}
		out := reflect.MakeSlice(rv.Type(), len(n.arr), len(n.arr))
		for i, item := range n.arr {
			if err := decode(item, out.Index(i)); err != nil {
				return err
			}
		}
		rv.Set(out)
		return nil
	case reflect.Array:
		if n.kind != kindArray {
			return invalidType(n, "a sequence")
		}
		if len(n.arr) != rv.Len() {
			return fmt.Errorf("invalid length %d, expected an array of length %d", len(n.arr), rv.Len())
		}
		for i, item := range n.arr {
			if err := decode(item, rv.Index(i)); err != nil {
				return err
			}
		}
		return nil
	case reflect.Map:
		if rv.Type().Key().Kind() != reflect.String {
			return fmt.Errorf("sjson: map keys must be strings, not %s", rv.Type().Key())
		}
		if n.kind != kindObject {
			return invalidType(n, "a map")
		}
		out := reflect.MakeMapWithSize(rv.Type(), len(n.keys))
		for i, key := range n.keys {
			v := reflect.New(rv.Type().Elem()).Elem()
			if err := decode(n.vals[i], v); err != nil {
				return err
			}
			out.SetMapIndex(reflect.ValueOf(key).Convert(rv.Type().Key()), v)
		}
		rv.Set(out)
		return nil
	case reflect.Struct:
		return decodeStruct(n, rv)
	}
	return fmt.Errorf("sjson: cannot decode into %s", rv.Type())
}

func decodeStruct(n *node, rv reflect.Value) error {
	info := structInfo(rv.Type())
	if n.kind != kindObject {
		return invalidType(n, "struct "+rv.Type().Name())
	}
	seen := make([]bool, len(info.fields))
	for i, key := range n.keys {
		at, ok := info.byName[key]
		if !ok {
			if info.deny {
				return fmt.Errorf("unknown field `%s`, expected %s", key, info.expected())
			}
			continue
		}
		if seen[at] {
			return fmt.Errorf("duplicate field `%s`", key)
		}
		seen[at] = true
		f := info.fields[at]
		if err := decode(n.vals[i], rv.FieldByIndex(f.index)); err != nil {
			return err
		}
	}
	for i, f := range info.fields {
		if seen[i] || info.defaults || f.defaults {
			continue
		}
		if f.typ.Kind() == reflect.Pointer {
			rv.FieldByIndex(f.index).SetZero()
			continue
		}
		return fmt.Errorf("missing field `%s`", f.name)
	}
	return nil
}

func generic(n *node) any {
	switch n.kind {
	case kindNull:
		return nil
	case kindBool:
		return n.b
	case kindNumber:
		return json.Number(n.s)
	case kindString:
		return n.s
	case kindArray:
		out := make([]any, len(n.arr))
		for i, item := range n.arr {
			out[i] = generic(item)
		}
		return out
	default:
		out := make(map[string]any, len(n.keys))
		for i, key := range n.keys {
			out[key] = generic(n.vals[i])
		}
		return out
	}
}

type field struct {
	name      string
	index     []int
	typ       reflect.Type
	defaults  bool
	omitEmpty bool
}

type typeInfo struct {
	fields   []field
	byName   map[string]int
	deny     bool
	defaults bool
}

func (t *typeInfo) expected() string {
	names := make([]string, len(t.fields))
	for i, f := range t.fields {
		names[i] = "`" + f.name + "`"
	}
	switch len(names) {
	case 0:
		return "no fields"
	case 1:
		return names[0]
	case 2:
		return names[0] + " or " + names[1]
	}
	return "one of " + strings.Join(names, ", ")
}

var infos sync.Map

func structInfo(t reflect.Type) *typeInfo {
	if v, ok := infos.Load(t); ok {
		return v.(*typeInfo)
	}
	info := &typeInfo{byName: map[string]int{}}
	for i := range t.NumField() {
		sf := t.Field(i)
		if sf.Name == "_" {
			for _, option := range strings.Split(sf.Tag.Get("serde"), ",") {
				switch option {
				case "default":
					info.defaults = true
				case "deny_unknown_fields":
					info.deny = true
				}
			}
			continue
		}
		if !sf.IsExported() {
			continue
		}
		tag := sf.Tag.Get("json")
		if tag == "-" {
			continue
		}
		name, options, _ := strings.Cut(tag, ",")
		if name == "" {
			name = sf.Name
		}
		f := field{name: name, index: sf.Index, typ: sf.Type}
		for _, option := range strings.Split(options, ",") {
			switch option {
			case "default":
				f.defaults = true
			case "omitempty":
				f.omitEmpty = true
			}
		}
		info.byName[name] = len(info.fields)
		info.fields = append(info.fields, f)
	}
	v, _ := infos.LoadOrStore(t, info)
	return v.(*typeInfo)
}

// Tagged splits serde's externally tagged enum encoding: a unit variant is
// a string, any other variant an object with exactly one key. payload is
// nil for a unit variant.
func Tagged(data []byte) (variant string, payload []byte, err error) {
	n, err := parse(data)
	if err != nil {
		return "", nil, err
	}
	switch {
	case n.kind == kindString:
		return n.s, nil, nil
	case n.kind == kindObject && len(n.keys) == 1:
		return n.keys[0], n.vals[0].raw, nil
	}
	return "", nil, invalidType(n, "an enum variant")
}
