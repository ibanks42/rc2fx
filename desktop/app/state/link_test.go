package state

import (
	"testing"
	"time"
)

func TestTheLinkStartsOff(t *testing.T) {
	var l LinkIntent
	if l.Auto() || l.Connecting() || l.phase == linkPausedForFlash || l.Due(false, time.Now()) {
		t.Fatalf("zero link %+v is not off", l)
	}
}

func TestAConnectAttemptIsConnectingUntilAnswered(t *testing.T) {
	var l LinkIntent
	l.Begin(true)
	if !l.Auto() || !l.Connecting() {
		t.Fatalf("begin: %+v", l)
	}
	l.Connected()
	if !l.Auto() || l.Connecting() || l.Due(false, time.Now().Add(time.Hour)) {
		t.Fatalf("connected: %+v", l)
	}
}

func TestAnUnsentConnectKeepsTryingWithoutARetry(t *testing.T) {
	var l LinkIntent
	l.Begin(false)
	if !l.Auto() || l.Connecting() || l.Due(false, time.Now().Add(time.Hour)) {
		t.Fatalf("unsent: %+v", l)
	}
}

func TestAFailureRetriesAfterThreeSeconds(t *testing.T) {
	now := time.Unix(1000, 0)
	var l LinkIntent
	l.Begin(true)
	l.Failed(false, now)
	if l.Connecting() || !l.Auto() {
		t.Fatalf("failed: %+v", l)
	}
	if l.Due(false, now.Add(retryWait-time.Millisecond)) {
		t.Fatal("retry before the wait")
	}
	if !l.Due(false, now.Add(retryWait)) {
		t.Fatal("no retry after the wait")
	}
	if l.Due(true, now.Add(retryWait)) {
		t.Fatal("retry while connected")
	}
	// Another failure pushes the retry back.
	l.Failed(false, now.Add(time.Second))
	if l.Due(false, now.Add(retryWait)) {
		t.Fatal("a later failure did not move the retry")
	}
	// A new attempt cancels the scheduled one.
	l.Begin(true)
	if l.Due(false, now.Add(time.Hour)) {
		t.Fatal("retry kept across a new attempt")
	}
}

func TestAFailureWhileConnectedOnlyEndsTheAttempt(t *testing.T) {
	now := time.Unix(1000, 0)
	var l LinkIntent
	l.Begin(true)
	l.Failed(true, now)
	if l.Connecting() || !l.Auto() || l.Due(false, now.Add(time.Hour)) {
		t.Fatalf("failure while connected: %+v", l)
	}
	l.Connected()
	l.Failed(true, now)
	if !l.Auto() || l.Due(false, now.Add(time.Hour)) {
		t.Fatalf("save failure: %+v", l)
	}
}

func TestDisconnectStopsTrying(t *testing.T) {
	now := time.Unix(1000, 0)
	var l LinkIntent
	l.Begin(true)
	l.Failed(false, now)
	l.Stop()
	if l.Auto() || l.Due(false, now.Add(time.Hour)) {
		t.Fatalf("stopped: %+v", l)
	}
	// Failures after Disconnect schedule nothing.
	l.Failed(false, now)
	if l.Auto() || l.Due(false, now.Add(time.Hour)) {
		t.Fatalf("failure after stop: %+v", l)
	}
	l.Connected()
	if l.Auto() {
		t.Fatal("a late answer turned the link on")
	}
}

func TestAFlashPausesAndThenRetriesAtOnce(t *testing.T) {
	now := time.Unix(1000, 0)
	var l LinkIntent
	l.Begin(true)
	l.Connected()
	if l.FlashDone(now) {
		t.Fatal("no pause to end")
	}
	l.Stop()
	l.PauseForFlash()
	if l.Auto() || l.phase != linkPausedForFlash || l.Due(false, now.Add(time.Hour)) {
		t.Fatalf("paused: %+v", l)
	}
	// Disconnect during the flash keeps the pause.
	l.Stop()
	l.Failed(false, now)
	if l.phase != linkPausedForFlash {
		t.Fatalf("pause lost: %+v", l)
	}
	if !l.FlashDone(now) {
		t.Fatal("pause not ended")
	}
	if l.phase == linkPausedForFlash || !l.Auto() || !l.Due(false, now) {
		t.Fatalf("after the flash: %+v", l)
	}
}
