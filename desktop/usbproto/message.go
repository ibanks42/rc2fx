package usbproto

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"unicode/utf8"
)

// Request is one control request. Body is raw JSON; nil is no body, which
// is written as `null`, as serde writes `None`.
type Request struct {
	Method string
	Path   string
	Body   json.RawMessage
}

// Reply is one control reply. Body is raw JSON; a decoded reply always has
// one, and nil is written as `null`.
type Reply struct {
	Status uint16
	Body   json.RawMessage
}

// MarshalJSON writes the request exactly as serde_json writes the Rust
// struct: fields in declaration order, compact, serde's string escaping.
// Call it directly for the bytes to send: json.Marshal HTML-escapes what a
// marshaler returns (<, >, &, U+2028, U+2029), which serde does not.
func (r Request) MarshalJSON() ([]byte, error) {
	out := []byte(`{"method":`)
	out = appendString(out, r.Method)
	out = append(out, `,"path":`...)
	out = appendString(out, r.Path)
	out = append(out, `,"body":`...)
	out, err := appendRaw(out, r.Body)
	if err != nil {
		return nil, err
	}
	return append(out, '}'), nil
}

// MarshalJSON writes the reply exactly as serde_json writes the Rust struct.
func (r Reply) MarshalJSON() ([]byte, error) {
	out := []byte(`{"status":`)
	out = strconv.AppendUint(out, uint64(r.Status), 10)
	out = append(out, `,"body":`...)
	out, err := appendRaw(out, r.Body)
	if err != nil {
		return nil, err
	}
	return append(out, '}'), nil
}

// UnmarshalJSON reads a request as serde does with `deny_unknown_fields`:
// unknown and duplicate fields are refused, `method` and `path` are
// required, and a missing or null `body` is no body.
func (r *Request) UnmarshalJSON(data []byte) error {
	fields, err := object(data, "struct Request", []string{"method", "path", "body"})
	if err != nil {
		return err
	}
	var out Request
	if out.Method, err = requiredString(fields, "method"); err != nil {
		return err
	}
	if out.Path, err = requiredString(fields, "path"); err != nil {
		return err
	}
	if body, ok := fields["body"]; ok && !isNull(body) {
		out.Body = bytes.Clone(body)
	}
	*r = out
	return nil
}

// UnmarshalJSON reads a reply as serde does with `deny_unknown_fields`.
// Both fields are required; a null body is the JSON value null.
func (r *Reply) UnmarshalJSON(data []byte) error {
	fields, err := object(data, "struct Reply", []string{"status", "body"})
	if err != nil {
		return err
	}
	raw, ok := fields["status"]
	if !ok {
		return errors.New("missing field `status`")
	}
	status, err := u16(raw)
	if err != nil {
		return err
	}
	body, ok := fields["body"]
	if !ok {
		return errors.New("missing field `body`")
	}
	*r = Reply{Status: status, Body: bytes.Clone(body)}
	return nil
}

// object splits a JSON object into its fields, refusing unknown and
// duplicate keys the way serde's derive does.
func object(data []byte, what string, known []string) (map[string]json.RawMessage, error) {
	if !utf8.Valid(data) || loneSurrogate(data) {
		return nil, errors.New("invalid unicode code point")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	if token != json.Delim('{') {
		return nil, fmt.Errorf("invalid type: %s, expected %s", describe(token), what)
	}
	fields := make(map[string]json.RawMessage, len(known))
	for decoder.More() {
		token, err := decoder.Token()
		if err != nil {
			return nil, err
		}
		key := token.(string)
		found := false
		for _, name := range known {
			found = found || name == key
		}
		if !found {
			return nil, fmt.Errorf("unknown field `%s`, %s", key, expected(known))
		}
		if _, ok := fields[key]; ok {
			return nil, fmt.Errorf("duplicate field `%s`", key)
		}
		var raw json.RawMessage
		if err := decoder.Decode(&raw); err != nil {
			return nil, err
		}
		fields[key] = raw
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	return fields, nil
}

// expected words serde's list of accepted fields.
func expected(known []string) string {
	quoted := func(s string) string { return "`" + s + "`" }
	switch len(known) {
	case 1:
		return "expected " + quoted(known[0])
	case 2:
		return "expected " + quoted(known[0]) + " or " + quoted(known[1])
	}
	text := "expected one of "
	for i, name := range known {
		if i > 0 {
			text += ", "
		}
		text += quoted(name)
	}
	return text
}

func describe(token json.Token) string {
	switch v := token.(type) {
	case nil:
		return "null"
	case bool:
		return "boolean `" + strconv.FormatBool(v) + "`"
	case json.Number:
		return "number"
	case string:
		return "string " + strconv.Quote(v)
	case json.Delim:
		if v == '[' {
			return "sequence"
		}
		return "map"
	}
	return "value"
}

func isNull(raw json.RawMessage) bool {
	return string(bytes.TrimSpace(raw)) == "null"
}

func requiredString(fields map[string]json.RawMessage, name string) (string, error) {
	raw, ok := fields[name]
	if !ok {
		return "", fmt.Errorf("missing field `%s`", name)
	}
	return stringValue(raw)
}

// stringValue reads a JSON string as serde reads a Rust String: null and
// other types are refused. object has already refused text that is not
// Unicode (invalid UTF-8 or an unpaired surrogate escape), which Go would
// otherwise replace.
func stringValue(raw json.RawMessage) (string, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || raw[0] != '"' {
		return "", fmt.Errorf("invalid type: %s, expected a string", describeRaw(raw))
	}
	var text string
	if err := json.Unmarshal(raw, &text); err != nil {
		return "", err
	}
	return text, nil
}

func u16(raw json.RawMessage) (uint16, error) {
	raw = bytes.TrimSpace(raw)
	text := string(raw)
	if len(raw) == 0 || !(raw[0] == '-' || raw[0] >= '0' && raw[0] <= '9') {
		return 0, fmt.Errorf("invalid type: %s, expected u16", describeRaw(raw))
	}
	if bytes.ContainsAny(raw, ".eE") {
		return 0, errors.New("invalid type: floating point, expected u16")
	}
	value, err := strconv.ParseInt(text, 10, 64)
	if err != nil || value < 0 || value > math.MaxUint16 {
		return 0, fmt.Errorf("invalid value: integer `%s`, expected u16", text)
	}
	return uint16(value), nil
}

func describeRaw(raw json.RawMessage) string {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	token, err := decoder.Token()
	if err != nil {
		return "value"
	}
	return describe(token)
}

// loneSurrogate reports whether a JSON string literal escapes half of a
// surrogate pair on its own.
func loneSurrogate(literal []byte) bool {
	hex := func(at int) (rune, bool) {
		if at+6 > len(literal) || literal[at] != '\\' || literal[at+1] != 'u' {
			return 0, false
		}
		v, err := strconv.ParseUint(string(literal[at+2:at+6]), 16, 32)
		return rune(v), err == nil
	}
	for i := 0; i < len(literal); i++ {
		if literal[i] != '\\' {
			continue
		}
		if i+1 < len(literal) && literal[i+1] != 'u' {
			i++
			continue
		}
		r, ok := hex(i)
		if !ok {
			continue
		}
		switch {
		case r >= 0xd800 && r < 0xdc00:
			low, ok := hex(i + 6)
			if !ok || low < 0xdc00 || low >= 0xe000 {
				return true
			}
			i += 11
		case r >= 0xdc00 && r < 0xe000:
			return true
		default:
			i += 5
		}
	}
	return false
}

// appendString writes s as serde_json does: `"` and `\` escaped, the short
// escapes \b \t \n \f \r, other control characters as lowercase \u00XX, and
// everything else, including non-ASCII, as is.
func appendString(out []byte, s string) []byte {
	const hexDigits = "0123456789abcdef"
	out = append(out, '"')
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch c {
		case '"':
			out = append(out, '\\', '"')
		case '\\':
			out = append(out, '\\', '\\')
		case '\b':
			out = append(out, '\\', 'b')
		case '\t':
			out = append(out, '\\', 't')
		case '\n':
			out = append(out, '\\', 'n')
		case '\f':
			out = append(out, '\\', 'f')
		case '\r':
			out = append(out, '\\', 'r')
		default:
			if c < 0x20 {
				out = append(out, '\\', 'u', '0', '0', hexDigits[c>>4], hexDigits[c&15])
			} else {
				out = append(out, c)
			}
		}
	}
	return append(out, '"')
}

// appendRaw appends a raw JSON value compactly. An empty value is null: a
// request's None, or a reply's JSON null.
func appendRaw(out []byte, raw json.RawMessage) ([]byte, error) {
	if len(raw) == 0 {
		return append(out, "null"...), nil
	}
	var compact bytes.Buffer
	if err := json.Compact(&compact, raw); err != nil {
		return nil, err
	}
	return append(out, compact.Bytes()...), nil
}
