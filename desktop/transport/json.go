package transport

import (
	"bytes"
	"encoding/json"
	"math"
	"strconv"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Untyped JSON is a json.RawMessage, its keys in the order they came. These
// helpers read and build it: field lookups, strings and unsigned integers,
// and compact output escaped as the mixer's serde_json escapes it.

// marshal encodes a typed value: core.Encode writes field order, floats and
// escapes as the mixer's serde_json does. Never pass it a
// json.RawMessage, which it would write as an array of bytes.
func marshal(v any) (json.RawMessage, error) {
	return core.Encode(v)
}

// decode decodes into out with the typed checks of the strict decoder
// (unknown fields, required fields, ranges) over a reply the control client
// has already parsed within its own limits, so no further size limit.
func decode(value json.RawMessage, out any) error {
	return core.DecodeStrictLimited(value, len(value), math.MaxInt, out)
}

// jsonString is s as a JSON string, escaped as serde_json escapes it.
func jsonString(s string) json.RawMessage {
	const hex = "0123456789abcdef"
	out := make([]byte, 0, len(s)+2)
	out = append(out, '"')
	for _, r := range s {
		switch {
		case r == '"':
			out = append(out, '\\', '"')
		case r == '\\':
			out = append(out, '\\', '\\')
		case r == '\b':
			out = append(out, '\\', 'b')
		case r == '\f':
			out = append(out, '\\', 'f')
		case r == '\n':
			out = append(out, '\\', 'n')
		case r == '\r':
			out = append(out, '\\', 'r')
		case r == '\t':
			out = append(out, '\\', 't')
		case r < 0x20:
			out = append(out, '\\', 'u', '0', '0', hex[r>>4], hex[r&0xf])
		default:
			// Invalid UTF-8 is written as U+FFFD.
			out = utf8.AppendRune(out, r)
		}
	}
	return append(out, '"')
}

func jsonUint(n uint64) json.RawMessage {
	return strconv.AppendUint(nil, n, 10)
}

func jsonBool(b bool) json.RawMessage {
	return strconv.AppendBool(nil, b)
}

// member is one field of an object built with object, in order.
type member struct {
	key   string
	value json.RawMessage
}

// object is the json! macro: the members in the order given.
func object(members ...member) json.RawMessage {
	out := []byte{'{'}
	for i, m := range members {
		if i > 0 {
			out = append(out, ',')
		}
		out = append(out, jsonString(m.key)...)
		out = append(out, ':')
		out = append(out, m.value...)
	}
	return append(out, '}')
}

func trimmed(raw json.RawMessage) []byte {
	return bytes.TrimLeft(bytes.TrimRight(raw, " \t\r\n"), " \t\r\n")
}

func isNull(raw json.RawMessage) bool {
	return string(trimmed(raw)) == "null"
}

func isObject(raw json.RawMessage) bool {
	t := trimmed(raw)
	return len(t) > 0 && t[0] == '{'
}

func isArray(raw json.RawMessage) bool {
	t := trimmed(raw)
	return len(t) > 0 && t[0] == '['
}

// members is an object's fields; false for anything else. Duplicate keys were
// refused when the reply was parsed.
func members(raw json.RawMessage) (map[string]json.RawMessage, bool) {
	if !isObject(raw) {
		return nil, false
	}
	var fields map[string]json.RawMessage
	if json.Unmarshal(raw, &fields) != nil {
		return nil, false
	}
	return fields, true
}

// field is raw's member key: false when raw is not an object or lacks key.
func field(raw json.RawMessage, key string) (json.RawMessage, bool) {
	fields, ok := members(raw)
	if !ok {
		return nil, false
	}
	value, ok := fields[key]
	return value, ok
}

// elements is an array's items; false for anything else, null included.
func elements(raw json.RawMessage) ([]json.RawMessage, bool) {
	if !isArray(raw) {
		return nil, false
	}
	var items []json.RawMessage
	if json.Unmarshal(raw, &items) != nil {
		return nil, false
	}
	return items, true
}

// asString is raw as a string; false when it is not a JSON string.
func asString(raw json.RawMessage) (string, bool) {
	t := trimmed(raw)
	if len(t) == 0 || t[0] != '"' {
		return "", false
	}
	var s string
	if json.Unmarshal(t, &s) != nil {
		return "", false
	}
	return s, true
}

// asUint is an integer literal from 0 to 2^64-1; never one with a fraction,
// an exponent or a sign.
func asUint(raw json.RawMessage) (uint64, bool) {
	t := trimmed(raw)
	if len(t) == 0 {
		return 0, false
	}
	for _, b := range t {
		if b < '0' || b > '9' {
			return 0, false
		}
	}
	n, err := strconv.ParseUint(string(t), 10, 64)
	return n, err == nil
}

func stringField(raw json.RawMessage, key string) (string, bool) {
	value, ok := field(raw, key)
	if !ok {
		return "", false
	}
	return asString(value)
}

func uintField(raw json.RawMessage, key string) (uint64, bool) {
	value, ok := field(raw, key)
	if !ok {
		return 0, false
	}
	return asUint(value)
}

// compact is raw as compact JSON.
func compact(raw json.RawMessage) (json.RawMessage, error) {
	var out bytes.Buffer
	if err := json.Compact(&out, raw); err != nil {
		return nil, err
	}
	return out.Bytes(), nil
}

// withMembers is `value[key] = new` for each member: a key already there
// keeps its place, a new one goes last. Null becomes an object first; false
// for any other kind of value.
func withMembers(raw json.RawMessage, set ...member) (json.RawMessage, bool) {
	var fields []member
	if !isNull(raw) {
		decoder := json.NewDecoder(bytes.NewReader(raw))
		if token, err := decoder.Token(); err != nil || token != json.Delim('{') {
			return nil, false
		}
		for decoder.More() {
			token, err := decoder.Token()
			if err != nil {
				return nil, false
			}
			key, ok := token.(string)
			if !ok {
				return nil, false
			}
			var value json.RawMessage
			if err := decoder.Decode(&value); err != nil {
				return nil, false
			}
			value, err = compact(value)
			if err != nil {
				return nil, false
			}
			fields = append(fields, member{key, value})
		}
		if token, err := decoder.Token(); err != nil || token != json.Delim('}') {
			return nil, false
		}
	}
next:
	for _, s := range set {
		for i := range fields {
			if fields[i].key == s.key {
				fields[i].value = s.value
				continue next
			}
		}
		fields = append(fields, s)
	}
	return object(fields...), true
}
