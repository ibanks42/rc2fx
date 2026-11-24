// Package levels computes curve geometry and frame-skipping decisions for
// Home's live level indicators. Level history lives in desktop/app/model.
package levels

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/num"
)

// LineState indicates whether the line has fresh level readings.
type LineState int

const (
	LineStateLive LineState = iota
	// Flat, dimmed line used when readings are missing so it isn't mistaken for silence.
	LineStateUnavailable
)

// LineStyle configures a live line's amplitude (as a fraction of element height)
// and stroke width.
type LineStyle struct {
	Amplitude float32
	Stroke    float32
}

// Mini is the compact waveform style drawn beside each processed channel in the sidebar.
var Mini = LineStyle{Amplitude: 0.42, Stroke: 1.6}

// Point is a 2D point in either centered offset coordinates (Offsets) or
// top-left element pixel coordinates (LinePoints).
type Point struct{ X, Y float32 }

// Offsets generates wave offsets every 3 px across width. Returns ok=false if
// any coordinate is non-finite so callers skip drawing instead of patching values.
func Offsets(width, phase, amplitude float32) ([]Point, bool) {
	steps := int(num.Max(float32(math.Ceil(float64(width/3))), 2))
	points := make([]Point, 0, steps+1)
	for i := 0; i <= steps; i++ {
		u := float32(i) / float32(steps)
		// Clamp sin(π*u) >= 0 because float32 sin(π) can be slightly negative,
		// which would make powf produce NaN.
		taper := num.Pow32(num.Max(num.Sin32(math.Pi*u), 0), 1.1)
		carrier := (num.Sin32(u*140-phase*8)*0.6 + num.Sin32(u*61+phase*3)*0.4) *
			(0.65 + 0.35*num.Sin32(u*6.3-phase*1.1))
		x, offset := u*width, taper*amplitude*carrier
		if !finite(x) || !finite(offset) {
			return nil, false
		}
		points = append(points, Point{x, offset})
	}
	return points, true
}

// LinePoints computes the polyline in element-local pixel coordinates, or
// ok=false if the box is too small (< 4×2 px) or the geometry is invalid.
func LinePoints(width, height, phase, level float32, state LineState, style LineStyle) ([]Point, bool) {
	if width < 4 || height < 2 {
		return nil, false
	}
	mid := height / 2
	var amplitude float32
	if state == LineStateLive {
		amplitude = min(max(level, 0), 1) * style.Amplitude * height
	}
	offsets, ok := Offsets(width, phase, amplitude)
	if !ok {
		return nil, false
	}
	for i := range offsets {
		offsets[i].Y = mid - offsets[i].Y
	}
	return offsets, true
}

func finite(v float32) bool { return !math.IsNaN(float64(v)) && !math.IsInf(float64(v), 0) }
