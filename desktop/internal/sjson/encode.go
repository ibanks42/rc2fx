package sjson

import (
	"bytes"
	"encoding"
	"encoding/json"
	"encoding/json/jsontext"
	"math"
	"reflect"
	"slices"
	"strconv"
)

// Marshal serializes value as compact JSON: struct fields in declaration order,
// maps in sorted key order, floats formatted with shortest round-trip digits
// (`.0` on integral values, exponent outside 1e-5..1e16), non-finite floats as
// null, and only `"`, `\`, and control characters escaped. See decode.go for
// tag rules.
func Marshal(value any) ([]byte, error) {
	return encodeAny(value, false)
}

// MarshalPretty serializes value with two-space indentation and no trailing
// newline.
func MarshalPretty(value any) ([]byte, error) {
	return encodeAny(value, true)
}

// String writes a compact JSON string without HTML escaping.
func String(s string) []byte {
	return appendJSONString(nil, s)
}

// AppendString appends s as a compact JSON string without HTML escaping.
func AppendString(dst []byte, s string) []byte {
	return appendJSONString(dst, s)
}

var (
	rawMessageType    = reflect.TypeFor[json.RawMessage]()
	jsonNumberType    = reflect.TypeFor[json.Number]()
	jsonMarshalerType = reflect.TypeFor[json.Marshaler]()
	textMarshalerType = reflect.TypeFor[encoding.TextMarshaler]()
	emptyCheckerType  = reflect.TypeFor[interface{ IsEmpty() bool }]()
)

func newEncoder(buf *bytes.Buffer, pretty bool) *jsontext.Encoder {
	if pretty {
		return jsontext.NewEncoder(buf,
			jsontext.AllowInvalidUTF8(true),
			jsontext.AllowDuplicateNames(true),
			jsontext.EscapeForHTML(false),
			jsontext.EscapeForJS(false),
			jsontext.WithIndent("  "),
		)
	}
	return jsontext.NewEncoder(buf,
		jsontext.AllowInvalidUTF8(true),
		jsontext.AllowDuplicateNames(true),
		jsontext.EscapeForHTML(false),
		jsontext.EscapeForJS(false),
	)
}

func encodeAny(value any, pretty bool) ([]byte, error) {
	var buf bytes.Buffer
	enc := newEncoder(&buf, pretty)
	if err := writeValue(enc, reflect.ValueOf(value)); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte{'\n'}), nil
}

func encodeTree(value Tree, pretty bool) []byte {
	var buf bytes.Buffer
	enc := newEncoder(&buf, pretty)
	_ = writeTree(enc, value)
	return bytes.TrimSuffix(buf.Bytes(), []byte{'\n'})
}

func writeValue(enc *jsontext.Encoder, value reflect.Value) error {
	if !value.IsValid() {
		return enc.WriteToken(jsontext.Null)
	}
	t := value.Type()
	if t == treeType {
		return writeTree(enc, value.Interface().(Tree))
	}
	if t == rawMessageType {
		// Pre-encoded JSON payload: compact or re-indent rather than encoding as bytes.
		raw := value.Bytes()
		if len(raw) == 0 {
			return enc.WriteToken(jsontext.Null)
		}
		return enc.WriteValue(jsontext.Value(raw))
	}
	if t == jsonNumberType {
		return enc.WriteValue(jsontext.Value(value.String()))
	}
	if t.Implements(jsonMarshalerType) && !(t.Kind() == reflect.Pointer && value.IsNil()) {
		data, err := value.Interface().(json.Marshaler).MarshalJSON()
		if err != nil {
			return err
		}
		return enc.WriteValue(jsontext.Value(data))
	}
	if t.Kind() != reflect.Pointer && value.CanAddr() && value.Addr().Type().Implements(jsonMarshalerType) {
		return writeValue(enc, value.Addr())
	}
	if t.Kind() != reflect.String && t.Kind() != reflect.Pointer && t.Implements(textMarshalerType) {
		text, err := value.Interface().(encoding.TextMarshaler).MarshalText()
		if err != nil {
			return err
		}
		return enc.WriteToken(jsontext.String(string(text)))
	}
	switch t.Kind() {
	case reflect.Pointer, reflect.Interface:
		if value.IsNil() {
			return enc.WriteToken(jsontext.Null)
		}
		return writeValue(enc, value.Elem())
	case reflect.Bool:
		return enc.WriteToken(jsontext.Bool(value.Bool()))
	case reflect.String:
		return enc.WriteToken(jsontext.String(value.String()))
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return enc.WriteToken(jsontext.Int(value.Int()))
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return enc.WriteToken(jsontext.Uint(value.Uint()))
	case reflect.Float32:
		return enc.WriteValue(jsontext.Value(appendJSONFloat(nil, value.Float(), 32)))
	case reflect.Float64:
		return enc.WriteValue(jsontext.Value(appendJSONFloat(nil, value.Float(), 64)))
	case reflect.Slice, reflect.Array:
		if err := enc.WriteToken(jsontext.BeginArray); err != nil {
			return err
		}
		for i := 0; i < value.Len(); i++ {
			if err := writeValue(enc, value.Index(i)); err != nil {
				return err
			}
		}
		return enc.WriteToken(jsontext.EndArray)
	case reflect.Map:
		if t.Key().Kind() != reflect.String {
			break
		}
		keys := value.MapKeys()
		slices.SortFunc(keys, func(a, b reflect.Value) int {
			switch {
			case a.String() < b.String():
				return -1
			case a.String() > b.String():
				return 1
			}
			return 0
		})
		if err := enc.WriteToken(jsontext.BeginObject); err != nil {
			return err
		}
		for _, key := range keys {
			if err := enc.WriteToken(jsontext.String(key.String())); err != nil {
				return err
			}
			if err := writeValue(enc, value.MapIndex(key)); err != nil {
				return err
			}
		}
		return enc.WriteToken(jsontext.EndObject)
	case reflect.Struct:
		info := structInfoFor(t)
		if err := enc.WriteToken(jsontext.BeginObject); err != nil {
			return err
		}
		for _, field := range info.fields {
			item := value.Field(field.index)
			if field.omitEmpty && isEmptyField(item) {
				continue
			}
			if err := enc.WriteToken(jsontext.String(field.name)); err != nil {
				return err
			}
			if err := writeValue(enc, item); err != nil {
				return err
			}
		}
		return enc.WriteToken(jsontext.EndObject)
	}
	return Error("JSON cannot encode " + t.String())
}

func isEmptyField(value reflect.Value) bool {
	if value.Type().Implements(emptyCheckerType) {
		if value.Kind() == reflect.Pointer && value.IsNil() {
			return true
		}
		return value.Interface().(interface{ IsEmpty() bool }).IsEmpty()
	}
	return value.IsZero()
}

func writeTree(enc *jsontext.Encoder, value Tree) error {
	switch value.kind {
	case KindNull:
		return enc.WriteToken(jsontext.Null)
	case KindBool:
		return enc.WriteToken(jsontext.Bool(value.boolean))
	case KindNumber:
		return enc.WriteValue(jsontext.Value(value.number.appendJSON(nil)))
	case KindString:
		return enc.WriteToken(jsontext.String(value.text))
	case KindArray:
		if err := enc.WriteToken(jsontext.BeginArray); err != nil {
			return err
		}
		for _, item := range value.items {
			if err := writeTree(enc, item); err != nil {
				return err
			}
		}
		return enc.WriteToken(jsontext.EndArray)
	}
	if err := enc.WriteToken(jsontext.BeginObject); err != nil {
		return err
	}
	for _, member := range value.members {
		if err := enc.WriteToken(jsontext.String(member.Key)); err != nil {
			return err
		}
		if err := writeTree(enc, member.Value); err != nil {
			return err
		}
	}
	return enc.WriteToken(jsontext.EndObject)
}

func appendJSONString(dst []byte, text string) []byte {
	out, _ := jsontext.AppendQuote(dst, text)
	return out
}

// appendJSONFloat formats a float with plain decimal notation for exponents in
// -5..=15 (f32: -6..=12), using `.0` on integral values (`1000.0`, `0.00012`),
// and scientific notation (`d.ddde+X`, `de-X`) outside that range. Zero is
// `0.0` or `-0.0`; NaN and infinities encode as `null`.
func appendJSONFloat(dst []byte, value float64, bitSize int) []byte {
	if math.IsNaN(value) || math.IsInf(value, 0) {
		return append(dst, "null"...)
	}
	if value == 0 {
		if math.Signbit(value) {
			return append(dst, "-0.0"...)
		}
		return append(dst, "0.0"...)
	}
	var scratch [32]byte
	formatted := strconv.AppendFloat(scratch[:0], value, 'e', -1, bitSize)
	if formatted[0] == '-' {
		dst = append(dst, '-')
		formatted = formatted[1:]
	}
	var digits []byte
	exponentAt := 0
	for i, c := range formatted {
		if c == 'e' {
			exponentAt = i
			break
		}
		if c != '.' {
			digits = append(digits, c)
		}
	}
	exponent, _ := strconv.Atoi(string(formatted[exponentAt+1:]))
	low, high := -5, 15
	if bitSize == 32 {
		low, high = -6, 12
	}
	n := len(digits)
	if exponent >= low && exponent <= high {
		switch {
		case n-1 <= exponent:
			dst = append(dst, digits...)
			for i := 0; i < exponent-(n-1); i++ {
				dst = append(dst, '0')
			}
			return append(dst, '.', '0')
		case exponent >= 0:
			dst = append(dst, digits[:exponent+1]...)
			dst = append(dst, '.')
			return append(dst, digits[exponent+1:]...)
		default:
			dst = append(dst, '0', '.')
			for i := 0; i < -exponent-1; i++ {
				dst = append(dst, '0')
			}
			return append(dst, digits...)
		}
	}
	dst = append(dst, digits[0])
	if n > 1 {
		dst = append(dst, '.')
		dst = append(dst, digits[1:]...)
	}
	dst = append(dst, 'e')
	if exponent >= 0 {
		dst = append(dst, '+')
	} else {
		dst = append(dst, '-')
		exponent = -exponent
	}
	return strconv.AppendInt(dst, int64(exponent), 10)
}
