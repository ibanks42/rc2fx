package levels

// Frame scheduling and dirty-checking for live level indicators.
// Presenting a frame on weak GPUs dominates paint cost, so we skip frames when
// nothing visibly changes (keeping a silent mixer nearly idle) and drop from
// ~60 Hz to ~30 Hz when the window is unfocused.

import (
	"math"
	"os"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

const (
	// Tick interval when the window is focused (~60 Hz).
	Frame = 16_667 * time.Microsecond
	// Tick interval when unfocused (~30 Hz).
	UnfocusedFrame = 33_333 * time.Microsecond
	// Half-height threshold in pixels below which a wave line looks flat.
	Flat float32 = 0.5
	// Minimum bar level delta that produces a visible pixel change.
	BarStep float32 = 0.004
)

var enabled = sync.OnceValue(func() bool {
	v, ok := os.LookupEnv("RC2FX_ANIMATED_LINE")
	return !ok || v != "0"
})

// Enabled reports whether animated indicators are on (set RC2FX_ANIMATED_LINE=0 to disable).
func Enabled() bool { return enabled() }

// Shape describes an indicator's visual type (animated line or level bar).
type Shape struct {
	IsBar bool
	Line  LineStyle
	Bar   Bar
}

func LineShape(style LineStyle) Shape { return Shape{Line: style} }
func BarShape(bar Bar) Shape          { return Shape{IsBar: true, Bar: bar} }

// Claim tracks one rendered indicator and its measured height. When Measured is
// false, Height is ignored and a line is never treated as flat.
type Claim struct {
	Shape    Shape
	Height   float32
	Measured bool
}

// Slots pairs the active indicator claims with the levels drawn in the last frame
// (nil represents a bar with no fresh reading).
type Slots struct {
	Claims []Claim
	Drawn  []*float32
}

// Unchanged reports whether redrawing with levels would look identical to the
// last frame (all lines remain flat and no bar moved past BarStep).
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

// StillUnchanged is the static-mode equivalent of Slots.Unchanged (used for
// reduced motion or RC2FX_ANIMATED_LINE=0). Without phase animation, a line
// only needs a redraw when its level shifts by at least Flat pixels.
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

// Tick returns the ticker interval and whether it should run. Go cannot query
// minimized state here, so minimized windows continue at UnfocusedFrame.
func Tick(animating int, reducedMotion, active bool) (period time.Duration, run bool) {
	if animating == 0 || reducedMotion || !Enabled() {
		return 0, false
	}
	if active {
		return Frame, true
	}
	return UnfocusedFrame, true
}
