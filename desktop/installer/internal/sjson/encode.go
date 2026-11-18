package sjson

import (
	"bytes"
	"encoding/json"
	"fmt"
	"math"
	"reflect"
	"slices"
	"strconv"
	"unicode/utf8"
)

// Marshal writes v as serde_json::to_vec does.
func Marshal(v any) ([]byte, error) {
	var out []byte
	out, err := encode(out, reflect.ValueOf(v))
	if err != nil {
		return nil, err
	}
	return out, nil
}

// MarshalPretty writes v as serde_json::to_vec_pretty does.
func MarshalPretty(v any) ([]byte, error) {
	compact, err := Marshal(v)
	if err != nil {
		return nil, err
	}
	var out bytes.Buffer
	if err := json.Indent(&out, compact, "", "  "); err != nil {
		return nil, err
	}
	return out.Bytes(), nil
}

// String writes a JSON string with serde_json's escaping.
func String(s string) []byte {
	return appendString(nil, s)
}

var marshalerType = reflect.TypeFor[json.Marshaler]()

func encode(out []byte, rv reflect.Value) ([]byte, error) {
	if !rv.IsValid() {
		return append(out, "null"...), nil
	}
	if rv.Type().Implements(marshalerType) && !(rv.Kind() == reflect.Pointer && rv.IsNil()) {
		data, err := rv.Interface().(json.Marshaler).MarshalJSON()
		if err != nil {
			return nil, err
		}
		var compact bytes.Buffer
		if err := json.Compact(&compact, data); err != nil {
			return nil, err
		}
		return append(out, compact.Bytes()...), nil
	}
	if rv.Kind() != reflect.Pointer && rv.CanAddr() && rv.Addr().Type().Implements(marshalerType) {
		return encode(out, rv.Addr())
	}
	switch rv.Kind() {
	case reflect.Pointer, reflect.Interface:
		if rv.IsNil() {
			return append(out, "null"...), nil
		}
		return encode(out, rv.Elem())
	case reflect.String:
		if rv.Type() == reflect.TypeFor[json.Number]() {
			return append(out, rv.String()...), nil
		}
		return appendString(out, rv.String()), nil
	case reflect.Bool:
		return strconv.AppendBool(out, rv.Bool()), nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.AppendInt(out, rv.Int(), 10), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return strconv.AppendUint(out, rv.Uint(), 10), nil
	case reflect.Float32, reflect.Float64:
		f := rv.Float()
		if math.IsNaN(f) || math.IsInf(f, 0) {
			return append(out, "null"...), nil
		}
		text := strconv.FormatFloat(f, 'g', -1, rv.Type().Bits())
		if !bytes.ContainsAny([]byte(text), ".e") {
			text += ".0"
		}
		return append(out, text...), nil
	case reflect.Slice, reflect.Array:
		out = append(out, '[')
		for i := range rv.Len() {
			if i > 0 {
				out = append(out, ',')
			}
			var err error
			if out, err = encode(out, rv.Index(i)); err != nil {
				return nil, err
			}
		}
		return append(out, ']'), nil
	case reflect.Map:
		keys := rv.MapKeys()
		if rv.Type().Key().Kind() != reflect.String {
			return nil, fmt.Errorf("sjson: map keys must be strings, not %s", rv.Type().Key())
		}
		slices.SortFunc(keys, func(a, b reflect.Value) int {
			return bytes.Compare([]byte(a.String()), []byte(b.String()))
		})
		out = append(out, '{')
		for i, key := range keys {
			if i > 0 {
				out = append(out, ',')
			}
			out = appendString(out, key.String())
			out = append(out, ':')
			var err error
			if out, err = encode(out, rv.MapIndex(key)); err != nil {
				return nil, err
			}
		}
		return append(out, '}'), nil
	case reflect.Struct:
		info := structInfo(rv.Type())
		out = append(out, '{')
		first := true
		for _, f := range info.fields {
			v := rv.FieldByIndex(f.index)
			if f.omitEmpty && (v.Kind() == reflect.Pointer || v.Kind() == reflect.Interface) && v.IsNil() {
				continue
			}
			if !first {
				out = append(out, ',')
			}
			first = false
			out = appendString(out, f.name)
			out = append(out, ':')
			var err error
			if out, err = encode(out, v); err != nil {
				return nil, err
			}
		}
		return append(out, '}'), nil
	}
	return nil, fmt.Errorf("sjson: cannot encode %s", rv.Type())
}

const hexDigits = "0123456789abcdef"

func appendString(out []byte, s string) []byte {
	out = append(out, '"')
	for i := 0; i < len(s); {
		c := s[i]
		if c >= 0x80 {
			r, size := utf8.DecodeRuneInString(s[i:])
			if r == utf8.RuneError && size == 1 {
				// Invalid UTF-8 is written as U+FFFD.
				out = append(out, "�"...)
			} else {
				out = append(out, s[i:i+size]...)
			}
			i += size
			continue
		}
		switch {
		case c == '"':
			out = append(out, '\\', '"')
		case c == '\\':
			out = append(out, '\\', '\\')
		case c == '\b':
			out = append(out, '\\', 'b')
		case c == '\f':
			out = append(out, '\\', 'f')
		case c == '\n':
			out = append(out, '\\', 'n')
		case c == '\r':
			out = append(out, '\\', 'r')
		case c == '\t':
			out = append(out, '\\', 't')
		case c < 0x20:
			out = append(out, '\\', 'u', '0', '0', hexDigits[c>>4], hexDigits[c&15])
		default:
			out = append(out, c)
		}
		i++
	}
	return append(out, '"')
}
