// Package edit holds the non-rendering channel editor logic: EQ graph math and
// curve caching, parameter metadata, lookahead figures, show input caching, and
// preset/settings file I/O. Kept free of gk imports so it tests with plain `go test`.
package edit

import (
	"fmt"
	"math"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Frequency axis bounds in Hz.
const (
	FMin = 10.
	FMax = 24000.
)

// DB is the visible +/- gain range on the EQ graph; larger gains clamp to the edge.
const DB = 18.

// FrequencyX maps a frequency in Hz to a normalized [0, 1] log-scale x coordinate.
func FrequencyX(frequency float64) float64 {
	return math.Log(frequency/FMin) / math.Log(FMax/FMin)
}

// XFrequency maps a normalized [0, 1] x coordinate back to Hz, clamping x first.
func XFrequency(x float64) float64 {
	return FMin * math.Pow(FMax/FMin, math.Min(math.Max(x, 0), 1))
}

// CurvePoints is the number of samples along the EQ response curve.
const CurvePoints = 241

// GridHz lists the vertical frequency grid lines in Hz.
var GridHz = [10]float64{20., 50., 100., 200., 500., 1000., 2000., 5000., 10000., 20000.}

// GridDB lists the horizontal gain grid lines in dB (0 dB is drawn separately).
var GridDB = [4]float64{12., 6., -6., -12.}

// Height maps a gain in dB to a normalized [0, 1] y coordinate (0 at top, 1 at bottom).
func Height(gain float64) float64 {
	return 0.5 - math.Min(math.Max(gain, -DB), DB)/(2.*DB)
}

// GraphPoint converts a window coordinate inside the EQ graph bounds into
// whole-Hz frequency and tenth-dB gain. Returns ok=false if the graph has zero size.
func GraphPoint(originX, originY, width, height, x, y float32) (frequency, gain float64, ok bool) {
	w, h := float64(width), float64(height)
	if w <= 0 || h <= 0 {
		return 0, 0, false
	}
	fx := float64(x-originX) / w
	fy := float64(y-originY) / h
	g := math.Min(math.Max((0.5-fy)*2.*DB, -36.), 36.)
	return math.Round(XFrequency(fx)), math.Round(g*10.) / 10., true
}

// GridLabel formats a frequency grid label (e.g. "20", "500", "1k", "20k").
func GridLabel(frequency float64) string {
	if frequency >= 1000. {
		return fmt.Sprint(frequency/1000.) + "k"
	}
	return fmt.Sprint(frequency)
}

// DBLabel formats a gain grid label using a typographic minus sign (e.g. "+12", "−6").
func DBLabel(db float64) string {
	return strings.ReplaceAll(fmt.Sprintf("%+.0f", db), "-", "\u2212")
}

// EQCurve caches the computed EQ response so frequent meter/hover redraws don't
// recompute it when settings haven't changed.
type EQCurve struct {
	eq      core.EQSettings
	channel core.Channel
	db      []float64
}

// Curve returns CurvePoints response values in dB across the log frequency axis,
// reusing and updating *cache when eq and channel match.
func Curve(cache **EQCurve, eq core.EQSettings, channel core.Channel) []float64 {
	if c := *cache; c != nil && c.channel == channel && c.eq.Equal(eq) {
		return c.db
	}
	frequencies := make([]float64, CurvePoints)
	for i := range frequencies {
		frequencies[i] = XFrequency(float64(i) / float64(CurvePoints-1))
	}
	db := core.ResponseCurve(eq, channel, frequencies, true)
	*cache = &EQCurve{eq: eq.Clone(), channel: channel, db: db}
	return db
}
