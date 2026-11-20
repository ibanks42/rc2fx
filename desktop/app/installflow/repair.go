package installflow

// State machine and copy for the "Repair mixer" page (rendered in
// app/repair.go). When a flash leaves the mixer unable to boot or keep its
// settings, holding power for 15s exposes its storage over USB so the
// privileged helper can inspect it and restore damaged files.

import (
	"fmt"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
)

const RepairLookEvery = time.Second

type RepairStageKind int

const (
	RepairWaiting RepairStageKind = iota
	RepairChecking
	RepairFindings
	RepairRepairing
	RepairDone
	RepairFailed
)

// RepairStage holds the active repair stage along with its payload
// (Findings for RepairFindings, Result for RepairDone, Error for RepairFailed).
type RepairStage struct {
	Kind     RepairStageKind
	Findings *installer.RepairFindings
	Result   *installer.RepairResult
	Error    string
}

type Repair struct {
	Stage    RepairStage
	LastLook time.Time
	// Disk is the detected mixer storage size in bytes while visible.
	Disk *uint64
	// Wrote is set once a repair write has been started.
	Wrote bool
}

func NewRepair() *Repair {
	return &Repair{Stage: RepairStage{Kind: RepairWaiting}}
}

func (r *Repair) Busy() bool {
	return r.Stage.Kind == RepairChecking || r.Stage.Kind == RepairRepairing
}

func IsRepair(request installer.Request) bool {
	return request.Kind == installer.RequestRepairInspect || request.Kind == installer.RequestRepair
}

func RepairInspectRequest() installer.Request {
	return installer.Request{Kind: installer.RequestRepairInspect}
}

const repairStopped = "The installer stopped unexpectedly. Close and reopen it."

// DueToLook throttles storage polling to RepairLookEvery while waiting.
func (r *Repair) DueToLook(now time.Time) bool {
	if r.Stage.Kind != RepairWaiting || (!r.LastLook.IsZero() && now.Sub(r.LastLook) < RepairLookEvery) {
		return false
	}
	r.LastLook = now
	return true
}

// Looked updates the detected disk size and starts inspection once the disk is
// present and privileged access is granted.
func (r *Repair) Looked(disk *uint64, allowed bool) (start, changed bool) {
	changed = !opt.Equal(disk, r.Disk)
	r.Disk = disk
	if disk != nil && allowed {
		r.Stage = RepairStage{Kind: RepairChecking}
		return true, true
	}
	return false, changed
}

func (r *Repair) NotSent() {
	r.Stage = RepairStage{Kind: RepairFailed, Error: repairStopped}
}

// StartRepair builds the repair request for the approved findings (the helper
// re-verifies each file before writing).
func (r *Repair) StartRepair() (installer.Request, bool) {
	if r.Stage.Kind != RepairFindings || r.Stage.Findings == nil {
		return installer.Request{}, false
	}
	targets := r.Stage.Findings.Targets()
	r.Stage = RepairStage{Kind: RepairRepairing}
	r.Wrote = true
	return installer.Request{Kind: installer.RequestRepair, Files: targets}, true
}

func (r *Repair) Outcome(request installer.Request, reply installer.Reply, err error) {
	switch {
	case err != nil && err.Error() == installer.Canceled:
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

// Steps returns the sidebar step list. Elevation is prompted by the check
// button itself, so it doesn't get its own step.
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

const RepairStepCount = len(repairTitles)

func (r *Repair) Step() int {
	switch {
	case r.Stage.Kind == RepairWaiting:
		return 0
	// Nothing to restore; jump straight to restarting out of storage mode.
	case r.Stage.Kind == RepairFindings && r.Stage.Findings != nil && r.Stage.Findings.Repairable() == 0:
		return 3
	case r.Stage.Kind == RepairChecking || r.Stage.Kind == RepairFindings || (r.Stage.Kind == RepairFailed && !r.Wrote):
		return 1
	case r.Stage.Kind == RepairRepairing || r.Stage.Kind == RepairFailed:
		return 2
	}
	return 3 // RepairDone
}

// Unplug reports whether the mixer can safely be disconnected (unsafe while
// reading, writing, or after an interrupted write).
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

func FindingsDamaged(f *installer.RepairFindings) int {
	n := 0
	for _, r := range f.Roots {
		n += len(r.Damaged)
	}
	return n
}

func files(n int, what string) string {
	if n == 1 {
		return "1 " + what + " file"
	}
	return fmt.Sprintf("%d %s files", n, what)
}

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

type FindingLine struct {
	Good   bool
	Strong bool
	Text   string
}

// FindingLines formats the inspection results in plain language without
// exposing raw file paths or partition numbers.
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

func DoneTitle(result *installer.RepairResult) string {
	if len(result.Repaired) == 0 {
		return "Nothing was restored"
	}
	if len(result.Repaired) == 1 {
		return "1 file restored"
	}
	return fmt.Sprintf("%d files restored", len(result.Repaired))
}
