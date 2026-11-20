package model

// Live editing state machine: pushes edits to the mixer's live audio immediately,
// auto-saves after edits pause, and tracks grouped undo/redo plus per-channel
// revert checkpoints. Driven by App.poll (no gk dependency).

import (
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// LiveSupport tracks whether the connected mixer supports the live-edit API.
type LiveSupport int

const (
	LiveUnknown LiveSupport = iota
	LiveYes
	// Older mixer without api/v1/live; edits wait for auto-save instead.
	LiveNo
)

const (
	// Idle gap that closes an undo group so a continuous drag undoes as one step.
	UndoGroup = 2 * time.Second
	// Idle delay after the last edit before auto-saving.
	SaveDelay = 2 * time.Second
	// Shorter auto-save delay on mixers without live editing, where changes aren't audible until saved.
	SaveDelayWithoutLive = 600 * time.Millisecond
	// Idle time on a channel before its next edit starts a fresh revert checkpoint.
	CheckpointQuiet = 10 * time.Minute
	// Backoff after the mixer responds busy to a live edit.
	LiveRetry = 250 * time.Millisecond
	// Exponential backoff bounds for consecutive save failures.
	SaveRetryFirst = time.Second
	SaveRetryMax   = 30 * time.Second
	undoLimit      = 200
)

// Checkpoint snapshots a channel's settings before its current editing session.
type Checkpoint struct {
	Target core.Target
	At     time.Time
}

// Live holds the live-editing, undo, and auto-save state for one connection.
type Live struct {
	Support LiveSupport
	// Active live targets when they differ from saved; nil when audio matches saved.
	running  []core.Target
	inFlight bool
	sent     []core.Target
	retryAt  time.Time
	// Last observed draft and timestamp of its most recent change.
	lastDraft  []core.Target
	lastChange time.Time
	undo, redo [][]core.Target
	// Cleared after undo/redo so the next edit opens a new undo step.
	grouping     bool
	checkpoints  map[string]Checkpoint
	lastEdit     map[string]time.Time
	saveFailures int
	saveRetryAt  time.Time
	saveError    string
}

// Reset clears all state for a new connection.
func (l *Live) Reset() { *l = Live{} }

// Observe records the current draft, grouping edits into undo steps and
// capturing per-channel checkpoints after CheckpointQuiet. Returns true if the
// draft changed.
func (l *Live) Observe(draft []core.Target, now time.Time) bool {
	if l.lastDraft == nil {
		l.lastDraft = core.CloneTargets(draft)
		return false
	}
	if core.TargetsEqual(draft, l.lastDraft) {
		return false
	}
	if !l.grouping || now.Sub(l.lastChange) > UndoGroup {
		l.undo = append(l.undo, l.lastDraft)
		if len(l.undo) > undoLimit {
			l.undo = l.undo[len(l.undo)-undoLimit:]
		}
		l.redo = nil
	}
	if l.checkpoints == nil {
		l.checkpoints = map[string]Checkpoint{}
		l.lastEdit = map[string]time.Time{}
	}
	for i := range draft {
		before := targetByID(l.lastDraft, draft[i].ID)
		if before == nil || before.Equal(draft[i]) {
			continue
		}
		if last, ok := l.lastEdit[draft[i].ID]; !ok || now.Sub(last) > CheckpointQuiet {
			l.checkpoints[draft[i].ID] = Checkpoint{Target: before.Clone(), At: now}
		}
		l.lastEdit[draft[i].ID] = now
	}
	l.grouping = true
	l.lastDraft = core.CloneTargets(draft)
	l.lastChange = now
	l.saveFailures = 0
	l.saveRetryAt = time.Time{}
	return true
}

func (l *Live) CanUndo() bool { return len(l.undo) > 0 }
func (l *Live) CanRedo() bool { return len(l.redo) > 0 }

// Undo pops the previous undo snapshot and pushes draft onto the redo stack.
func (l *Live) Undo(draft []core.Target, now time.Time) ([]core.Target, bool) {
	if len(l.undo) == 0 {
		return nil, false
	}
	previous := l.undo[len(l.undo)-1]
	l.undo = l.undo[:len(l.undo)-1]
	l.redo = append(l.redo, core.CloneTargets(draft))
	l.jump(previous, now)
	return core.CloneTargets(previous), true
}

// Redo pops the next redo snapshot and pushes draft onto the undo stack.
func (l *Live) Redo(draft []core.Target, now time.Time) ([]core.Target, bool) {
	if len(l.redo) == 0 {
		return nil, false
	}
	next := l.redo[len(l.redo)-1]
	l.redo = l.redo[:len(l.redo)-1]
	l.undo = append(l.undo, core.CloneTargets(draft))
	l.jump(next, now)
	return core.CloneTargets(next), true
}

func (l *Live) jump(draft []core.Target, now time.Time) {
	l.lastDraft = core.CloneTargets(draft)
	l.lastChange = now
	l.grouping = false
	l.saveFailures = 0
	l.saveRetryAt = time.Time{}
}

// Changed reports whether current differs from its checkpoint and returns when
// that checkpoint was taken.
func (l *Live) Changed(current core.Target) (time.Time, bool) {
	cp, ok := l.checkpoints[current.ID]
	if !ok || cp.Target.Equal(current) {
		return time.Time{}, false
	}
	return cp.At, true
}

// CheckpointOf returns a clone of channel id's checkpoint, if one exists.
func (l *Live) CheckpointOf(id string) (core.Target, bool) {
	cp, ok := l.checkpoints[id]
	return cp.Target.Clone(), ok
}

// Running returns the currently audible targets (live override if active, else saved).
func (l *Live) Running(saved []core.Target) []core.Target {
	if l.running != nil {
		return l.running
	}
	return saved
}

// LiveStep is the action NextLive recommends for the current draft.
type LiveStep int

const (
	LiveNothing LiveStep = iota
	// Send the draft as a live edit (already marked in-flight by NextLive).
	LiveSend
	// Draft requires an engine restart and can only take effect via save.
	LiveRestart
)

// NextLive decides whether to push draft live now, require a restart, or wait.
func (l *Live) NextLive(draft, saved []core.Target, now time.Time) LiveStep {
	if l.Support == LiveNo || l.inFlight || now.Before(l.retryAt) {
		return LiveNothing
	}
	running := l.Running(saved)
	if core.TargetsEqual(draft, running) {
		return LiveNothing
	}
	if core.LiveNeedsSave(running, draft) {
		return LiveRestart
	}
	l.inFlight = true
	l.sent = core.CloneTargets(draft)
	return LiveSend
}

// LiveAnswered records the mixer's reply to the in-flight live edit and returns
// whether the mixer rejected it as requiring a full save/restart.
func (l *Live) LiveAnswered(accepted, unsupported, busy, needsSave bool, now time.Time) bool {
	l.inFlight = false
	switch {
	case accepted:
		l.Support = LiveYes
		l.running = l.sent
	case unsupported:
		l.Support = LiveNo
	case busy:
		l.retryAt = now.Add(LiveRetry)
	}
	l.sent = nil
	return needsSave
}

// Saved clears the live override once the saved settings catch up to it.
func (l *Live) Saved(saved []core.Target) {
	if l.running != nil && core.TargetsEqual(l.running, saved) {
		l.running = nil
	}
}

// SaveFailed records a save failure and schedules exponential backoff up to SaveRetryMax.
func (l *Live) SaveFailed(reason string, now time.Time) {
	wait := SaveRetryFirst
	for range l.saveFailures {
		wait *= 2
		if wait >= SaveRetryMax {
			wait = SaveRetryMax
			break
		}
	}
	l.saveFailures++
	l.saveRetryAt = now.Add(wait)
	l.saveError = reason
}

// SaveSucceeded clears save failure state after a successful save.
func (l *Live) SaveSucceeded() {
	l.saveFailures = 0
	l.saveRetryAt = time.Time{}
	l.saveError = ""
}

// SaveError returns the most recent save error message, or "" if none.
func (l *Live) SaveError() string { return l.saveError }

// SaveDue reports whether it is time to auto-save draft: it is dirty, edits have
// settled, no live edit or retry backoff is active, and either the live audio
// already matches draft or live editing is unsupported.
func (l *Live) SaveDue(draft, saved []core.Target, dirty bool, now time.Time) bool {
	if !dirty || l.inFlight || now.Before(l.saveRetryAt) {
		return false
	}
	delay := SaveDelay
	if l.Support == LiveNo {
		delay = SaveDelayWithoutLive
	}
	if now.Sub(l.lastChange) < delay {
		return false
	}
	return l.Support == LiveNo || core.TargetsEqual(draft, l.Running(saved))
}

// Quiet returns the duration since the draft last changed.
func (l *Live) Quiet(now time.Time) time.Duration { return now.Sub(l.lastChange) }

func targetByID(targets []core.Target, id string) *core.Target {
	for i := range targets {
		if targets[i].ID == id {
			return &targets[i]
		}
	}
	return nil
}
