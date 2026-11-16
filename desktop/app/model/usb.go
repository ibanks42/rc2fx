package model

import (
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// USBObservation is the mixer's last reported USB 1 link state. Hardware
// freshness is independent of audio callback and network freshness.
type USBObservation struct {
	status core.USBStatus
	at     time.Time
	ok     bool
}

// Receive keeps value, a USB status as the mixer reports it, observed at
// now. Anything that is not a valid status discards the previous one.
func (o *USBObservation) Receive(value []byte, now time.Time) {
	status, err := core.FromJSON[core.USBStatus](string(value))
	if err != nil {
		o.Clear()
		return
	}
	*o = USBObservation{status: status, at: now, ok: true}
}

// Clear forgets the observation.
func (o *USBObservation) Clear() {
	*o = USBObservation{}
}

// Expire forgets an observation 3 s old or older; true if it did.
func (o *USBObservation) Expire(now time.Time) bool {
	if o.ok && now.Sub(o.at) >= 3*time.Second {
		o.Clear()
		return true
	}
	return false
}

// Label says what the observation means.
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

// NeedsAttention reports a known link that is not ready.
func (o *USBObservation) NeedsAttention() bool {
	return o.ok &&
		o.status.USB1 != core.USBLinkStateConfigured &&
		o.status.USB1 != core.USBLinkStateUnavailable
}
