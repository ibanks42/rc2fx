package levels

// line_overlay.rs's decisions, kept from the Rust overlay after its redesign
// (docs/developer/desktop-go.md): in Go each animated indicator is its
// own view, redrawn alone, so the overlay's slots, claims, latches and
// content masks are gone. What stays is when a frame is worth drawing
// (Slots.Unchanged) and how often to tick.
//
// The Rust overlay's reasoning, which still holds: presenting a frame costs a
// weak GPU far more than anything drawn in it, so a tick that would show
// nothing new draws no frame (Slots.Unchanged): a silent mixer costs almost
// nothing. The tick aims at 60 Hz, or 30 Hz while the window is not focused;
// it doesn't guarantee either.

import (
	"math"
	"os"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

const (
	// Frame is the tick while the window is focused.
	Frame = 16_667 * time.Microsecond
	// UnfocusedFrame is the tick while it is not.
	UnfocusedFrame = 33_333 * time.Microsecond
	// Flat: a line lower than this, in pixels either side of its middle,
	// looks flat.
	Flat float32 = 0.5
	// BarStep: a bar's level changing by less than this moves it less than a
	// pixel.
	BarStep float32 = 0.004
)

var enabled = sync.OnceValue(func() bool {
	v, ok := os.LookupEnv("RC2FX_ANIMATED_LINE")
	return !ok || v != "0"
})

// Enabled is whether Home's indicators animate. On by default;
// RC2FX_ANIMATED_LINE=0 opens the window with Home's indicators drawn still.
// Read once.
func Enabled() bool { return enabled() }

// Shape is how an indicator is drawn: a line in a style, or a bar.
type Shape struct {
	IsBar bool
	Line  LineStyle
	Bar   Bar
}

// LineShape and BarShape make a Shape.
func LineShape(style LineStyle) Shape { return Shape{Line: style} }
func BarShape(bar Bar) Shape          { return Shape{IsBar: true, Bar: bar} }

// Claim is one indicator on screen: its shape and the height it was drawn at
// (Height is ignored without Measured: a line of unknown height is never
// taken for flat).
type Claim struct {
	Shape    Shape
	Height   float32
	Measured bool
}

// Slots is the indicators drawn in the last frame and each one's level then
// (nil for a bar without a fresh reading).
type Slots struct {
	Claims []Claim
	Drawn  []*float32
}

// Unchanged is whether painting levels (one per claim) would look like the
// last paint: every line was and stays flat, whatever its phase, and no bar
// has moved.
func (s *Slots) Unchanged(levels []*float32) bool {
	if len(s.Drawn) != len(s.Claims) || len(levels) != len(s.Claims) {
		return false
	}
	for i, claim := range s.Claims {
		if !same(claim, s.Drawn[i], levels[i]) {
			return false
		}
	}
	return true
}

func same(claim Claim, drawn, level *float32) bool {
	if !claim.Shape.IsBar {
		height := float32(math.Inf(1))
		if claim.Measured {
			height = claim.Height
		}
		for _, l := range [2]*float32{drawn, level} {
			v := float32(1)
			if l != nil {
				v = *l
			}
			if !(v*claim.Shape.Line.Amplitude*height < Flat) {
				return false
			}
		}
		return true
	}
	if drawn != nil && level != nil {
		return num.Abs32(*drawn-*level) < BarStep
	}
	return drawn == nil && level == nil
}

// StillUnchanged is Unchanged for one indicator drawn still (reduced motion,
// a page without animation, RC2FX_ANIMATED_LINE=0): its shape does not move
// with time, so it changes only when its level moves it by a pixel or more.
// The Rust app redrew still indicators with every app render; in Go they
// are views of their own, checked on the app's poll instead.
func StillUnchanged(claim Claim, drawn, level *float32) bool {
	if claim.Shape.IsBar {
		return same(claim, drawn, level)
	}
	if drawn == nil || level == nil {
		return drawn == nil && level == nil
	}
	if !claim.Measured {
		return *drawn == *level
	}
	return num.Abs32(*drawn-*level)*claim.Shape.Line.Amplitude*claim.Height < Flat
}

// Tick is how often the shared ticker runs: Frame, UnfocusedFrame while the
// window is not active, and run false when it should stop: nothing on screen
// can animate (no fresh reading, reduced motion, animation off). Window
// visibility is not readable in Go, so a minimised window keeps ticking at
// UnfocusedFrame.
func Tick(animating int, reducedMotion, active bool) (period time.Duration, run bool) {
	if animating == 0 || reducedMotion || !Enabled() {
		return 0, false
	}
	if active {
		return Frame, true
	}
	return UnfocusedFrame, true
}
