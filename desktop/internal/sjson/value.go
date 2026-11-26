package sjson

import (
	"math"
	"strconv"
	"strings"
)

// Kind is the type of a JSON Tree.
type Kind uint8

const (
	KindNull Kind = iota
	KindBool
	KindNumber
	KindString
	KindArray
	KindObject
)

type numberKind uint8

const (
	numberU64 numberKind = iota // serde_json PosInt
	numberI64                   // serde_json NegInt: always negative
	numberF64                   // serde_json Float: always finite
)

// Number is a JSON number kept the way serde_json keeps it: a non-negative
// integer, a negative integer, or a finite float. The kind is part of the
// value: 1 and 1.0 are different numbers and serialize differently.
type Number struct {
	kind numberKind
	bits uint64
}

// NumberFromU64 is a non-negative integer.
func NumberFromU64(value uint64) Number { return Number{kind: numberU64, bits: value} }

// NumberFromF64 is a float; false for NaN and infinities, which JSON cannot
// carry.
func NumberFromF64(value float64) (Number, bool) {
	if math.IsNaN(value) || math.IsInf(value, 0) {
		return Number{}, false
	}
	return Number{kind: numberF64, bits: math.Float64bits(value)}, true
}

// IsF64 reports a number written with a fraction or exponent, or one too
// large for an integer.
func (n Number) IsF64() bool { return n.kind == numberF64 }

// AsU64 is serde_json's as_u64: only non-negative integers.
func (n Number) AsU64() (uint64, bool) {
	if n.kind == numberU64 {
		return n.bits, true
	}
	return 0, false
}

// AsF64 is serde_json's as_f64, which always succeeds; large integers round.
func (n Number) AsF64() float64 {
	switch n.kind {
	case numberU64:
		return float64(n.bits)
	case numberI64:
		return float64(int64(n.bits))
	}
	return math.Float64frombits(n.bits)
}

// String is the number as serde_json writes it.
func (n Number) String() string { return string(n.appendJSON(nil)) }

func (n Number) appendJSON(dst []byte) []byte {
	switch n.kind {
	case numberU64:
		return strconv.AppendUint(dst, n.bits, 10)
	case numberI64:
		return strconv.AppendInt(dst, int64(n.bits), 10)
	}
	return appendJSONFloat(dst, math.Float64frombits(n.bits), 64)
}

// Member is one entry of a JSON object, in document order.
type Member struct {
	Key   string
	Value Tree
}

// Error is a strict JSON parse, decode, or encode failure.
type Error string

func (e Error) Error() string { return string(e) }

// Tree is an untyped JSON tree, the port of serde_json::Value. Objects keep
// their members in document (insertion) order; the strict parser never
// produces duplicate keys. The zero Tree is null.
type Tree struct {
	kind    Kind
	boolean bool
	number  Number
	text    string
	items   []Tree
	members []Member
	raw     []byte
}

// Raw returns the original JSON slice this node was parsed from, or nil for
// programmatically constructed trees.
func (v Tree) Raw() []byte { return v.raw }

// NullTree is JSON null.
func NullTree() Tree { return Tree{} }

// BoolTree is true or false.
func BoolTree(value bool) Tree { return Tree{kind: KindBool, boolean: value} }

// StringTree is a JSON string.
func StringTree(value string) Tree { return Tree{kind: KindString, text: value} }

// NumberTree wraps a Number.
func NumberTree(value Number) Tree { return Tree{kind: KindNumber, number: value} }

// U64Tree is a non-negative integer.
func U64Tree(value uint64) Tree { return NumberTree(NumberFromU64(value)) }

// F64Tree is a float; NaN and infinities become null, as in serde_json's
// `json!`.
func F64Tree(value float64) Tree {
	n, ok := NumberFromF64(value)
	if !ok {
		return NullTree()
	}
	return NumberTree(n)
}

// ArrayTree is a JSON array of the items given.
func ArrayTree(items ...Tree) Tree {
	if items == nil {
		items = []Tree{}
	}
	return Tree{kind: KindArray, items: items}
}

// ObjectTree is a JSON object of the members given, in order. Later
// duplicates replace earlier ones.
func ObjectTree(members ...Member) Tree {
	object := Tree{kind: KindObject, members: []Member{}}
	for _, member := range members {
		object.Set(member.Key, member.Value)
	}
	return object
}

// Kind is the type of the value.
func (v Tree) Kind() Kind { return v.kind }

// IsNull reports JSON null.
func (v Tree) IsNull() bool { return v.kind == KindNull }

// IsString reports a JSON string.
func (v Tree) IsString() bool { return v.kind == KindString }

// AsBool returns the boolean value if v is a boolean.
func (v Tree) AsBool() (bool, bool) { return v.boolean, v.kind == KindBool }

// AsString returns the string value if v is a string.
func (v Tree) AsString() (string, bool) { return v.text, v.kind == KindString }

// AsNumber returns the Number if v is a number.
func (v Tree) AsNumber() (Number, bool) { return v.number, v.kind == KindNumber }

// AsF64 returns any JSON number as a float64.
func (v Tree) AsF64() (float64, bool) {
	if v.kind != KindNumber {
		return 0, false
	}
	return v.number.AsF64(), true
}

// AsU64 returns the value as a uint64 if v is a non-negative integer.
func (v Tree) AsU64() (uint64, bool) {
	if v.kind != KindNumber {
		return 0, false
	}
	return v.number.AsU64()
}

// AsArray returns the array elements if v is an array. The slice aliases v.
func (v Tree) AsArray() ([]Tree, bool) { return v.items, v.kind == KindArray }

// AsObject returns the object members in document order if v is an object.
// The slice aliases v.
func (v Tree) AsObject() ([]Member, bool) { return v.members, v.kind == KindObject }

// Len is the number of array items or object members, else 0.
func (v Tree) Len() int {
	switch v.kind {
	case KindArray:
		return len(v.items)
	case KindObject:
		return len(v.members)
	}
	return 0
}

// Keys lists an object's keys in order.
func (v Tree) Keys() []string {
	keys := make([]string, 0, len(v.members))
	for _, member := range v.members {
		keys = append(keys, member.Key)
	}
	return keys
}

// Get is an object member.
func (v Tree) Get(key string) (Tree, bool) {
	if v.kind != KindObject {
		return Tree{}, false
	}
	for _, member := range v.members {
		if member.Key == key {
			return member.Value, true
		}
	}
	return Tree{}, false
}

// Index is an array item.
func (v Tree) Index(index int) (Tree, bool) {
	if v.kind != KindArray || index < 0 || index >= len(v.items) {
		return Tree{}, false
	}
	return v.items[index], true
}

// GetMut is a pointer to an object member, nil if absent.
func (v *Tree) GetMut(key string) *Tree {
	if v.kind != KindObject {
		return nil
	}
	for i := range v.members {
		if v.members[i].Key == key {
			return &v.members[i].Value
		}
	}
	return nil
}

// IndexMut is a pointer to an array item, nil if absent.
func (v *Tree) IndexMut(index int) *Tree {
	if v.kind != KindArray || index < 0 || index >= len(v.items) {
		return nil
	}
	return &v.items[index]
}

// Set replaces an object member in place or appends it. Null becomes an
// empty object first, as with serde_json's IndexMut; any other kind panics.
func (v *Tree) Set(key string, value Tree) {
	if v.kind == KindNull {
		*v = Tree{kind: KindObject, members: []Member{}}
	}
	if v.kind != KindObject {
		panic("sjson.Tree.Set on a JSON value that is not an object")
	}
	for i := range v.members {
		if v.members[i].Key == key {
			v.members[i].Value = value
			return
		}
	}
	v.members = append(v.members, Member{Key: key, Value: value})
}

// Remove deletes an object member, keeping the others in order.
func (v *Tree) Remove(key string) (Tree, bool) {
	if v.kind != KindObject {
		return Tree{}, false
	}
	for i := range v.members {
		if v.members[i].Key == key {
			removed := v.members[i].Value
			v.members = append(v.members[:i:i], v.members[i+1:]...)
			return removed, true
		}
	}
	return Tree{}, false
}

// Push appends an array item. Any other kind panics.
func (v *Tree) Push(value Tree) {
	if v.kind != KindArray {
		panic("sjson.Tree.Push on a JSON value that is not an array")
	}
	v.items = append(v.items, value)
}

// Pointer resolves an RFC 6901 JSON pointer ("" is the value itself,
// "/a/0/b~1c" walks object keys and array indices), the port of serde_json's
// pointer_mut. Nil if the path does not exist. The result aliases v.
func (v *Tree) Pointer(pointer string) *Tree {
	if pointer == "" {
		return v
	}
	if !strings.HasPrefix(pointer, "/") {
		return nil
	}
	target := v
	for _, token := range strings.Split(pointer[1:], "/") {
		token = strings.ReplaceAll(strings.ReplaceAll(token, "~1", "/"), "~0", "~")
		switch target.kind {
		case KindObject:
			target = target.GetMut(token)
		case KindArray:
			index, ok := pointerIndex(token)
			if !ok {
				return nil
			}
			target = target.IndexMut(index)
		default:
			return nil
		}
		if target == nil {
			return nil
		}
	}
	return target
}

func pointerIndex(token string) (int, bool) {
	if token == "" || token[0] == '+' || (token[0] == '0' && len(token) > 1) {
		return 0, false
	}
	index, err := strconv.Atoi(token)
	if err != nil || index < 0 {
		return 0, false
	}
	return index, true
}

// Clone is a deep copy.
func (v Tree) Clone() Tree {
	switch v.kind {
	case KindArray:
		items := make([]Tree, len(v.items))
		for i, item := range v.items {
			items[i] = item.Clone()
		}
		v.items = items
	case KindObject:
		members := make([]Member, len(v.members))
		for i, member := range v.members {
			members[i] = Member{Key: member.Key, Value: member.Value.Clone()}
		}
		v.members = members
	}
	return v
}

// Equal is serde_json's Tree equality: object member order does not
// matter, number kinds do (1 is not 1.0).
func (v Tree) Equal(other Tree) bool {
	if v.kind != other.kind {
		return false
	}
	switch v.kind {
	case KindNull:
		return true
	case KindBool:
		return v.boolean == other.boolean
	case KindNumber:
		if v.number.kind != other.number.kind {
			return false
		}
		if v.number.kind == numberF64 {
			return math.Float64frombits(v.number.bits) == math.Float64frombits(other.number.bits)
		}
		return v.number.bits == other.number.bits
	case KindString:
		return v.text == other.text
	case KindArray:
		if len(v.items) != len(other.items) {
			return false
		}
		for i := range v.items {
			if !v.items[i].Equal(other.items[i]) {
				return false
			}
		}
		return true
	case KindObject:
		if len(v.members) != len(other.members) {
			return false
		}
		for _, member := range v.members {
			match, ok := other.Get(member.Key)
			if !ok || !member.Value.Equal(match) {
				return false
			}
		}
		return true
	}
	return false
}

// String is the compact JSON text, serde_json's Display / to_string.
func (v Tree) String() string { return string(encodeTree(v, false)) }

// Pretty is serde_json's to_string_pretty: two-space indentation.
func (v Tree) Pretty() string {
	return string(encodeTree(v, true))
}

// PrettyCompactArrays formats with two-space indentation while keeping arrays
// of scalar values (numbers, strings, booleans, nulls) on a single line.
func (v Tree) PrettyCompactArrays() string {
	var out []byte
	out = appendPrettyCompact(out, v, 0)
	return string(out)
}

func isScalarArray(items []Tree) bool {
	for _, item := range items {
		if item.kind == KindArray || item.kind == KindObject {
			return false
		}
	}
	return true
}

func appendIndent(dst []byte, depth int) []byte {
	for range depth {
		dst = append(dst, ' ', ' ')
	}
	return dst
}

func appendPrettyCompact(dst []byte, v Tree, depth int) []byte {
	switch v.kind {
	case KindNull:
		return append(dst, "null"...)
	case KindBool:
		if v.boolean {
			return append(dst, "true"...)
		}
		return append(dst, "false"...)
	case KindNumber:
		return v.number.appendJSON(dst)
	case KindString:
		return appendJSONString(dst, v.text)
	case KindArray:
		if len(v.items) == 0 {
			return append(dst, "[]"...)
		}
		if isScalarArray(v.items) {
			dst = append(dst, '[')
			for i, item := range v.items {
				if i > 0 {
					dst = append(dst, ", "...)
				}
				dst = appendPrettyCompact(dst, item, 0)
			}
			return append(dst, ']')
		}
		dst = append(dst, "[\n"...)
		for i, item := range v.items {
			if i > 0 {
				dst = append(dst, ",\n"...)
			}
			dst = appendIndent(dst, depth+1)
			dst = appendPrettyCompact(dst, item, depth+1)
		}
		dst = append(dst, '\n')
		dst = appendIndent(dst, depth)
		return append(dst, ']')
	case KindObject:
		if len(v.members) == 0 {
			return append(dst, "{}"...)
		}
		dst = append(dst, "{\n"...)
		for i, member := range v.members {
			if i > 0 {
				dst = append(dst, ",\n"...)
			}
			dst = appendIndent(dst, depth+1)
			dst = appendJSONString(dst, member.Key)
			dst = append(dst, ": "...)
			dst = appendPrettyCompact(dst, member.Value, depth+1)
		}
		dst = append(dst, '\n')
		dst = appendIndent(dst, depth)
		return append(dst, '}')
	}
	return append(dst, "null"...)
}
