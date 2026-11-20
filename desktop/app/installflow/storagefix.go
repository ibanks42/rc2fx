package installflow

// Storage fix helpers shared between the Install task and the standalone fix
// task (fixtask.go).
//
// RØDE's updater formats the target system slot; because slot 1 is slightly
// under 512 MiB it gets 1 KB ext4 blocks, which can lose file data under the
// mixer's kernel. The fix grows slot 1 so future updates use 4 KB blocks.
// Until RC2FX can report the partition table directly, FixAuto only runs on
// Pro II mixers that are not running RC2FX.

import (
	"strings"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// WantsStorageFix reports whether a run started now should run the storage fix
// first (Pro II in normal mode only).
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

func (m *Model) FixRunning() bool {
	busy := m.BusyRequest()
	return m.Run != nil && m.Run.Fix != nil && busy != nil && busy.Kind == installer.RequestStorageFix
}

func (m *Model) FixProgress(p installer.Progress) {
	if !m.FixRunning() {
		return
	}
	if _, ok := installer.StorageFixStage(p); ok {
		m.Run.Fix.Event = &p
	}
}

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

// FixConfirming reports whether p is waiting for the mixer to report the fix
// result after the carrier update.
func FixConfirming(p *installer.Progress) bool {
	if p == nil {
		return false
	}
	pass, _ := FixPass(p)
	stage, _ := installer.StorageFixStage(*p)
	return pass == 1 && (stage == autofix.StageReport || stage == autofix.StageWait && p.Total > 0)
}

// FixTouching reports whether the fix has entered update mode and may have
// modified mixer state.
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

// FixPass returns which update pass p belongs to: 1 for the carrier flash
// (with 1-based attempt), 2 for the final firmware flash, or 0 before either.
func FixPass(p *installer.Progress) (pass, attempt int) {
	if p == nil || p.Flash == 0 {
		return 0, 0
	}
	if p.Carrier {
		return 1, p.Flash
	}
	return 2, 0
}

// FixTransfer returns the copy progress in [0, 1] during firmware upload, or
// nil for read-back/wait events that don't show a progress bar.
func FixTransfer(p *installer.Progress) *float32 {
	if p == nil || p.Total == 0 {
		return nil
	}
	if stage, _ := installer.StorageFixStage(*p); stage != autofix.StageCopy || !strings.HasPrefix(p.Detail, "Copying ") {
		return nil
	}
	f := min(max(float32(p.Completed)/float32(p.Total), 0), 1)
	return &f
}

type FixNoteKind int

const (
	FixNoteNone FixNoteKind = iota
	FixNoteFixed
	FixNoteUnrecognized
	FixNoteNotApplied
)

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

// FixStopped reports whether the fix failed mid-way so Retry will fall back to
// a normal flash.
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

// FixUnsafe reports whether the fix halted to avoid flashing an undersized
// slot, meaning Retry must rerun the fix rather than flash normally.
func (m *Model) FixUnsafe() bool {
	run := m.Run
	return run != nil && run.status.Kind == RunFailed && run.Fix != nil && run.Fix.Reply != nil && run.Fix.Reply.Unsafe
}
