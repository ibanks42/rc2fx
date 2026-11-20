package model

import (
	"math"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// Mirror of the 60 dB meter scale from package app so model tests stay UI-free.
func level(peak float64) float32 {
	return float32(min(max((20*math.Log10(max(peak, 1e-6))+60)/60, 0), 1))
}

func (h *LevelHistory) lastAt() (time.Time, bool) {
	if len(h.points) == 0 {
		return time.Time{}, false
	}
	return h.points[len(h.points)-1].at, true
}

func steady(h *LevelHistory, from time.Time, step time.Duration, count int, peak float64) time.Time {
	for i := range count {
		h.Push(from.Add(step*time.Duration(i)), peak)
	}
	return from.Add(step * time.Duration(max(count-1, 0)))
}

func TestARiseReachesTheReadingQuicklyWithoutJumping(t *testing.T) {
	start := time.Now()
	var h LevelHistory
	h.Push(start, 0.5) // about -6 dBFS
	if got := h.Display(start, level); got != 0 {
		t.Fatalf("starts from nothing: %v", got)
	}
	soon := h.Display(start.Add(16*time.Millisecond), level)
	if !(soon > 0 && soon < level(0.5)*0.5) {
		t.Fatalf("%v", soon)
	}
	settled := h.Display(start.Add(200*time.Millisecond), level)
	if num.Abs32(settled-level(0.5)) >= 0.01 {
		t.Fatalf("%v", settled)
	}
}

func TestTheLevelDropsSoonAfterSoundStops(t *testing.T) {
	start := time.Now()
	step := 150 * time.Millisecond
	var h LevelHistory
	last := steady(&h, start, step, 8, 0.5)
	stop := last.Add(step)
	h.Push(stop, 0)
	// Still on its way down a frame later, not snapped to the bottom.
	frame := h.Display(stop.Add(16*time.Millisecond), level)
	if !(frame > level(0.5)*0.7) {
		t.Fatalf("%v", frame)
	}
	// Nearly gone within half a second.
	later := stop.Add(500 * time.Millisecond)
	h.Push(stop.Add(step), 0)
	h.Push(stop.Add(step*2), 0)
	h.Push(stop.Add(step*3), 0)
	gone := h.Display(later, level)
	if !(gone < 0.02) {
		t.Fatalf("%v", gone)
	}
}

func TestANewReadingChangesDirectionWithoutMovingTheLevel(t *testing.T) {
	start := time.Now()
	step := 150 * time.Millisecond
	var h LevelHistory
	last := steady(&h, start, step, 8, 0.5)
	for _, c := range []struct {
		peak float64
		at   time.Time
	}{{1.0, last.Add(step)}, {0.01, last.Add(step * 2)}} {
		before := h.Display(c.at, level)
		h.Push(c.at, c.peak)
		if num.Abs32(h.Display(c.at, level)-before) >= 1e-5 {
			t.Fatalf("peak %v moved the level", c.peak)
		}
	}
}

func TestSparseReadingsMoveInSmallStepsEachFrame(t *testing.T) {
	start := time.Now()
	step := 150 * time.Millisecond
	frame := 16_667 * time.Microsecond
	var h LevelHistory
	var last float32
	next := 0
	for i := range 180 {
		now := start.Add(frame * time.Duration(i))
		for !start.Add(step * time.Duration(next)).After(now) {
			peak := 0.05
			if next%2 == 0 {
				peak = 0.5
			}
			h.Push(start.Add(step*time.Duration(next)), peak)
			next++
		}
		shown := h.Display(now, level)
		// After the first rise from nothing.
		if i > 30 && num.Abs32(shown-last) >= 0.1 {
			t.Fatalf("frame %d: %v to %v", i, last, shown)
		}
		last = shown
	}
}

func TestTheLevelDoesNotDependOnHowOftenItIsDrawn(t *testing.T) {
	start := time.Now()
	var h LevelHistory
	h.Push(start, 0.5)
	at := start.Add(70 * time.Millisecond)
	if a, b := h.Display(at, level), h.Display(at, level); a != b {
		t.Fatalf("%v != %v", a, b)
	}
}

func TestAReadingFromBeforeAGapNeverShowsAfterIt(t *testing.T) {
	start := time.Now()
	var h LevelHistory
	steady(&h, start, 100*time.Millisecond, 5, 1.0)
	// Telemetry stops for 1.5 s, then returns quietly.
	back := start.Add(1900 * time.Millisecond)
	h.Push(back, 0.01)
	if got := h.Display(back, level); got != 0 {
		t.Fatalf("%v", got)
	}
	settled := h.Display(back.Add(300*time.Millisecond), level)
	if num.Abs32(settled-level(0.01)) >= 0.01 {
		t.Fatalf("%v", settled)
	}
	// And with no reading for longer than the gap, nothing is shown.
	if got := h.Display(back.Add(1100*time.Millisecond), level); got != 0 {
		t.Fatalf("%v", got)
	}
}

func TestHistoryIsBoundedAndOrdered(t *testing.T) {
	start := time.Now()
	var h LevelHistory
	for i := range 2000 {
		h.Push(start.Add(time.Duration(i)*time.Millisecond), 0.1)
	}
	if len(h.points) > levelCapacity {
		t.Fatalf("%d points", len(h.points))
	}
	h.Push(start, 0.9)
	if h.points[len(h.points)-1].peak == 0.9 {
		t.Fatal("older readings are dropped")
	}
	h.Push(start.Add(60*time.Second), math.NaN())
	if at, ok := h.lastAt(); !ok || !at.Equal(start.Add(1999*time.Millisecond)) {
		t.Fatalf("last at %v %v", at, ok)
	}
}

func pairs(ps ...any) func(func(string, float64) bool) {
	return func(yield func(string, float64) bool) {
		for i := 0; i+1 < len(ps); i += 2 {
			if !yield(ps[i].(string), ps[i+1].(float64)) {
				return
			}
		}
	}
}

func TestPathsThatDisappearStartOver(t *testing.T) {
	start := time.Now()
	var all LevelHistories
	all.Receive(start, pairs("mic-1", 0.5, "usb-main", 0.2))
	all.Receive(start.Add(50*time.Millisecond), pairs("mic-1", 0.4))
	if all.Get("usb-main") != nil {
		t.Fatal("usb-main kept")
	}
	if got := len(all.Get("mic-1").points); got != 2 {
		t.Fatalf("%d points", got)
	}
	all.Clear()
	if all.Get("mic-1") != nil {
		t.Fatal("mic-1 kept")
	}
}
