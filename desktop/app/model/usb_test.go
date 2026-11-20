package model

import (
	"testing"
	"time"
)

func TestLiveMetersDoNotHideUSBLossAndStaleStatusIsNotCurrent(t *testing.T) {
	now := time.Now()
	var value USBObservation
	value.Receive([]byte(`{"usb1":"configured"}`), now)
	if value.NeedsAttention() {
		t.Fatal("configured needs attention")
	}
	value.Receive([]byte(`{"usb1":"notAttached"}`), now)
	if !value.NeedsAttention() {
		t.Fatal("not attached needs no attention")
	}
	if got := value.Label(); got != "USB disconnected" {
		t.Fatalf("label %q", got)
	}
	if value.Expire(now.Add(2999 * time.Millisecond)) {
		t.Fatal("expired early")
	}
	if !value.Expire(now.Add(3 * time.Second)) {
		t.Fatal("not expired")
	}
	if value.NeedsAttention() {
		t.Fatal("expired needs attention")
	}
	if got := value.Label(); got != "USB status unavailable" {
		t.Fatalf("label %q", got)
	}
	value.Receive([]byte(`{"usb1":"configured"}`), now)
	value.Clear() // stream or device change must discard the previous observation
	if got := value.Label(); got != "USB status unavailable" {
		t.Fatalf("label %q", got)
	}
}

func TestUnknownOrInvalidObservationIsNotTreatedAsDisconnect(t *testing.T) {
	now := time.Now()
	var value USBObservation
	value.Receive([]byte(`{"usb1":"notAttached"}`), now)
	for _, raw := range []string{
		`{"usb1":"unavailable"}`,
		`{"usb1":"invented"}`,
		`{"usb1":"configured","extra":true}`,
	} {
		value.Receive([]byte(raw), now)
		if value.NeedsAttention() {
			t.Fatalf("%s needs attention", raw)
		}
		if got := value.Label(); got != "USB status unavailable" {
			t.Fatalf("%s: label %q", raw, got)
		}
	}
	value.Receive([]byte(`{"usb1":"suspended"}`), now)
	if !value.NeedsAttention() {
		t.Fatal("suspended needs no attention")
	}
	if got := value.Label(); got != "USB suspended" {
		t.Fatalf("label %q", got)
	}
}
