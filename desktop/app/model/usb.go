package model

import (
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// USBObservation tracks the mixer's last reported USB 1 link state independently
// of audio and network freshness.
type USBObservation struct {
	status core.USBStatus
	at     time.Time
	ok     bool
}

// Receive parses a raw USB status payload; invalid payloads clear any prior reading.
func (o *USBObservation) Receive(value []byte, now time.Time) {
	status, err := core.FromJSON[core.USBStatus](string(value))
	if err != nil {
		o.Clear()
		return
	}
	*o = USBObservation{status: status, at: now, ok: true}
}

// Clear resets the stored observation.
func (o *USBObservation) Clear() {
	*o = USBObservation{}
}

// Expire clears the observation if it is at least 3 s old, returning true if cleared.
func (o *USBObservation) Expire(now time.Time) bool {
	if o.ok && now.Sub(o.at) >= 3*time.Second {
		o.Clear()
		return true
	}
	return false
}

// Label returns the user-facing status string for the current USB state.
func (o *USBObservation) Label() string {
	if !o.ok {
		return "USB status unavailable"
	}
	switch o.status.USB1 {
	case core.USBLinkStateUnavailable:
		return "USB status unavailable"
	case core.USBLinkStateConfigured:
		return "USB connected"
	case core.USBLinkStateNotAttached:
		return "USB disconnected"
	case core.USBLinkStateSuspended:
		return "USB suspended"
	default:
		return "USB connection not ready"
	}
}

// NeedsAttention reports whether the USB link is in a known non-ready state.
func (o *USBObservation) NeedsAttention() bool {
	return o.ok &&
		o.status.USB1 != core.USBLinkStateConfigured &&
		o.status.USB1 != core.USBLinkStateUnavailable
}
