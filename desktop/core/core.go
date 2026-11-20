// Package core implements the RC2FX configuration schema, session/editor state
// machine, EQ response math, and preset import/export, with no audio, network,
// or GUI dependencies.
//
// It ports the Rust crate rc2fx-core (which stays on the mixer). External JSON
// is parsed with this package's strict decoder (DecodeStrict, FromJSON, etc.),
// and outgoing JSON uses Encode / EncodePretty to match serde_json byte-for-byte.
package core

import (
	"math"
	"strconv"
)

// CoreError is a user-facing error returned by core operations.
type CoreError string

func (e CoreError) Error() string { return string(e) }

func rangeCheck(value, min, max float64, name string) error {
	if math.IsNaN(value) || math.IsInf(value, 0) || value < min || value > max {
		return CoreError(name + " must be within " + displayFloat(min) + ".." + displayFloat(max))
	}
	return nil
}

// displayFloat matches Rust's f64 Display formatting (shortest round-trip
// decimal without scientific notation).
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
