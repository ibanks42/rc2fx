package installflow

// mod.rs selection_tests and access_step_tests, case for case.

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

func wantPending(t *testing.T, s *Selection, want *installer.Request, why string) {
	t.Helper()
	if got := s.Pending(); !sameRequest(got, want) {
		t.Fatalf("%s: pending %#v, want %#v", why, got, want)
	}
}

// The helper refuses RC2FX images it did not record, and this process
// cannot write that record.
func TestTheHelperPreparesWhatItFlashes(t *testing.T) {
	for _, stock := range []bool{false, true} {
		if !Privileged(installer.Request{
			Kind:    installer.RequestPrepare,
			Channel: "stable",
			Version: "1.7.7",
			Serial:  "GV1",
			Stock:   stock,
		}) {
			t.Fatalf("Prepare (stock %v) must run in the helper", stock)
		}
	}
	if Privileged(installer.Request{Kind: installer.RequestProbe}) {
		t.Fatal("Probe is not privileged")
	}
}

func TestReadinessIsJudgedAgainAfterAnythingThatCanChangeTheCatalog(t *testing.T) {
	for _, request := range []installer.Request{
		{Kind: installer.RequestReleases},
		{Kind: installer.RequestMixerReleases},
		{Kind: installer.RequestSelectBundledMixer},
	} {
		if !ChangesCatalog(request) {
			t.Fatalf("%#v changes the catalog", request)
		}
	}
	if ChangesCatalog(installer.Request{Kind: installer.RequestProbe}) {
		t.Fatal("Probe does not change the catalog")
	}
}

func TestTheHelperGetsExactlyWhatTheEngineSelectedAndAgainAfterARestart(t *testing.T) {
	var s Selection
	wantPending(t, &s, nil, "nothing selected: stock needs nothing")
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, true)
	wantPending(t, &s, &installer.Request{Kind: installer.RequestSelectBundledMixer}, "selected")
	s.HelperAnswered()
	wantPending(t, &s, nil, "sent once")
	s.HelperCleared()
	wantPending(t, &s, &installer.Request{Kind: installer.RequestSelectBundledMixer}, "a restarted helper holds nothing")
}

func TestASelectionTheHelperRefusedIsSentAgainEvenIfItHeldItBefore(t *testing.T) {
	var s Selection
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, true)
	s.HelperAnswered()
	// Something else was sent and failed, which cleared the helper.
	s.HelperCleared()
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, true)
	wantPending(t, &s, &installer.Request{Kind: installer.RequestSelectBundledMixer}, "sent again")
}

func TestAFailedSelectionIsNeverSentAndStockNeedsNone(t *testing.T) {
	var s Selection
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, true)
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, false)
	wantPending(t, &s, nil, "failed selection")
	s.EngineAnswered(installer.Request{Kind: installer.RequestSelectBundledMixer}, true)
	s.Clear()
	wantPending(t, &s, nil, "cleared for stock")
}

func TestPermissionIsRequiredOnlyUntilGranted(t *testing.T) {
	model := NewModel()
	for _, access := range []Access{
		{Kind: AccessNeeded},
		{Kind: AccessAsking},
		{Kind: AccessRefused, Why: "Dismissed"},
	} {
		if !NeedsAccess(model, access) {
			t.Fatalf("%#v needs access", access)
		}
	}
	for _, access := range []Access{{Kind: AccessGranted}, {Kind: AccessNotNeeded}} {
		if NeedsAccess(model, access) {
			t.Fatalf("%#v needs no access", access)
		}
	}
}

func TestLostAccessNeverHidesRunningProgressOrACompletionReceipt(t *testing.T) {
	model := NewModel()
	model.Run = &Run{}
	if NeedsAccess(model, Access{Kind: AccessNeeded}) {
		t.Fatal("a running run is never hidden")
	}
	failed := "Access ended"
	model.Run.Failed = &failed
	if !NeedsAccess(model, Access{Kind: AccessNeeded}) {
		t.Fatal("a failed run asks for access again")
	}
	if NeedsAccess(model, Access{Kind: AccessGranted}) {
		t.Fatal("granted access is enough")
	}
	model.Run = nil
	model.Finished = true
	if NeedsAccess(model, Access{Kind: AccessNeeded}) {
		t.Fatal("a completion receipt is never hidden")
	}
}
