package installflow

import (
	"errors"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Not in the Rust (installer/repair.rs has no tests): the repair page's
// logic, which the Go port keeps out of the view.

func marks(steps []RepairStep) []RepairMark {
	out := make([]RepairMark, len(steps))
	for i, s := range steps {
		out[i] = s.Mark
	}
	return out
}

func sameMarks(a, b []RepairMark) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func TestRepairRequestsNeedTheHelper(t *testing.T) {
	for _, r := range []installer.Request{RepairInspectRequest(), {Kind: installer.RequestRepair}} {
		if !Privileged(r) || !IsRepair(r) {
			t.Fatalf("%v", r.Kind)
		}
	}
	if IsRepair(installer.Request{Kind: installer.RequestFlash}) {
		t.Fatal("a flash is not the repair page's")
	}
}

func TestTheRepairPageLooksOnceASecondAndChecksOnceAllowed(t *testing.T) {
	r := NewRepair()
	now := time.Now()
	if !r.DueToLook(now) || r.DueToLook(now.Add(500*time.Millisecond)) {
		t.Fatal("one look a second")
	}
	size := uint64(7 << 30)
	start, changed := r.Looked(&size, false)
	if start || !changed || r.Stage.Kind != RepairWaiting {
		t.Fatal("no check without access")
	}
	if !r.DueToLook(now.Add(time.Second)) {
		t.Fatal("looks again")
	}
	if start, _ := r.Looked(&size, true); !start || r.Stage.Kind != RepairChecking || !r.Busy() {
		t.Fatal("the check starts")
	}
	if r.DueToLook(now.Add(time.Hour)) {
		t.Fatal("no looking while checking")
	}
}

func TestRepairOutcomesMoveThePage(t *testing.T) {
	r := NewRepair()
	r.Stage = RepairStage{Kind: RepairChecking}
	r.Outcome(RepairInspectRequest(), installer.Reply{}, errors.New(installer.Cancelled))
	if r.Stage.Kind != RepairWaiting {
		t.Fatal("a cancelled check waits again")
	}
	why := "no identical copy to restore it from"
	findings := &installer.RepairFindings{SettingsOK: true, Roots: []installer.RootFindings{
		{Index: 1, Damaged: []installer.DamagedFile{{Path: "a"}, {Path: "b", Why: &why}}},
		{Index: 2},
	}}
	r.Outcome(RepairInspectRequest(), installer.Reply{Kind: installer.ReplyRepairFindings, Findings: findings}, nil)
	if r.Stage.Kind != RepairFindings || FindingsTitle(findings) != "Found 1 damaged file" {
		t.Fatalf("%+v", r.Stage)
	}
	if !sameMarks(marks(r.Steps(true, false)), []RepairMark{RepairMarkDone, RepairMarkDone, RepairMarkDone, RepairMarkNow, RepairMarkPending}) {
		t.Fatalf("%v", marks(r.Steps(true, false)))
	}
	request, ok := r.StartRepair()
	if !ok || request.Kind != installer.RequestRepair || len(request.Files) != 1 || request.Files[0] != (installer.RepairTarget{Root: 1, Path: "a"}) {
		t.Fatalf("%+v", request)
	}
	r.Outcome(request, installer.Reply{Kind: installer.ReplyDone}, nil)
	if r.Stage.Kind != RepairFailed || r.Stage.Error != "The installer gave an unexpected answer." {
		t.Fatalf("%+v", r.Stage)
	}
	// Without the access step, a failed check marks the check.
	if !sameMarks(marks(r.Steps(false, false)), []RepairMark{RepairMarkDone, RepairMarkFailed, RepairMarkPending, RepairMarkPending}) {
		t.Fatalf("%v", marks(r.Steps(false, false)))
	}
	// Nothing to restore: only leaving storage mode is left.
	r.Stage = RepairStage{Kind: RepairFindings, Findings: &installer.RepairFindings{}}
	if !sameMarks(marks(r.Steps(false, false)), []RepairMark{RepairMarkDone, RepairMarkDone, RepairMarkDone, RepairMarkNow}) {
		t.Fatalf("%v", marks(r.Steps(false, false)))
	}
}
