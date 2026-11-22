// Package core implements the RC2FX configuration schema, session and editor
// state, EQ response math, and preset import/export, with no audio, network,
// or GUI dependencies.
//
// External JSON is parsed with this package's strict decoder (DecodeStrict,
// FromJSON, and friends), and outgoing JSON uses Encode / EncodePretty.
package core

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// CoreError is a user-facing error returned by core operations.
type CoreError string

func (e CoreError) Error() string { return string(e) }

func rangeCheck(value, min, max float64, name string) error {
	if math.IsNaN(value) || math.IsInf(value, 0) || value < min || value > max {
		return CoreError(name + " must be within " + num.FormatDecimal(min) + ".." + num.FormatDecimal(max))
	}
	return nil
}
