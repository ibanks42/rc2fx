package levels

import (
	"math"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

func f(v float32) *float32 { return &v }

// The live line.

func TestEveryPointIsFiniteAndBothEndsMeetTheMiddle(t *testing.T) {
	for _, style := range []LineStyle{Channel, Mini} {
		for _, height := range []float32{34, 170} {
			// Levels from silence to full scale, as paint_line scales them.
			for _, level := range []float32{0, 0.5, 1} {
				amplitude := level * style.Amplitude * height
				for _, width := range []float32{4, 300, 899, 1280} {
					for _, phase := range []float32{0, 0.016_667, 1.7, 1234.5} {
						points, ok := Offsets(width, phase, amplitude)
						if !ok {
							t.Fatalf("a valid line for valid inputs: width %v phase %v amplitude %v", width, phase, amplitude)
						}
						first, last := points[0], points[len(points)-1]
						if first.X != 0 || last.X != width {
							t.Fatalf("ends at %v and %v, want 0 and %v", first.X, last.X, width)
						}
						if num.Abs32(first.Y) >= 1e-3 || num.Abs32(last.Y) >= 1e-3 {
							t.Fatalf("tapered ends: %v, %v", first.Y, last.Y)
						}
						for _, p := range points {
							if num.Abs32(p.Y) > amplitude+1e-3 {
								t.Fatalf("offset %v beyond amplitude %v", p.Y, amplitude)
							}
						}
					}
				}
			}
		}
	}
}

func TestAnInvalidLineIsRejectedNotPatched(t *testing.T) {
	if _, ok := Offsets(300, float32(math.NaN()), 20); ok {
		t.Fatal("a NaN phase drew a line")
	}
	if _, ok := Offsets(300, 0, float32(math.Inf(1))); ok {
		t.Fatal("an infinite amplitude drew a line")
	}
}

// LinePoints is a line's geometry: the middle of the element, flat when
// unavailable, nothing below 4 × 2 px.
func TestLinePointsSitOnTheMiddleOfTheElement(t *testing.T) {
	if _, ok := LinePoints(3.9, 40, 0, 1, LineStateLive, Mini); ok {
		t.Fatal("drew in a too narrow element")
	}
	if _, ok := LinePoints(100, 1.9, 0, 1, LineStateLive, Mini); ok {
		t.Fatal("drew in a too low element")
	}
	points, ok := LinePoints(100, 40, 1.7, 1, LineStateUnavailable, Channel)
	if !ok {
		t.Fatal("no unavailable line")
	}
	for _, p := range points {
		if p.Y != 20 {
			t.Fatalf("unavailable line not flat at the middle: %v", p.Y)
		}
	}
	points, _ = LinePoints(100, 40, 1.7, 2, LineStateLive, Channel)
	for _, p := range points {
		if num.Abs32(p.Y-20) > Channel.Amplitude*40+1e-3 {
			t.Fatalf("level not clamped to 1: %v", p.Y)
		}
	}
}

// The level bars.

func track() Rect { return Rect{10, 20, 100, 200} }

func TestAVerticalBarRisesFromTheBottomOfItsTrack(t *testing.T) {
	fill := BarVertical.Fill(track(), f(0.5))
	if fill.W != 100 || fill.H != 100 {
		t.Fatalf("size %v × %v", fill.W, fill.H)
	}
	if fill.X != 10 || fill.Y != 120 {
		t.Fatalf("origin %v, %v", fill.X, fill.Y)
	}
	if fill.Bottom() != track().Bottom() {
		t.Fatalf("bottom %v, want %v", fill.Bottom(), track().Bottom())
	}
}

func TestAHorizontalBarGrowsFromTheLeftOfItsTrack(t *testing.T) {
	fill := BarHorizontal.Fill(track(), f(0.25))
	if fill.X != track().X || fill.Y != track().Y {
		t.Fatalf("origin %v, %v", fill.X, fill.Y)
	}
	if fill.W != 25 || fill.H != 200 {
		t.Fatalf("size %v × %v", fill.W, fill.H)
	}
}

func TestLevelsStayInsideTheTrackAndSilenceStaysVisible(t *testing.T) {
	if full := BarVertical.Fill(track(), f(2)); full != track() {
		t.Fatalf("full %v, want %v", full, track())
	}
	if silent := BarHorizontal.Fill(track(), f(0)); silent.W != 100*Lowest {
		t.Fatalf("silent width %v", silent.W)
	}
}

func TestNoReadingIsAFixedStubAtTheStartOfTheTrack(t *testing.T) {
	if h := BarVertical.Fill(track(), nil).H; h != Stub {
		t.Fatalf("vertical stub %v", h)
	}
	if w := BarHorizontal.Fill(track(), nil).W; w != Stub {
		t.Fatalf("horizontal stub %v", w)
	}
}

// When an indicator redraws and the ticker runs.

func slots(shape Shape, drawn *float32) Slots {
	return Slots{
		Claims: []Claim{{Shape: shape, Height: 40, Measured: true}},
		Drawn:  []*float32{drawn},
	}
}

func line() Shape { return LineShape(Mini) }

func TestAFlatLineNeedsNoFrameButAMovingOneDoes(t *testing.T) {
	s := slots(line(), f(0))
	if !s.Unchanged([]*float32{f(0)}) {
		t.Fatal("a flat line asked for a frame")
	}
	// Same level, but the shape moves with time unless it is flat.
	s = slots(line(), f(0.5))
	if s.Unchanged([]*float32{f(0.5)}) {
		t.Fatal("a moving line was taken as unchanged")
	}
}

func TestALineGoingFlatOrRisingDrawsThatChange(t *testing.T) {
	s := slots(line(), f(0.5))
	if s.Unchanged([]*float32{f(0)}) {
		t.Fatal("a line going flat was taken as unchanged")
	}
	s = slots(line(), f(0))
	if s.Unchanged([]*float32{f(0.5)}) {
		t.Fatal("a rising line was taken as unchanged")
	}
}

func TestANewSetOfClaimsAlwaysDraws(t *testing.T) {
	s := slots(line(), f(0))
	s.Drawn = nil
	if s.Unchanged([]*float32{f(0)}) {
		t.Fatal("new claims were taken as unchanged")
	}
}

// The bar rule, unmeasured lines, still indicators and the tick choice.

func TestABarRedrawsOnlyWhenItMovesAPixelOrLosesItsReading(t *testing.T) {
	bar := BarShape(BarVertical)
	if s := slots(bar, f(0.5)); !s.Unchanged([]*float32{f(0.5 + BarStep/2)}) {
		t.Fatal("a bar moving less than a step redrew")
	}
	if s := slots(bar, f(0.5)); s.Unchanged([]*float32{f(0.5 + 2*BarStep)}) {
		t.Fatal("a bar moving two steps did not redraw")
	}
	if s := slots(bar, f(0.5)); s.Unchanged([]*float32{nil}) {
		t.Fatal("a bar losing its reading did not redraw")
	}
	if s := slots(bar, nil); !s.Unchanged([]*float32{nil}) {
		t.Fatal("a stub redrew")
	}
}

func TestALineOfUnknownHeightIsNeverTakenForFlat(t *testing.T) {
	s := Slots{Claims: []Claim{{Shape: line()}}, Drawn: []*float32{f(0)}}
	if s.Unchanged([]*float32{f(0)}) {
		t.Fatal("an unmeasured line was taken as flat")
	}
}

func TestAStillLineRedrawsWhenItsLevelMovesIt(t *testing.T) {
	claim := Claim{Shape: line(), Height: 40, Measured: true}
	// 0.42 × 40 = 16.8 px per unit of level: 0.01 moves it 0.168 px.
	if !StillUnchanged(claim, f(0.5), f(0.51)) {
		t.Fatal("a still line redrew for a sub-pixel change")
	}
	if StillUnchanged(claim, f(0.5), f(0.6)) {
		t.Fatal("a still line kept a 1.7 px old shape")
	}
	if StillUnchanged(Claim{Shape: line()}, f(0.5), f(0.51)) {
		t.Fatal("an unmeasured still line kept an old level")
	}
	if StillUnchanged(claim, f(0.5), nil) || !StillUnchanged(claim, nil, nil) {
		t.Fatal("still line freshness")
	}
}

func TestTheTickRunsOnlyWhileSomethingCanAnimate(t *testing.T) {
	if !Enabled() {
		t.Skip("RC2FX_ANIMATED_LINE=0")
	}
	cases := []struct {
		animating      int
		reduced, focus bool
		period         time.Duration
		run            bool
	}{
		{0, false, true, 0, false},
		{2, true, true, 0, false},
		{1, false, true, Frame, true},
		{1, false, false, UnfocusedFrame, true},
	}
	for _, c := range cases {
		period, run := Tick(c.animating, c.reduced, c.focus)
		if period != c.period || run != c.run {
			t.Errorf("Tick(%d, %v, %v) = %v, %v; want %v, %v", c.animating, c.reduced, c.focus, period, run, c.period, c.run)
		}
	}
}
