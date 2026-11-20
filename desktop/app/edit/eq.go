package edit

import (
	"fmt"
	"math"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Uses returns the editable parameters for a filter band type.
func Uses(kind core.BandType) []Parameter {
	switch kind {
	case core.BandTypeBell, core.BandTypeLowShelf, core.BandTypeHighShelf:
		return []Parameter{ParameterFrequency, ParameterBandGain, ParameterQ}
	case core.BandTypeLowPass, core.BandTypeHighPass, core.BandTypeNotch:
		return []Parameter{ParameterFrequency, ParameterQ}
	}
	return nil
}

// HasGain reports whether a filter type uses a gain control.
func HasGain(kind core.BandType) bool {
	return slices.Contains(Uses(kind), ParameterBandGain)
}

// Types lists the filter types shown in the type selector. Off is excluded
// because bands are toggled via their switch; Off only appears in imported files.
var Types = [6]core.BandType{
	core.BandTypeBell,
	core.BandTypeLowShelf,
	core.BandTypeHighShelf,
	core.BandTypeLowPass,
	core.BandTypeHighPass,
	core.BandTypeNotch,
}

// ShortHz formats a frequency compactly for band chips (e.g. "90", "3.5k").
func ShortHz(frequency float64) string {
	if frequency >= 1000. {
		k := fmt.Sprintf("%.1f", frequency/1000.)
		return strings.TrimRight(strings.TrimRight(k, "0"), ".") + "k"
	}
	return fmt.Sprintf("%.0f", frequency)
}

// ShortDB formats a gain compactly for band chips (e.g. "+3.25", "−2.40").
func ShortDB(gain float64) string {
	if gain > 0. {
		return fmt.Sprintf("+%.2f", gain)
	}
	return strings.ReplaceAll(fmt.Sprintf("%.2f", gain), "-", "\u2212")
}

// TypeShape evaluates a normalized [0, 1] icon curve for kind at horizontal position t.
func TypeShape(kind core.BandType, t float32) float32 {
	e := func(v float32) float32 { return float32(math.Exp(float64(v))) }
	pow := func(v, p float32) float32 { return float32(math.Pow(float64(v), float64(p))) }
	switch kind {
	case core.BandTypeBell:
		d := (t - 0.5) / 0.18
		return e(-(d * d))
	case core.BandTypeLowShelf:
		return 1. / (1. + e((t-0.5)*12.))
	case core.BandTypeHighShelf:
		return 1. / (1. + e(-(t-0.5)*12.))
	case core.BandTypeLowPass:
		if t < 0.5 {
			return 0.75
		}
		return 0.75 - 0.75*pow((t-0.5)/0.5, 1.4)
	case core.BandTypeHighPass:
		if t > 0.5 {
			return 0.75
		}
		return 0.75 - 0.75*pow((0.5-t)/0.5, 1.4)
	case core.BandTypeNotch:
		d := (t - 0.5) / 0.07
		return 0.75 - 0.75*e(-(d*d))
	}
	return 0.4
}

// TypeIconPoints samples 25 polyline points for kind inside a w×h box with
// 1 px horizontal and 2 px vertical padding.
func TypeIconPoints(kind core.BandType, w, h float32) [][2]float32 {
	points := make([][2]float32, 0, 25)
	for i := 0; i <= 24; i++ {
		t := float32(i) / 24.
		points = append(points, [2]float32{1. + t*(w-2.), h - 2. - TypeShape(kind, t)*(h-4.)})
	}
	return points
}

// SelectBandAfterMove finds the new index of selectedID after reordering bands,
// falling back to index if selectedID is nil or no longer present.
func SelectBandAfterMove(bands []core.Band, selectedID *string, index int) int {
	if selectedID == nil {
		return index
	}
	if at := slices.IndexFunc(bands, func(b core.Band) bool { return b.ID == *selectedID }); at >= 0 {
		return at
	}
	return index
}
