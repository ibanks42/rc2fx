// Package num provides small numeric helpers not covered by Go's built-ins
// or math package, mainly NaN-ignoring float min/max, saturating subtraction,
// and float32 math wrappers.
//
// Use the built-in min/max unless NaN inputs should fall back to the other
// argument instead of propagating.
package num

import "math"

type Float interface{ ~float32 | ~float64 }

// Max returns the larger of a and b, falling back to the other argument if
// one is NaN (unlike built-in max, which propagates NaN).
func Max[T Float](a, b T) T {
	switch {
	case a != a:
		return b
	case b != b:
		return a
	}
	return max(a, b)
}

// Min returns the smaller of a and b, falling back to the other argument if
// one is NaN (unlike built-in min, which propagates NaN).
func Min[T Float](a, b T) T {
	switch {
	case a != a:
		return b
	case b != b:
		return a
	}
	return min(a, b)
}

// Clamp restricts v to [lo, hi] (assuming lo <= hi), preserving NaN.
func Clamp[T Float](v, lo, hi T) T {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

// SatSub returns a - b, clamping at 0 on underflow.
func SatSub(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}

// float32 wrappers around stdlib math functions.
func Sin32(x float32) float32    { return float32(math.Sin(float64(x))) }
func Pow32(x, y float32) float32 { return float32(math.Pow(float64(x), float64(y))) }
func Floor32(x float32) float32  { return float32(math.Floor(float64(x))) }
func Abs32(x float32) float32    { return float32(math.Abs(float64(x))) }
