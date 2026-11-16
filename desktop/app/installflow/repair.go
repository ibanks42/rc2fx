package installflow

// installer/repair.rs, the part without views: Repair mixer's stages, which
// step of the sidebar each one reached, and the words it shows. For a mixer
// a flash left unable to keep its setup, or to start: the user holds the
// power button for 15 seconds so the bootloader shows the mixer's storage
// over USB; the administrator helper then checks it and restores damaged
// files (the installer's repair). The page (app/repair.go) is reached from
// the first screen whenever RC2FX isn't running.

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
	files := r.Stage.Findings.Targets()
	r.Stage = RepairStage{Kind: RepairRepairing}
	return installer.Request{Kind: installer.RequestRepair, Files: files}, true
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

// RepairMark is how far one step of the sidebar is.
type RepairMark int

const (
	RepairMarkDone RepairMark = iota
	RepairMarkNow
	RepairMarkFailed
	RepairMarkPending
)

// RepairStep is one step of the sidebar.
type RepairStep struct {
	Title string
	Mark  RepairMark
}

var repairTitles = [5]string{
	"Administrator access",
	"Storage mode",
	"Check",
	"Back up and repair",
	"Restart the mixer",
}

// Steps is the sidebar: the access step only where access applies
// (accessStep), and how far the page got.
func (r *Repair) Steps(accessStep, needAccess bool) []RepairStep {
	var reached int
	switch {
	case needAccess:
		reached = 0
	case r.Stage.Kind == RepairWaiting:
		reached = 1
	case r.Stage.Kind == RepairChecking || r.Stage.Kind == RepairFailed:
		reached = 2
	// Nothing to restore: what is left is leaving storage mode.
	case r.Stage.Kind == RepairFindings && r.Stage.Findings != nil && r.Stage.Findings.Repairable() == 0:
		reached = 4
	case r.Stage.Kind == RepairFindings || r.Stage.Kind == RepairRepairing:
		reached = 3
	default: // RepairDone
		reached = 4
	}
	failed := r.Stage.Kind == RepairFailed
	var steps []RepairStep
	for i, title := range repairTitles {
		if i == 0 && !accessStep {
			continue
		}
		mark := RepairMarkPending
		switch {
		case i < reached:
			mark = RepairMarkDone
		case i == reached && failed:
			mark = RepairMarkFailed
		case i == reached:
			mark = RepairMarkNow
		}
		steps = append(steps, RepairStep{Title: title, Mark: mark})
	}
	return steps
}

// RepairAccessPurpose is what the access card says administrator access is
// for on the repair page.
const RepairAccessPurpose = "Administrator access lets RC2FX read the mixer's storage while it is in storage mode, and restore damaged files."

// FindingsDamaged is the number of damaged files on both roots.
func FindingsDamaged(f *installer.RepairFindings) int {
	n := 0
	for _, r := range f.Roots {
		n += len(r.Damaged)
	}
	return n
}

// FindingsTitle is the findings' heading.
func FindingsTitle(f *installer.RepairFindings) string {
	damaged, repairable := FindingsDamaged(f), f.Repairable()
	switch {
	case damaged == 0:
		return "No damaged files found"
	case repairable == 0:
		return "Found damaged files RC2FX can't restore"
	case repairable == 1:
		return "Found 1 damaged file"
	}
	return fmt.Sprintf("Found %d damaged files", repairable)
}

// FindingsExplanation is the sentence under the findings' heading.
func FindingsExplanation(f *installer.RepairFindings) string {
	switch {
	case FindingsDamaged(f) == 0:
		return "Nothing on the mixer's system needs restoring. If it still forgets its setup, its settings partition may be the problem."
	case f.Repairable() == 0:
		return "RC2FX only restores a file from an identical copy, so it leaves these alone. Flashing the mixer again replaces the whole system."
	}
	return "Restoring writes only the damaged files, with byte-identical copies, after copying the settings partition to this computer."
}

// SettingsLine is the findings' line about the settings partition.
func SettingsLine(f *installer.RepairFindings) string {
	if f.SettingsOK {
		return "Settings partition looks healthy: your setup is still there."
	}
	return "Settings partition looks damaged. RC2FX doesn't repair it, but copies it to this computer first."
}

// RootLine is the findings' line for a root with no damaged files.
func RootLine(root installer.RootFindings) string {
	return fmt.Sprintf("System partition %d: no damaged files (compared with %s).", root.Index, root.Reference)
}

// DamagedLine is the findings' line for one damaged file.
func DamagedLine(root installer.RootFindings, file installer.DamagedFile) string {
	if file.Why == nil {
		return fmt.Sprintf("System partition %d: %s is damaged; an identical copy restores it.", root.Index, file.Path)
	}
	return fmt.Sprintf("System partition %d: %s is damaged, and %s.", root.Index, file.Path, *file.Why)
}

// DoneTitle is the heading after a repair.
func DoneTitle(result *installer.RepairResult) string {
	if len(result.Repaired) == 0 {
		return "Nothing was restored"
	}
	return "Repaired. One last step."
}
