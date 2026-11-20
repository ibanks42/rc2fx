package core

import (
	"unicode"
	"unicode/utf8"
)

// Diagnostics event log types for the mixer's bounded, sequenced event ring.
//
// Events are synthesized off the audio path by rc2fxd. Timestamps are in the
// mixer's CLOCK_MONOTONIC nanoseconds; clients map them to host time using
// NowNs at receipt because the mixer's wall clock is unreliable.

// EventsPage is the max events per page, sized to fit inside the USB control
// transport frame limit.
const EventsPage = 256

// DiagnosticsEvent is one log entry. Seq increments monotonically and does not
// reset on diagnostics clear (which appends a reset event instead).
type DiagnosticsEvent struct {
	Seq     uint64          `json:"seq"`
	Kind    DiagnosticsKind `json:"kind"`
	Path    *string         `json:"path,omitempty"` // nil for engine-wide events
	AtNs    uint64          `json:"atNs"`           // mixer CLOCK_MONOTONIC ns
	Message string          `json:"message"`
	Details EventDetails    `json:"details,omitempty,default"`
}

// MaxEventMessage is the byte cap for sanitized event messages.
const MaxEventMessage = 512

// ForDisplay strips control and bidi characters, truncates Message to
// MaxEventMessage bytes, and returns false if Path is not a valid target ID.
func (e DiagnosticsEvent) ForDisplay() (DiagnosticsEvent, bool) {
	if e.Path != nil && !ValidID(*e.Path) {
		return DiagnosticsEvent{}, false
	}
	message := make([]byte, 0, min(len(e.Message), MaxEventMessage))
	for _, c := range e.Message {
		if unicode.IsControl(c) || IsBidiControl(c) {
			continue
		}
		if len(message)+utf8.RuneLen(c) > MaxEventMessage {
			break
		}
		message = utf8.AppendRune(message, c)
	}
	e.Message = string(message)
	return e, true
}

// DiagnosticsKind classifies a diagnostics event (camelCase on the wire).
// Unknown values from newer mixers decode as DiagnosticsKindOther.
type DiagnosticsKind string

const (
	DiagnosticsKindXrun             DiagnosticsKind = "xrun"
	DiagnosticsKindHelperBusy       DiagnosticsKind = "helperBusy"       // helper still busy with previous frame
	DiagnosticsKindHelperStall      DiagnosticsKind = "helperStall"      // callback bypassed helper after timeout
	DiagnosticsKindHelperLateFinish DiagnosticsKind = "helperLateFinish" // helper finished past its cycle
	DiagnosticsKindLateEnd          DiagnosticsKind = "lateEnd"
	DiagnosticsKindOverrun          DiagnosticsKind = "overrun" // callback exceeded one period
	DiagnosticsKindLateStart        DiagnosticsKind = "lateStart"
	DiagnosticsKindGap              DiagnosticsKind = "gap"           // > 1.5 periods between callbacks
	DiagnosticsKindCycleSkip        DiagnosticsKind = "cycleSkip"     // estimated cycle start jumped > 1.5 periods
	DiagnosticsKindVendorJobLate    DiagnosticsKind = "vendorJobLate" // vendor job finished after next callback started
	DiagnosticsKindUSBGadget        DiagnosticsKind = "usbGadget"     // USB mode change rebuilt gadget
	DiagnosticsKindMissedDeadline   DiagnosticsKind = "missedDeadline"
	DiagnosticsKindLateFrame        DiagnosticsKind = "lateFrame" // arrived late, still used
	DiagnosticsKindStaleResults     DiagnosticsKind = "staleResults"
	DiagnosticsKindInputDrops       DiagnosticsKind = "inputDrops"
	DiagnosticsKindOutputDrops      DiagnosticsKind = "outputDrops"
	DiagnosticsKindClipped          DiagnosticsKind = "clipped"
	DiagnosticsKindLost             DiagnosticsKind = "lost" // evicted from engine ring before read
	DiagnosticsKindReset            DiagnosticsKind = "reset"
	DiagnosticsKindOther            DiagnosticsKind = "other"
)

var diagnosticsKinds = []DiagnosticsKind{
	DiagnosticsKindXrun, DiagnosticsKindHelperBusy, DiagnosticsKindHelperStall,
	DiagnosticsKindHelperLateFinish, DiagnosticsKindLateEnd, DiagnosticsKindOverrun,
	DiagnosticsKindLateStart, DiagnosticsKindGap, DiagnosticsKindCycleSkip,
	DiagnosticsKindVendorJobLate, DiagnosticsKindUSBGadget, DiagnosticsKindMissedDeadline,
	DiagnosticsKindLateFrame, DiagnosticsKindStaleResults, DiagnosticsKindInputDrops,
	DiagnosticsKindOutputDrops, DiagnosticsKindClipped, DiagnosticsKindLost,
	DiagnosticsKindReset, DiagnosticsKindOther,
}

func (k *DiagnosticsKind) UnmarshalText(text []byte) error {
	if err := enumUnmarshal(k, text, diagnosticsKinds); err != nil {
		*k = DiagnosticsKindOther
	}
	return nil
}

// EventDetails holds optional per-event measurements.
type EventDetails struct {
	ValueNs    *uint64 `json:"valueNs,omitempty"`
	DelayedUs  *uint64 `json:"delayedUs,omitempty"` // jackd xrun delay
	Delta      *uint64 `json:"delta,omitempty"`     // counter increase since last reading
	IntervalNs *uint64 `json:"intervalNs,omitempty"`
}

func (d EventDetails) IsEmpty() bool {
	return d.ValueNs == nil && d.DelayedUs == nil && d.Delta == nil && d.IntervalNs == nil
}

// DiagnosticsEvents is one page of the diagnostics log.
type DiagnosticsEvents struct {
	NowNs   uint64             `json:"nowNs"`   // mixer CLOCK_MONOTONIC at reply time
	NextSeq uint64             `json:"nextSeq"` // pass as `after` for the next page
	Lost    uint64             `json:"lost"`    // entries evicted after `after`
	More    bool               `json:"more"`
	Events  []DiagnosticsEvent `json:"events"`
}
