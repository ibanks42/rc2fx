package model

import "reflect"

// equal compares like Rust's derived PartialEq: field by field, with nil and
// empty slices and maps equal and NaN unequal to itself. reflect.DeepEqual
// tells nil from empty, which JSON decoding and literals do not keep apart.
func equal(a, b any) bool {
	return equalValue(reflect.ValueOf(a), reflect.ValueOf(b))
}

func equalValue(a, b reflect.Value) bool {
	if !a.IsValid() || !b.IsValid() {
		return a.IsValid() == b.IsValid()
	}
	if a.Type() != b.Type() {
		return false
	}
	switch a.Kind() {
	case reflect.Slice, reflect.Array:
		if a.Len() != b.Len() {
			return false
		}
		for i := range a.Len() {
			if !equalValue(a.Index(i), b.Index(i)) {
				return false
			}
		}
		return true
	case reflect.Map:
		if a.Len() != b.Len() {
			return false
		}
		for _, key := range a.MapKeys() {
			other := b.MapIndex(key)
			if !other.IsValid() || !equalValue(a.MapIndex(key), other) {
				return false
			}
		}
		return true
	case reflect.Pointer, reflect.Interface:
		if a.IsNil() || b.IsNil() {
			return a.IsNil() == b.IsNil()
		}
		return equalValue(a.Elem(), b.Elem())
	case reflect.Struct:
		for i := range a.NumField() {
			if !equalValue(a.Field(i), b.Field(i)) {
				return false
			}
		}
		return true
	case reflect.Float32, reflect.Float64:
		return a.Float() == b.Float()
	case reflect.Complex64, reflect.Complex128:
		return a.Complex() == b.Complex()
	case reflect.Bool:
		return a.Bool() == b.Bool()
	case reflect.String:
		return a.String() == b.String()
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return a.Int() == b.Int()
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return a.Uint() == b.Uint()
	default:
		// Funcs, channels and unsafe pointers have no value equality.
		return false
	}
}
