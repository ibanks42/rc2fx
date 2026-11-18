package installflow

import (
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Not in the Rust (installer/repair.rs has no tests): the repair page's
// logic, which the Go port keeps out of the view.

func marks(steps []TaskStep) []Mark {
	out := make([]Mark, len(steps))
	for i, s := range steps {
		out[i] = s.Mark
	}
	return out
}

func sameMarks(a, b []Mark) bool {
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
	if r.Stage.Kind != RepairFindings || FindingsTitle(findings) != "1 damaged file found" {
		t.Fatalf("%+v", r.Stage)
	}
	if !sameMarks(marks(r.Steps()), []Mark{MarkDone, MarkNow, MarkToCome, MarkToCome}) {
		t.Fatalf("%v", marks(r.Steps()))
	}
	if u := r.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("nothing written yet: %+v", u)
	}
	request, ok := r.StartRepair()
	if !ok || request.Kind != installer.RequestRepair || len(request.Files) != 1 || request.Files[0] != (installer.RepairTarget{Root: 1, Path: "a"}) {
		t.Fatalf("%+v", request)
	}
	if u := r.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("writing: %+v", u)
	}
	r.Outcome(request, installer.Reply{Kind: installer.ReplyDone}, nil)
	if r.Stage.Kind != RepairFailed || r.Stage.Error != "The installer gave an unexpected answer." {
		t.Fatalf("%+v", r.Stage)
	}
	// A repair that didn't finish marks the repair, and the mixer may not
	// be unplugged.
	if !sameMarks(marks(r.Steps()), []Mark{MarkDone, MarkDone, MarkFailed, MarkToCome}) {
		t.Fatalf("%v", marks(r.Steps()))
	}
	if u := r.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("after a failed repair: %+v", u)
	}
	// A failed check marks the check, and nothing was written.
	r = NewRepair()
	r.Stage = RepairStage{Kind: RepairFailed, Error: "x"}
	if !sameMarks(marks(r.Steps()), []Mark{MarkDone, MarkFailed, MarkToCome, MarkToCome}) {
		t.Fatalf("%v", marks(r.Steps()))
	}
	if u := r.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("after a failed check: %+v", u)
	}
	// Nothing to restore: only leaving storage mode is left.
	r.Stage = RepairStage{Kind: RepairFindings, Findings: &installer.RepairFindings{}}
	if !sameMarks(marks(r.Steps()), []Mark{MarkDone, MarkDone, MarkDone, MarkNow}) {
		t.Fatalf("%v", marks(r.Steps()))
	}
}

func TestFindingsAreInPlainWords(t *testing.T) {
	why := "no identical copy to restore it from"
	findings := &installer.RepairFindings{SettingsOK: true, Roots: []installer.RootFindings{
		{Index: 1, Reference: "the image's own list", Damaged: []installer.DamagedFile{
			{Path: "usr/bin/rode-init-update-env.sh"}, {Path: "etc/a"}, {Path: "etc/b", Why: &why}}},
		{Index: 2, Reference: "the image's own list", Unclean: true},
	}}
	lines := FindingLines(findings)
	var texts []string
	for _, l := range lines {
		texts = append(texts, l.Text)
	}
	all := strings.Join(texts, "\n")
	for _, leak := range []string{"rode-init", "etc/", "partition", "image's own list", why} {
		if strings.Contains(all, leak) {
			t.Errorf("the findings name %q:\n%s", leak, all)
		}
	}
	want := []string{
		"Mixer settings: readable. They are copied to this computer first.",
		"First system copy: 3 damaged files; 2 can be restored, 1 can’t.",
		"Second system copy: no damage.",
		"Second system copy wasn’t shut down cleanly, so the app writes nothing to it.",
	}
	if !slices.Equal(texts, want) {
		t.Fatalf("lines\n%s", all)
	}
	if got := FindingsTitle(findings); got != "2 damaged files found" {
		t.Errorf("title %q", got)
	}
	if got := DoneTitle(&installer.RepairResult{Repaired: []string{"a", "b"}}); got != "2 files restored" {
		t.Errorf("done %q", got)
	}
	if got := DoneTitle(&installer.RepairResult{}); got != "Nothing was restored" {
		t.Errorf("done %q", got)
	}
}
