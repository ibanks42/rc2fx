package model

// Live editing: every edit is heard at once (a live preview on the mixer)
// and saved once edits stop, with undo in steps and a checkpoint per channel
// to go back to. This file does not import gk: the app's poll drives it.

import (
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// PreviewSupport is whether the mixer takes live previews.
type PreviewSupport int

const (
	PreviewUnknown PreviewSupport = iota
	PreviewYes
	// PreviewNo: an older mixer (no api/v1/preview). Edits are saved shortly
	// after they stop instead.
	PreviewNo
)

const (
	// UndoGroup is the pause that ends one undo step: a drag, or a burst of
	// edits with no longer pause, undoes as one.
	UndoGroup = 2 * time.Second
	// SaveDelay is how long after the last edit the settings are saved.
	SaveDelay = 2 * time.Second
	// SaveDelayWithoutPreview is SaveDelay for a mixer without preview, where
	// nothing is heard until it saves.
	SaveDelayWithoutPreview = 600 * time.Millisecond
	// CheckpointQuiet is how long a channel goes without edits before its
	// next edit starts a new checkpoint.
	CheckpointQuiet = 10 * time.Minute
	// UnsmoothedSettle is how long compressor and limiter edits rest before
	// they are previewed: their settings are not smoothed on the mixer, so
	// they go out once a drag pauses, not at every step of it.
	UnsmoothedSettle = 300 * time.Millisecond
	// PreviewRetry is the wait after the mixer said it was busy.
	PreviewRetry = 250 * time.Millisecond
	// undoLimit is the most undo steps kept.
	undoLimit = 200
)

// Checkpoint is a channel as it was before its current round of edits.
type Checkpoint struct {
	Target core.Target
	At     time.Time
}

// Live is the live-editing state of one connection.
type Live struct {
	Support PreviewSupport
	// running is what the audio plays when a preview differs from the saved
	// settings; nil when it plays the saved settings.
	running []core.Target
	// inFlight: a preview was sent and has no answer yet; sent is it.
	inFlight bool
	sent     []core.Target
	retryAt  time.Time
	// lastDraft is the draft at the last Observe; lastChange when it last
	// changed.
	lastDraft  []core.Target
	lastChange time.Time
	undo, redo [][]core.Target
	// grouping is false right after an undo or redo, so the next edit starts
	// a new step.
	grouping    bool
	checkpoints map[string]Checkpoint
	lastEdit    map[string]time.Time
}

// Reset forgets everything: a new connection.
func (l *Live) Reset() { *l = Live{} }

// Observe notes the draft at now: a change opens an undo step after a pause
// (UndoGroup) and a checkpoint for a channel quiet for CheckpointQuiet. It
// reports whether the draft changed.
func (l *Live) Observe(draft []core.Target, now time.Time) bool {
	if l.lastDraft == nil {
		l.lastDraft = core.CloneTargets(draft)
		return false
	}
	if TargetsEqual(draft, l.lastDraft) {
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
	return true
}

// CanUndo and CanRedo are whether there is a step to go back or forward to.
func (l *Live) CanUndo() bool { return len(l.undo) > 0 }
func (l *Live) CanRedo() bool { return len(l.redo) > 0 }

// Undo is the draft before the last step, given the current one; ok false
// with nothing to undo. The caller puts it in the draft.
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

// Redo is Undo's reverse.
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

// jump records a draft set by undo or redo, which is not itself an edit.
func (l *Live) jump(draft []core.Target, now time.Time) {
	l.lastDraft = core.CloneTargets(draft)
	l.lastChange = now
	l.grouping = false
}

// Changed is when channel id's checkpoint was taken, if its settings differ
// from it now.
func (l *Live) Changed(current core.Target) (time.Time, bool) {
	cp, ok := l.checkpoints[current.ID]
	if !ok || cp.Target.Equal(current) {
		return time.Time{}, false
	}
	return cp.At, true
}

// CheckpointOf is channel id's checkpoint, if it has one.
func (l *Live) CheckpointOf(id string) (core.Target, bool) {
	cp, ok := l.checkpoints[id]
	return cp.Target.Clone(), ok
}

// Running is what the audio plays: the preview, or else saved.
func (l *Live) Running(saved []core.Target) []core.Target {
	if l.running != nil {
		return l.running
	}
	return saved
}

// PreviewStep is what to do with the draft now.
type PreviewStep int

const (
	PreviewNothing PreviewStep = iota
	// PreviewSend: send the draft as a preview (NextPreview marked it sent).
	PreviewSend
	// PreviewRestart: the draft needs the engine restarted, so only a save
	// can make it heard.
	PreviewRestart
)

// NextPreview is what to do with draft at now, given the saved targets.
func (l *Live) NextPreview(draft, saved []core.Target, now time.Time) PreviewStep {
	if l.Support == PreviewNo || l.inFlight || now.Before(l.retryAt) {
		return PreviewNothing
	}
	running := l.Running(saved)
	if TargetsEqual(draft, running) {
		return PreviewNothing
	}
	if core.PreviewNeedsSave(running, draft) {
		return PreviewRestart
	}
	if unsmoothedChange(running, draft) && now.Sub(l.lastChange) < UnsmoothedSettle {
		return PreviewNothing
	}
	l.inFlight = true
	l.sent = core.CloneTargets(draft)
	return PreviewSend
}

// PreviewAnswered takes the mixer's answer to the preview in flight. It
// reports whether the change needs a save instead (the mixer refused it as
// needing a restart).
func (l *Live) PreviewAnswered(accepted, unsupported, busy, needsSave bool, now time.Time) bool {
	l.inFlight = false
	switch {
	case accepted:
		l.Support = PreviewYes
		l.running = l.sent
	case unsupported:
		l.Support = PreviewNo
	case busy:
		l.retryAt = now.Add(PreviewRetry)
	}
	l.sent = nil
	return needsSave
}

// Saved takes newly saved settings: a preview that matches them is now just
// the saved settings.
func (l *Live) Saved(saved []core.Target) {
	if l.running != nil && TargetsEqual(l.running, saved) {
		l.running = nil
	}
}

// SaveDue is whether to save now: the draft differs from the saved settings
// (dirty), edits stopped long enough ago, no preview is in flight, and the
// audio already plays the draft (so the save changes nothing heard) or the
// mixer has no preview.
func (l *Live) SaveDue(draft, saved []core.Target, dirty bool, now time.Time) bool {
	if !dirty || l.inFlight {
		return false
	}
	delay := SaveDelay
	if l.Support == PreviewNo {
		delay = SaveDelayWithoutPreview
	}
	if now.Sub(l.lastChange) < delay {
		return false
	}
	return l.Support == PreviewNo || TargetsEqual(draft, l.Running(saved))
}

// Quiet is how long since the draft last changed.
func (l *Live) Quiet(now time.Time) time.Duration { return now.Sub(l.lastChange) }

// TargetsEqual is whether two target lists hold the same settings.
func TargetsEqual(a, b []core.Target) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if !a[i].Equal(b[i]) {
			return false
		}
	}
	return true
}

// unsmoothedChange is whether draft changes a compressor or limiter from
// running.
func unsmoothedChange(running, draft []core.Target) bool {
	for i := range draft {
		now := targetByID(running, draft[i].ID)
		if now == nil {
			continue
		}
		if now.Compressor != draft[i].Compressor || now.Limiter != draft[i].Limiter {
			return true
		}
	}
	return false
}

func targetByID(targets []core.Target, id string) *core.Target {
	for i := range targets {
		if targets[i].ID == id {
			return &targets[i]
		}
	}
	return nil
}
