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

func TestLivePreviewAndSave(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	saved := liveTargets(0)
	l.Observe(saved, at)
	draft := liveTargets(2)
	l.Observe(draft, at.Add(time.Second))
	if step := l.NextPreview(draft, saved, at.Add(time.Second)); step != PreviewSend {
		t.Fatalf("step %v, want send", step)
	}
	// One at a time.
	if step := l.NextPreview(liveTargets(3), saved, at.Add(time.Second)); step != PreviewNothing {
		t.Fatalf("step %v while in flight, want nothing", step)
	}
	if l.SaveDue(draft, saved, true, at.Add(10*time.Second)) {
		t.Fatal("no save while a preview is in flight")
	}
	l.PreviewAnswered(true, false, false, false, at.Add(1100*time.Millisecond))
	if l.SaveDue(draft, saved, true, at.Add(2*time.Second)) {
		t.Fatal("save before SaveDelay")
	}
	if !l.SaveDue(draft, saved, true, at.Add(3100*time.Millisecond)) {
		t.Fatal("save due after SaveDelay with the draft heard")
	}
	l.Saved(draft)
	if !TargetsEqual(l.Running(draft), draft) || l.running != nil {
		t.Fatal("after the save the audio plays the saved settings")
	}
	// Suppression switched on needs a restart: not previewed.
	restart := liveTargets(2)
	restart[0].Suppression.Enabled = true
	if step := l.NextPreview(restart, draft, at.Add(4*time.Second)); step != PreviewRestart {
		t.Fatalf("step %v, want restart", step)
	}
	// An older mixer: saves sooner, without previews.
	l.inFlight = true
	l.sent = core.CloneTargets(restart)
	l.PreviewAnswered(false, true, false, false, at.Add(5*time.Second))
	if l.NextPreview(liveTargets(9), draft, at.Add(6*time.Second)) != PreviewNothing {
		t.Fatal("no previews to a mixer without them")
	}
	l.Observe(liveTargets(9), at.Add(6*time.Second))
	if !l.SaveDue(liveTargets(9), draft, true, at.Add(6*time.Second+SaveDelayWithoutPreview)) {
		t.Fatal("without preview the save comes after SaveDelayWithoutPreview")
	}
}

func TestLiveCompressorWaitsForPause(t *testing.T) {
	var l Live
	at := time.Unix(1000, 0)
	saved := liveTargets(0)
	l.Observe(saved, at)
	draft := liveTargets(0)
	draft[0].Compressor.Threshold = -30
	l.Observe(draft, at.Add(time.Second))
	if step := l.NextPreview(draft, saved, at.Add(time.Second+100*time.Millisecond)); step != PreviewNothing {
		t.Fatalf("step %v mid-drag, want nothing", step)
	}
	if step := l.NextPreview(draft, saved, at.Add(time.Second+UnsmoothedSettle)); step != PreviewSend {
		t.Fatalf("step %v after the pause, want send", step)
	}
}
