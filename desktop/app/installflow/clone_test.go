package installflow

import "reflect"

// deepCopy copies a test value deeply: slices, maps and pointers are copied,
// not shared, so changing the copy leaves the original alone.
// Unexported fields are copied shallowly.
func deepCopy[T any](v T) T {
	var out T
	copyValue(reflect.ValueOf(&out).Elem(), reflect.ValueOf(&v).Elem())
	return out
}

func copyValue(dst, src reflect.Value) {
	switch src.Kind() {
	case reflect.Slice:
		if src.IsNil() {
			return
		}
		dst.Set(reflect.MakeSlice(src.Type(), src.Len(), src.Len()))
		for i := range src.Len() {
			copyValue(dst.Index(i), src.Index(i))
		}
	case reflect.Array:
		for i := range src.Len() {
			copyValue(dst.Index(i), src.Index(i))
		}
	case reflect.Map:
		if src.IsNil() {
			return
		}
		dst.Set(reflect.MakeMapWithSize(src.Type(), src.Len()))
		iter := src.MapRange()
		for iter.Next() {
			value := reflect.New(iter.Value().Type()).Elem()
			copyValue(value, iter.Value())
			dst.SetMapIndex(iter.Key(), value)
		}
	case reflect.Pointer:
		if src.IsNil() {
			return
		}
		dst.Set(reflect.New(src.Type().Elem()))
		copyValue(dst.Elem(), src.Elem())
	case reflect.Interface:
		if src.IsNil() {
			return
		}
		value := reflect.New(src.Elem().Type()).Elem()
		copyValue(value, src.Elem())
		dst.Set(value)
	case reflect.Struct:
		dst.Set(src)
		for i := range src.NumField() {
			if dst.Field(i).CanSet() {
				copyValue(dst.Field(i), src.Field(i))
			}
		}
	default:
		dst.Set(src)
	}
}
