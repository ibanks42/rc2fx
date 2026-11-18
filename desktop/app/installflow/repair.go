package installflow

// installer/repair.rs, the part without views: Repair mixer's stages, which
// step of the sidebar each one reached, and the words it shows. For a mixer
// a flash left unable to keep its setup, or to start: the user holds the
// power button for 15 seconds so the bootloader shows the mixer's storage
// over USB; the administrator helper then checks it and restores damaged
// files (the installer's repair). The page (app/repair.go) is reached from
// the first screen while the mixer is in storage mode, on systems that can
// repair it (installer.RepairSupported).

import (
	"fmt"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// RepairLookEvery is how often the page looks for the mixer's storage while
// waiting.
const RepairLookEvery = time.Second

// RepairStageKind is Stage's variant.
type RepairStageKind int

const (
	// RepairWaiting: waiting for the mixer's storage to appear.
	RepairWaiting RepairStageKind = iota
	RepairChecking
	RepairFindings
	RepairRepairing
	RepairDone
	RepairFailed
)

// RepairStage is repair.rs Stage. Findings is for RepairFindings, Result for
// RepairDone and Error for RepairFailed.
type RepairStage struct {
	Kind     RepairStageKind
	Findings *installer.RepairFindings
	Result   *installer.RepairResult
	Error    string
}

// Repair is the Repair mixer page's state.
type Repair struct {
	Stage RepairStage
	// LastLook is when the page last looked for the storage; zero for never.
	LastLook time.Time
	// Disk is the size of the mixer's storage while it is visible.
	Disk *uint64
	// Wrote: a repair was started, so the mixer may have been written to.
	Wrote bool
}

// NewRepair is a page waiting for the mixer's storage.
func NewRepair() *Repair {
	return &Repair{Stage: RepairStage{Kind: RepairWaiting}}
}

// Busy: a request of the page's is in flight.
func (r *Repair) Busy() bool {
	return r.Stage.Kind == RepairChecking || r.Stage.Kind == RepairRepairing
}

// IsRepair reports requests that belong to the repair page, not the install
// flow.
func IsRepair(request installer.Request) bool {
	return request.Kind == installer.RequestRepairInspect || request.Kind == installer.RequestRepair
}

// RepairInspectRequest is Request::RepairInspect.
func RepairInspectRequest() installer.Request {
	return installer.Request{Kind: installer.RequestRepairInspect}
}

// repairStopped is what a request that could not be sent leaves.
const repairStopped = "The installer stopped unexpectedly. Close and reopen it."

// DueToLook reports whether to look for the storage now: the page is
// waiting and has not looked in the last RepairLookEvery. It records the
// look.
func (r *Repair) DueToLook(now time.Time) bool {
	if r.Stage.Kind != RepairWaiting || (!r.LastLook.IsZero() && now.Sub(r.LastLook) < RepairLookEvery) {
		return false
	}
	r.LastLook = now
	return true
}

// Looked records what a look found (nil: no storage). It returns whether
// the check starts now, which it does once the storage is there and access
// is granted, and whether anything the page shows changed.
func (r *Repair) Looked(disk *uint64, allowed bool) (start, changed bool) {
	changed = !sameU64(disk, r.Disk)
	r.Disk = disk
	if disk != nil && allowed {
		r.Stage = RepairStage{Kind: RepairChecking}
		return true, true
	}
	return false, changed
}

func sameU64(a, b *uint64) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// NotSent: the runner did not take the page's request.
func (r *Repair) NotSent() {
	r.Stage = RepairStage{Kind: RepairFailed, Error: repairStopped}
}

// StartRepair is the request that restores what the user saw and approved
// (the helper checks each again), or false when there are no findings.
func (r *Repair) StartRepair() (installer.Request, bool) {
	if r.Stage.Kind != RepairFindings || r.Stage.Findings == nil {
		return installer.Request{}, false
	}
	targets := r.Stage.Findings.Targets()
	r.Stage = RepairStage{Kind: RepairRepairing}
	r.Wrote = true
	return installer.Request{Kind: installer.RequestRepair, Files: targets}, true
}

// Outcome is a finished repair request.
func (r *Repair) Outcome(request installer.Request, reply installer.Reply, err error) {
	switch {
	case err != nil && err.Error() == installer.Cancelled:
		r.Stage = RepairStage{Kind: RepairWaiting}
	case err != nil:
		r.Stage = RepairStage{Kind: RepairFailed, Error: err.Error()}
	case request.Kind == installer.RequestRepairInspect && reply.Kind == installer.ReplyRepairFindings && reply.Findings != nil:
		r.Stage = RepairStage{Kind: RepairFindings, Findings: reply.Findings}
	case request.Kind == installer.RequestRepair && reply.Kind == installer.ReplyRepaired && reply.Repaired != nil:
		r.Stage = RepairStage{Kind: RepairDone, Result: reply.Repaired}
	default:
		r.Stage = RepairStage{Kind: RepairFailed, Error: "The installer gave an unexpected answer."}
	}
}

var repairTitles = [4]string{
	"Put the mixer in storage mode",
	"Check",
	"Back up and repair",
	"Restart the mixer",
}

// Steps is the sidebar: how far the page got. Administrator access is
// asked for by the button that starts the check, so it has no step.
func (r *Repair) Steps() []TaskStep {
	reached := r.Step()
	failed := r.Stage.Kind == RepairFailed
	steps := make([]TaskStep, len(repairTitles))
	for i, title := range repairTitles {
		mark := MarkToCome
		switch {
		case i < reached:
			mark = MarkDone
		case i == reached && failed:
			mark = MarkFailed
		case i == reached:
			mark = MarkNow
		}
		steps[i] = TaskStep{Title: title, Mark: mark}
	}
	return steps
}

// RepairStepCount is the number of the repair's steps.
const RepairStepCount = len(repairTitles)

// Step is the step the page is at, from 0: the findings are the check's
// result, and a failed request is at the step that failed.
func (r *Repair) Step() int {
	switch {
	case r.Stage.Kind == RepairWaiting:
		return 0
	// Nothing to restore: what is left is leaving storage mode.
	case r.Stage.Kind == RepairFindings && r.Stage.Findings != nil && r.Stage.Findings.Repairable() == 0:
		return 3
	case r.Stage.Kind == RepairChecking || r.Stage.Kind == RepairFindings || (r.Stage.Kind == RepairFailed && !r.Wrote):
		return 1
	case r.Stage.Kind == RepairRepairing || r.Stage.Kind == RepairFailed:
		return 2
	}
	return 3 // RepairDone
}

// Unplug says whether the mixer may be unplugged: not while the app reads
// or writes it, nor after a repair that didn't finish.
func (r *Repair) Unplug() Unplug {
	switch r.Stage.Kind {
	case RepairWaiting:
		return Unplug{Kind: UnplugYes, Reason: "nothing has changed yet"}
	case RepairChecking:
		return Unplug{Reason: "the app is reading the mixer"}
	case RepairRepairing:
		return Unplug{Reason: "the app is writing to the mixer"}
	case RepairFindings:
		return Unplug{Kind: UnplugYes, Reason: "nothing has been written"}
	case RepairDone:
		return Unplug{Kind: UnplugYes, Reason: "that is the next step"}
	}
	if r.Wrote {
		return Unplug{Reason: "the repair didn't finish"}
	}
	return Unplug{Kind: UnplugYes, Reason: "nothing has been written"}
}

// FindingsDamaged is the number of damaged files on both roots.
func FindingsDamaged(f *installer.RepairFindings) int {
	n := 0
	for _, r := range f.Roots {
		n += len(r.Damaged)
	}
	return n
}

// files is "1 damaged file" or "n damaged files".
func files(n int, what string) string {
	if n == 1 {
		return "1 " + what + " file"
	}
	return fmt.Sprintf("%d %s files", n, what)
}

// FindingsTitle is the findings' heading.
func FindingsTitle(f *installer.RepairFindings) string {
	damaged, repairable := FindingsDamaged(f), f.Repairable()
	switch {
	case damaged == 0:
		return "No damaged files found"
	case repairable == 0:
		return "Damaged files the app can’t restore"
	}
	return files(repairable, "damaged") + " found"
}

// FindingsExplanation is the sentence under the findings' heading.
func FindingsExplanation(f *installer.RepairFindings) string {
	damaged, repairable := FindingsDamaged(f), f.Repairable()
	switch {
	case damaged == 0:
		return "Nothing on the mixer’s system needs restoring. If it still forgets its setup, its settings may be the problem."
	case repairable == 0:
		return "The app only restores a file from an identical copy, and has none for these, so it leaves them alone. Installing the firmware again replaces the whole system."
	case repairable == 1:
		return "It can be restored from an identical copy. Only that file is written, after your settings are copied to this computer."
	case repairable == 2:
		return "Both can be restored from identical copies. Only those files are written, after your settings are copied to this computer."
	}
	return "All of them can be restored from identical copies. Only those files are written, after your settings are copied to this computer."
}

// FindingLine is one line of the findings, in plain words; Good when it is
// as it should be.
type FindingLine struct {
	Good   bool
	Strong bool
	Text   string
}

// FindingLines is the findings in plain words: the settings, then each of
// the mixer's two copies of its system. No file names or partition
// numbers.
func FindingLines(f *installer.RepairFindings) []FindingLine {
	lines := []FindingLine{{Good: true, Text: "Mixer settings: readable. They are copied to this computer first."}}
	if !f.SettingsOK {
		lines[0] = FindingLine{Text: "Mixer settings: they look damaged. The app doesn’t repair them, but copies them to this computer first."}
	}
	for n, root := range f.Roots {
		name := "First system copy"
		if n > 0 {
			name = "Second system copy"
		}
		repairable := 0
		for _, d := range root.Damaged {
			if d.Why == nil {
				repairable++
			}
		}
		left := len(root.Damaged) - repairable
		switch {
		case len(root.Damaged) == 0:
			lines = append(lines, FindingLine{Good: true, Text: name + ": no damage."})
		case left == 0 && repairable == 1:
			lines = append(lines, FindingLine{Strong: true, Text: name + ": 1 damaged file, which can be restored."})
		case left == 0:
			lines = append(lines, FindingLine{Strong: true, Text: fmt.Sprintf("%s: %s, all can be restored.", name, files(repairable, "damaged"))})
		case repairable == 0:
			lines = append(lines, FindingLine{Strong: true, Text: fmt.Sprintf("%s: %s the app can’t restore.", name, files(left, "damaged"))})
		default:
			lines = append(lines, FindingLine{Strong: true, Text: fmt.Sprintf("%s: %s; %d can be restored, %d can’t.", name, files(len(root.Damaged), "damaged"), repairable, left)})
		}
		if root.Unclean {
			lines = append(lines, FindingLine{Text: name + " wasn’t shut down cleanly, so the app writes nothing to it."})
		}
	}
	return lines
}

// DoneTitle is the heading after a repair.
func DoneTitle(result *installer.RepairResult) string {
	if len(result.Repaired) == 0 {
		return "Nothing was restored"
	}
	if len(result.Repaired) == 1 {
		return "1 file restored"
	}
	return fmt.Sprintf("%d files restored", len(result.Repaired))
}
