package edit

// The EQ page's helpers that do not draw.

import (
	"fmt"
	"math"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Uses is what each filter type is set by.
func Uses(kind core.BandType) []Parameter {
	switch kind {
	case core.BandTypeBell, core.BandTypeLowShelf, core.BandTypeHighShelf:
		return []Parameter{ParameterFrequency, ParameterBandGain, ParameterQ}
	case core.BandTypeLowPass, core.BandTypeHighPass, core.BandTypeNotch:
		return []Parameter{ParameterFrequency, ParameterQ}
	}
	return nil
}

// HasGain is whether a band's height on the graph is its gain.
func HasGain(kind core.BandType) bool {
	return slices.Contains(Uses(kind), ParameterBandGain)
}

// Types is the types offered as pills: every type but Off (the band's
// switch turns a band off; Off as a type only comes from imported files).
var Types = [6]core.BandType{
	core.BandTypeBell,
	core.BandTypeLowShelf,
	core.BandTypeHighShelf,
	core.BandTypeLowPass,
	core.BandTypeHighPass,
	core.BandTypeNotch,
}

// ShortHz is a band chip's frequency: "90", "3.5k".
func ShortHz(frequency float64) string {
	if frequency >= 1000. {
		k := fmt.Sprintf("%.1f", frequency/1000.)
		return strings.TrimRight(strings.TrimRight(k, "0"), ".") + "k"
	}
	return fmt.Sprintf("%.0f", frequency)
}

// ShortDB is a band chip's gain: "+3.25", "−2.40".
func ShortDB(gain float64) string {
	if gain > 0. {
		return fmt.Sprintf("+%.2f", gain)
	}
	return strings.ReplaceAll(fmt.Sprintf("%.2f", gain), "-", "\u2212")
}

// TypeShape is the filter type's icon: its height, 0 to 1, at t across.
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

// TypeIconPoints is the icon's line in a w×h box: 25 points, 1 px in from
// the sides and 2 px from the top and bottom.
func TypeIconPoints(kind core.BandType, w, h float32) [][2]float32 {
	points := make([][2]float32, 0, 25)
	for i := 0; i <= 24; i++ {
		t := float32(i) / 24.
		points = append(points, [2]float32{1. + t*(w-2.), h - 2. - TypeShape(kind, t)*(h-4.)})
	}
	return points
}

// SelectBandAfterMove is where the selected band is after PlaceBand: the
// position of the band whose id was selected, or index when there was none
// or it is gone.
func SelectBandAfterMove(bands []core.Band, selectedID *string, index int) int {
	if selectedID == nil {
		return index
	}
	if at := slices.IndexFunc(bands, func(b core.Band) bool { return b.ID == *selectedID }); at >= 0 {
		return at
	}
	return index
}
