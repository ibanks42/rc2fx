package model

// Recent post-processing output peaks per path for smoothed level meters.
// Meter events carry no device timestamp, so arrival times are approximate
// (including transport jitter). App.poll drives redraws when readings stop.

import (
	"iter"
	"math"
	"slices"
	"time"
)

const (
	// Retention window for past readings; older samples no longer affect the curve.
	levelKeep     = time.Second
	levelCapacity = 300
	// Exponential smoothing time constants (attack / release) so sparse meter
	// updates ease smoothly without feeling sluggish on peaks.
	levelRise = 40 * time.Millisecond
	levelFall = 120 * time.Millisecond
	// Gaps longer than 1 s start a fresh segment (matching telemetry.Progress.Fresh)
	// so stale readings from before a dropout never bleed into new ones.
	levelGap = time.Second
)

type levelPoint struct {
	at   time.Time
	peak float32
}

// LevelHistory stores recent peak readings for one path in chronological order.
type LevelHistory struct {
	points []levelPoint
}

// Scale maps a linear peak amplitude to a [0, 1] meter position.
type Scale func(peak float64) float32

func sinceSaturating(a, b time.Time) time.Duration {
	return max(a.Sub(b), 0)
}

// Push appends a reading, dropping non-finite or out-of-order samples.
func (h *LevelHistory) Push(at time.Time, peak float64) {
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

// Display evaluates the smoothed [0, 1] meter level at now. Because smoothing
// is integrated directly from sample timestamps, the result is independent of
// frame rate. Only samples after the most recent gap (> levelGap) contribute.
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

func approach(level, target float32, elapsed time.Duration) float32 {
	constant := levelFall
	if target > level {
		constant = levelRise
	}
	exponent := -float32(elapsed.Seconds()) / float32(constant.Seconds())
	return target + (level-target)*float32(math.Exp(float64(exponent)))
}

// LevelHistories tracks LevelHistory instances keyed by target ID.
type LevelHistories struct {
	paths map[string]*LevelHistory
}

// Receive records one meter event and drops any paths no longer reported so
// they start clean if they reappear.
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

// Clear drops all stored histories (e.g. on reconnect or engine change).
func (l *LevelHistories) Clear() {
	clear(l.paths)
}

// Get returns the history for id, or nil.
func (l *LevelHistories) Get(id string) *LevelHistory {
	return l.paths[id]
}
