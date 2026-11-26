package app

import (
	"bytes"
	"math"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/goui-kit/gk"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func TestWindowsMatchThePackagedDesktopIdentity(t *testing.T) {
	if appID != "rc2fx" {
		t.Fatalf("appID = %q", appID)
	}
	desktop, err := os.ReadFile("../packaging/rc2fx.desktop")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(desktop), "Icon=rc2fx\n") || !strings.Contains(string(desktop), "StartupWMClass=rc2fx\n") {
		t.Fatalf("the desktop entry does not name rc2fx:\n%s", desktop)
	}
}

func TestWindowsCarryThePackagedIcon(t *testing.T) {
	data, err := os.ReadFile("../packaging/rc2fx.png")
	if err != nil {
		t.Fatal(err)
	}
	SetWindowIcon(data)
	defer SetWindowIcon(nil)
	c := windowIdentity(gk.WindowConfig{Title: "RC2FX"})
	if c.AppID != "rc2fx" || !bytes.Equal(c.Icon, data) || c.Title != "RC2FX" {
		t.Fatalf("windowIdentity gives app id %q, %d icon bytes, title %q", c.AppID, len(c.Icon), c.Title)
	}
}

func TestAmbientAnimationsPaceEllipsisUnfocusedAndPauseUnderReducedMotion(t *testing.T) {
	cases := []struct {
		sig, spin, ellipsis, reduced, active bool
		wantPeriod                           time.Duration
		wantRun                              bool
	}{
		{true, true, true, false, true, ambientFrame, true},
		{true, false, false, false, true, ambientFrame, true},
		{false, true, false, false, true, ambientFrame, true},
		{false, false, false, false, true, 0, false},
		{true, true, true, false, false, unfocusedEllipsisFrame, true},
		{false, true, true, false, false, unfocusedEllipsisFrame, true},
		{false, true, false, false, false, 0, false},
		{true, false, false, false, false, 0, false},
		{true, true, true, true, true, 0, false},
		{true, true, true, true, false, 0, false},
	}
	for _, c := range cases {
		period, run := ambientTick(c.sig, c.spin, c.ellipsis, c.reduced, c.active)
		if period != c.wantPeriod || run != c.wantRun {
			t.Errorf("ambientTick(%v, %v, %v, %v, %v) = %v, %v; want %v, %v",
				c.sig, c.spin, c.ellipsis, c.reduced, c.active, period, run, c.wantPeriod, c.wantRun)
		}
	}
}

func TestSpinnerStepAvoidsEightSpokeAliasing(t *testing.T) {
	if got := spinnerAngle(0); got != 0 {
		t.Fatalf("spinnerAngle(0) = %v, want 0", got)
	}
	step := spinnerAngle(1)
	// Eight identical spokes sit 45° (pi/4) apart; stay below the pi/8 Nyquist limit.
	if !(step > 0 && step < math.Pi/8) {
		t.Fatalf("step %v rad per frame aliases an 8-spoke loader", step)
	}
	if full := spinnerAngle(spinnerFramesPerTurn); full != 0 {
		t.Fatalf("spinnerAngle(spinnerFramesPerTurn) = %v, want 0", full)
	}
}

func TestPrettyReportKeepsScalarArraysInline(t *testing.T) {
	raw := []byte(`{"a":{"histogram":[1,2,0,0,0,0,0,0],"items":[{"x":1}]}}`)
	pretty, err := prettyReport(raw)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(pretty, `"histogram": [1, 2, 0, 0, 0, 0, 0, 0]`) {
		t.Fatalf("expected inline histogram array:\n%s", pretty)
	}
	if !strings.Contains(pretty, "\"items\": [\n") {
		t.Fatalf("expected multi-line object array:\n%s", pretty)
	}
}

func TestUpdateToastPersistsUntilDismissedAndReappearsForNewerVersion(t *testing.T) {
	a := &App{}
	if _, ok := a.updateToastState(); ok {
		t.Fatal("toast shown with no update")
	}
	a.appUpdate.Finish(appupdate.UpdateSnapshot{
		App: &appupdate.UpdateCheck{
			Kind:    appupdate.UpdateAvailable,
			Version: "0.2.5",
			URL:     appupdate.AppReleasesURL + "/tag/v0.2.5",
		},
	}, false)
	if m, ok := a.updateToastState(); !ok || m.version != "0.2.5" {
		t.Fatalf("toast not shown when update is available: %+v %v", m, ok)
	}
	// Dismissing hides 0.2.5 even if another background check reports 0.2.5.
	a.appUpdate.dismissedVersion = "0.2.5"
	if _, ok := a.updateToastState(); ok {
		t.Fatal("toast still shown after dismiss")
	}
	// A newer version shows the toast again.
	a.appUpdate.Finish(appupdate.UpdateSnapshot{
		App: &appupdate.UpdateCheck{
			Kind:    appupdate.UpdateAvailable,
			Version: "0.2.6",
			URL:     appupdate.AppReleasesURL + "/tag/v0.2.6",
		},
	}, false)
	if m, ok := a.updateToastState(); !ok || m.version != "0.2.6" {
		t.Fatalf("toast did not reappear for newer version: %+v %v", m, ok)
	}
}

func TestMixerUpdateToastCoversInPlaceFlashAndDismiss(t *testing.T) {
	// Disconnected mixer shows no toast.
	a := connectedTo("0.2.4", 1, signedOffer("0.3.0", "0.0.1", testProfile))
	a.session.Connected = false
	if _, ok := a.mixerUpdateToastState(); ok {
		t.Fatal("toast shown while disconnected")
	}

	// Connected mixer with newer compatible signed offer shows in-place toast.
	a.session.Connected = true
	m, ok := a.mixerUpdateToastState()
	if !ok || !m.inPlace || m.version != "0.3.0" || m.title != "Mixer software 0.3.0 is available" {
		t.Fatalf("in-place toast = %+v, %v", m, ok)
	}

	// Dismissing hides 0.3.0 until a newer mixer version appears.
	a.appUpdate.dismissedMixerUpdate = m.dismissKey
	if _, ok := a.mixerUpdateToastState(); ok {
		t.Fatal("toast still shown after dismiss")
	}
	a.appUpdate.SetSigned([]installer.MixerOffer{
		signedOffer("0.3.0", "0.0.1", testProfile),
		signedOffer("0.3.1", "0.0.1", testProfile),
	})
	if m2, ok := a.mixerUpdateToastState(); !ok || m2.version != "0.3.1" || !m2.inPlace {
		t.Fatalf("toast did not reappear for 0.3.1: %+v, %v", m2, ok)
	}

	// Partial native mismatch already shows the inline notice, so toast stays hidden.
	nativeProto := uint32(1)
	a.deviceVersions.NativeProtocol = &nativeProto
	if _, ok := a.mixerUpdateToastState(); ok {
		t.Fatal("toast shown alongside mixerUpdateNotice")
	}
	a.deviceVersions.NativeProtocol = nil

	// Offers requiring a newer desktop app do not trigger a mixer update toast.
	needsNewerApp := connectedTo("0.2.4", 1, signedOffer("0.4.0", "99.0.0", testProfile))
	if _, ok := needsNewerApp.mixerUpdateToastState(); ok {
		t.Fatal("toast shown for offer requiring a newer app")
	}

	// Bundled mixer update without in-place support shows a flash toast.
	proto := uint32(core.MixerProtocolVersion)
	fw := "1.7.7"
	oldRuntime := "0.2.4"
	bundled := "0.3.0"
	flashApp := connectedTo(oldRuntime, 0)
	m, ok = flashApp.flashMixerUpdateToastState(&oldRuntime, &bundled, &fw, &fw, &proto)
	if !ok || m.inPlace || m.version != "0.3.0" {
		t.Fatalf("flash toast = %+v, %v", m, ok)
	}
	flashApp.appUpdate.dismissedMixerUpdate = m.dismissKey
	if _, ok := flashApp.flashMixerUpdateToastState(&oldRuntime, &bundled, &fw, &fw, &proto); ok {
		t.Fatal("flash toast still shown after dismiss")
	}

	// Mixer supporting in-place waits while initial signed check is still running.
	waiting := connectedTo(oldRuntime, 1)
	waiting.bundledMixerSoftware = &bundled
	waiting.deviceVersions.Firmware = &fw
	waiting.deviceVersions.Protocol = &proto
	waiting.appUpdate = AppUpdate{signedChecking: true}
	if _, ok := waiting.mixerUpdateToastState(); ok {
		t.Fatal("flash fallback toast shown while signed check is still in flight")
	}

	// Combined mixer + RØDE firmware flash update mentions both.
	oldFW := "1.6.0"
	bothApp := connectedTo(oldRuntime, 0)
	m, ok = bothApp.flashMixerUpdateToastState(&oldRuntime, &bundled, &oldFW, &fw, &proto)
	if !ok || m.inPlace || m.version != "0.3.0" || !strings.Contains(m.detail, "1.7.7") {
		t.Fatalf("combined flash toast = %+v, %v", m, ok)
	}
}

func TestChangedTargetsReturnsOnlyModifiedChannels(t *testing.T) {
	cfg := core.DefaultConfig()
	saved := core.CloneTargets(cfg.Targets)
	draft := core.CloneTargets(cfg.Targets)
	if got := changedTargets(draft, saved); got == nil || len(got) != 0 {
		t.Fatalf("unchanged: got %v (nil=%v), want empty non-nil slice", len(got), got == nil)
	}
	draft[0].Gain.DB = -5.0
	got := changedTargets(draft, saved)
	if len(got) != 1 || got[0].ID != draft[0].ID || got[0].Gain.DB != -5.0 {
		t.Fatalf("single change: got %+v", got)
	}
}
