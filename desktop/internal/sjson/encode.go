package sjson

import (
	"bytes"
	"encoding"
	"encoding/json"
	"math"
	"reflect"
	"slices"
	"strconv"
	"unicode/utf8"
)

// Encode is serde_json::to_vec / to_string: compact JSON, struct fields in
// declaration order, maps in key order, floats as serde_json writes them
// (shortest round-trip digits, `.0` on integral values, exponent outside
// 1e-5..1e16), non-finite floats as null, and only `"`, `\` and control
// characters escaped. See json_decode.go for the tag rules.
func Encode(value any) ([]byte, error) {
	return appendAny(nil, reflect.ValueOf(value), nil)
}

// EncodePretty is serde_json::to_vec_pretty / to_string_pretty: two-space
// indentation and `": "` after keys, no trailing newline.
func EncodePretty(value any) ([]byte, error) {
	return appendAny(nil, reflect.ValueOf(value), &indenter{})
}

type indenter struct{ depth int }

func (in *indenter) newline(dst []byte) []byte {
	dst = append(dst, '\n')
	for i := 0; i < in.depth; i++ {
		dst = append(dst, ' ', ' ')
	}
	return dst
}

// String writes a JSON string with serde_json's escaping.
func String(s string) []byte {
	return appendJSONString(nil, s)
}

// AppendString appends s as a JSON string with serde_json's escaping.
func AppendString(dst []byte, s string) []byte {
	return appendJSONString(dst, s)
}

// Marshal writes v as serde_json::to_vec does.
func Marshal(v any) ([]byte, error) { return Encode(v) }

// MarshalPretty writes v as serde_json::to_vec_pretty does.
func MarshalPretty(v any) ([]byte, error) { return EncodePretty(v) }

var (
	rawMessageType    = reflect.TypeFor[json.RawMessage]()
	jsonNumberType    = reflect.TypeFor[json.Number]()
	jsonMarshalerType = reflect.TypeFor[json.Marshaler]()
	textMarshalerType = reflect.TypeFor[encoding.TextMarshaler]()
	emptyCheckerType  = reflect.TypeFor[interface{ IsEmpty() bool }]()
)

func appendAny(dst []byte, value reflect.Value, pretty *indenter) ([]byte, error) {
	if !value.IsValid() {
		return append(dst, "null"...), nil
	}
	t := value.Type()
	if t == treeType {
		return appendTreeJSON(dst, value.Interface().(Tree), pretty), nil
	}
	if t == rawMessageType {
		// Already JSON, as a serde_json Value inside a typed struct: written
		// as it is (compacted), not as an array of bytes.
		raw := value.Bytes()
		if len(raw) == 0 {
			return append(dst, "null"...), nil
		}
		if pretty != nil {
			tree, err := parseSerde(raw)
			if err != nil {
				return dst, err
			}
			return appendTreeJSON(dst, tree, pretty), nil
		}
		var buf bytes.Buffer
		if err := json.Compact(&buf, raw); err != nil {
			return dst, err
		}
		return append(dst, buf.Bytes()...), nil
	}
	if t == jsonNumberType {
		return append(dst, value.String()...), nil
	}
	if t.Implements(jsonMarshalerType) && !(t.Kind() == reflect.Pointer && value.IsNil()) {
		data, err := value.Interface().(json.Marshaler).MarshalJSON()
		if err != nil {
			return dst, err
		}
		if pretty != nil {
			tree, err := parseSerde(data)
			if err != nil {
				return dst, err
			}
			return appendTreeJSON(dst, tree, pretty), nil
		}
		var buf bytes.Buffer
		if err := json.Compact(&buf, data); err != nil {
			return dst, err
		}
		return append(dst, buf.Bytes()...), nil
	}
	if t.Kind() != reflect.Pointer && value.CanAddr() && value.Addr().Type().Implements(jsonMarshalerType) {
		return appendAny(dst, value.Addr(), pretty)
	}
	if t.Kind() != reflect.String && t.Kind() != reflect.Pointer && t.Implements(textMarshalerType) {
		text, err := value.Interface().(encoding.TextMarshaler).MarshalText()
		if err != nil {
			return dst, err
		}
		return appendJSONString(dst, string(text)), nil
	}
	switch t.Kind() {
	case reflect.Pointer, reflect.Interface:
		if value.IsNil() {
			return append(dst, "null"...), nil
		}
		return appendAny(dst, value.Elem(), pretty)
	case reflect.Bool:
		return strconv.AppendBool(dst, value.Bool()), nil
	case reflect.String:
		return appendJSONString(dst, value.String()), nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return strconv.AppendInt(dst, value.Int(), 10), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return strconv.AppendUint(dst, value.Uint(), 10), nil
	case reflect.Float32:
		return appendJSONFloat(dst, value.Float(), 32), nil
	case reflect.Float64:
		return appendJSONFloat(dst, value.Float(), 64), nil
	case reflect.Slice, reflect.Array:
		n := value.Len()
		if n == 0 {
			return append(dst, "[]"...), nil
		}
		dst = append(dst, '[')
		if pretty != nil {
			pretty.depth++
		}
		for i := 0; i < n; i++ {
			if i > 0 {
				dst = append(dst, ',')
			}
			if pretty != nil {
				dst = pretty.newline(dst)
			}
			var err error
			if dst, err = appendAny(dst, value.Index(i), pretty); err != nil {
				return dst, err
			}
		}
		if pretty != nil {
			pretty.depth--
			dst = pretty.newline(dst)
		}
		return append(dst, ']'), nil
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
		if len(keys) == 0 {
			return append(dst, "{}"...), nil
		}
		dst = append(dst, '{')
		if pretty != nil {
			pretty.depth++
		}
		for i, key := range keys {
			dst = beginMember(dst, i, key.String(), pretty)
			var err error
			if dst, err = appendAny(dst, value.MapIndex(key), pretty); err != nil {
				return dst, err
			}
		}
		if pretty != nil {
			pretty.depth--
			dst = pretty.newline(dst)
		}
		return append(dst, '}'), nil
	case reflect.Struct:
		info := structInfoFor(t)
		written := 0
		for _, field := range info.fields {
			item := value.Field(field.index)
			if field.omitEmpty && isEmptyField(item) {
				continue
			}
			if written == 0 {
				dst = append(dst, '{')
				if pretty != nil {
					pretty.depth++
				}
			}
			dst = beginMember(dst, written, field.name, pretty)
			written++
			var err error
			if dst, err = appendAny(dst, item, pretty); err != nil {
				return dst, err
			}
		}
		if written == 0 {
			return append(dst, "{}"...), nil
		}
		if pretty != nil {
			pretty.depth--
			dst = pretty.newline(dst)
		}
		return append(dst, '}'), nil
	}
	return dst, Error("JSON cannot encode " + t.String())
}

func beginMember(dst []byte, index int, key string, pretty *indenter) []byte {
	if index > 0 {
		dst = append(dst, ',')
	}
	if pretty != nil {
		dst = pretty.newline(dst)
	}
	dst = appendJSONString(dst, key)
	if pretty != nil {
		return append(dst, ':', ' ')
	}
	return append(dst, ':')
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

func appendTreeJSON(dst []byte, value Tree, pretty *indenter) []byte {
	switch value.kind {
	case KindNull:
		return append(dst, "null"...)
	case KindBool:
		return strconv.AppendBool(dst, value.boolean)
	case KindNumber:
		return value.number.appendJSON(dst)
	case KindString:
		return appendJSONString(dst, value.text)
	case KindArray:
		if len(value.items) == 0 {
			return append(dst, "[]"...)
		}
		dst = append(dst, '[')
		if pretty != nil {
			pretty.depth++
		}
		for i, item := range value.items {
			if i > 0 {
				dst = append(dst, ',')
			}
			if pretty != nil {
				dst = pretty.newline(dst)
			}
			dst = appendTreeJSON(dst, item, pretty)
		}
		if pretty != nil {
			pretty.depth--
			dst = pretty.newline(dst)
		}
		return append(dst, ']')
	}
	if len(value.members) == 0 {
		return append(dst, "{}"...)
	}
	dst = append(dst, '{')
	if pretty != nil {
		pretty.depth++
	}
	for i, member := range value.members {
		dst = beginMember(dst, i, member.Key, pretty)
		dst = appendTreeJSON(dst, member.Value, pretty)
	}
	if pretty != nil {
		pretty.depth--
		dst = pretty.newline(dst)
	}
	return append(dst, '}')
}

const hexDigits = "0123456789abcdef"

// appendJSONString escapes as serde_json does: `"` and `\`, the short
// escapes \b \t \n \f \r, other bytes below 0x20 as \u00xx; nothing else
// (not `/`, `<`, DEL or U+2028). Invalid UTF-8, which a Rust string cannot
// hold, is written as U+FFFD.
func appendJSONString(dst []byte, text string) []byte {
	dst = append(dst, '"')
	start := 0
	for i := 0; i < len(text); {
		c := text[i]
		if c >= utf8.RuneSelf {
			r, size := utf8.DecodeRuneInString(text[i:])
			if r == utf8.RuneError && size == 1 {
				dst = append(dst, text[start:i]...)
				dst = append(dst, "\uFFFD"...)
				i++
				start = i
				continue
			}
			i += size
			continue
		}
		if c >= 0x20 && c != '"' && c != '\\' {
			i++
			continue
		}
		dst = append(dst, text[start:i]...)
		switch c {
		case '"':
			dst = append(dst, '\\', '"')
		case '\\':
			dst = append(dst, '\\', '\\')
		case '\b':
			dst = append(dst, '\\', 'b')
		case '\t':
			dst = append(dst, '\\', 't')
		case '\n':
			dst = append(dst, '\\', 'n')
		case '\f':
			dst = append(dst, '\\', 'f')
		case '\r':
			dst = append(dst, '\\', 'r')
		default:
			dst = append(dst, '\\', 'u', '0', '0', hexDigits[c>>4], hexDigits[c&0xf])
		}
		i++
		start = i
	}
	dst = append(dst, text[start:]...)
	return append(dst, '"')
}

// appendJSONFloat writes a float as serde_json 1.0.154 (zmij) does. With
// d.ddd×10^e the shortest round-trip digits: e within -5..=15 (f32:
// -6..=12) is written in full (`1000.0`, `0.00012`), otherwise as `d.ddde+X`
// or `de-X`. Zero is `0.0` or `-0.0`; NaN and infinities are `null`.
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
