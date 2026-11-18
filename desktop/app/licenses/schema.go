// Package licenses is the data side of the license dialog: the bundle's
// shape, which bundle a build embeds, and the embedded sections. The dialog
// itself is UI and lives elsewhere.
package licenses

// The license bundle's shape, shared by the generator (which checks the file
// before embedding it) and the app (which reads the embedded copy).
//
// `{"schemaVersion":1,"sections":[{"title":"…","text":"…"}]}`, UTF-8.
// Unknown fields are refused so a newer generator is noticed, not ignored.

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"
)

// SchemaVersion is the bundle format this app reads.
const SchemaVersion uint64 = 1

// MaxBytes is far above any real bundle; refuses a wrong file passed by
// mistake.
const MaxBytes = 16 * 1024 * 1024

// maxDepth bounds nesting like the core strict decoder.
const maxDepth = 64

// Section is one notice: RC2FX's own license or a third-party one.
type Section struct {
	Title string
	Text  string
}

// Parse reads a license bundle.
func Parse(text string) ([]Section, error) {
	if len(text) > MaxBytes {
		return nil, fmt.Errorf("larger than %d bytes", MaxBytes)
	}
	value, err := decode(text)
	if err != nil {
		return nil, fmt.Errorf("not valid JSON: %w", err)
	}
	object, ok := value.(map[string]any)
	if !ok {
		return nil, errors.New("not a JSON object")
	}
	if key, ok := unknownKey(object, "schemaVersion", "sections"); ok {
		return nil, fmt.Errorf("unknown field `%s`", key)
	}
	switch version, ok := asUint64(object["schemaVersion"]); {
	case !ok:
		return nil, errors.New("missing numeric schemaVersion")
	case version != SchemaVersion:
		return nil, fmt.Errorf("unsupported schemaVersion %d", version)
	}
	sections, ok := object["sections"].([]any)
	if !ok {
		return nil, errors.New("missing sections array")
	}
	if len(sections) == 0 {
		return nil, errors.New("no sections")
	}
	result := make([]Section, 0, len(sections))
	for i, raw := range sections {
		s, ok := raw.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("section %d is not an object", i)
		}
		if key, ok := unknownKey(s, "title", "text"); ok {
			return nil, fmt.Errorf("section %d: unknown field `%s`", i, key)
		}
		field := func(name string) (string, error) {
			value, ok := s[name].(string)
			if !ok {
				return "", fmt.Errorf("section %d: missing string `%s`", i, name)
			}
			return value, nil
		}
		title, err := field("title")
		if err != nil {
			return nil, err
		}
		if strings.TrimSpace(title) == "" {
			return nil, fmt.Errorf("section %d: empty title", i)
		}
		text, err := field("text")
		if err != nil {
			return nil, err
		}
		result = append(result, Section{Title: title, Text: text})
	}
	return result, nil
}

// unknownKey is the first key, in sorted order so the error names the same
// key every time, that is not one of allowed.
func unknownKey(object map[string]any, allowed ...string) (string, bool) {
	keys := make([]string, 0, len(object))
	for key := range object {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	for _, key := range keys {
		if !slices.Contains(allowed, key) {
			return key, true
		}
	}
	return "", false
}

// asUint64 is an integer literal from 0 to 2^64-1; never a fraction,
// exponent or negative number.
func asUint64(value any) (uint64, bool) {
	number, ok := value.(json.Number)
	if !ok {
		return 0, false
	}
	n, err := strconv.ParseUint(number.String(), 10, 64)
	return n, err == nil
}

// decode reads one JSON value strictly: valid UTF-8, no duplicate keys, no
// nesting deeper than maxDepth and nothing after the value. Numbers stay
// json.Number.
func decode(text string) (any, error) {
	if !utf8.ValidString(text) {
		return nil, errors.New("invalid UTF-8")
	}
	decoder := json.NewDecoder(strings.NewReader(text))
	decoder.UseNumber()
	value, err := decodeValue(decoder, 0)
	if err != nil {
		return nil, err
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, errors.New("trailing characters")
	}
	return value, nil
}

func decodeValue(decoder *json.Decoder, depth int) (any, error) {
	if depth > maxDepth {
		return nil, errors.New("JSON is too deeply nested")
	}
	token, err := decoder.Token()
	if err != nil {
		if err == io.EOF {
			return nil, errors.New("EOF while parsing a value")
		}
		return nil, err
	}
	switch t := token.(type) {
	case json.Delim:
		switch t {
		case '{':
			object := map[string]any{}
			for decoder.More() {
				keyToken, err := decoder.Token()
				if err != nil {
					return nil, err
				}
				key, ok := keyToken.(string)
				if !ok {
					return nil, errors.New("object key is not a string")
				}
				if _, seen := object[key]; seen {
					return nil, fmt.Errorf("Duplicate JSON key: %s", key)
				}
				value, err := decodeValue(decoder, depth+1)
				if err != nil {
					return nil, err
				}
				object[key] = value
			}
			if _, err := decoder.Token(); err != nil {
				return nil, err
			}
			return object, nil
		case '[':
			array := []any{}
			for decoder.More() {
				value, err := decodeValue(decoder, depth+1)
				if err != nil {
					return nil, err
				}
				array = append(array, value)
			}
			if _, err := decoder.Token(); err != nil {
				return nil, err
			}
			return array, nil
		default:
			return nil, fmt.Errorf("unexpected %q", t)
		}
	default:
		return t, nil
	}
}
