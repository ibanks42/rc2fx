package telemetry

import (
	"fmt"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// Capacity is the maximum number of diagnostic event rows retained in memory.
const Capacity = 2048

// Poll is how often an open diagnostic events window polls the mixer.
const Poll = 500 * time.Millisecond

// Group categorizes diagnostic events for filter chips, in display order.
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

// GroupAll lists all filter groups in chip order.
var GroupAll = [7]Group{GroupXruns, GroupHelper, GroupTiming, GroupFallbacks, GroupDrops, GroupStale, GroupOther}

// Label returns the filter chip label for g.
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

// GroupOf maps a diagnostics event kind to its filter group.
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
	return GroupOther
}

// KindLabel returns the badge text for a diagnostics event kind.
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

// KindTone selects the semantic color for an event kind badge.
type KindTone int

const (
	KindToneWarn KindTone = iota
	KindToneDanger
	KindToneMuted
)

// KindToneOf returns the badge color tone for kind.
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

// XrunNote clarifies what JACK's backend delay figure does and does not mean.
const XrunNote = "This is JACK’s last backend delay, not a measurement tied to this notification. It does not establish audio loss or audibility."

// DetailsLines formats the measurement phrases shown when an event row is
// expanded. Each event kind interprets ValueNs differently; periodNs is the
// engine block period when known.
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

// Message formats the row's headline sentence, normalizing xruns to avoid
// misleading mixer copy and capitalizing the first character of other messages.
func Message(kind core.DiagnosticsKind, mixer string) string {
	if kind == core.DiagnosticsKindXrun {
		return "JACK xrun notification · impact unknown"
	}
	first, size := utf8.DecodeRuneInString(mixer)
	if size == 0 {
		return mixer
	}
	return string(unicode.ToUpper(first)) + mixer[size:]
}

// Row is one entry in the diagnostic events list, either an event or a marker
// for events dropped on the mixer before they could be polled.
type Row struct {
	Lost    bool
	Count   uint64
	Seq     uint64
	Kind    core.DiagnosticsKind
	Path    *string
	At      time.Time
	Message string
	Details []string
}

// EventLog holds polled diagnostic events and UI filter state shared between
// the main window and the events window.
type EventLog struct {
	rows         []Row // newest first
	NextSeq      *uint64
	Hidden       map[Group]bool
	Expanded     *uint64
	Connected    bool
	Since        *time.Time
	Epoch        uint64
	PeriodNs     *uint64
	ConfirmReset bool
	Error        string
	// Engine startup interval on the host clock; events inside it are hidden unless ShowStartup is set.
	Startup     *[2]time.Time
	ShowStartup bool
}

// NewEventLog returns an empty log with all groups visible.
func NewEventLog() EventLog { return EventLog{Hidden: map[Group]bool{}} }

// Rows returns the stored rows newest-first. Callers must not mutate the slice.
func (l *EventLog) Rows() []Row { return l.rows }

// Clear discards all stored events and resets pagination state.
func (l *EventLog) Clear() {
	l.rows = nil
	l.NextSeq = nil
	l.Expanded = nil
}

// Receive prepends an oldest-first batch of events (plus any lost-event marker)
// into the newest-first log, clearing prior rows on a reset event.
func (l *EventLog) Receive(lost uint64, events []Row) {
	var pushed []Row
	if lost > 0 {
		pushed = append(pushed, Row{Lost: true, Count: lost})
	}
	for _, row := range events {
		if !row.Lost && row.Kind == core.DiagnosticsKindReset {
			// Mirror the mixer clearing its ring buffer on reset.
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

// ReceivePage converts a wire DiagnosticsEvents page into display rows and
// appends them to the log, substituting path IDs with display names via name.
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
			// Mixer messages embed raw target IDs (e.g. "3 output drops on mic-1").
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

// XrunsBetween counts xrun events in [from, until).
func (l *EventLog) XrunsBetween(from, until time.Time) uint64 {
	var n uint64
	for _, row := range l.rows {
		if !row.Lost && row.Kind == core.DiagnosticsKindXrun && !row.At.Before(from) && row.At.Before(until) {
			n++
		}
	}
	return n
}

func (l *EventLog) duringStartup(row *Row) bool {
	return !row.Lost && l.Startup != nil && !row.At.Before(l.Startup[0]) && row.At.Before(l.Startup[1])
}

func (l *EventLog) counted(row *Row) bool { return l.ShowStartup || !l.duringStartup(row) }

// Count returns the number of counted events in group.
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

// StartupCount returns how many events fell inside the startup window.
func (l *EventLog) StartupCount() int {
	n := 0
	for i := range l.rows {
		if l.duringStartup(&l.rows[i]) {
			n++
		}
	}
	return n
}

// Visible reports whether row passes the current startup and group filters.
func (l *EventLog) Visible(row *Row) bool {
	if !l.counted(row) {
		return false
	}
	return row.Lost || !l.Hidden[GroupOf(row.Kind)]
}

// ToggleGroup flips the visibility of group.
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

// ToggleExpanded expands seq, or collapses it if already expanded.
func (l *EventLog) ToggleExpanded(seq uint64) {
	if l.Expanded != nil && *l.Expanded == seq {
		l.Expanded = nil
		return
	}
	l.Expanded = &seq
}

// HostTime maps a mixer monotonic timestamp onto the host clock using the
// reply's mixer timestamp (nowNs) and host arrival time (received).
func HostTime(atNs, nowNs uint64, received time.Time) time.Time {
	return received.Add(-nanos(num.SatSub(nowNs, atNs)))
}

// Ago formats a relative age such as "just now", "12 s ago", "4 min ago", or "2 h ago".
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

// Clock formats at as local wall-clock time with milliseconds ("15:04:05.000").
func Clock(at time.Time) string { return at.Local().Format("15:04:05.000") }

// SinceLabel describes when counters started based on a known reset time or
// the diagnostics epoch.
func SinceLabel(since *time.Time, epoch uint64) string {
	switch {
	case since != nil:
		return fmt.Sprintf("since %s (reset)", since.Local().Format("15:04:05"))
	case epoch > 0:
		return "since the last reset"
	}
	return "since the engine started"
}
