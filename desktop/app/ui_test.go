package app

import (
	"bytes"
	"math"
	"os"
	"strings"
	"testing"
	"time"

	"gpuikit.dev/gk"
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
