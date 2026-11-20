package sjson

import (
	"encoding/json"
	"reflect"
)

// Unmarshal decodes data into v, a non-nil pointer, with serde_json's rules:
// exact field names, no duplicate struct fields, missing required fields
// refused unless defaulted, serde:"default,deny_unknown_fields" honored,
// nesting bounded at SerdeRecursionLimit (128).
func Unmarshal(data []byte, v any) error {
	rv := reflect.ValueOf(v)
	if rv.Kind() != reflect.Pointer || rv.IsNil() {
		return Error("sjson: Unmarshal needs a non-nil pointer")
	}
	tree, err := parseSerde(data)
	if err != nil {
		return err
	}
	return decodeInto(tree, rv.Elem(), false)
}

// Valid reports whether data is one JSON value by serde_json's grammar.
func Valid(data []byte) error {
	_, err := parseSerde(data)
	return err
}

// Value parses data into a generic tree: nil, bool, json.Number, string,
// []any and map[string]any (a repeated key keeps its last value, as
// serde_json's Value does).
func Value(data []byte) (any, error) {
	tree, err := parseSerde(data)
	if err != nil {
		return nil, err
	}
	return toGeneric(tree), nil
}

// Tagged splits serde's externally tagged enum encoding: a unit variant is
// a string, any other variant an object with exactly one key. payload is
// nil for a unit variant.
func Tagged(data []byte) (variant string, payload []byte, err error) {
	tree, err := parseSerde(data)
	if err != nil {
		return "", nil, err
	}
	switch {
	case tree.kind == KindString:
		return tree.text, nil, nil
	case tree.kind == KindObject && len(tree.members) == 1:
		return tree.members[0].Key, tree.members[0].Value.raw, nil
	}
	return "", nil, invalidType(tree, "an enum variant")
}

func toGeneric(tree Tree) any {
	switch tree.kind {
	case KindNull:
		return nil
	case KindBool:
		return tree.boolean
	case KindNumber:
		if len(tree.raw) > 0 {
			return json.Number(tree.raw)
		}
		return json.Number(tree.number.String())
	case KindString:
		return tree.text
	case KindArray:
		out := make([]any, len(tree.items))
		for i, item := range tree.items {
			out[i] = toGeneric(item)
		}
		return out
	default:
		out := make(map[string]any, len(tree.members))
		for _, member := range tree.members {
			out[member.Key] = toGeneric(member.Value)
		}
		return out
	}
}
