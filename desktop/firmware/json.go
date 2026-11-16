package firmware

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strconv"
	"unicode/utf8"
)

// The Rust reads the catalog with serde_json and `deny_unknown_fields`.
// encoding/json alone accepts unknown, duplicate and missing fields, null
// for strings and numbers, and unpaired surrogates, so the catalog is read
// field by field here. Messages follow serde's wording without its
// line/column suffix.
//
// TODO(port): move onto core's strict decoder (core.DecodeStrict, see
// docs/developer/desktop-go.md) once it offers typed field access, which is
// all this file adds on top of it.

type fields map[string]json.RawMessage

func decodeCatalog(text string) (*Catalog, error) {
	data := []byte(text)
	if !utf8.Valid(data) || loneSurrogate(data) {
		return nil, errors.New("invalid unicode code point")
	}
	if !json.Valid(data) {
		// Let encoding/json word the syntax error.
		var v any
		if err := json.Unmarshal(data, &v); err != nil {
			return nil, err
		}
		return nil, errors.New("invalid JSON")
	}
	f, err := object(data, "struct Catalog", []string{"schemaVersion", "profiles"})
	if err != nil {
		return nil, err
	}
	var c Catalog
	if c.SchemaVersion, err = f.u32("schemaVersion"); err != nil {
		return nil, err
	}
	raw, err := f.required("profiles")
	if err != nil {
		return nil, err
	}
	items, err := array(raw, "a sequence")
	if err != nil {
		return nil, err
	}
	c.Profiles = make([]Profile, 0, len(items))
	for _, item := range items {
		p, err := decodeProfile(item)
		if err != nil {
			return nil, err
		}
		c.Profiles = append(c.Profiles, p)
	}
	return &c, nil
}

func decodeProfile(data json.RawMessage) (Profile, error) {
	var p Profile
	f, err := object(data, "struct Profile", []string{"minimumAppVersion", "id", "model", "version",
		"mixerSha256", "launcherSha256", "archiveSha256", "archiveMd5", "runtime", "notes"})
	if err != nil {
		return p, err
	}
	// `#[serde(default)]`: missing is empty, null is still refused.
	if raw, ok := f["minimumAppVersion"]; ok {
		if p.MinimumAppVersion, err = stringValue(raw); err != nil {
			return p, err
		}
	}
	for _, field := range []struct {
		name string
		to   *string
	}{
		{"id", &p.ID}, {"model", &p.Model}, {"version", &p.Version},
		{"mixerSha256", &p.MixerSHA256}, {"launcherSha256", &p.LauncherSHA256},
		{"archiveSha256", &p.ArchiveSHA256}, {"archiveMd5", &p.ArchiveMD5},
	} {
		if *field.to, err = f.str(field.name); err != nil {
			return p, err
		}
	}
	// An Option: missing or null is None.
	if raw, ok := f["runtime"]; ok && !isNull(raw) {
		runtime, err := decodeRuntime(raw)
		if err != nil {
			return p, err
		}
		p.Runtime = &runtime
	}
	if p.Notes, err = f.str("notes"); err != nil {
		return p, err
	}
	return p, nil
}

func decodeRuntime(data json.RawMessage) (Runtime, error) {
	var r Runtime
	f, err := object(data, "struct Runtime", []string{"callbackAbi", "routeLayout", "sourceEnum",
		"sampleRate", "blockFrames", "showPath", "callback", "jobLifetime"})
	if err != nil {
		return r, err
	}
	if r.CallbackABI, err = f.str("callbackAbi"); err != nil {
		return r, err
	}
	if r.RouteLayout, err = f.str("routeLayout"); err != nil {
		return r, err
	}
	if r.SourceEnum, err = f.str("sourceEnum"); err != nil {
		return r, err
	}
	if r.SampleRate, err = f.u32("sampleRate"); err != nil {
		return r, err
	}
	if r.BlockFrames, err = f.u32("blockFrames"); err != nil {
		return r, err
	}
	if r.ShowPath, err = f.str("showPath"); err != nil {
		return r, err
	}
	if r.Callback, err = f.site("callback"); err != nil {
		return r, err
	}
	raw, err := f.required("jobLifetime")
	if err != nil {
		return r, err
	}
	j, err := object(raw, "struct JobLifetime", []string{"abi", "enqueue", "manager", "invoker", "body"})
	if err != nil {
		return r, err
	}
	if r.JobLifetime.ABI, err = j.str("abi"); err != nil {
		return r, err
	}
	for _, site := range []struct {
		name string
		to   *Site
	}{{"enqueue", &r.JobLifetime.Enqueue}, {"manager", &r.JobLifetime.Manager}, {"invoker", &r.JobLifetime.Invoker}, {"body", &r.JobLifetime.Body}} {
		if *site.to, err = j.site(site.name); err != nil {
			return r, err
		}
	}
	return r, nil
}

func (f fields) site(name string) (Site, error) {
	var s Site
	raw, err := f.required(name)
	if err != nil {
		return s, err
	}
	g, err := object(raw, "struct Site", []string{"offset", "expected"})
	if err != nil {
		return s, err
	}
	if s.Offset, err = g.str("offset"); err != nil {
		return s, err
	}
	if s.Expected, err = g.str("expected"); err != nil {
		return s, err
	}
	return s, nil
}

func (f fields) required(name string) (json.RawMessage, error) {
	raw, ok := f[name]
	if !ok {
		return nil, fmt.Errorf("missing field `%s`", name)
	}
	return raw, nil
}

func (f fields) str(name string) (string, error) {
	raw, err := f.required(name)
	if err != nil {
		return "", err
	}
	return stringValue(raw)
}

func (f fields) u32(name string) (uint32, error) {
	raw, err := f.required(name)
	if err != nil {
		return 0, err
	}
	raw = bytes.TrimSpace(raw)
	text := string(raw)
	if len(raw) == 0 || !(raw[0] == '-' || raw[0] >= '0' && raw[0] <= '9') {
		return 0, fmt.Errorf("invalid type: %s, expected u32", describeRaw(raw))
	}
	if bytes.ContainsAny(raw, ".eE") {
		return 0, fmt.Errorf("invalid type: floating point `%s`, expected u32", text)
	}
	if text == "-0" {
		// serde_json reads -0 as the integer 0.
		return 0, nil
	}
	if value, err := strconv.ParseUint(text, 10, 64); err == nil && value <= math.MaxUint32 {
		return uint32(value), nil
	}
	return 0, fmt.Errorf("invalid value: integer `%s`, expected u32", text)
}

// object splits a JSON object into its fields, refusing unknown and
// duplicate keys the way serde's derive does.
func object(data json.RawMessage, what string, known []string) (fields, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	token, err := decoder.Token()
	if err != nil {
		return nil, err
	}
	if token != json.Delim('{') {
		return nil, fmt.Errorf("invalid type: %s, expected %s", describe(token), what)
	}
	f := make(fields, len(known))
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
		if _, ok := f[key]; ok {
			return nil, fmt.Errorf("duplicate field `%s`", key)
		}
		var raw json.RawMessage
		if err := decoder.Decode(&raw); err != nil {
			return nil, err
		}
		f[key] = raw
	}
	if _, err := decoder.Token(); err != nil {
		return nil, err
	}
	return f, nil
}

func array(data json.RawMessage, what string) ([]json.RawMessage, error) {
	data = bytes.TrimSpace(data)
	if len(data) == 0 || data[0] != '[' {
		return nil, fmt.Errorf("invalid type: %s, expected %s", describeRaw(data), what)
	}
	var items []json.RawMessage
	if err := json.Unmarshal(data, &items); err != nil {
		return nil, err
	}
	return items, nil
}

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

func describeRaw(raw json.RawMessage) string {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	token, err := decoder.Token()
	if err != nil {
		return "value"
	}
	return describe(token)
}

func isNull(raw json.RawMessage) bool {
	return string(bytes.TrimSpace(raw)) == "null"
}

// stringValue reads a JSON string as serde reads a Rust String: null and
// other types are refused.
func stringValue(raw json.RawMessage) (string, error) {
	raw = bytes.TrimSpace(raw)
	if len(raw) == 0 || raw[0] != '"' {
		return "", fmt.Errorf("invalid type: %s, expected a string", describeRaw(raw))
	}
	if !utf8.Valid(raw) || loneSurrogate(raw) {
		return "", errors.New("invalid unicode code point")
	}
	var text string
	if err := json.Unmarshal(raw, &text); err != nil {
		return "", err
	}
	return text, nil
}

// loneSurrogate reports whether JSON text escapes half of a surrogate pair
// on its own, which serde refuses and Go would replace.
func loneSurrogate(text []byte) bool {
	hex := func(at int) (rune, bool) {
		if at+6 > len(text) || text[at] != '\\' || text[at+1] != 'u' {
			return 0, false
		}
		v, err := strconv.ParseUint(string(text[at+2:at+6]), 16, 32)
		return rune(v), err == nil
	}
	for i := 0; i < len(text); i++ {
		if text[i] != '\\' {
			continue
		}
		if i+1 < len(text) && text[i+1] != 'u' {
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
