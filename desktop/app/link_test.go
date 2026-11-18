package app

import (
	"testing"
	"time"
)

func TestTheLinkStartsOff(t *testing.T) {
	var l linkIntent
	if l.auto() || l.connecting() || l.pausedForFlash() || l.due(false, time.Now()) {
		t.Fatalf("zero link %+v is not off", l)
	}
}

func TestAConnectAttemptIsConnectingUntilAnswered(t *testing.T) {
	var l linkIntent
	l.begin(true)
	if !l.auto() || !l.connecting() {
		t.Fatalf("begin: %+v", l)
	}
	l.connected()
	if !l.auto() || l.connecting() || l.due(false, time.Now().Add(time.Hour)) {
		t.Fatalf("connected: %+v", l)
	}
}

func TestAnUnsentConnectKeepsTryingWithoutARetry(t *testing.T) {
	var l linkIntent
	l.begin(false)
	if !l.auto() || l.connecting() || l.due(false, time.Now().Add(time.Hour)) {
		t.Fatalf("unsent: %+v", l)
	}
}

func TestAFailureRetriesAfterThreeSeconds(t *testing.T) {
	now := time.Unix(1000, 0)
	var l linkIntent
	l.begin(true)
	l.failed(false, now)
	if l.connecting() || !l.auto() {
		t.Fatalf("failed: %+v", l)
	}
	if l.due(false, now.Add(retryWait-time.Millisecond)) {
		t.Fatal("retry before the wait")
	}
	if !l.due(false, now.Add(retryWait)) {
		t.Fatal("no retry after the wait")
	}
	if l.due(true, now.Add(retryWait)) {
		t.Fatal("retry while connected")
	}
	// Another failure pushes the retry back.
	l.failed(false, now.Add(time.Second))
	if l.due(false, now.Add(retryWait)) {
		t.Fatal("a later failure did not move the retry")
	}
	// A new attempt cancels the scheduled one.
	l.begin(true)
	if l.due(false, now.Add(time.Hour)) {
		t.Fatal("retry kept across a new attempt")
	}
}

func TestAFailureWhileConnectedOnlyEndsTheAttempt(t *testing.T) {
	now := time.Unix(1000, 0)
	var l linkIntent
	l.begin(true)
	l.failed(true, now)
	if l.connecting() || !l.auto() || l.due(false, now.Add(time.Hour)) {
		t.Fatalf("failure while connected: %+v", l)
	}
	l.connected()
	l.failed(true, now)
	if !l.auto() || l.due(false, now.Add(time.Hour)) {
		t.Fatalf("save failure: %+v", l)
	}
}

func TestDisconnectStopsTrying(t *testing.T) {
	now := time.Unix(1000, 0)
	var l linkIntent
	l.begin(true)
	l.failed(false, now)
	l.stop()
	if l.auto() || l.due(false, now.Add(time.Hour)) {
		t.Fatalf("stopped: %+v", l)
	}
	// Failures after Disconnect schedule nothing.
	l.failed(false, now)
	if l.auto() || l.due(false, now.Add(time.Hour)) {
		t.Fatalf("failure after stop: %+v", l)
	}
	l.connected()
	if l.auto() {
		t.Fatal("a late answer turned the link on")
	}
}

func TestAFlashPausesAndThenRetriesAtOnce(t *testing.T) {
	now := time.Unix(1000, 0)
	var l linkIntent
	l.begin(true)
	l.connected()
	if l.flashDone(now) {
		t.Fatal("no pause to end")
	}
	l.stop()
	l.pauseForFlash()
	if l.auto() || !l.pausedForFlash() || l.due(false, now.Add(time.Hour)) {
		t.Fatalf("paused: %+v", l)
	}
	// Disconnect during the flash keeps the pause.
	l.stop()
	l.failed(false, now)
	if !l.pausedForFlash() {
		t.Fatalf("pause lost: %+v", l)
	}
	if !l.flashDone(now) {
		t.Fatal("pause not ended")
	}
	if l.pausedForFlash() || !l.auto() || !l.due(false, now) {
		t.Fatalf("after the flash: %+v", l)
	}
}
