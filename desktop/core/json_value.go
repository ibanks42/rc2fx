package core

import (
	"math"
	"strconv"
	"strings"
)

// ValueKind is the type of a JSON Value.
type ValueKind uint8

const (
	ValueNull ValueKind = iota
	ValueBool
	ValueNumber
	ValueString
	ValueArray
	ValueObject
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

// NumberFromI64 is an integer; a non-negative one is stored as serde_json
// stores it, as an unsigned integer.
func NumberFromI64(value int64) Number {
	if value >= 0 {
		return Number{kind: numberU64, bits: uint64(value)}
	}
	return Number{kind: numberI64, bits: uint64(value)}
}

// NumberFromF64 is a float; false for NaN and infinities, which JSON cannot
// carry.
func NumberFromF64(value float64) (Number, bool) {
	if math.IsNaN(value) || math.IsInf(value, 0) {
		return Number{}, false
	}
	return Number{kind: numberF64, bits: math.Float64bits(value)}, true
}

// IsU64 reports a non-negative integer.
func (n Number) IsU64() bool { return n.kind == numberU64 }

// IsI64 reports an integer that fits in int64.
func (n Number) IsI64() bool {
	return n.kind == numberI64 || (n.kind == numberU64 && n.bits <= math.MaxInt64)
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

// AsI64 is serde_json's as_i64: integers that fit in int64.
func (n Number) AsI64() (int64, bool) {
	switch {
	case n.kind == numberI64:
		return int64(n.bits), true
	case n.kind == numberU64 && n.bits <= math.MaxInt64:
		return int64(n.bits), true
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
	Value Value
}

// Value is an untyped JSON tree, the port of serde_json::Value. Objects keep
// their members in document (insertion) order; the strict parser never
// produces duplicate keys. The zero Value is null.
type Value struct {
	kind    ValueKind
	boolean bool
	number  Number
	text    string
	items   []Value
	members []Member
}

// NullValue is JSON null.
func NullValue() Value { return Value{} }

// BoolValue is true or false.
func BoolValue(value bool) Value { return Value{kind: ValueBool, boolean: value} }

// StringValue is a JSON string.
func StringValue(value string) Value { return Value{kind: ValueString, text: value} }

// NumberValue wraps a Number.
func NumberValue(value Number) Value { return Value{kind: ValueNumber, number: value} }

// U64Value is a non-negative integer.
func U64Value(value uint64) Value { return NumberValue(NumberFromU64(value)) }

// I64Value is an integer.
func I64Value(value int64) Value { return NumberValue(NumberFromI64(value)) }

// F64Value is a float; NaN and infinities become null, as in serde_json's
// `json!`.
func F64Value(value float64) Value {
	n, ok := NumberFromF64(value)
	if !ok {
		return NullValue()
	}
	return NumberValue(n)
}

// ArrayValue is a JSON array of the items given.
func ArrayValue(items ...Value) Value {
	if items == nil {
		items = []Value{}
	}
	return Value{kind: ValueArray, items: items}
}

// ObjectValue is a JSON object of the members given, in order. Later
// duplicates replace earlier ones.
func ObjectValue(members ...Member) Value {
	object := Value{kind: ValueObject, members: []Member{}}
	for _, member := range members {
		object.Set(member.Key, member.Value)
	}
	return object
}

// Kind is the type of the value.
func (v Value) Kind() ValueKind { return v.kind }

// IsNull reports JSON null.
func (v Value) IsNull() bool { return v.kind == ValueNull }

// IsString reports a JSON string.
func (v Value) IsString() bool { return v.kind == ValueString }

// AsBool is the boolean, if the value is one.
func (v Value) AsBool() (bool, bool) { return v.boolean, v.kind == ValueBool }

// AsString is the string, if the value is one.
func (v Value) AsString() (string, bool) { return v.text, v.kind == ValueString }

// AsNumber is the number, if the value is one.
func (v Value) AsNumber() (Number, bool) { return v.number, v.kind == ValueNumber }

// AsF64 is serde_json's as_f64: any number.
func (v Value) AsF64() (float64, bool) {
	if v.kind != ValueNumber {
		return 0, false
	}
	return v.number.AsF64(), true
}

// AsU64 is serde_json's as_u64: a non-negative integer.
func (v Value) AsU64() (uint64, bool) {
	if v.kind != ValueNumber {
		return 0, false
	}
	return v.number.AsU64()
}

// AsI64 is serde_json's as_i64.
func (v Value) AsI64() (int64, bool) {
	if v.kind != ValueNumber {
		return 0, false
	}
	return v.number.AsI64()
}

// AsArray is the array's items, if the value is an array. The slice aliases
// the value.
func (v Value) AsArray() ([]Value, bool) { return v.items, v.kind == ValueArray }

// AsObject is the object's members in order, if the value is an object. The
// slice aliases the value.
func (v Value) AsObject() ([]Member, bool) { return v.members, v.kind == ValueObject }

// Len is the number of array items or object members, else 0.
func (v Value) Len() int {
	switch v.kind {
	case ValueArray:
		return len(v.items)
	case ValueObject:
		return len(v.members)
	}
	return 0
}

// Keys lists an object's keys in order.
func (v Value) Keys() []string {
	keys := make([]string, 0, len(v.members))
	for _, member := range v.members {
		keys = append(keys, member.Key)
	}
	return keys
}

// Get is an object member.
func (v Value) Get(key string) (Value, bool) {
	if v.kind != ValueObject {
		return Value{}, false
	}
	for _, member := range v.members {
		if member.Key == key {
			return member.Value, true
		}
	}
	return Value{}, false
}

// Index is an array item.
func (v Value) Index(index int) (Value, bool) {
	if v.kind != ValueArray || index < 0 || index >= len(v.items) {
		return Value{}, false
	}
	return v.items[index], true
}

// GetMut is a pointer to an object member, nil if absent.
func (v *Value) GetMut(key string) *Value {
	if v.kind != ValueObject {
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
func (v *Value) IndexMut(index int) *Value {
	if v.kind != ValueArray || index < 0 || index >= len(v.items) {
		return nil
	}
	return &v.items[index]
}

// Set replaces an object member in place or appends it. Null becomes an
// empty object first, as with serde_json's IndexMut; any other kind panics.
func (v *Value) Set(key string, value Value) {
	if v.kind == ValueNull {
		*v = Value{kind: ValueObject, members: []Member{}}
	}
	if v.kind != ValueObject {
		panic("core.Value.Set on a JSON value that is not an object")
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
func (v *Value) Remove(key string) (Value, bool) {
	if v.kind != ValueObject {
		return Value{}, false
	}
	for i := range v.members {
		if v.members[i].Key == key {
			removed := v.members[i].Value
			v.members = append(v.members[:i:i], v.members[i+1:]...)
			return removed, true
		}
	}
	return Value{}, false
}

// Push appends an array item. Any other kind panics.
func (v *Value) Push(value Value) {
	if v.kind != ValueArray {
		panic("core.Value.Push on a JSON value that is not an array")
	}
	v.items = append(v.items, value)
}

// Pointer resolves an RFC 6901 JSON pointer ("" is the value itself,
// "/a/0/b~1c" walks object keys and array indices), the port of serde_json's
// pointer_mut. Nil if the path does not exist. The result aliases v.
func (v *Value) Pointer(pointer string) *Value {
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
		case ValueObject:
			target = target.GetMut(token)
		case ValueArray:
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
func (v Value) Clone() Value {
	switch v.kind {
	case ValueArray:
		items := make([]Value, len(v.items))
		for i, item := range v.items {
			items[i] = item.Clone()
		}
		v.items = items
	case ValueObject:
		members := make([]Member, len(v.members))
		for i, member := range v.members {
			members[i] = Member{Key: member.Key, Value: member.Value.Clone()}
		}
		v.members = members
	}
	return v
}

// Equal is serde_json's Value equality: object member order does not
// matter, number kinds do (1 is not 1.0).
func (v Value) Equal(other Value) bool {
	if v.kind != other.kind {
		return false
	}
	switch v.kind {
	case ValueNull:
		return true
	case ValueBool:
		return v.boolean == other.boolean
	case ValueNumber:
		if v.number.kind != other.number.kind {
			return false
		}
		if v.number.kind == numberF64 {
			return math.Float64frombits(v.number.bits) == math.Float64frombits(other.number.bits)
		}
		return v.number.bits == other.number.bits
	case ValueString:
		return v.text == other.text
	case ValueArray:
		if len(v.items) != len(other.items) {
			return false
		}
		for i := range v.items {
			if !v.items[i].Equal(other.items[i]) {
				return false
			}
		}
		return true
	case ValueObject:
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
func (v Value) String() string { return string(appendValueJSON(nil, v, nil)) }

// Pretty is serde_json's to_string_pretty: two-space indentation.
func (v Value) Pretty() string {
	return string(appendValueJSON(nil, v, &indenter{}))
}
