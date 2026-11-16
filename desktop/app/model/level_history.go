package model

// Recent output peaks per processed path, for the level indicator.
//
// Each reading is one meter event's output peak (measured after the path's
// processing), at the time this app received it. Meter events carry no
// timestamp of their own, so times are approximate: they include transport
// delay, and readings the device delayed or reordered can't be detected.
// What redraws an idle Home when readings stop is the app's poll (main.rs).

import (
	"iter"
	"math"
	"slices"
	"time"
)

const (
	// levelKeep is how long a reading can still affect the displayed level:
	// by then a reading has been replaced, whichever way the level was
	// moving.
	levelKeep = time.Second
	// levelCapacity bounds readings per path, whatever the event rate.
	levelCapacity = 300
	// levelRise and levelFall are how quickly the displayed level closes on
	// the latest reading, as the time to cover about two thirds of the
	// distance. Readings arrive a few times a second, so the level eases
	// between them instead of stepping. A peak still shows almost at once; a
	// level that stops is gone in about half a second.
	levelRise = 40 * time.Millisecond
	levelFall = 120 * time.Millisecond
	// levelGap: readings further apart than this belong to different
	// segments: the displayed level uses only the latest segment, so a
	// reading from before a gap (telemetry stale, then back) never shows
	// after it. The same 1 s after which a path stops counting as fresh
	// (telemetry.rs).
	levelGap = time.Second
)

type levelPoint struct {
	at   time.Time
	peak float32
}

// LevelHistory is one path's recent readings, oldest first.
type LevelHistory struct {
	points []levelPoint
}

// Scale maps a peak amplitude to a 0..1 position on the display's meter.
type Scale func(peak float64) float32

// sinceSaturating is a - b, or zero when b is later.
func sinceSaturating(a, b time.Time) time.Duration {
	return max(a.Sub(b), 0)
}

// Push records a reading.
func (h *LevelHistory) Push(at time.Time, peak float64) {
	// Out-of-order or invalid readings are dropped, never reordered.
	if math.IsNaN(peak) || math.IsInf(peak, 0) {
		return
	}
	if n := len(h.points); n > 0 && at.Before(h.points[n-1].at) {
		return
	}
	h.points = append(h.points, levelPoint{at: at, peak: float32(max(peak, 0))})
	drop := 0
	for len(h.points)-drop > levelCapacity ||
		(len(h.points)-drop > 0 && sinceSaturating(at, h.points[drop].at) > levelKeep) {
		drop++
	}
	if drop > 0 {
		h.points = slices.Delete(h.points, 0, drop)
	}
}

// Display is the displayed level at now, 0 to 1 on the display's meter scale
// (scale, desk's level): it closes on each reading in turn (levelRise,
// levelFall), starting from nothing. Display smoothing, computed from the
// readings' times so it doesn't depend on how often it's drawn. Only the
// latest segment counts: walking back from the newest reading, it stops at
// the first gap longer than levelGap, and nothing counts if the newest
// reading is itself older than levelGap.
func (h *LevelHistory) Display(now time.Time, scale Scale) float32 {
	first := len(h.points)
	later := now
	for index := len(h.points) - 1; index >= 0; index-- {
		at := h.points[index].at
		if at.After(now) {
			continue
		}
		if sinceSaturating(later, at) > levelGap {
			break
		}
		first = index
		later = at
	}
	var level float32
	var since time.Time
	var target float32
	latest := false
	for _, p := range h.points[first:] {
		if p.at.After(now) {
			continue
		}
		if latest {
			level = approach(level, target, sinceSaturating(p.at, since))
		}
		since, target, latest = p.at, scale(float64(p.peak)), true
	}
	if !latest {
		return 0
	}
	return approach(level, target, sinceSaturating(now, since))
}

// approach is where level is after closing on target for elapsed.
func approach(level, target float32, elapsed time.Duration) float32 {
	constant := levelFall
	if target > level {
		constant = levelRise
	}
	exponent := -float32(elapsed.Seconds()) / float32(constant.Seconds())
	return target + (level-target)*float32(math.Exp(float64(exponent)))
}

// LevelHistories is every reported path's readings, keyed by target id.
type LevelHistories struct {
	paths map[string]*LevelHistory
}

// Receive records one accepted meter event: each path's id and output peak.
// Paths the event no longer reports are dropped, so a path that returns
// starts over.
func (l *LevelHistories) Receive(at time.Time, paths iter.Seq2[string, float64]) {
	if l.paths == nil {
		l.paths = make(map[string]*LevelHistory)
	}
	var seen []string
	for id, peak := range paths {
		h, ok := l.paths[id]
		if !ok {
			h = &LevelHistory{}
			l.paths[id] = h
		}
		h.Push(at, peak)
		seen = append(seen, id)
	}
	for id := range l.paths {
		if !slices.Contains(seen, id) {
			delete(l.paths, id)
		}
	}
}

// Clear forgets everything: a new connection, engine or source.
func (l *LevelHistories) Clear() {
	clear(l.paths)
}

// Get is a path's readings, or nil.
func (l *LevelHistories) Get(id string) *LevelHistory {
	return l.paths[id]
}
