package core

import (
	"unicode"
	"unicode/utf8"
)

// The mixer's diagnostics event list: a bounded, sequenced log of what went
// wrong and when, and the paged reply a client reads it with.
//
// Entries are made off the audio path by rc2fxd (see the device crate): it
// turns the engine's own event ring, each worker's miss and late histories,
// and the counter increases it can only see as totals, into these. Timestamps
// are the mixer's CLOCK_MONOTONIC nanoseconds; a client maps them to host time
// through `nowNs` at receipt (the mixer's wall clock is not reliable).

// EventsPage is the longest page a client can ask for. Chosen so a full reply
// stays well inside the USB control transport's frame limit.
const EventsPage = 256

// DiagnosticsEvent is one entry. `seq` increases by one per entry, never
// repeats, and survives a diagnostics reset (which adds a `reset` entry
// rather than restarting).
type DiagnosticsEvent struct {
	Seq  uint64          `json:"seq"`
	Kind DiagnosticsKind `json:"kind"`
	// The path it concerns; absent for engine-wide events.
	Path *string `json:"path,omitempty"`
	// The mixer's CLOCK_MONOTONIC nanoseconds.
	AtNs uint64 `json:"atNs"`
	// A short sentence for the window, already phrased for a person.
	Message string `json:"message"`
	// What the kind measured, when it measured something.
	Details EventDetails `json:"details,omitempty,default"`
}

// MaxEventMessage is the longest event sentence a client keeps.
const MaxEventMessage = 512

// ForDisplay is this event as it is safe to show: the sentence cut to
// MaxEventMessage bytes with control characters removed. False if `path` is
// not an id, which no mixer sends.
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

// DiagnosticsKind is what happened, in a closed set: the window can label
// each kind without guessing, and a newer mixer's addition arrives as
// `other`. Serde `rename_all = "camelCase"`.
type DiagnosticsKind string

const (
	// A JACK xrun notification (the engine was late somewhere).
	DiagnosticsKindXrun DiagnosticsKind = "xrun"
	// A callback found the helper still busy with the previous frame.
	DiagnosticsKindHelperBusy DiagnosticsKind = "helperBusy"
	// A callback gave up waiting for the helper and passed its frame through.
	DiagnosticsKindHelperStall DiagnosticsKind = "helperStall"
	// The helper finished after its frame's cycle had ended.
	DiagnosticsKindHelperLateFinish DiagnosticsKind = "helperLateFinish"
	// The callback ended this late into its cycle.
	DiagnosticsKindLateEnd DiagnosticsKind = "lateEnd"
	// The callback took longer than a period.
	DiagnosticsKindOverrun DiagnosticsKind = "overrun"
	// The callback started this late into its cycle.
	DiagnosticsKindLateStart DiagnosticsKind = "lateStart"
	// Consecutive callbacks were further apart than a period and a half.
	DiagnosticsKindGap DiagnosticsKind = "gap"
	// The estimated cycle start jumped by more than a period and a half.
	DiagnosticsKindCycleSkip DiagnosticsKind = "cycleSkip"
	// A vendor job finished after the next callback had started.
	DiagnosticsKindVendorJobLate DiagnosticsKind = "vendorJobLate"
	// The mixer rebuilt its USB gadget (a USB mode change).
	DiagnosticsKindUSBGadget DiagnosticsKind = "usbGadget"
	// The suppression worker missed a frame's deadline: the listener heard
	// the delayed original for that frame.
	DiagnosticsKindMissedDeadline DiagnosticsKind = "missedDeadline"
	// A worker result arrived after its deadline (still used, but late).
	DiagnosticsKindLateFrame DiagnosticsKind = "lateFrame"
	// Stale suppression results, counted between two readings.
	DiagnosticsKindStaleResults DiagnosticsKind = "staleResults"
	// Dropped input frames, counted between two readings.
	DiagnosticsKindInputDrops DiagnosticsKind = "inputDrops"
	// Dropped output frames, counted between two readings.
	DiagnosticsKindOutputDrops DiagnosticsKind = "outputDrops"
	// Clipped samples, counted between two readings.
	DiagnosticsKindClipped DiagnosticsKind = "clipped"
	// Events that fell out of the engine's ring before they could be read.
	DiagnosticsKindLost DiagnosticsKind = "lost"
	// The user reset diagnostics; the counters the window shows restart here.
	DiagnosticsKindReset DiagnosticsKind = "reset"
	// A kind this build does not know (a newer mixer).
	DiagnosticsKindOther DiagnosticsKind = "other"
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
	return enumUnmarshal(k, text, diagnosticsKinds)
}

// EventDetails is the numbers a kind carries. Every field is optional: an
// xrun has no delta, a counter increase has no duration.
type EventDetails struct {
	// What the kind measured: a cycle position, a gap, a turnaround.
	ValueNs *uint64 `json:"valueNs,omitempty"`
	// jackd's xrun delay in microseconds, when it reported one.
	DelayedUs *uint64 `json:"delayedUs,omitempty"`
	// How much a counted figure rose (drops, clipped samples, lost events).
	Delta *uint64 `json:"delta,omitempty"`
	// The interval the delta accumulated over, from the previous reading.
	IntervalNs *uint64 `json:"intervalNs,omitempty"`
}

func (d EventDetails) IsEmpty() bool {
	return d.ValueNs == nil && d.DelayedUs == nil && d.Delta == nil && d.IntervalNs == nil
}

// DiagnosticsEvents is one page of the log.
type DiagnosticsEvents struct {
	// The mixer's CLOCK_MONOTONIC at the moment of the reply: a client adds
	// (local receive time − this) to each `atNs`.
	NowNs uint64 `json:"nowNs"`
	// The sequence number the next entry will take, i.e. what to pass as
	// `after` to continue reading.
	NextSeq uint64 `json:"nextSeq"`
	// Entries that fell out of the bounded log after `after` and can no
	// longer be read. Zero when the client kept up.
	Lost uint64 `json:"lost"`
	// More entries are waiting: ask again with `after = nextSeq`.
	More   bool               `json:"more"`
	Events []DiagnosticsEvent `json:"events"`
}
