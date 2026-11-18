// Package edit is the channel editor's logic that does not draw (graph.rs's
// axis maths and curve cache, editor.rs's parameters and lookahead figures,
// eq_controls.rs's band helpers, inputs.rs's show cache, selection.rs,
// restart.rs, interchange.rs and draft_files.rs's file logic). It does not
// import gk, so it tests with plain `go test`; package app draws with it.
package edit

import (
	"fmt"
	"math"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// The graph's frequency axis, in Hz.
const (
	FMin = 10.
	FMax = 24000.
)

// DB is the graph's range above and below 0 dB; bands beyond it sit at the
// edge.
const DB = 18.

// FrequencyX is where frequency sits across the graph, 0 to 1 (log axis).
func FrequencyX(frequency float64) float64 {
	return math.Log(frequency/FMin) / math.Log(FMax/FMin)
}

// XFrequency is the frequency at x across the graph (clamped to 0..1).
func XFrequency(x float64) float64 {
	return FMin * math.Pow(FMax/FMin, math.Min(math.Max(x, 0), 1))
}

// CurvePoints is how many points the response is drawn with.
const CurvePoints = 241

// GridHz is the grid lines and labels across the frequency axis.
var GridHz = [10]float64{20., 50., 100., 200., 500., 1000., 2000., 5000., 10000., 20000.}

// GridDB is the grid lines and labels up and down (0 dB is drawn apart).
var GridDB = [4]float64{12., 6., -6., -12.}

// Height is where gain sits down the graph, 0 (top) to 1 (bottom); gains
// beyond DB sit at the edge.
func Height(gain float64) float64 {
	return 0.5 - math.Min(math.Max(gain, -DB), DB)/(2.*DB)
}

// GraphPoint is the frequency (whole hertz) and gain (tenths of a dB) at a
// window position over the graph whose bounds are origin (x, y) and size
// (width, height); ok is false while the graph has no size.
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

// GridLabel is a frequency grid line's label: "20", "500", "1k", "20k".
func GridLabel(frequency float64) string {
	if frequency >= 1000. {
		return rustFloat(frequency/1000.) + "k"
	}
	return rustFloat(frequency)
}

// rustFloat is Rust's `{}` of an f64 that holds a whole number or a short
// decimal (the grid's values): no trailing ".0".
func rustFloat(v float64) string {
	return fmt.Sprint(v)
}

// DBLabel is a gain grid line's label: "+12", "−6".
func DBLabel(db float64) string {
	return strings.ReplaceAll(fmt.Sprintf("%+.0f", db), "-", "\u2212")
}

// EQCurve is graph.rs EqCurve: the selected EQ's response on the graph
// axis, kept until the EQ changes. Meter updates and hovering redraw the
// window often; the curve rarely moves.
type EQCurve struct {
	eq      core.EQSettings
	channel core.Channel
	db      []float64
}

// Curve is the response in dB at CurvePoints evenly spaced points across
// the log frequency axis, from the cache when eq and channel are the ones
// it holds (cache may be nil, and is replaced when it does not match).
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
