package installflow

// "Fix RØDE's update bug" on its own, for a mixer that stays on RØDE's
// firmware: the steps it shows, how far a run is among them, its ending,
// whether Try again can help, and whether the mixer may be unplugged. One
// request does the whole fix (installer.RequestStorageFix, without Final),
// so the page follows its progress. Package app draws it (fix_task.go).

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// The fix task's steps.
const (
	FixStepBefore = iota
	FixStepGet
	FixStepFix
	FixStepReinstall
	FixStepCheck
	// FixStepCount is the number of steps, and the step of a finished run.
	FixStepCount
)

var fixSteps = [FixStepCount]string{
	"Before you start",
	"Get RØDE’s firmware",
	"Fix the storage",
	"Reinstall RØDE’s firmware",
	"Check and finish",
}

// FixKind is how far the fix task is.
type FixKind int

const (
	FixBefore FixKind = iota
	// FixRunning: the request is in flight.
	FixRunning
	// FixEnded: the request returned; Reply or Error says how.
	FixEnded
)

// Fix is the fix task's state.
type Fix struct {
	Kind FixKind
	// Event is the run's newest progress; kept after it ends, for where it
	// stopped.
	Event *installer.Progress
	// Touched: the run got as far as telling the mixer to switch to update
	// mode, so it may not be as it was.
	Touched bool
	// Reply is what the run did, once it ended without an engine error.
	Reply *installer.StorageFix
	// Error is why the request failed (the engine, not the fix).
	Error string
	// Cancelling: Stop asked the run to stop before it touched the mixer.
	Cancelling bool
}

// NewFix is the task before it starts.
func NewFix() *Fix { return &Fix{Kind: FixBefore} }

// Busy: the fix's request is in flight.
func (f *Fix) Busy() bool { return f.Kind == FixRunning }

// IsStorageFix reports the fix's request.
func IsStorageFix(request installer.Request) bool {
	return request.Kind == installer.RequestStorageFix
}

// Start is the request that runs the fix, false when it can't start now:
// only before a run or after one that Try again can help (CanRetry).
func (f *Fix) Start(normal bool) (installer.Request, bool) {
	if f.Kind == FixRunning || (f.Kind == FixEnded && !f.CanRetry(normal)) {
		return installer.Request{}, false
	}
	*f = Fix{Kind: FixRunning}
	return installer.Request{Kind: installer.RequestStorageFix}, true
}

// NotSent: the runner did not take the request.
func (f *Fix) NotSent() {
	*f = Fix{Kind: FixEnded, Error: "The app's installer stopped unexpectedly. Close this page and open it again."}
}

// Progress hands over the run's newest progress.
func (f *Fix) Progress(p installer.Progress) {
	if f.Kind != FixRunning {
		return
	}
	if _, ok := installer.StorageFixStage(p); !ok {
		return
	}
	f.Event = &p
	f.Touched = f.Touched || FixTouching(&p)
}

// CanStop reports that Stop can stop the run now: before it touches the
// mixer.
func (f *Fix) CanStop() bool {
	return f.Kind == FixRunning && !f.Touched && !f.Cancelling
}

// Cancel asks the run to stop; false once it has touched the mixer.
func (f *Fix) Cancel() bool {
	if !f.CanStop() {
		return false
	}
	f.Cancelling = true
	return true
}

// Outcome is the finished request. A run stopped by Stop goes back to
// before it started.
func (f *Fix) Outcome(reply installer.Reply, err error) {
	if f.Kind != FixRunning {
		return
	}
	cancelling := f.Cancelling
	f.Kind, f.Cancelling = FixEnded, false
	switch {
	case err != nil && err.Error() == installer.Cancelled && cancelling:
		*f = Fix{Kind: FixBefore}
	case err != nil:
		f.Error = err.Error()
	case reply.Kind != installer.ReplyStorageFix || reply.StorageFix == nil:
		f.Error = "The installer gave an unexpected answer."
	default:
		r := *reply.StorageFix
		f.Reply = &r
		f.Touched = f.Touched || r.Touched
	}
}

// FixEnding is how a fix run ended.
type FixEnding int

const (
	// FixEndingNone: it hasn't ended.
	FixEndingNone FixEnding = iota
	// FixEndingFixed: the storage was fixed and RØDE's firmware is back on
	// it (G3).
	FixEndingFixed
	// FixEndingAlready: the mixer already had the fix (G4).
	FixEndingAlready
	// FixEndingUnrecognized: the storage is not laid out the way the app
	// expects; nothing was changed and there is nothing to retry.
	FixEndingUnrecognized
	// FixEndingNotApplied: the script could not finish, or reported nothing;
	// RØDE's firmware was installed again.
	FixEndingNotApplied
	// FixEndingRefused: the run would not start (Reply.Error says why);
	// starting again gives the same answer.
	FixEndingRefused
	// FixEndingStopped: the run stopped part way (Reply.Error or Error).
	FixEndingStopped
)

// Ending is how the run ended.
func (f *Fix) Ending() FixEnding {
	if f.Kind != FixEnded {
		return FixEndingNone
	}
	r := f.Reply
	switch {
	case r == nil:
		return FixEndingStopped
	case r.Refused:
		return FixEndingRefused
	case r.Error != "":
		return FixEndingStopped
	case r.Outcome == string(autofix.OutcomeFixed):
		return FixEndingFixed
	case r.Outcome == string(autofix.OutcomeAlreadyFixed):
		return FixEndingAlready
	case r.Outcome == string(autofix.OutcomeUnrecognized):
		return FixEndingUnrecognized
	}
	return FixEndingNotApplied
}

// CanRetry reports that Try again can help: the script couldn't finish or
// report, or the run stopped part way and the mixer is back alone in
// normal mode (normal), which a new run needs. Never after a refusal or an
// unrecognized layout, which give the same answer again.
func (f *Fix) CanRetry(normal bool) bool {
	switch f.Ending() {
	case FixEndingNotApplied:
		return normal
	case FixEndingStopped:
		return normal
	}
	return false
}

// SavedCopy reports that the run saved a copy of what it changed on this
// computer.
func (f *Fix) SavedCopy() bool {
	return f.Reply != nil && f.Reply.Backup != ""
}

// FixSeen is what the app sees of the mixer on USB now, for the fix task.
type FixSeen int

const (
	// FixSeenUnknown: no answer from USB yet.
	FixSeenUnknown FixSeen = iota
	FixSeenNone
	FixSeenTwo
	FixSeenUpdateMode
	FixSeenNormal
)

// FixSeenOf is what the model's last look at USB found.
func FixSeenOf(m *Model) FixSeen {
	switch usb := m.USB(); {
	case m.Status == nil:
		return FixSeenUnknown
	case len(usb) == 0:
		return FixSeenNone
	case len(usb) > 1:
		return FixSeenTwo
	case usb[0].UpdateMode:
		return FixSeenUpdateMode
	}
	return FixSeenNormal
}

// Step is the step the run is at: FixStepBefore before it, FixStepCount
// once it ended with RØDE's firmware back, else the step it is on or
// stopped at.
func (f *Fix) Step() int {
	switch f.Ending() {
	case FixEndingNone:
		if f.Kind == FixBefore {
			return FixStepBefore
		}
	case FixEndingFixed, FixEndingAlready, FixEndingUnrecognized, FixEndingNotApplied:
		return FixStepCount
	case FixEndingRefused:
		return FixStepGet
	}
	e := f.Event
	stage := ""
	if e != nil {
		stage, _ = installer.StorageFixStage(*e)
	}
	pass, _ := FixPass(e)
	switch {
	case e == nil, stage == autofix.StageFind, stage == autofix.StageDownload,
		stage == autofix.StageInspect, stage == autofix.StageBuild:
		return FixStepGet
	// Check and finish once the mixer is back from the second update (the
	// wait's settle has begun); while it installs and restarts, it is
	// still Reinstall.
	case pass == 2 && stage == autofix.StageWait && e.Total > 0, stage == autofix.StageFinish:
		return FixStepCheck
	case pass == 2:
		return FixStepReinstall
	}
	return FixStepFix
}

// Steps is the sidebar. A run that ended without fixing marks Fix the
// storage failed and the steps after it done, since RØDE's firmware was
// put back; one that stopped marks where it stopped.
func (f *Fix) Steps() []TaskStep {
	at := f.Step()
	ending := f.Ending()
	failedAt := -1
	switch ending {
	case FixEndingUnrecognized, FixEndingNotApplied:
		failedAt = FixStepFix
	case FixEndingRefused, FixEndingStopped:
		failedAt = at
	}
	steps := make([]TaskStep, FixStepCount)
	for n, title := range fixSteps {
		mark := MarkToCome
		switch {
		case n == failedAt:
			mark = MarkFailed
		case n < at:
			mark = MarkDone
		case n == at:
			mark = MarkNow
		}
		steps[n] = TaskStep{Title: title, Mark: mark}
	}
	return steps
}

// Unplug says whether the mixer may be unplugged: yes before the run
// touches it, and once it ended with the mixer back; no from the first
// update-mode command until then. seen is what the app sees on USB now.
func (f *Fix) Unplug(seen FixSeen) Unplug {
	switch {
	case f.Kind == FixBefore:
		return Unplug{Kind: UnplugYes, Reason: "nothing has changed yet"}
	case f.Kind == FixRunning && !f.Touched:
		return Unplug{Kind: UnplugYes, Reason: "nothing on the mixer has changed yet"}
	case f.Kind == FixRunning:
		return Unplug{Reason: "the mixer is updating"}
	}
	switch f.Ending() {
	case FixEndingFixed, FixEndingAlready, FixEndingUnrecognized, FixEndingNotApplied:
		// The run ended only once the mixer had been back for a while.
		return Unplug{Kind: UnplugYes}
	}
	if !f.Touched && f.Reply != nil {
		return Unplug{Kind: UnplugYes, Reason: "nothing on the mixer has changed"}
	}
	switch seen {
	case FixSeenNormal:
		return Unplug{Kind: UnplugYes, Reason: "the mixer is back to normal"}
	case FixSeenUpdateMode:
		return Unplug{Reason: "the mixer is in update mode"}
	case FixSeenNone:
		return Unplug{Reason: "the app can't see the mixer to check it"}
	}
	return Unplug{Reason: "the mixer may still be updating"}
}
