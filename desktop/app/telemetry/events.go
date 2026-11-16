package telemetry

// events.rs without drawing: the event log the main window fills and the
// events window shows, the filter groups, and the wording of each row.

import (
	"fmt"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Capacity is how many events the window keeps.
const Capacity = 2048

// Poll is how often an open window asks the mixer for new events.
const Poll = 500 * time.Millisecond

// Group is a filter group, in chip order.
type Group int

const (
	GroupXruns Group = iota
	GroupHelper
	GroupTiming
	GroupFallbacks
	GroupDrops
	GroupStale
	GroupOther
)

// GroupAll is every group, in chip order.
var GroupAll = [7]Group{GroupXruns, GroupHelper, GroupTiming, GroupFallbacks, GroupDrops, GroupStale, GroupOther}

// Label is the group's chip text.
func (g Group) Label() string {
	switch g {
	case GroupXruns:
		return "Xruns"
	case GroupHelper:
		return "Helper stalls / late"
	case GroupTiming:
		return "Callback timing"
	case GroupFallbacks:
		return "Fallbacks / missed deadlines"
	case GroupDrops:
		return "Drops"
	case GroupStale:
		return "Stale results"
	}
	return "Other"
}

// GroupOf is the group a kind falls into.
func GroupOf(kind core.DiagnosticsKind) Group {
	switch kind {
	case core.DiagnosticsKindXrun:
		return GroupXruns
	case core.DiagnosticsKindHelperBusy, core.DiagnosticsKindHelperStall, core.DiagnosticsKindHelperLateFinish:
		return GroupHelper
	case core.DiagnosticsKindLateEnd, core.DiagnosticsKindOverrun, core.DiagnosticsKindLateStart,
		core.DiagnosticsKindGap, core.DiagnosticsKindCycleSkip, core.DiagnosticsKindVendorJobLate:
		return GroupTiming
	case core.DiagnosticsKindMissedDeadline, core.DiagnosticsKindLateFrame:
		return GroupFallbacks
	case core.DiagnosticsKindInputDrops, core.DiagnosticsKindOutputDrops:
		return GroupDrops
	case core.DiagnosticsKindStaleResults:
		return GroupStale
	}
	// UsbGadget, Clipped, Lost, Reset, Other.
	return GroupOther
}

// KindLabel is the text of a kind's chip.
func KindLabel(kind core.DiagnosticsKind) string {
	switch kind {
	case core.DiagnosticsKindXrun:
		return "xrun"
	case core.DiagnosticsKindHelperBusy:
		return "helper busy"
	case core.DiagnosticsKindHelperStall:
		return "helper stall"
	case core.DiagnosticsKindHelperLateFinish:
		return "helper late"
	case core.DiagnosticsKindLateEnd:
		return "late end"
	case core.DiagnosticsKindOverrun:
		return "overrun"
	case core.DiagnosticsKindLateStart:
		return "late start"
	case core.DiagnosticsKindGap:
		return "gap"
	case core.DiagnosticsKindCycleSkip:
		return "cycle skip"
	case core.DiagnosticsKindVendorJobLate:
		return "vendor job late"
	case core.DiagnosticsKindUSBGadget:
		return "USB gadget"
	case core.DiagnosticsKindMissedDeadline:
		return "missed deadline"
	case core.DiagnosticsKindLateFrame:
		return "late frame"
	case core.DiagnosticsKindStaleResults:
		return "stale results"
	case core.DiagnosticsKindInputDrops:
		return "input drops"
	case core.DiagnosticsKindOutputDrops:
		return "output drops"
	case core.DiagnosticsKindClipped:
		return "clipped"
	case core.DiagnosticsKindLost:
		return "lost"
	case core.DiagnosticsKindReset:
		return "reset"
	}
	return "other"
}

// KindTone is how a kind's chip is coloured: the app maps it to Danger,
// Muted or Warn (events.rs kind_color).
type KindTone int

const (
	KindToneWarn KindTone = iota
	KindToneDanger
	KindToneMuted
)

// KindToneOf is events.rs kind_color without the colours.
func KindToneOf(kind core.DiagnosticsKind) KindTone {
	switch kind {
	case core.DiagnosticsKindXrun, core.DiagnosticsKindLost, core.DiagnosticsKindClipped:
		return KindToneDanger
	case core.DiagnosticsKindUSBGadget, core.DiagnosticsKindReset, core.DiagnosticsKindOther:
		return KindToneMuted
	}
	return KindToneWarn
}

func msText(ns uint64) string { return fmt.Sprintf("%.2f ms", float64(ns)/1_000_000.) }

// XrunNote is what JACK's xrun delay does and does not mean.
const XrunNote = "This is JACK’s last backend delay, not a measurement tied to this notification. It does not establish audio loss or audibility."

// DetailsLines is what an event measured, one phrase per figure, for its
// expanded row. periodNs is the engine's block period, nil when unknown.
// Each kind's valueNs measures something different; none is a bare "value".
func DetailsLines(kind core.DiagnosticsKind, details *core.EventDetails, periodNs *uint64) []string {
	intoCycle := func(ns uint64) string {
		if periodNs != nil {
			return fmt.Sprintf("%s into the %s cycle", msText(ns), msText(*periodNs))
		}
		return fmt.Sprintf("%s into its cycle", msText(ns))
	}
	lines := []string{}
	if details.ValueNs != nil {
		ns := *details.ValueNs
		var line string
		switch kind {
		case core.DiagnosticsKindXrun:
			line = "notified " + intoCycle(ns)
		case core.DiagnosticsKindHelperBusy:
			line = "the helper had been running " + msText(ns)
		case core.DiagnosticsKindHelperStall:
			line = fmt.Sprintf("waited %s, then gave up", msText(ns))
		case core.DiagnosticsKindHelperLateFinish:
			line = "finished " + intoCycle(ns)
		case core.DiagnosticsKindLateEnd:
			line = "ended " + intoCycle(ns)
		case core.DiagnosticsKindLateStart:
			line = "started " + intoCycle(ns)
		case core.DiagnosticsKindOverrun:
			if periodNs != nil {
				line = fmt.Sprintf("ran %s against a %s period", msText(ns), msText(*periodNs))
			} else {
				line = "ran " + msText(ns)
			}
		case core.DiagnosticsKindGap:
			line = fmt.Sprintf("callbacks %s apart", msText(ns))
		case core.DiagnosticsKindCycleSkip:
			line = "cycle start moved " + msText(ns)
		case core.DiagnosticsKindVendorJobLate:
			line = fmt.Sprintf("finished %s after the next callback started", msText(ns))
		case core.DiagnosticsKindUSBGadget:
			if ns == 0 {
				return lines
			}
			line = "measured " + msText(ns)
		default:
			line = "measured " + msText(ns)
		}
		lines = append(lines, line)
	}
	if details.DelayedUs != nil {
		lines = append(lines, fmt.Sprintf("last backend delay reported by JACK: %d µs", *details.DelayedUs))
	}
	switch {
	case details.Delta != nil && details.IntervalNs != nil:
		lines = append(lines, fmt.Sprintf("%d more in the %s before", *details.Delta, msText(*details.IntervalNs)))
	case details.Delta != nil:
		lines = append(lines, fmt.Sprintf("%d more", *details.Delta))
	case details.IntervalNs != nil:
		lines = append(lines, "over "+msText(*details.IntervalNs))
	}
	return lines
}

// Message is the row's sentence. The mixer's own wording is kept except for
// xruns, whose figures it reads ambiguously.
func Message(kind core.DiagnosticsKind, mixer string) string {
	if kind == core.DiagnosticsKindXrun {
		return "JACK xrun notification · impact unknown"
	}
	// "the callback ended late" reads as a sentence once capitalised.
	first, size := utf8.DecodeRuneInString(mixer)
	if size == 0 {
		return mixer
	}
	// unicode.ToUpper is the simple mapping: unlike Rust's to_uppercase it
	// leaves a first 'ß' as it is. The mixer's sentences start in ASCII.
	return string(unicode.ToUpper(first)) + mixer[size:]
}

// Row is one row of the window: an event, or (Lost) events the mixer dropped,
// older than the ones after it, before they could be read.
type Row struct {
	// Lost marks a lost-events row; Count is how many.
	Lost  bool
	Count uint64

	Seq  uint64
	Kind core.DiagnosticsKind
	// Path is the path's display name, when the event belongs to one.
	Path *string
	// At is when it happened, on this computer's clock.
	At      time.Time
	Message string
	// Details is what it measured, one phrase per figure.
	Details []string
}

// EventLog is the events read so far, shared by the main window (which
// fills it) and the events window (which shows it).
type EventLog struct {
	// rows newest first.
	rows []Row
	// NextSeq is the sequence number to ask for next; nil until the first
	// read.
	NextSeq   *uint64
	Hidden    map[Group]bool
	Expanded  *uint64
	Connected bool
	// Since is when the counters and events were last reset, if this app saw
	// it.
	Since *time.Time
	// Epoch is the mixer's diagnostics epoch, from its meters: above 0 after
	// a reset.
	Epoch uint64
	// PeriodNs is the engine's block period, which cycle positions are
	// measured in.
	PeriodNs     *uint64
	ConfirmReset bool
	// Error is why the last read failed; "" for none.
	Error string
	// Startup is the engine's first seconds of audio, on this computer's
	// clock, when known. Bursts there are expected, so they are hidden unless
	// asked.
	Startup     *[2]time.Time
	ShowStartup bool
}

// NewEventLog is EventLog::new.
func NewEventLog() EventLog { return EventLog{Hidden: map[Group]bool{}} }

// Rows is the log, newest first. Read only.
func (l *EventLog) Rows() []Row { return l.rows }

// Clear forgets every event, as after a reset or a new connection.
func (l *EventLog) Clear() {
	l.rows = nil
	l.NextSeq = nil
	l.Expanded = nil
}

// Receive adds events read in one reply, oldest first, and any the mixer
// lost before them.
func (l *EventLog) Receive(lost uint64, events []Row) {
	// Pushed in order; the log is newest first, so they go in front reversed.
	var pushed []Row
	if lost > 0 {
		pushed = append(pushed, Row{Lost: true, Count: lost})
	}
	for _, row := range events {
		if !row.Lost && row.Kind == core.DiagnosticsKindReset {
			// The mixer clears its list at a reset; so does the window.
			l.rows = nil
			pushed = pushed[:0]
			at := row.At
			l.Since = &at
		}
		pushed = append(pushed, row)
	}
	rows := make([]Row, 0, min(len(pushed)+len(l.rows), Capacity))
	for i := len(pushed) - 1; i >= 0 && len(rows) < Capacity; i-- {
		rows = append(rows, pushed[i])
	}
	for i := 0; i < len(l.rows) && len(rows) < Capacity; i++ {
		rows = append(rows, l.rows[i])
	}
	l.rows = rows
}

// ReceivePage takes one page from the mixer, received at received; name
// gives a path's display name. The one place the wire format meets the
// window.
func (l *EventLog) ReceivePage(page core.DiagnosticsEvents, received time.Time, name func(id string) string) {
	rows := make([]Row, 0, len(page.Events))
	for _, raw := range page.Events {
		event, ok := raw.ForDisplay()
		if !ok {
			continue
		}
		mixer := event.Message
		var path *string
		if event.Path != nil {
			id := *event.Path
			display := name(id)
			// The mixer names paths by id ("3 output drops on mic-1").
			mixer = strings.ReplaceAll(event.Message, id, display)
			path = &display
		}
		rows = append(rows, Row{
			Seq:     event.Seq,
			Kind:    event.Kind,
			Message: Message(event.Kind, mixer),
			Details: DetailsLines(event.Kind, &event.Details, l.PeriodNs),
			Path:    path,
			At:      HostTime(event.AtNs, page.NowNs, received),
		})
	}
	l.Receive(page.Lost, rows)
	next := page.NextSeq
	l.NextSeq = &next
}

// XrunsBetween is the xruns read from the mixer that happened from from
// until until.
func (l *EventLog) XrunsBetween(from, until time.Time) uint64 {
	var n uint64
	for _, row := range l.rows {
		if !row.Lost && row.Kind == core.DiagnosticsKindXrun && !row.At.Before(from) && row.At.Before(until) {
			n++
		}
	}
	return n
}

// duringStartup is whether row happened while the engine was starting.
func (l *EventLog) duringStartup(row *Row) bool {
	return !row.Lost && l.Startup != nil && !row.At.Before(l.Startup[0]) && row.At.Before(l.Startup[1])
}

// counted: startup rows are left out of everything unless shown.
func (l *EventLog) counted(row *Row) bool { return l.ShowStartup || !l.duringStartup(row) }

// Count is how many counted events fall in group.
func (l *EventLog) Count(group Group) int {
	n := 0
	for i := range l.rows {
		row := &l.rows[i]
		if l.counted(row) && !row.Lost && GroupOf(row.Kind) == group {
			n++
		}
	}
	return n
}

// StartupCount is how many events fell in the startup window.
func (l *EventLog) StartupCount() int {
	n := 0
	for i := range l.rows {
		if l.duringStartup(&l.rows[i]) {
			n++
		}
	}
	return n
}

// Visible is whether row shows under the current filters.
func (l *EventLog) Visible(row *Row) bool {
	if !l.counted(row) {
		return false
	}
	return row.Lost || !l.Hidden[GroupOf(row.Kind)]
}

// ToggleGroup shows a hidden group or hides a shown one.
func (l *EventLog) ToggleGroup(group Group) {
	if l.Hidden == nil {
		l.Hidden = map[Group]bool{}
	}
	if l.Hidden[group] {
		delete(l.Hidden, group)
	} else {
		l.Hidden[group] = true
	}
}

// ToggleExpanded opens an event's details, or closes them when open.
func (l *EventLog) ToggleExpanded(seq uint64) {
	if l.Expanded != nil && *l.Expanded == seq {
		l.Expanded = nil
		return
	}
	l.Expanded = &seq
}

// HostTime is a mixer time (nanoseconds on its clock) on this computer's
// clock, given the mixer's clock when the reply was made and when it
// arrived.
func HostTime(atNs, nowNs uint64, received time.Time) time.Time {
	return received.Add(-nanos(satSub(nowNs, atNs)))
}

// Ago is "just now", "12 s ago", "4 min ago", "2 h ago".
func Ago(at, now time.Time) string {
	seconds := uint64(max(now.Sub(at), 0) / time.Second)
	switch {
	case seconds == 0:
		return "just now"
	case seconds < 60:
		return fmt.Sprintf("%d s ago", seconds)
	case seconds < 3600:
		return fmt.Sprintf("%d min ago", seconds/60)
	}
	return fmt.Sprintf("%d h ago", seconds/3600)
}

// Clock is a row's time of day: "20:41:07.123" (chrono's %H:%M:%S%.3f).
func Clock(at time.Time) string { return at.Local().Format("15:04:05.000") }

// SinceLabel is "since 20:41:07 (reset)", "since the last reset" (one this
// app did not see) or "since the engine started"; epoch is the meter's
// diagnostics epoch.
func SinceLabel(since *time.Time, epoch uint64) string {
	switch {
	case since != nil:
		return fmt.Sprintf("since %s (reset)", since.Local().Format("15:04:05"))
	case epoch > 0:
		return "since the last reset"
	}
	return "since the engine started"
}
