package installflow

// What the helper is sent and holds, and when the page asks for access.

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
