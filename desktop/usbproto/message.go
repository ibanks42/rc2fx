package usbproto

import (
	"bytes"
	"encoding/json"
	"strconv"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// Request is one control request. Body is raw JSON; nil is no body, which
// is written as `null`.
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

// MarshalJSON encodes the request in declaration field order, compactly, and
// without HTML escaping. Call it directly rather than json.Marshal, which
// HTML-escapes <, >, &, U+2028, and U+2029.
func (r Request) MarshalJSON() ([]byte, error) {
	out := []byte(`{"method":`)
	out = sjson.AppendString(out, r.Method)
	out = append(out, `,"path":`...)
	out = sjson.AppendString(out, r.Path)
	out = append(out, `,"body":`...)
	out, err := appendRaw(out, r.Body)
	if err != nil {
		return nil, err
	}
	return append(out, '}'), nil
}

// MarshalJSON writes the reply in compact declaration field order.
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

// UnmarshalJSON reads a request strictly: unknown and duplicate fields are
// refused, `method` and `path` are required, and a missing or null `body` is
// no body.
func (r *Request) UnmarshalJSON(data []byte) error {
	var wire struct {
		_      struct{}         `serde:"deny_unknown_fields"`
		Method string           `json:"method"`
		Path   string           `json:"path"`
		Body   *json.RawMessage `json:"body"`
	}
	if err := sjson.Unmarshal(data, &wire); err != nil {
		return err
	}
	var body json.RawMessage
	if wire.Body != nil {
		body = *wire.Body
	}
	*r = Request{Method: wire.Method, Path: wire.Path, Body: body}
	return nil
}

// UnmarshalJSON reads a reply strictly, refusing unknown and duplicate fields.
// Both fields are required; a null body is the JSON value null.
func (r *Reply) UnmarshalJSON(data []byte) error {
	var wire struct {
		_      struct{}        `serde:"deny_unknown_fields"`
		Status uint16          `json:"status"`
		Body   json.RawMessage `json:"body"`
	}
	if err := sjson.Unmarshal(data, &wire); err != nil {
		return err
	}
	*r = Reply{Status: wire.Status, Body: wire.Body}
	return nil
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
