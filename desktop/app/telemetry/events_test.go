package telemetry

import (
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// The event log: counting, groups, paging, resets, wording and host times.

// page is a page as the mixer would send it, decoded the way the transport
// decodes one (strictly), with room for the hostile case's megabyte.
func page(t *testing.T, lost uint64, events any) core.DiagnosticsEvents {
	t.Helper()
	text, err := json.Marshal(map[string]any{
		"nowNs": uint64(10_000_000_000), "nextSeq": 99, "lost": lost, "more": false, "events": events,
	})
	check(t, err == nil, "%v", err)
	p, err := core.FromJSONLimited[core.DiagnosticsEvents](string(text), 64<<20, 1<<20)
	check(t, err == nil, "page: %v", err)
	return p
}

func event(seq uint64, kind string) map[string]any {
	return map[string]any{"seq": seq, "kind": kind, "atNs": seq, "message": ""}
}

func ownName(id string) string { return id }

func TestXrunsAreCountedOnlyInsideTheRange(t *testing.T) {
	start := time.Now()
	at := func(seconds uint64, kind core.DiagnosticsKind) Row {
		return Row{Seq: seconds, Kind: kind, At: start.Add(time.Duration(seconds) * time.Second)}
	}
	log := NewEventLog()
	log.Receive(0, []Row{
		at(1, core.DiagnosticsKindXrun),
		at(5, core.DiagnosticsKindXrun),
		at(6, core.DiagnosticsKindOverrun),
		at(10, core.DiagnosticsKindXrun),
	})
	from, until := start.Add(2*time.Second), start.Add(10*time.Second)
	check(t, log.XrunsBetween(from, until) == 1, "xruns %d", log.XrunsBetween(from, until))
}

func order(log *EventLog) []int64 {
	var out []int64
	for _, row := range log.Rows() {
		if row.Lost {
			out = append(out, -int64(row.Count))
		} else {
			out = append(out, int64(row.Seq))
		}
	}
	return out
}

func TestKindsFallIntoTheirGroups(t *testing.T) {
	for _, c := range []struct {
		kind  core.DiagnosticsKind
		group Group
	}{
		{core.DiagnosticsKindXrun, GroupXruns},
		{core.DiagnosticsKindHelperStall, GroupHelper},
		{core.DiagnosticsKindHelperLateFinish, GroupHelper},
		{core.DiagnosticsKindGap, GroupTiming},
		{core.DiagnosticsKindMissedDeadline, GroupFallbacks},
		{core.DiagnosticsKindLateFrame, GroupFallbacks},
		{core.DiagnosticsKindOutputDrops, GroupDrops},
		{core.DiagnosticsKindStaleResults, GroupStale},
		{core.DiagnosticsKindReset, GroupOther},
	} {
		check(t, GroupOf(c.kind) == c.group, "%s in %v", c.kind, GroupOf(c.kind))
	}
}

func TestAHostileEventCannotMultiplyItsMessageOrReachTheWindowRaw(t *testing.T) {
	log := NewEventLog()
	long := strings.Repeat("x", 1<<20)
	log.ReceivePage(page(t, 0, []any{
		map[string]any{"seq": 1, "kind": "gap", "path": "", "atNs": 1, "message": long},
		map[string]any{"seq": 2, "kind": "gap", "path": "Not An Id", "atNs": 1, "message": "x"},
		map[string]any{"seq": 3, "kind": "gap", "atNs": 1, "message": "a\nb‮c"},
		map[string]any{"seq": 4, "kind": "gap", "path": "mic-1", "atNs": 1, "message": long},
	}), time.Now(), func(string) string { return "Very long display name" })
	var rows []Row
	for _, row := range log.Rows() {
		if !row.Lost {
			rows = append(rows, row)
		}
	}
	check(t, len(rows) == 2, "events with invalid paths are dropped: %d rows", len(rows))
	for _, row := range rows {
		switch row.Seq {
		case 3:
			check(t, !strings.ContainsAny(row.Message, "\n‮"), "%q", row.Message)
		default:
			check(t, len(row.Message) <= 512+64, "%d", len(row.Message))
		}
	}
}

func TestPagesArriveNewestFirstCappedWithLostMarkers(t *testing.T) {
	log := NewEventLog()
	now := time.Now()
	name := func(id string) string { return "name of " + id }
	log.ReceivePage(page(t, 0, []any{event(1, "xrun"), event(2, "xrun"), event(3, "xrun")}), now, name)
	check(t, log.NextSeq != nil && *log.NextSeq == 99, "next seq %v", log.NextSeq)
	log.ReceivePage(page(t, 5, []any{map[string]any{"seq": 9, "kind": "staleResults", "path": "mic-1",
		"atNs": 1, "message": "3 drops on mic-1", "details": map[string]any{"delta": 3}}}), now, name)
	check(t, slices.Equal(order(&log), []int64{9, -5, 3, 2, 1}), "order %v", order(&log))
	check(t, log.Count(GroupXruns) == 3, "xruns %d", log.Count(GroupXruns))
	row := log.Rows()[0]
	check(t, !row.Lost && row.Path != nil && *row.Path == "name of mic-1", "path %v", row.Path)
	check(t, row.Message == "3 drops on name of mic-1", "message %q", row.Message)
	check(t, slices.Equal(row.Details, []string{"3 more"}), "details %q", row.Details)
	var many []any
	for seq := uint64(10); seq < 3000; seq++ {
		many = append(many, event(seq, "gap"))
	}
	log.ReceivePage(page(t, 0, many), now, name)
	check(t, len(log.Rows()) == Capacity, "rows %d", len(log.Rows()))
}

func TestAResetClearsTheWindowAndDatesItsCounters(t *testing.T) {
	log := NewEventLog()
	now := time.Now()
	log.ReceivePage(page(t, 0, []any{event(1, "xrun")}), now, ownName)
	log.ReceivePage(page(t, 0, []any{event(2, "reset"), event(3, "gap")}), now, ownName)
	check(t, slices.Equal(order(&log), []int64{3, 2}), "order %v", order(&log))
	want := HostTime(2, 10_000_000_000, now)
	check(t, log.Since != nil && log.Since.Equal(want), "the reset's own time: %v", log.Since)
}

func TestFiguresSayWhatTheyMeasure(t *testing.T) {
	period := u64p(2_333_333)
	value := func(ns uint64) *core.EventDetails { return &core.EventDetails{ValueNs: u64p(ns)} }
	xrun := func(delayed uint64) *core.EventDetails {
		return &core.EventDetails{ValueNs: u64p(688_000), DelayedUs: u64p(delayed)}
	}
	eq := func(got []string, want ...string) {
		t.Helper()
		if want == nil {
			want = []string{}
		}
		check(t, slices.Equal(got, want), "got %q, want %q", got, want)
	}
	eq(DetailsLines(core.DiagnosticsKindXrun, xrun(0), period),
		"notified 0.69 ms into the 2.33 ms cycle", "last backend delay reported by JACK: 0 µs")
	eq(DetailsLines(core.DiagnosticsKindXrun, xrun(1840), nil),
		"notified 0.69 ms into its cycle", "last backend delay reported by JACK: 1840 µs")
	eq(DetailsLines(core.DiagnosticsKindXrun, value(688_000), period), "notified 0.69 ms into the 2.33 ms cycle")
	eq(DetailsLines(core.DiagnosticsKindXrun, &core.EventDetails{}, nil))
	eq(DetailsLines(core.DiagnosticsKindLateEnd, value(2_100_000), period), "ended 2.10 ms into the 2.33 ms cycle")
	eq(DetailsLines(core.DiagnosticsKindHelperStall, value(200_000), period), "waited 0.20 ms, then gave up")
	eq(DetailsLines(core.DiagnosticsKindGap, value(230_000_000), period), "callbacks 230.00 ms apart")
	eq(DetailsLines(core.DiagnosticsKindUSBGadget, value(0), period))
	check(t, Message(core.DiagnosticsKindLateEnd, "the callback ended late") == "The callback ended late", "message")
}

func TestXrunImpactIsUnknownForZeroNonzeroAndMissingBackendDelay(t *testing.T) {
	for _, delayed := range []*uint64{u64p(0), u64p(1840), nil} {
		log := NewEventLog()
		var d any
		if delayed != nil {
			d = *delayed
		}
		log.ReceivePage(page(t, 0, []any{map[string]any{
			"seq": 1, "kind": "xrun", "atNs": 1,
			"message": "untrusted mixer interpretation",
			"details": map[string]any{"delayedUs": d},
		}}), time.Now(), ownName)
		row := log.Rows()[0]
		check(t, !row.Lost, "expected an xrun row")
		check(t, row.Message == "JACK xrun notification · impact unknown", "message %q", row.Message)
		want := []string{}
		if delayed != nil {
			want = append(want, fmt.Sprintf("last backend delay reported by JACK: %d µs", *delayed))
		}
		check(t, slices.Equal(row.Details, want), "details %q, want %q", row.Details, want)
		check(t, log.Count(GroupXruns) == 1, "xruns %d", log.Count(GroupXruns))
	}
}

func TestMixerTimesMapOntoTheHostClock(t *testing.T) {
	received := time.Unix(1000, 0)
	check(t, HostTime(7_000_000_000, 10_000_000_000, received).Equal(received.Add(-3*time.Second)), "host time")
	// A time after the reply's own clock is shown as the receipt.
	check(t, HostTime(11, 10, received).Equal(received), "future time")
	check(t, Ago(received.Add(-12*time.Second), received) == "12 s ago", "%q", Ago(received.Add(-12*time.Second), received))
	check(t, Ago(received, received) == "just now", "%q", Ago(received, received))
}
