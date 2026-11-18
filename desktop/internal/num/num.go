// Package num holds the small numeric helpers the desktop code shares where
// Go's built-ins and standard library do not fit: NaN-ignoring float min
// and max, saturating subtraction and float32 math.
//
// Prefer the built-in min and max where NaN cannot occur or must spread; use
// Max and Min only where a NaN argument must give the other one.
package num

import "math"

// Float is the floating-point types.
type Float interface{ ~float32 | ~float64 }

// Max is the larger of a and b, but a NaN argument gives the other one.
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

// Min is the smaller of a and b, but a NaN argument gives the other one.
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

// Clamp is v limited to [lo, hi] (for lo <= hi), and a NaN v stays NaN.
func Clamp[T Float](v, lo, hi T) T {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

// SatSub is a - b, or 0 when b is larger.
func SatSub(a, b uint64) uint64 {
	if a < b {
		return 0
	}
	return a - b
}

// Go has no float32 math functions, so these compute in float64 and round
// the result to float32.

// Sin32 is math.Sin for a float32.
func Sin32(x float32) float32 { return float32(math.Sin(float64(x))) }

// Pow32 is math.Pow for float32s.
func Pow32(x, y float32) float32 { return float32(math.Pow(float64(x), float64(y))) }

// Floor32 is math.Floor for a float32.
func Floor32(x float32) float32 { return float32(math.Floor(float64(x))) }

// Abs32 is math.Abs for a float32.
func Abs32(x float32) float32 { return float32(math.Abs(float64(x))) }
