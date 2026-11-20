package installflow

// The storage fix's decisions shared by the Install task and the fix on its
// own (fixtask.go): whether an install runs it, where its progress puts the
// run, which of its updates the mixer is on, and what the finished install
// says about it. installer.RequestStorageFix runs it (rodecaster/autofix).
//
// RØDE's updater formats the system slot it is about to write. Slot 1 is
// slightly under 512 MiB, so it gets 1 KB blocks, on which the mixer's
// kernel sometimes loses file data; the fix grows slot 1 so it gets 4 KB
// blocks from then on. A mixer running RC2FX could be asked for its
// partition table, but RC2FX cannot report it yet, so the fix runs only
// where RC2FX isn't running.

import (
	"strings"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// WantsStorageFix reports that a run started now fixes the mixer's storage
// first: RC2FX firmware for one Pro II in normal mode that does not run
// RC2FX (FixAuto), unless the policy says otherwise. autofix refuses
// anything but the Pro II.
func (m *Model) WantsStorageFix() bool {
	if m.Stock || m.Device != firmware.ModelProIi || m.usbOne(false) == nil {
		return false
	}
	switch m.FixPolicy {
	case FixNever:
		return false
	case FixAlways:
		return true
	}
	return !m.Answering() && m.Installed == nil
}

// FixRunning reports the storage fix's request in flight.
func (m *Model) FixRunning() bool {
	busy := m.BusyRequest()
	return m.Run != nil && m.Run.Fix != nil && busy != nil && busy.Kind == installer.RequestStorageFix
}

// FixProgress hands over the fix's newest progress while it runs.
func (m *Model) FixProgress(p installer.Progress) {
	if !m.FixRunning() {
		return
	}
	if _, ok := installer.StorageFixStage(p); ok {
		m.Run.Fix.Event = &p
	}
}

// FixWaiting reports a fix progress event from a wait for the mixer: its
// restarts after a flash, or reading what the fix found.
func FixWaiting(p *installer.Progress) bool {
	if p == nil {
		return false
	}
	switch stage, _ := installer.StorageFixStage(*p); stage {
	case autofix.StageWait, autofix.StageReport, autofix.StageReadResult:
		return true
	}
	return false
}

// FixConfirming reports a fix progress event from the wait for the mixer,
// back after the update that carries the fix, to report the fix's result.
func FixConfirming(p *installer.Progress) bool {
	if p == nil {
		return false
	}
	pass, _ := FixPass(p)
	stage, _ := installer.StorageFixStage(*p)
	return pass == 1 && (stage == autofix.StageReport || stage == autofix.StageWait && p.Total > 0)
}

// FixTouching reports a fix progress event from after the mixer was first
// told to switch to update mode: from then on it may not be as it was.
func FixTouching(p *installer.Progress) bool {
	if p == nil {
		return false
	}
	switch stage, _ := installer.StorageFixStage(*p); stage {
	case autofix.StageUpdateMode, autofix.StageCopy, autofix.StageFlash, autofix.StageWait, autofix.StageReport, autofix.StageReadResult:
		return true
	}
	return false
}

// FixPass is which of the fix's two updates an event belongs to: 1 for the
// one that carries the fix (attempt counts its tries, from 1), 2 for the one
// that puts the firmware back on the fixed storage; 0 before either.
func FixPass(p *installer.Progress) (pass, attempt int) {
	if p == nil || p.Flash == 0 {
		return 0, 0
	}
	if p.Carrier {
		return 1, p.Flash
	}
	return 2, 0
}

// FixTransfer is how far copying the firmware to the mixer is, 0 to 1, for
// a fix event that is that copy; nil for anything else (reading it back,
// waiting), which has no bar.
func FixTransfer(p *installer.Progress) *float32 {
	if p == nil || p.Total == 0 {
		return nil
	}
	// autofix's text for the copy itself, not for reading it back.
	if stage, _ := installer.StorageFixStage(*p); stage != autofix.StageCopy || !strings.HasPrefix(p.Detail, "Copying ") {
		return nil
	}
	f := min(max(float32(p.Completed)/float32(p.Total), 0), 1)
	return &f
}

// FixNoteKind is what a finished install says about the storage fix.
type FixNoteKind int

const (
	// FixNoteNone: nothing; the run didn't try it, or the mixer already
	// had it.
	FixNoteNone FixNoteKind = iota
	// FixNoteFixed: the storage was fixed too.
	FixNoteFixed
	// FixNoteUnrecognized: the storage is not laid out the way the app
	// expects; it was left as it was.
	FixNoteUnrecognized
	// FixNoteNotApplied: it could not be applied this time.
	FixNoteNotApplied
)

// FixNote is what the finished install says about the storage fix it
// tried. Only an outcome the script reported counts as fixed.
func (m *Model) FixNote() FixNoteKind {
	if m.finished == nil || m.finished.Fix == nil {
		return FixNoteNone
	}
	reply := m.finished.Fix.Reply
	switch {
	case reply == nil:
		return FixNoteNotApplied
	case reply.Outcome == string(autofix.OutcomeFixed):
		return FixNoteFixed
	case reply.Outcome == string(autofix.OutcomeAlreadyFixed):
		return FixNoteNone
	case reply.Outcome == string(autofix.OutcomeUnrecognized):
		return FixNoteUnrecognized
	}
	return FixNoteNotApplied
}

// FixStopped reports a failed run whose storage fix stopped part way, so
// trying again installs with a normal flash.
func (m *Model) FixStopped() bool {
	run := m.Run
	if run == nil || run.status.Kind != RunFailed || run.Fix == nil {
		return false
	}
	if run.Fix.Reply != nil && run.Fix.Reply.Unsafe {
		return false
	}
	return run.Fix.Error != "" || (run.Fix.Reply != nil && run.Fix.Reply.Touched && !run.Fix.Reply.FinalSent)
}

// FixUnsafe reports a failed run whose storage fix stopped because the slot
// a flash would format is still too small for 4 KB blocks; trying again
// runs the fix again.
func (m *Model) FixUnsafe() bool {
	run := m.Run
	return run != nil && run.status.Kind == RunFailed && run.Fix != nil && run.Fix.Reply != nil && run.Fix.Reply.Unsafe
}
