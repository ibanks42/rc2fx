package telemetry

import (
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// The tests of the Rust app's telemetry_view.rs, case for case.

func viewEngine() core.EngineStatus {
	return core.EngineStatus{
		Generation:      1,
		RestartAllowed:  false,
		Mode:            core.EngineModeNative,
		Status:          core.EnginePhaseRunning,
		AppliedRevision: 1,
		SampleRate:      48_000,
		BlockSize:       112,
		AudioAdvancing:  true,
	}
}

func viewRouting() core.RoutingStatus {
	return core.RoutingStatus{Status: core.RoutingPhaseActive, Channels: 4, Recovered: 0}
}

// path is a clean path; change makes it less so. Its lifetime counters are
// not what telemetry warns about: only Recent is.
func path(change func(m *core.TargetMeter)) []PathView {
	timing := func(calls, mean, peak uint64) core.Timing {
		return core.Timing{Calls: calls, TotalNs: calls * mean, MaxNs: peak}
	}
	m := core.TargetMeter{
		ID: "mic-1", InputPeak: 0.5, OutputPeak: 0.5, RMS: 0.1,
		GateOpen: 1.0, Spectrum: []float64{}, Callbacks: 15_292, ProcessedFrames: 1_680_000,
		Performance: &core.Performance{
			CallbackBudgetNs: 2_000_000, WorkerBudgetNs: 10_000_000,
			HelperLate: u64p(0), HelperStalls: u64p(0),
			CallbackRecentMaxNs: u64p(950_000), WorkerRecentMaxNs: u64p(6_260_000),
			Callback:   timing(100, 500_000, 192_000_000),
			WorkerWall: timing(10, 2_000_000, 4_000_000),
			WorkerCPU:  timing(10, 1_000_000, 2_000_000),
		},
	}
	change(&m)
	return []PathView{{Name: "Mic 4", Channels: 1, Meter: m}}
}

func viewNow() time.Time { return time.Unix(10_000, 0) }

func snapshot(paths []PathView, xruns Recent) Snapshot {
	return Snapshot{
		HasEngine: true, Engine: viewEngine(), Routing: viewRouting(),
		Paths: paths, Xruns: xruns, Now: viewNow(),
	}
}

func stateOf(s *Snapshot) Status {
	e, r := viewEngine(), viewRouting()
	return StatusOf(&e, &r, s)
}

func timeP(t time.Time) *time.Time { return &t }

func checkIssues(t *testing.T, path *PathView, now time.Time, want string, wantRecent, wantOK bool) {
	t.Helper()
	text, recent, ok := Issues(path, now)
	if ok != wantOK || text != want || recent != wantRecent {
		t.Fatalf("issues: got (%q, %v, %v), want (%q, %v, %v)", text, recent, ok, want, wantRecent, wantOK)
	}
}

func TestACleanMixerIsHealthyAndEveryCheckPasses(t *testing.T) {
	s := snapshot(path(func(*core.TargetMeter) {}), Recent{})
	check(t, stateOf(&s) == Status{Kind: StatusHealthy}, "status %v", stateOf(&s))
	checkIssues(t, &s.Paths[0], s.Now, "", false, false)
	all := Checks(&s)
	var answers []string
	for _, c := range all {
		check(t, c.State == CheckOK && c.History == "", "%+v", c)
		answers = append(answers, c.Answer)
	}
	check(t, slices.Equal(answers, []string{"25 % load", "0 dropouts", "20 % load", "0 late"}), "answers %q", answers)
}

func TestLifetimeCountersAloneNeverWarn(t *testing.T) {
	// What the mixer counted before this app watched, or inaudible timing:
	// an over-limit frame, overruns, late helper work.
	s := snapshot(path(func(m *core.TargetMeter) {
		m.MissedDeadlines = 12
		p := m.Performance
		p.Xruns = 3
		p.WorkerWall.Overruns = 1
		p.Callback.Overruns = 4
		p.HelperLate = u64p(5)
	}), Recent{})
	check(t, stateOf(&s) == Status{Kind: StatusHealthy}, "status %v", stateOf(&s))
	checkIssues(t, &s.Paths[0], s.Now, "", false, false)
	for _, c := range Checks(&s) {
		check(t, c.State == CheckOK, "%+v", c)
	}
}

func TestARecentProblemIsNamedInTheStatusAndItsCheck(t *testing.T) {
	paths := path(func(*core.TargetMeter) {})
	paths[0].Recent.Late = Recent{Count: 2, Last: timeP(viewNow())}
	s := snapshot(paths, Recent{})
	check(t, stateOf(&s) == Status{StatusRecent, "Late audio on Mic 4 just now"}, "status %v", stateOf(&s))
	suppression := Checks(&s)[2]
	check(t, suppression.State == CheckWarn && suppression.Problem == "2 late", "%+v", suppression)
	checkIssues(t, &s.Paths[0], s.Now, "2 late", true, true)
	s = snapshot(path(func(*core.TargetMeter) {}), Recent{Count: 3})
	check(t, stateOf(&s) == Status{StatusRecent, "3 dropouts just now"}, "status %v", stateOf(&s))
	check(t, Checks(&s)[1].Problem == "in the last minute", "%+v", Checks(&s)[1])
}

func TestAnOlderProblemIsQuietHistory(t *testing.T) {
	earlier := viewNow().Add(-20 * time.Minute)
	paths := path(func(*core.TargetMeter) {})
	paths[0].Recent.Dropped = Recent{Count: 0, Last: timeP(earlier)}
	s := snapshot(paths, Recent{Count: 0, Last: timeP(earlier)})
	check(t, stateOf(&s) == Status{Kind: StatusHealthy}, "status %v", stateOf(&s))
	all := Checks(&s)
	dropouts, suppression := all[1], all[2]
	check(t, dropouts.State == CheckOK, "%+v", dropouts)
	check(t, strings.HasSuffix(dropouts.History, "(20 min ago)"), "history %q", dropouts.History)
	check(t, suppression.History != "", "no suppression history")
	text, recent, ok := Issues(&s.Paths[0], s.Now)
	check(t, ok && strings.HasPrefix(text, "Last ") && !recent, "issues (%q, %v, %v)", text, recent, ok)
}

func TestStoppedProcessingAndAFailedWorkerAreRed(t *testing.T) {
	s := snapshot(path(func(*core.TargetMeter) {}), Recent{})
	stalled, routing := viewEngine(), viewRouting()
	stalled.AudioAdvancing = false
	check(t, StatusOf(&stalled, &routing, &s) == Status{Kind: StatusStopped}, "stalled not stopped")
	check(t, Status{Kind: StatusStopped}.Label() == "RC2FX stopped", "label")
	engine, broken := viewEngine(), viewRouting()
	broken.Status = core.RoutingPhaseError
	check(t, StatusOf(&engine, &broken, &s) == Status{Kind: StatusStopped}, "broken routing not stopped")
	s = snapshot(path(func(m *core.TargetMeter) { m.WorkerFailed = true }), Recent{})
	check(t, stateOf(&s) == Status{StatusFailed, "Noise suppression failed on Mic 4"}, "status %v", stateOf(&s))
	check(t, stateOf(&s).NeedsAction() && !(Status{Kind: StatusHealthy}).NeedsAction(), "needs action")
}

func TestChecksHoldTheirPlaceWhenNothingIsReported(t *testing.T) {
	silent := snapshot(path(func(m *core.TargetMeter) { m.Performance = nil }), Recent{})
	for _, c := range Checks(&silent) {
		check(t, c.State == CheckOff && c.Answer == "Not reported", "%+v", c)
	}
	idle := snapshot(path(func(m *core.TargetMeter) { m.Performance.WorkerWall.Calls = 0 }), Recent{})
	check(t, Checks(&idle)[2].Answer == "Not running", "%+v", Checks(&idle)[2])
	jack := snapshot(path(func(m *core.TargetMeter) {
		m.Performance.HelperLate = nil
		m.Performance.HelperStalls = nil
	}), Recent{})
	check(t, Checks(&jack)[3].State == CheckOff, "%+v", Checks(&jack)[3])
}

func TestTechnicalDetailsShowRecentWorstsNotTheStartupPeak(t *testing.T) {
	engine := viewEngine()
	all := Facts(&engine, path(func(*core.TargetMeter) {}))
	value := func(label string) string {
		t.Helper()
		for _, f := range all {
			if f.Label == label {
				return f.Value
			}
		}
		t.Fatalf("no fact %q", label)
		return ""
	}
	check(t, value("Sample rate · block") == "48 kHz · 112 (2.33 ms)", "%q", value("Sample rate · block"))
	check(t, value("Callback avg · worst") == "0.50 · 0.95 ms", "%q", value("Callback avg · worst"))
	check(t, value("Suppression avg · worst · limit") == "2.00 · 6.26 · 10.00 ms", "%q", value("Suppression avg · worst · limit"))
	check(t, value("Suppression CPU avg") == "1.00 ms", "%q", value("Suppression CPU avg"))
	check(t, value("Blocks · suppression frames") == "15,292 · 0", "%q", value("Blocks · suppression frames"))
	for _, f := range all {
		check(t, !strings.Contains(f.Value, "192"), "%q shows the startup peak", f.Label)
		check(t, f.Help != "", "%q has no help", f.Label)
	}
	// A mixer from before recent worsts were reported.
	old := path(func(m *core.TargetMeter) { m.Performance.CallbackRecentMaxNs = nil })
	all = Facts(&engine, old)
	check(t, all[1].Value == "0.50 · – ms", "%q", all[1].Value)
}

func TestNumbersReadPlainly(t *testing.T) {
	for _, c := range []struct{ got, want string }{
		{Grouped(0), "0"},
		{Grouped(999), "999"},
		{Grouped(1_234_567), "1,234,567"},
		{Duration(43*48_000, 48_000), "43 s"},
		{Duration((25*60+3)*48_000, 48_000), "25 min 03 s"},
		{Duration((2*3600+5*60)*48_000, 48_000), "2 h 05 min"},
		{DBValue(0.), "silent"},
		{DBValue(1.), "0.0"},
		{DBValue(0.1), "-20.0"},
	} {
		check(t, c.got == c.want, "got %q, want %q", c.got, c.want)
	}
}
