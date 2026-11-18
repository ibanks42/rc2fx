package installflow

import (
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func fixEvent(stage string, flash int, carrier bool) installer.Progress {
	return installer.Progress{Phase: installer.StorageFixPhase + stage, Flash: flash, Carrier: carrier}
}

// backEvent is a wait event once the mixer is back: the settle counts up.
func backEvent(p installer.Progress) installer.Progress {
	p.Completed, p.Total = 3, 150
	return p
}

// startedFix is an install on a Pro II without RC2FX that has prepared its
// firmware and sent the storage fix.
func startedFix(t *testing.T) (*Model, *installer.Request) {
	t.Helper()
	m := setUp(status(gv1(false), absent(), false, false))
	m.FixPolicy = FixAuto
	if !m.WantsStorageFix() {
		t.Fatal("a mixer without RC2FX gets the fix")
	}
	prepare := selected(t, m, m.Start())
	refresh := answer(t, m, prepare, installer.Reply{Kind: installer.ReplyPrepared})
	fix := report(t, m, refresh, status(gv1(false), absent(), true, false))
	wantRequest(t, fix, installer.Request{Kind: installer.RequestStorageFix, Final: true},
		"the fix, flashing the prepared firmware last, instead of update mode")
	return m, fix
}

func TestWhichInstallsFixTheStorage(t *testing.T) {
	for name, tc := range map[string]struct {
		seen   Seen
		policy FixPolicy
		setup  func(m *Model)
		want   bool
	}{
		"no RC2FX":                              {status(gv1(false), absent(), false, false), FixAuto, nil, true},
		"RC2FX answers":                         {status(gv1(false), ready(), false, false), FixAuto, nil, false},
		"RC2FX reported":                        {status(gv1(false), absent(), false, false), FixAuto, func(m *Model) { m.Installed = ptr("0.2.1") }, false},
		"update mode":                           {status(gv1(true), absent(), false, false), FixAuto, nil, false},
		"no mixer":                              {status(nil, absent(), false, false), FixAuto, nil, false},
		"removing RC2FX":                        {status(gv1(false), absent(), false, false), FixAuto, func(m *Model) { m.SetStock(true) }, false},
		"a Duo":                                 {status(gv1(false), absent(), false, false), FixAuto, func(m *Model) { m.Device = firmware.ModelDuo }, false},
		"the policy says never":                 {status(gv1(false), absent(), false, false), FixNever, nil, false},
		"the policy says always, RC2FX answers": {status(gv1(false), ready(), false, false), FixAlways, nil, true},
	} {
		t.Run(name, func(t *testing.T) {
			m := setUp(tc.seen)
			m.FixPolicy = tc.policy
			if tc.setup != nil {
				tc.setup(m)
			}
			if got := m.WantsStorageFix(); got != tc.want {
				t.Fatalf("wants the fix: %v", got)
			}
		})
	}
	// A new model runs no fix until the app says it can.
	if setUp(status(gv1(false), absent(), false, false)).WantsStorageFix() {
		t.Fatal("the zero policy runs the fix")
	}
}

func TestTheFixMovesTheRunBetweenUpdateAndWait(t *testing.T) {
	m, _ := startedFix(t)
	if !m.Run.Committed || m.CanCancel() {
		t.Fatal("the fix can't be stopped once sent")
	}
	if m.Stage() != StageUpdate {
		t.Fatalf("stage %v", m.Stage())
	}
	if u := m.Unplug(); u.Kind != UnplugNo || u.Reason != "the install has started" {
		t.Fatalf("unplug before the mixer is touched %+v", u)
	}
	copying := fixEvent("copy", 1, true)
	copying.Completed, copying.Total, copying.Detail = 41, 100, "Copying archive.tar.gz to the mixer"
	m.FixProgress(copying)
	if f := FixTransfer(m.Run.Fix.Event); f == nil || *f != 0.41 {
		t.Fatalf("transfer %v", f)
	}
	if pass, attempt := FixPass(m.Run.Fix.Event); pass != 1 || attempt != 1 {
		t.Fatalf("pass %d attempt %d", pass, attempt)
	}
	if u := m.Unplug(); u.Kind != UnplugNo || u.Reason != "the mixer is updating" {
		t.Fatalf("unplug while updating %+v", u)
	}
	readBack := copying
	readBack.Detail = "Reading the firmware back from the mixer"
	if FixTransfer(&readBack) != nil {
		t.Fatal("reading back has no bar")
	}
	m.FixProgress(fixEvent("wait", 1, true))
	if m.Stage() != StageWait {
		t.Fatalf("waiting for the mixer: stage %v", m.Stage())
	}
	m.FixProgress(fixEvent("update-mode", 2, false))
	if m.Stage() != StageUpdate {
		t.Fatalf("the second update: stage %v", m.Stage())
	}
	if pass, _ := FixPass(m.Run.Fix.Event); pass != 2 {
		t.Fatalf("pass %d", pass)
	}
	// Other progress does not count.
	m.FixProgress(installer.Progress{Phase: "stage"})
	if pass, _ := FixPass(m.Run.Fix.Event); pass != 2 {
		t.Fatal("non-fix progress was taken")
	}
}

func TestAFixedInstallWaitsForRC2FXAndSaysSo(t *testing.T) {
	m, fix := startedFix(t)
	reply := installer.StorageFix{Outcome: "fixed", Backup: "/backups/rcfix", Touched: true, FinalSent: true, Flashes: 2}
	refresh := answer(t, m, fix, installer.Reply{Kind: installer.ReplyStorageFix, StorageFix: &reply})
	if !m.Run.Flashed || m.Failed() {
		t.Fatalf("run %+v", m.Run)
	}
	if report(t, m, refresh, status(nil, absent(), true, false)) != nil {
		t.Fatal("nothing more is sent: the fix flashed the firmware")
	}
	wantState(t, m, StepStart, StepStateWaiting)
	m.finished = m.Run
	if got := m.FixNote(); got != FixNoteFixed {
		t.Fatalf("note %v", got)
	}
}

func TestAFixThatLeftTheMixerAloneInstallsWithANormalFlash(t *testing.T) {
	for name, reply := range map[string]installer.StorageFix{
		"refused": {Refused: true, Error: "firmware 1.7.6 is not supported by this fix yet. Nothing was changed"},
		"offline": {Error: "RØDE's checksum could not be fetched. Nothing was changed"},
	} {
		t.Run(name, func(t *testing.T) {
			m, fix := startedFix(t)
			refresh := answer(t, m, fix, installer.Reply{Kind: installer.ReplyStorageFix, StorageFix: &reply})
			if m.Failed() || m.Run.Committed {
				t.Fatalf("run %+v", m.Run)
			}
			usb := report(t, m, refresh, status(gv1(false), absent(), true, false))
			wantRequest(t, usb, installer.Request{Kind: installer.RequestEnterUSBMode, Serial: "GV1"}, "the normal flash")
			m.finished = m.Run
			if got := m.FixNote(); got != FixNoteNotApplied {
				t.Fatalf("note %v", got)
			}
		})
	}
}

func TestAFixStoppedPartWayFailsAndTryAgainFlashesNormally(t *testing.T) {
	m, fix := startedFix(t)
	reply := installer.StorageFix{Touched: true, Error: "the mixer's update drive did not appear within 2 minutes"}
	refresh := answer(t, m, fix, installer.Reply{Kind: installer.ReplyStorageFix, StorageFix: &reply})
	if why := failedRun(t, m); !strings.Contains(why, "storage fix stopped") || !strings.Contains(why, "did not appear") {
		t.Fatalf("failure %q", why)
	}
	report(t, m, refresh, status(gv1(true), absent(), true, true))
	if !m.FixStopped() {
		t.Fatal("the page says trying again flashes normally")
	}
	if from, ok := m.RetryStage(); !ok || from != StageUpdate {
		t.Fatalf("retry from %v", from)
	}
	poll := m.Retry()
	flash := report(t, m, poll, status(gv1(true), absent(), true, true))
	wantRequest(t, flash, installer.Request{Kind: installer.RequestFlash, Serial: "GV1"}, "a normal flash, never the fix again")
}

func TestAFixRequestThatFailedIsNotSentAgain(t *testing.T) {
	m, fix := startedFix(t)
	refresh := m.Finish(*fix, installer.Reply{}, errors.New("the installer helper stopped; choose Try again or start again"))
	if m.Run.Fix.Error == "" || !m.Failed() {
		t.Fatalf("run %+v", m.Run)
	}
	report(t, m, refresh, status(gv1(false), absent(), true, false))
	poll := m.Retry()
	usb := report(t, m, poll, status(gv1(false), absent(), true, false))
	wantRequest(t, usb, installer.Request{Kind: installer.RequestEnterUSBMode, Serial: "GV1"}, "a normal flash")
}

func TestTheFinishedInstallSaysWhatTheFixDid(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if m.FixNote() != FixNoteNone {
		t.Fatal("no run, no note")
	}
	m.finished = &Run{}
	if m.FixNote() != FixNoteNone {
		t.Fatal("a run without the fix says nothing about it")
	}
	for outcome, want := range map[string]FixNoteKind{
		"fixed":         FixNoteFixed,
		"already-fixed": FixNoteNone,
		"unrecognized":  FixNoteUnrecognized,
		"failed":        FixNoteNotApplied,
		"unconfirmed":   FixNoteNotApplied,
	} {
		m.finished = &Run{Fix: &RunFix{Reply: &installer.StorageFix{Outcome: outcome, FinalSent: true}}}
		if got := m.FixNote(); got != want {
			t.Errorf("%s: note %v", outcome, got)
		}
	}
	m.finished = &Run{Fix: &RunFix{}}
	if m.FixNote() != FixNoteNotApplied {
		t.Fatal("a fix that never ran was not applied")
	}
}

// ---- The fix on its own ------------------------------------------------

func fixMarks(f *Fix) []Mark {
	var out []Mark
	for _, step := range f.Steps() {
		out = append(out, step.Mark)
	}
	return out
}

func TestTheFixTaskFollowsItsRun(t *testing.T) {
	f := NewFix()
	if got := fixMarks(f); !slices.Equal(got, []Mark{MarkNow, MarkToCome, MarkToCome, MarkToCome, MarkToCome}) {
		t.Fatalf("marks %v", got)
	}
	if u := f.Unplug(FixSeenNormal); u.Kind != UnplugYes {
		t.Fatalf("unplug %+v", u)
	}
	request, ok := f.Start(true)
	if !ok || request.Kind != installer.RequestStorageFix || request.Final || !f.Busy() {
		t.Fatalf("request %+v", request)
	}
	if _, again := f.Start(true); again {
		t.Fatal("started twice")
	}
	f.Progress(fixEvent("download", 0, false))
	if f.Step() != FixStepGet || !f.CanStop() || f.Unplug(FixSeenNormal).Kind != UnplugYes {
		t.Fatalf("downloading: step %d, stop %v, unplug %+v", f.Step(), f.CanStop(), f.Unplug(FixSeenNormal))
	}
	for _, tc := range []struct {
		event installer.Progress
		step  int
	}{
		{fixEvent("update-mode", 1, true), FixStepFix},
		{fixEvent("wait", 1, true), FixStepFix},
		{fixEvent("read-result", 1, true), FixStepFix},
		{fixEvent("copy", 2, true), FixStepFix},
		{fixEvent("copy", 3, false), FixStepReinstall},
		// Restarting after the second update is still Reinstall; once the
		// mixer is back (the settle counts up) it is Check and finish.
		{fixEvent("wait", 3, false), FixStepReinstall},
		{backEvent(fixEvent("wait", 3, false)), FixStepCheck},
	} {
		f.Progress(tc.event)
		if f.Step() != tc.step {
			t.Fatalf("%s: step %d, want %d", tc.event.Phase, f.Step(), tc.step)
		}
		if f.CanStop() || f.Unplug(FixSeenNormal).Kind != UnplugNo {
			t.Fatalf("%s: the mixer is updating", tc.event.Phase)
		}
	}
	if f.Cancel() {
		t.Fatal("cancelled after touching the mixer")
	}
	f.Outcome(installer.Reply{Kind: installer.ReplyStorageFix, StorageFix: &installer.StorageFix{
		Outcome: "fixed", Backup: "/b", Touched: true, FinalSent: true}}, nil)
	if f.Ending() != FixEndingFixed || !f.SavedCopy() || f.CanRetry(true) {
		t.Fatalf("ending %v", f.Ending())
	}
	if got := fixMarks(f); !slices.Equal(got, []Mark{MarkDone, MarkDone, MarkDone, MarkDone, MarkDone}) {
		t.Fatalf("marks %v", got)
	}
	if u := f.Unplug(FixSeenUnknown); u.Kind != UnplugYes {
		t.Fatalf("unplug %+v", u)
	}
}

func TestFixEndings(t *testing.T) {
	ended := func(reply installer.StorageFix) *Fix {
		f := NewFix()
		f.Start(true)
		f.Outcome(installer.Reply{Kind: installer.ReplyStorageFix, StorageFix: &reply}, nil)
		return f
	}
	f := ended(installer.StorageFix{Outcome: "already-fixed", Touched: true, FinalSent: true})
	if f.Ending() != FixEndingAlready || f.SavedCopy() {
		t.Fatalf("already fixed: %v", f.Ending())
	}
	f = ended(installer.StorageFix{Outcome: "unrecognized", Reason: "partition 3 moved", Touched: true, FinalSent: true})
	if f.Ending() != FixEndingUnrecognized || f.CanRetry(true) {
		t.Fatal("an unrecognized layout has nothing to retry")
	}
	if got := fixMarks(f); !slices.Equal(got, []Mark{MarkDone, MarkDone, MarkFailed, MarkDone, MarkDone}) {
		t.Fatalf("marks %v", got)
	}
	if _, ok := f.Start(true); ok {
		t.Fatal("started again after an unrecognized layout")
	}
	for _, outcome := range []string{"failed", "unconfirmed"} {
		f = ended(installer.StorageFix{Outcome: outcome, Touched: true, FinalSent: true})
		if f.Ending() != FixEndingNotApplied || !f.CanRetry(true) || f.CanRetry(false) {
			t.Fatalf("%s: ending %v", outcome, f.Ending())
		}
	}
	f = ended(installer.StorageFix{Refused: true, Error: "this fix has only been tried on the RØDECaster Pro II"})
	if f.Ending() != FixEndingRefused || f.CanRetry(true) {
		t.Fatal("a refusal gives the same answer again")
	}
	if u := f.Unplug(FixSeenUnknown); u.Kind != UnplugYes {
		t.Fatalf("nothing was changed: %+v", u)
	}
	f = ended(installer.StorageFix{Touched: true, Error: "the mixer has not come back"})
	if f.Ending() != FixEndingStopped || !f.CanRetry(true) || f.CanRetry(false) {
		t.Fatalf("stopped: %v", f.Ending())
	}
	for seen, want := range map[FixSeen]UnplugKind{
		FixSeenNormal: UnplugYes, FixSeenUpdateMode: UnplugNo, FixSeenNone: UnplugNo, FixSeenUnknown: UnplugNo,
	} {
		if u := f.Unplug(seen); u.Kind != want {
			t.Errorf("stopped, seen %v: unplug %+v", seen, u)
		}
	}
	f = NewFix()
	f.Start(true)
	f.Progress(fixEvent("copy", 1, true))
	f.Outcome(installer.Reply{}, errors.New("the installer helper stopped"))
	if f.Ending() != FixEndingStopped || f.Step() != FixStepFix || f.Unplug(FixSeenNone).Kind != UnplugNo {
		t.Fatalf("an engine error: %v, step %d", f.Ending(), f.Step())
	}
	if got := fixMarks(f); !slices.Equal(got, []Mark{MarkDone, MarkDone, MarkFailed, MarkToCome, MarkToCome}) {
		t.Fatalf("marks %v", got)
	}
}

func TestStopBeforeTheMixerIsTouchedGoesBack(t *testing.T) {
	f := NewFix()
	f.Start(true)
	f.Progress(fixEvent("download", 0, false))
	if !f.Cancel() || f.CanStop() {
		t.Fatal("stop before the mixer is touched")
	}
	f.Outcome(installer.Reply{}, errors.New(installer.Cancelled))
	if f.Kind != FixBefore {
		t.Fatalf("kind %v", f.Kind)
	}
}

// Back after the update that carries the fix, the wait is for the mixer to
// report the fix's result, whether over its USB disk or by the settle wait;
// the restarts before it, and the waits of the second update, are not.
func TestTheWaitForTheFixsResult(t *testing.T) {
	m, _ := startedFix(t)
	restarting := fixEvent("wait", 1, true)
	back := restarting
	back.Completed, back.Total = 12, 150
	report := fixEvent("report", 1, true)
	for _, tc := range []struct {
		name       string
		p          installer.Progress
		confirming bool
	}{
		{"restarting", restarting, false},
		{"back", back, true},
		{"usb report", report, true},
		{"read result", fixEvent("read-result", 1, true), false},
		{"second update back", func() installer.Progress { p := back; p.Flash, p.Carrier = 2, false; return p }(), false},
	} {
		p := tc.p
		if got := FixConfirming(&p); got != tc.confirming {
			t.Errorf("%s: confirming %v", tc.name, got)
		}
		m.FixProgress(p)
		if m.Stage() != StageWait {
			t.Errorf("%s: stage %v", tc.name, m.Stage())
		}
		if !FixTouching(&p) {
			t.Errorf("%s: not counted as touching the mixer", tc.name)
		}
	}
	if FixConfirming(nil) {
		t.Error("no event is confirming")
	}
}
