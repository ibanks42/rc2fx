// Package opt holds the helpers for optional values, which the desktop code
// keeps as pointers that may be nil (Rust's Option in the code it was ported
// from).
package opt

// Equal is whether a and b are both nil, or both set to equal values.
func Equal[T comparable](a, b *T) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// Clone is a new pointer to a copy of *p, or nil when p is nil. The copy is
// shallow: slices and maps inside T are shared.
func Clone[T any](p *T) *T {
	if p == nil {
		return nil
	}
	return new(*p)
}
