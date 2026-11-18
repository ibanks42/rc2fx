// Package levels is the drawing maths of Home's live indicators, without gk:
// the live line's shape (live_line.rs), the level bars' fill (level_bar.rs)
// and the animation's decisions (line_overlay.rs: Slots::unchanged and the
// tick choice). The level history itself is desktop/app/model.
package levels

import (
	"math"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// LineState is live_line.rs LineState.
type LineState int

const (
	// LineStateLive is fresh readings: the RC2FX gradient at the displayed
	// level.
	LineStateLive LineState = iota
	// LineStateUnavailable is no fresh readings: a flat, dim line, labelled by
	// the caller as unavailable so it can't be read as measured silence.
	LineStateUnavailable
)

// LineStyle is the sizes of one indicator.
type LineStyle struct {
	// Amplitude is the line's largest half-height, as a fraction of the
	// element's height.
	Amplitude float32
	Stroke    float32
	// Glow is how far the glow spreads, in multiples of the stroke; 0 for
	// none.
	Glow float32
}

var (
	// Channel is the channel editor's line under the channel name.
	Channel = LineStyle{Amplitude: 0.34, Stroke: 2.5, Glow: 2}
	// Mini is the sidebar's small line beside each processed channel.
	Mini = LineStyle{Amplitude: 0.42, Stroke: 1.6, Glow: 0}
)

// Point is a point of the line: X across the element, Y the offset from its
// middle (Offsets), or a position in pixels inside the element (LinePoints).
type Point struct{ X, Y float32 }

// Glows are the live line's two glow passes under the stroke: the stroke
// widened by Spread (times the style's Glow), at Alpha.
var Glows = [2]struct{ Spread, Alpha float32 }{{7.0 / 3.0, 0.12}, {1, 0.25}}

// Offsets is the line's points across width: (x, offset from the middle), one
// every 3 px. ok is false if any point isn't finite: an invalid line is not
// drawn, rather than drawn with a substituted point.
func Offsets(width, phase, amplitude float32) ([]Point, bool) {
	steps := int(num.Max(float32(math.Ceil(float64(width/3))), 2))
	points := make([]Point, 0, steps+1)
	for i := 0; i <= steps; i++ {
		u := float32(i) / float32(steps)
		// Tapered at the ends, like the design. The shape doesn't fall to
		// zero between the ends, so it draws no silence that wasn't
		// measured. sin(π) is slightly negative in f32, and a negative
		// base would make powf NaN and the whole path invalid.
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

// LinePoints is paint_line's geometry: the points of the indicator drawn in
// an element of width × height, in pixels from the element's top left, and
// ok false when nothing is drawn (too small, or an invalid line). phase
// (seconds) moves the decorative shape; level scales its height.
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
