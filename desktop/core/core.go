// Package core is the desktop's copy of the RC2FX schema: typed device
// configuration, editor and session logic, EQ response and preset
// interchange. No audio, network or GUI runtime.
//
// It is the Go port of the Rust crate rc2fx-core (the crate stays for the
// mixer). JSON from outside the process is read with the strict decoder in
// this package (DecodeStrict, FromJSON, FromJSONLimited, ParseJSONLimited),
// and JSON the app writes is produced with Encode / EncodePretty, which match
// serde_json's output byte for byte.
package core

import (
	"math"
	"strconv"
)

// CoreError is the error every operation in this package returns. Its text
// is shown to the user as is.
type CoreError string

func (e CoreError) Error() string { return string(e) }

// rangeCheck is the Rust `range`: finite and within min..=max, or
// "{name} must be within {min}..{max}".
func rangeCheck(value, min, max float64, name string) error {
	if math.IsNaN(value) || math.IsInf(value, 0) || value < min || value > max {
		return CoreError(name + " must be within " + displayFloat(min) + ".." + displayFloat(max))
	}
	return nil
}

// displayFloat formats like Rust's `Display` for f64: the shortest digits
// that round-trip, never in exponent notation.
func displayFloat(value float64) string {
	if math.IsNaN(value) {
		return "NaN"
	}
	if math.IsInf(value, 1) {
		return "inf"
	}
	if math.IsInf(value, -1) {
		return "-inf"
	}
	return strconv.FormatFloat(value, 'f', -1, 64)
}

// fmax and fmin follow Rust's f64::max and f64::min: a NaN operand yields
// the other operand.
func fmax(a, b float64) float64 {
	switch {
	case math.IsNaN(a):
		return b
	case math.IsNaN(b):
		return a
	case a > b:
		return a
	default:
		return b
	}
}

func fmin(a, b float64) float64 {
	switch {
	case math.IsNaN(a):
		return b
	case math.IsNaN(b):
		return a
	case a < b:
		return a
	default:
		return b
	}
}

// fclamp follows Rust's f64::clamp for valid bounds: NaN stays NaN.
func fclamp(value, min, max float64) float64 {
	if value < min {
		return min
	}
	if value > max {
		return max
	}
	return value
}
