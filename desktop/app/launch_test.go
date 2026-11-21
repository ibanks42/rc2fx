package app

import (
	"testing"
	"time"
)

func launchedApp(now time.Time) *App {
	a := &App{launched: now, quietUntil: now.Add(launchQuiet)}
	a.link.Begin(true)
	return a
}

func TestALaunchStaysQuietThroughTheMixersUSBLockoutAndAsksAgainSoon(t *testing.T) {
	now := time.Now()
	a := launchedApp(now)
	if !a.launchQuiet() {
		t.Fatal("not quiet at launch")
	}
	at := now.Add(100 * time.Millisecond)
	a.link.Failed(false, at)
	a.launchFailure("USB is in use by another RC2FX client; try again in a few seconds", at)
	if !a.quietUntil.Equal(now.Add(usbLockout)) {
		t.Fatalf("quiet until %v", a.quietUntil.Sub(now))
	}
	if a.link.Due(false, at.Add(lockoutRetry-time.Millisecond)) || !a.link.Due(false, at.Add(lockoutRetry)) {
		t.Fatal("retry is not soon")
	}
}

func TestAnyOtherLaunchFailureShowsTheConnectionScreenAtOnce(t *testing.T) {
	now := time.Now()
	a := launchedApp(now)
	a.link.Failed(false, now)
	a.launchFailure("No RØDECaster found", now)
	if a.launchQuiet() {
		t.Fatal("still quiet")
	}
	// A lockout long after launch is not the launch's.
	a = launchedApp(now)
	later := now.Add(usbLockout + time.Second)
	a.quietUntil = later.Add(time.Second)
	a.link.Failed(false, later)
	a.launchFailure("USB is in use by another RC2FX client", later)
	if a.launchQuiet() {
		t.Fatal("quiet after the lockout window")
	}
}
