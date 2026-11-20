package core

import "reflect"

// deepEqual is Rust's derived PartialEq for the schema types: field by field,
// floats with == (NaN differs from itself, -0 equals 0), a nil slice equal to
// an empty one (Rust has no nil Vec), pointers by what they point to (an
// Option), and Values by Value.Equal.
func deepEqual(a, b any) bool {
	return equalValues(reflect.ValueOf(a), reflect.ValueOf(b))
}

func equalValues(a, b reflect.Value) bool {
	if !a.IsValid() || !b.IsValid() {
		return a.IsValid() == b.IsValid()
	}
	if a.Type() != b.Type() {
		return false
	}
	if a.Type() == reflect.TypeFor[Value]() && a.CanInterface() && b.CanInterface() {
		return a.Interface().(Value).Equal(b.Interface().(Value))
	}
	switch a.Kind() {
	case reflect.Bool:
		return a.Bool() == b.Bool()
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return a.Int() == b.Int()
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return a.Uint() == b.Uint()
	case reflect.Float32, reflect.Float64:
		return a.Float() == b.Float()
	case reflect.String:
		return a.String() == b.String()
	case reflect.Pointer, reflect.Interface:
		if a.IsNil() || b.IsNil() {
			return a.IsNil() && b.IsNil()
		}
		return equalValues(a.Elem(), b.Elem())
	case reflect.Slice, reflect.Array:
		if a.Len() != b.Len() {
			return false
		}
		for i := 0; i < a.Len(); i++ {
			if !equalValues(a.Index(i), b.Index(i)) {
				return false
			}
		}
		return true
	case reflect.Map:
		if a.Len() != b.Len() {
			return false
		}
		iterator := a.MapRange()
		for iterator.Next() {
			other := b.MapIndex(iterator.Key())
			if !other.IsValid() || !equalValues(iterator.Value(), other) {
				return false
			}
		}
		return true
	case reflect.Struct:
		for i := 0; i < a.NumField(); i++ {
			if !equalValues(a.Field(i), b.Field(i)) {
				return false
			}
		}
		return true
	}
	return false
}
