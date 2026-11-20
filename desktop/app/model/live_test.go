package model

import (
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func liveTargets(gain float64) []core.Target {
	return []core.Target{{ID: "mic-1", Kind: core.TargetKindMic, Channels: 1,
		Order: []core.Effect{core.EffectEQ}, Gain: core.Gain{DB: gain}}}
}

func TestLiveUndoGroupsBursts(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	l.Observe(liveTargets(0), at)
	// One drag: three changes within UndoGroup of each other.
	l.Observe(liveTargets(1), at.Add(100*time.Millisecond))
	l.Observe(liveTargets(2), at.Add(500*time.Millisecond))
	l.Observe(liveTargets(3), at.Add(900*time.Millisecond))
	// A later, separate edit.
	l.Observe(liveTargets(5), at.Add(5*time.Second))
	got, ok := l.Undo(liveTargets(5), at.Add(6*time.Second))
	if !ok || got[0].Gain.DB != 3 {
		t.Fatalf("first undo: %v %v, want gain 3", ok, got)
	}
	got, ok = l.Undo(got, at.Add(7*time.Second))
	if !ok || got[0].Gain.DB != 0 {
		t.Fatalf("second undo: %v %v, want gain 0", ok, got)
	}
	if l.CanUndo() {
		t.Fatal("nothing left to undo")
	}
	got, ok = l.Redo(got, at.Add(8*time.Second))
	if !ok || got[0].Gain.DB != 3 {
		t.Fatalf("redo: %v %v, want gain 3", ok, got)
	}
	// An edit right after a redo is a new step, and clears what is left to
	// redo.
	l.Observe(liveTargets(4), at.Add(8100*time.Millisecond))
	if l.CanRedo() {
		t.Fatal("an edit clears redo")
	}
	got, _ = l.Undo(liveTargets(4), at.Add(9*time.Second))
	if got[0].Gain.DB != 3 {
		t.Fatalf("undo after redo: %v, want gain 3", got)
	}
}

func TestLiveCheckpoint(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	l.Observe(liveTargets(0), at)
	l.Observe(liveTargets(2), at.Add(time.Second))
	l.Observe(liveTargets(4), at.Add(2*time.Minute))
	since, changed := l.Changed(liveTargets(4)[0])
	if !changed || !since.Equal(at.Add(time.Second)) {
		t.Fatalf("changed %v since %v", changed, since)
	}
	cp, _ := l.CheckpointOf("mic-1")
	if cp.Gain.DB != 0 {
		t.Fatalf("checkpoint gain %v, want 0", cp.Gain.DB)
	}
	// After CheckpointQuiet without edits, the next edit starts a new one.
	l.Observe(liveTargets(6), at.Add(2*time.Minute+CheckpointQuiet+time.Second))
	cp, _ = l.CheckpointOf("mic-1")
	if cp.Gain.DB != 4 {
		t.Fatalf("new checkpoint gain %v, want 4", cp.Gain.DB)
	}
	if _, changed := l.Changed(liveTargets(4)[0]); changed {
		t.Fatal("back at the checkpoint is not changed")
	}
}

func TestLiveEditAndSave(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	saved := liveTargets(0)
	l.Observe(saved, at)
	draft := liveTargets(2)
	l.Observe(draft, at.Add(time.Second))
	if step := l.NextLive(draft, saved, at.Add(time.Second)); step != LiveSend {
		t.Fatalf("step %v, want send", step)
	}
	// One at a time.
	if step := l.NextLive(liveTargets(3), saved, at.Add(time.Second)); step != LiveNothing {
		t.Fatalf("step %v while in flight, want nothing", step)
	}
	if l.SaveDue(draft, saved, true, at.Add(10*time.Second)) {
		t.Fatal("no save while a live edit is in flight")
	}
	l.LiveAnswered(true, false, false, false, at.Add(1100*time.Millisecond))
	if l.SaveDue(draft, saved, true, at.Add(2*time.Second)) {
		t.Fatal("save before SaveDelay")
	}
	if !l.SaveDue(draft, saved, true, at.Add(3100*time.Millisecond)) {
		t.Fatal("save due after SaveDelay with the draft heard")
	}
	l.Saved(draft)
	if !core.TargetsEqual(l.Running(draft), draft) || l.running != nil {
		t.Fatal("after the save the audio plays the saved settings")
	}
	// Suppression switched on needs a restart: not sent live.
	restart := liveTargets(2)
	restart[0].Suppression.Enabled = true
	if step := l.NextLive(restart, draft, at.Add(4*time.Second)); step != LiveRestart {
		t.Fatalf("step %v, want restart", step)
	}
	// An older mixer: saves sooner, without live editing.
	l.inFlight = true
	l.sent = core.CloneTargets(restart)
	l.LiveAnswered(false, true, false, false, at.Add(5*time.Second))
	if l.NextLive(liveTargets(9), draft, at.Add(6*time.Second)) != LiveNothing {
		t.Fatal("no live edits to a mixer without them")
	}
	l.Observe(liveTargets(9), at.Add(6*time.Second))
	if !l.SaveDue(liveTargets(9), draft, true, at.Add(6*time.Second+SaveDelayWithoutLive)) {
		t.Fatal("without live editing the save comes after SaveDelayWithoutLive")
	}
}

// The mixer smooths compressor settings like any other, so a compressor
// edit goes live at once, mid-drag.
func TestLiveCompressorGoesLiveMidDrag(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	saved := liveTargets(0)
	l.Observe(saved, at)
	draft := liveTargets(0)
	draft[0].Compressor.Threshold = -30
	l.Observe(draft, at.Add(time.Second))
	if step := l.NextLive(draft, saved, at.Add(time.Second+10*time.Millisecond)); step != LiveSend {
		t.Fatalf("step %v mid-drag, want send", step)
	}
}

// A failed save is not resent at every poll: it waits 1 s, then twice as long
// after each failure in a row up to SaveRetryMax, and an edit ends the wait.
func TestLiveFailedSaveBacksOff(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	saved := liveTargets(0)
	l.Observe(saved, at)
	l.Support = LiveNo
	draft := liveTargets(2)
	l.Observe(draft, at)
	now := at.Add(SaveDelayWithoutLive)
	if !l.SaveDue(draft, saved, true, now) {
		t.Fatal("the save is not due")
	}
	if l.SaveError() != "" {
		t.Fatal("an error before any save failed")
	}
	for _, wait := range []time.Duration{1, 2, 4, 8, 16, 30, 30} {
		wait *= time.Second
		l.SaveFailed("disk full", now)
		if l.SaveError() != "disk full" {
			t.Fatalf("SaveError = %q", l.SaveError())
		}
		if l.SaveDue(draft, saved, true, now.Add(wait-time.Millisecond)) {
			t.Fatalf("saved again before %v", wait)
		}
		now = now.Add(wait)
		if !l.SaveDue(draft, saved, true, now) {
			t.Fatalf("not saved again after %v", wait)
		}
	}
	// An edit ends the wait: the new draft is saved once edits stop, and a
	// failure of it waits the first step again.
	l.SaveFailed("disk full", now)
	edited := liveTargets(3)
	l.Observe(edited, now.Add(time.Millisecond))
	if !l.SaveDue(edited, saved, true, now.Add(time.Millisecond+SaveDelayWithoutLive)) {
		t.Fatal("an edit did not end the wait")
	}
	if l.SaveError() != "disk full" {
		t.Fatal("an edit is not a successful save: the error stays shown")
	}
	l.SaveFailed("disk full", now.Add(time.Second))
	if !l.SaveDue(edited, saved, true, now.Add(2*time.Second)) {
		t.Fatal("the first failure of a new draft waits more than SaveRetryFirst")
	}
	// So does undo, which changes the draft without an edit.
	l.SaveFailed("disk full", now.Add(2*time.Second))
	previous, ok := l.Undo(edited, now.Add(2*time.Second))
	if !ok {
		t.Fatal("nothing to undo")
	}
	if !l.SaveDue(previous, saved, true, now.Add(2*time.Second+SaveDelayWithoutLive)) {
		t.Fatal("undo did not end the wait")
	}
	l.SaveSucceeded()
	if l.SaveError() != "" {
		t.Fatalf("SaveError after a save = %q", l.SaveError())
	}
}
