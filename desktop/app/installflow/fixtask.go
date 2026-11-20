package installflow

// Standalone "Fix RØDE's update bug" flow for mixers staying on RØDE's
// firmware. A single RequestStorageFix (with Final=false) drives the whole
// operation; this type tracks progress, step state, retry eligibility, and
// unplug safety for app/fix_task.go.

import (
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

const (
	FixStepBefore = iota
	FixStepGet
	FixStepFix
	FixStepReinstall
	FixStepCheck
	// FixStepCount is the number of steps, and also the step index once finished.
	FixStepCount
)

var fixSteps = [FixStepCount]string{
	"Before you start",
	"Get RØDE’s firmware",
	"Fix the storage",
	"Reinstall RØDE’s firmware",
	"Check and finish",
}

type FixKind int

const (
	FixBefore FixKind = iota
	FixRunning
	FixEnded
)

// Fix holds the state of a standalone storage-fix run.
type Fix struct {
	Kind FixKind
	// Event is the latest progress event, kept after completion to show where it stopped.
	Event *installer.Progress
	// Touched is set once the mixer has been told to enter update mode.
	Touched bool
	// Reply is populated when the request completes without an engine-level error.
	Reply *installer.StorageFix
	// Error holds an engine/transport error when the request itself failed.
	Error string
	// Canceling is true after Cancel was requested before the mixer was touched.
	Canceling bool
}

func NewFix() *Fix { return &Fix{Kind: FixBefore} }

func (f *Fix) Busy() bool { return f.Kind == FixRunning }

func IsStorageFix(request installer.Request) bool {
	return request.Kind == installer.RequestStorageFix
}

// Start begins a fix run if idle or after a retryable ending.
func (f *Fix) Start(normal bool) (installer.Request, bool) {
	if f.Kind == FixRunning || (f.Kind == FixEnded && !f.CanRetry(normal)) {
		return installer.Request{}, false
	}
	*f = Fix{Kind: FixRunning}
	return installer.Request{Kind: installer.RequestStorageFix}, true
}

// NotSent handles the runner refusing to accept the request.
func (f *Fix) NotSent() {
	*f = Fix{Kind: FixEnded, Error: "The app's installer stopped unexpectedly. Close this page and open it again."}
}

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

// CanStop reports whether the run can still be canceled (only before touching the mixer).
func (f *Fix) CanStop() bool {
	return f.Kind == FixRunning && !f.Touched && !f.Canceling
}

func (f *Fix) Cancel() bool {
	if !f.CanStop() {
		return false
	}
	f.Canceling = true
	return true
}

// Outcome records the completed request. A user-requested cancel resets back
// to FixBefore.
func (f *Fix) Outcome(reply installer.Reply, err error) {
	if f.Kind != FixRunning {
		return
	}
	canceling := f.Canceling
	f.Kind, f.Canceling = FixEnded, false
	switch {
	case err != nil && err.Error() == installer.Canceled && canceling:
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

type FixEnding int

const (
	FixEndingNone FixEnding = iota
	FixEndingFixed
	FixEndingAlready
	// FixEndingUnrecognized means the partition layout did not match expectations,
	// so nothing was changed and retrying won't help.
	FixEndingUnrecognized
	// FixEndingNotApplied means the fix script failed or didn't report back, and
	// stock firmware was reinstalled.
	FixEndingNotApplied
	// FixEndingRefused means the fix refused to start (see Reply.Error).
	FixEndingRefused
	// FixEndingStopped means the run aborted partway through.
	FixEndingStopped
)

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

// CanRetry reports whether retrying makes sense. Refusals and unrecognized
// layouts are permanent; other failures can retry once a single mixer is back
// in normal mode.
func (f *Fix) CanRetry(normal bool) bool {
	switch f.Ending() {
	case FixEndingNotApplied:
		return normal
	case FixEndingStopped:
		return normal
	}
	return false
}

// SavedCopy reports whether the run saved a local backup of what it modified.
func (f *Fix) SavedCopy() bool {
	return f.Reply != nil && f.Reply.Backup != ""
}

type FixSeen int

const (
	FixSeenUnknown FixSeen = iota
	FixSeenNone
	FixSeenTwo
	FixSeenUpdateMode
	FixSeenNormal
)

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

// Step returns the current sidebar step index (FixStepCount once completed
// with RØDE's firmware reinstalled).
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
	// Move to Check and finish once the mixer is back from the second flash
	// and the settle countdown has started.
	case pass == 2 && stage == autofix.StageWait && e.Total > 0, stage == autofix.StageFinish:
		return FixStepCheck
	case pass == 2:
		return FixStepReinstall
	}
	return FixStepFix
}

// Steps builds the sidebar step list. When the fix itself failed or was
// unrecognized but stock firmware was still put back, FixStepFix is marked
// failed while later steps are marked done.
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

// Unplug reports whether it is safe to disconnect the mixer. It stays unsafe
// from the first update-mode command until the run finishes or the mixer is
// observed back in normal mode.
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
		// These endings only occur after the mixer has come back and settled.
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
