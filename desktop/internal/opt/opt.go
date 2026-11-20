// Package opt provides helpers for optional pointer values.
package opt

// Equal reports whether a and b are both nil or point to equal values.
func Equal[T comparable](a, b *T) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// Clone returns a shallow copy of *p, or nil if p is nil.
func Clone[T any](p *T) *T {
	if p == nil {
		return nil
	}
	return new(*p)
}
