// Package num holds the small numeric helpers the desktop code shares where
// the language and the standard library differ from the Rust it was ported
// from: NaN-ignoring float min and max, saturating subtraction and float32
// math.
//
// Prefer the built-in min and max where NaN cannot occur or must spread; use
// Max and Min only where a NaN argument must give the other one.
package num

import "math"

// Float is the floating-point types.
type Float interface{ ~float32 | ~float64 }

// Max is Rust's f64::max and f32::max: a NaN argument gives the other one.
// (The built-in max gives NaN.)
func Max[T Float](a, b T) T {
	switch {
	case a != a:
		return b
	case b != b:
		return a
	}
	return max(a, b)
}

// Min is Rust's f64::min and f32::min: a NaN argument gives the other one.
// (The built-in min gives NaN.)
func Min[T Float](a, b T) T {
	switch {
	case a != a:
		return b
	case b != b:
		return a
	}
	return min(a, b)
}

// Clamp is Rust's f64::clamp for lo <= hi: v limited to [lo, hi], and a NaN
// v stays NaN.
func Clamp[T Float](v, lo, hi T) T {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

// SatSub is Rust's u64::saturating_sub: a - b, or 0 when b is larger.
func SatSub(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}

// Go has no float32 math functions, so these compute in float64 and round
// the result to float32, like Rust's f32 methods up to the last bit.

// Sin32 is f32::sin.
func Sin32(x float32) float32 { return float32(math.Sin(float64(x))) }

// Pow32 is f32::powf.
func Pow32(x, y float32) float32 { return float32(math.Pow(float64(x), float64(y))) }

// Floor32 is f32::floor.
func Floor32(x float32) float32 { return float32(math.Floor(float64(x))) }

// Abs32 is f32::abs.
func Abs32(x float32) float32 { return float32(math.Abs(float64(x))) }
