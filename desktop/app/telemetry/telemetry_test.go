package telemetry

import (
	"encoding/json"
	"fmt"
	"os"
	"slices"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Meter freshness, audio progress, fault baselines and the startup window.

func failedState() core.DeviceState {
	return core.DeviceState{
		Selection: core.SelectionStatus{
			Status:             core.SelectionPhaseIdle,
			Allowed:            false,
			QualifiedTargetIDs: []string{"mic-1"},
			ActiveTargetIDs:    []string{"mic-1"},
			PendingTargetIDs:   []string{},
		},
		Engine: core.EngineStatus{
			Generation:      1,
			RestartAllowed:  false,
			Mode:            core.EngineModeJack,
			Status:          core.EnginePhaseError,
			AppliedRevision: 1,
			SampleRate:      48_000,
			BlockSize:       112,
			Message:         "native suppression worker failed for mic-1; stopping processing",
			AudioAdvancing:  false,
		},
		Routing: core.RoutingStatus{
			Status:    core.RoutingPhaseActive,
			Channels:  2,
			Recovered: 0,
		},
	}
}

func check(t *testing.T, ok bool, format string, args ...any) {
	t.Helper()
	if !ok {
		t.Fatalf(format, args...)
	}
}

func TestStoppedEngineErrorRemainsVisibleWithoutAnyFreshMeter(t *testing.T) {
	state := failedState()
	check(t, !state.Engine.AudioAdvancing, "advancing")
	notice, ok := EngineError(&state)
	check(t, ok, "no notice")
	check(t, notice.Message == state.Engine.Message, "message %q", notice.Message)
	check(t, notice.RecoveryDetail == "Original audio route recovery is not confirmed.", "detail %q", notice.RecoveryDetail)
}

func TestOldQueuedMetersCannotConfirmANewEngineAtTheSameRevision(t *testing.T) {
	state := failedState()
	state.Engine.Status = core.EnginePhaseRunning
	state.Engine.Generation = 2
	queued := core.Meter{
		MessageType: core.MeterTypeMeter,
		Revision:    state.Engine.AppliedRevision,
		Generation:  1,
		Targets:     []core.TargetMeter{meter()},
	}
	check(t, !MeterMatchesEngine(&state, &queued), "old generation matched")
	queued.Generation = 2
	check(t, MeterMatchesEngine(&state, &queued), "current did not match")
	original := slices.Clone(queued.Targets)
	queued.Targets = nil
	check(t, !MeterMatchesEngine(&state, &queued), "empty matched")
	queued.Targets = append(slices.Clone(original), original[0])
	check(t, !MeterMatchesEngine(&state, &queued), "duplicate matched")
	queued.Targets = original
	state.Selection.ActiveTargetIDs = []string{"usb-main"}
	check(t, !MeterMatchesEngine(&state, &queued), "other target matched")
	state.Selection.ActiveTargetIDs = []string{"mic-1"}
	state.Selection.Status = core.SelectionPhaseChanging
	check(t, !MeterMatchesEngine(&state, &queued), "changing matched")
	state.Selection.Status = core.SelectionPhaseIdle
	queued.Revision++
	check(t, !MeterMatchesEngine(&state, &queued), "revision matched")
	queued.Revision = state.Engine.AppliedRevision
	for _, phase := range []core.EnginePhase{core.EnginePhaseRestarting, core.EnginePhaseStarting, core.EnginePhaseError} {
		state.Engine.Status = phase
		check(t, !MeterMatchesEngine(&state, &queued), "%s matched", phase)
	}
}

func TestRecoveryRequiresACompleteConfirmedBypass(t *testing.T) {
	state := failedState()
	const restored = "Original audio routes were restored."
	detail := func() string {
		t.Helper()
		n, ok := EngineError(&state)
		check(t, ok, "no notice")
		return n.RecoveryDetail
	}
	for _, phase := range []core.RoutingPhase{core.RoutingPhaseUnmanaged, core.RoutingPhaseStarting,
		core.RoutingPhaseActive, core.RoutingPhaseBypassing, core.RoutingPhaseError} {
		state.Routing.Status = phase
		state.Routing.Recovered = state.Routing.Channels
		check(t, detail() != restored, "%s restored", phase)
	}
	state.Routing.Status = core.RoutingPhaseBypassed
	for _, c := range [][2]uint32{{0, 0}, {2, 0}, {2, 1}, {2, 3}} {
		state.Routing.Channels, state.Routing.Recovered = c[0], c[1]
		check(t, detail() != restored, "%v restored", c)
	}
	state.Routing.Channels, state.Routing.Recovered = 2, 2
	check(t, detail() != restored, "2/2 restored")
	state.Routing.Channels, state.Routing.Recovered = 0, 0
	state.Selection.ActiveTargetIDs = nil
	check(t, detail() != restored, "restart not allowed restored")
	state.Engine.RestartAllowed = true
	check(t, detail() == restored, "not restored: %q", detail())
	for _, mode := range []core.EngineMode{core.EngineModeSimulation, core.EngineModeOffline} {
		state.Engine.Mode = mode
		check(t, detail() != restored, "%s restored", mode)
	}
}

func TestTerminalNoticeClearsOnlyWithANewNonerrorState(t *testing.T) {
	state := failedState()
	state.Engine.Message = " "
	n, ok := EngineError(&state)
	check(t, ok && n.Message == "The audio engine reported an error without further details.", "message %q", n.Message)
	for _, phase := range []core.EnginePhase{core.EnginePhaseStarting, core.EnginePhaseRunning, core.EnginePhaseOffline} {
		state.Engine.Status = phase
		_, ok := EngineError(&state)
		check(t, !ok, "%s has a notice", phase)
	}
}

func ms(n int) time.Duration  { return time.Duration(n) * time.Millisecond }
func sec(n int) time.Duration { return time.Duration(n) * time.Second }

func TestReceiptAndRepeatedCountersAreNotAudioProgress(t *testing.T) {
	now := time.Now()
	progress := NewProgress(100)
	check(t, !progress.Fresh(now), "fresh at start")
	progress.Observe(100, now.Add(ms(50)))
	check(t, !progress.Fresh(now), "fresh after repeat")
	progress.Observe(212, now.Add(ms(100)))
	check(t, progress.Fresh(now.Add(ms(500))), "not fresh after advance")
	progress.Observe(212, now.Add(ms(900)))
	check(t, !progress.Fresh(now.Add(ms(1100))), "fresh after silence")
}

func TestRestartNeedsNewProgress(t *testing.T) {
	now := time.Now()
	progress := NewProgress(10_000)
	progress.Observe(10_112, now)
	check(t, progress.Fresh(now), "not fresh")
	progress.Observe(0, now)
	check(t, !progress.Fresh(now), "fresh after restart")
	progress.Observe(112, now)
	check(t, progress.Fresh(now), "not fresh after new progress")
}

func meter() core.TargetMeter {
	return core.TargetMeter{
		ID: "mic-1", InputPeak: 0.1, OutputPeak: 0.1, RMS: 0.05,
		Spectrum: []float64{}, Callbacks: 100,
		ProcessedFrames: 11200, SuppressionFrames: 20, MissedDeadlines: 7,
		StaleResults: 7, LatencySamples: 1184,
	}
}

func warning(p *Progress, now time.Time) (Warning, bool) { return p.WarningFor(now, true) }

func checkWarning(t *testing.T, got Warning, ok bool, want Warning, wantOK bool) {
	t.Helper()
	if ok != wantOK || (ok && got != want) {
		if wantOK {
			t.Fatalf("warning: got %v %v, want %v", got.Label(), ok, want.Label())
		}
		t.Fatalf("warning: got %v, want none", got.Label())
	}
}

func checkNoWarning(t *testing.T, w Warning, ok bool) { t.Helper(); checkWarning(t, w, ok, 0, false) }

func timing(calls, overruns uint64) core.Timing {
	return core.Timing{Calls: calls, TotalNs: calls * 100, MaxNs: 200, Overruns: overruns}
}

func u64p(v uint64) *uint64 { return &v }

// Native telemetry with a full Performance block: the counters the app now
// receives must not be mistaken for faults while they only grow slowly, and a
// rebuild (a decrease) must merely reset the baseline.
func TestSteadyPerformanceDataRaisesNoWarningAndAResetIsQuiet(t *testing.T) {
	performance := func(calls, overruns, workerCalls uint64) core.Performance {
		return core.Performance{
			CallbackBudgetNs: 2_333_333,
			WorkerBudgetNs:   9_333_333,
			Callback:         timing(calls, overruns),
			WorkerWall:       timing(workerCalls, 0),
			WorkerCPU:        timing(workerCalls, 0),
			HelperLate:       u64p(0),
			HelperStalls:     u64p(0),
		}
	}
	now := time.Now()
	step := func(p *Progress, calls, frames uint64, perf core.Performance) bool {
		target := meter()
		target.Callbacks = calls
		target.ProcessedFrames = frames
		target.MissedDeadlines = 0
		target.StaleResults = 0
		target.Performance = &perf
		target.ID = "mic-1"
		return p.ObserveMeter(1, 0, &target, now)
	}
	progress := NewProgress(11200)
	// The first sample is a baseline, not a fault.
	step(progress, 100, 11200, performance(100, 0, 20))
	// Steady growth, same interval: no warning at all.
	step(progress, 200, 22400, performance(200, 0, 40))
	step(progress, 300, 33600, performance(300, 0, 60))
	w, ok := warning(progress, now)
	checkNoWarning(t, w, ok)
	// A real new overrun is still reported.
	step(progress, 400, 44800, performance(400, 1, 80))
	w, ok = warning(progress, now)
	checkWarning(t, w, ok, WarningAudioDeadline, true)
	// A rebuild resets the counters downwards. On its own sample that is a
	// new baseline, not an unsigned fault, and growth within the new
	// baseline is quiet too.
	rebuilt := NewProgress(11200)
	step(rebuilt, 100, 11200, performance(100, 2, 20))
	step(rebuilt, 200, 22400, performance(50, 1, 10))
	w, ok = warning(rebuilt, now)
	checkNoWarning(t, w, ok)
	step(rebuilt, 300, 33600, performance(150, 1, 12))
	w, ok = warning(rebuilt, now)
	checkNoWarning(t, w, ok)
}

func completeMeter() core.Meter {
	m := core.Meter{MessageType: core.MeterTypeMeter, Revision: 235, Generation: 5}
	for _, id := range []string{"mic-1", "usb-main", "send-chat"} {
		target := meter()
		target.ID = id
		m.Targets = append(m.Targets, target)
	}
	return m
}

func cloneMeter(m core.Meter) core.Meter {
	c := m
	c.Targets = make([]core.TargetMeter, len(m.Targets))
	for i := range m.Targets {
		c.Targets[i] = CloneTargetMeter(&m.Targets[i])
	}
	return c
}

func ids(m *core.Meter) []string {
	var out []string
	for _, t := range m.Targets {
		out = append(out, t.ID)
	}
	return out
}

func TestIdlePassthroughPathsDoNotHoldBackActivePaths(t *testing.T) {
	start := time.Now()
	m := completeMeter()
	progress := map[string]*Progress{}
	active := []string{"mic-1"}
	check(t, !observeMeterProgress(progress, &m, active, start), "first sample advanced")
	// Only the active microphone advances; usb-main and send-chat idle.
	m.Targets[0].Callbacks++
	m.Targets[0].ProcessedFrames += 112
	now := start.Add(ms(100))
	check(t, observeMeterProgress(progress, &m, active, now), "active path did not confirm")
	check(t, progress["mic-1"].Fresh(now), "mic-1 not fresh")
	check(t, !progress["usb-main"].Fresh(now), "usb-main fresh")
	// A stalled active path still blocks renewal while another advances.
	m.Targets[1].Callbacks++
	m.Targets[1].ProcessedFrames += 112
	later := now.Add(ms(100))
	check(t, !observeMeterProgress(progress, &m, active, later), "stalled active path confirmed")
	// With nothing active, any advancing path confirms the engine.
	m.Targets[2].Callbacks++
	m.Targets[2].ProcessedFrames += 112
	last := later.Add(ms(100))
	check(t, observeMeterProgress(progress, &m, nil, last), "no active: not confirmed")
}

func runningState() core.DeviceState {
	state := failedState()
	state.Engine.Status = core.EnginePhaseRunning
	state.Engine.Generation = 5
	state.Engine.AppliedRevision = 235
	state.Engine.AudioAdvancing = true
	state.Selection.ActiveTargetIDs = []string{"mic-1", "usb-main", "send-chat"}
	return state
}

func timeEq(a, b *time.Time) bool {
	if a == nil || b == nil {
		return a == b
	}
	return a.Equal(*b)
}

func TestNativeBoundMetersKeepEngineFreshAfterPathDisableAndGlobalOff(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		start := time.Now()
		state := runningState()
		state.Engine.Mode = core.EngineModeNative
		state.Selection.QualifiedTargetIDs = []string{"mic-1", "usb-main", "send-chat"}
		state.Config.SelectedTargets = []string{"mic-1"}
		if enabled {
			state.Selection.ActiveTargetIDs = []string{"mic-1"}
			state.Routing.Status = core.RoutingPhaseActive
		} else {
			state.Selection.ActiveTargetIDs = []string{}
			state.Routing.Status = core.RoutingPhaseBypassed
		}
		current := completeMeter()
		check(t, MeterMatchesEngine(&state, &current), "complete meter did not match")
		progress := map[string]*Progress{}
		var seen *time.Time
		check(t, ReceiveMeter(&state, &current, progress, &seen, start), "first meter refused")
		now := start.Add(ms(100))
		for i := range current.Targets {
			current.Targets[i].ProcessedFrames += 4480
			current.Targets[i].Callbacks += 40
		}
		check(t, ReceiveMeter(&state, &current, progress, &seen, now), "second meter refused")
		check(t, timeEq(seen, &now), "seen %v, want %v", seen, now)
		m, _ := FreshMeter(&state, &current, progress, "usb-main", seen, now)
		check(t, m != nil, "usb-main not fresh")
		missing := cloneMeter(current)
		missing.Targets = missing.Targets[:len(missing.Targets)-1]
		check(t, !MeterMatchesEngine(&state, &missing), "missing target matched")
		state.Engine.Mode = core.EngineModeJack
		check(t, !MeterMatchesEngine(&state, &current), "jack matched (enabled %v)", enabled)
	}
}

func TestReceiveToUSBMeterRenderRemainsFreshPastThirtySeconds(t *testing.T) {
	start := time.Now()
	state := runningState()
	progress := map[string]*Progress{}
	engineSeen := &start
	current := completeMeter()
	check(t, ReceiveMeter(&state, &current, progress, &engineSeen, start), "first refused")
	m, _ := FreshMeter(&state, &current, progress, "usb-main", engineSeen, start)
	check(t, m == nil, "fresh on the first sample")
	for n := 1; n <= 700; n++ {
		now := start.Add(ms(n * 100))
		for i := range current.Targets {
			current.Targets[i].Callbacks += 40
			current.Targets[i].ProcessedFrames += 4480
		}
		check(t, ReceiveMeter(&state, &current, progress, &engineSeen, now), "refused %d", n)
		displayed, synthetic := FreshMeter(&state, &current, progress, "usb-main", engineSeen, now)
		check(t, displayed != nil && displayed.ID == "usb-main", "not displayed at %d", n)
		check(t, !synthetic, "synthetic")
	}
	end := start.Add(sec(70))
	m, _ = FreshMeter(&state, &current, progress, "usb-main", &start, end)
	check(t, m == nil, "the original stale engine timestamp reproduces the missing meters")
	observed := engineSeen
	for _, c := range []string{"old-generation", "future-generation", "old-revision", "future-revision",
		"wrong-target", "invalid-meter"} {
		bad := cloneMeter(current)
		switch c {
		case "old-generation":
			bad.Generation--
		case "future-generation":
			bad.Generation++
		case "old-revision":
			bad.Revision--
		case "future-revision":
			bad.Revision++
		case "wrong-target":
			bad.Targets[1].ID = "usb-chat"
		case "invalid-meter":
			bad.Targets[1].RMS = -1.0
		}
		check(t, !ReceiveMeter(&state, &bad, progress, &engineSeen, end.Add(ms(100))), "%s accepted", c)
		check(t, timeEq(engineSeen, observed), "%s renewed", c)
	}
	check(t, ReceiveMeter(&state, &current, progress, &engineSeen, end.Add(ms(200))), "current refused")
	check(t, timeEq(engineSeen, observed), "repeated receipt must not renew observation")
	m, _ = FreshMeter(&state, &current, progress, "usb-main", engineSeen, end.Add(sec(1)))
	check(t, m == nil, "silence expires the visible meter")
}

// Skipped unless asked for: it needs locally saved device state and
// diagnostics, and never contacts the mixer. Set RC2FX_TEST_STATE and
// RC2FX_TEST_DIAGNOSTICS to run it.
func TestSavedDeviceMetersFollowTheDesktopReceiveAndRenderPath(t *testing.T) {
	statePath, diagnosticsPath := os.Getenv("RC2FX_TEST_STATE"), os.Getenv("RC2FX_TEST_DIAGNOSTICS")
	if statePath == "" || diagnosticsPath == "" {
		t.Skip("requires explicit locally saved device state and diagnostics; never contacts the mixer")
	}
	stateText, err := os.ReadFile(statePath)
	check(t, err == nil, "%v", err)
	state, err := core.DeviceStateFromJSON(string(stateText))
	check(t, err == nil, "%v", err)
	reportText, err := os.ReadFile(diagnosticsPath)
	check(t, err == nil, "%v", err)
	var report struct {
		Samples []map[string]json.RawMessage `json:"samples"`
	}
	check(t, json.Unmarshal(reportText, &report) == nil, "report")
	var meters []core.Meter
	for _, sample := range report.Samples {
		raw, ok := sample["telemetry"]
		if !ok {
			continue
		}
		var telemetry map[string]json.RawMessage
		if json.Unmarshal(raw, &telemetry) != nil || telemetry["meter"] == nil {
			continue
		}
		m, err := core.FromJSON[core.Meter](string(telemetry["meter"]))
		check(t, err == nil, "%v", err)
		if m.Generation == state.Engine.Generation && m.Revision == state.Engine.AppliedRevision {
			meters = append(meters, m)
		}
	}
	check(t, len(meters) > 61, "need more than30seconds of unchanged saved-state replay")
	start := time.Now()
	progress := map[string]*Progress{}
	engineSeen := &start
	for n := range meters {
		// Replay real serialized payloads at a deterministic desktop receipt
		// cadence; this tests acceptance/display, not target timing.
		now := start.Add(ms(n * 500))
		check(t, ReceiveMeter(&state, &meters[n], progress, &engineSeen, now), "rejected real meter%d", n)
		if n > 0 {
			displayed, _ := FreshMeter(&state, &meters[n], progress, "usb-main", engineSeen, now)
			check(t, displayed != nil && displayed.ID == "usb-main", "actual USBmain telemetry should render")
		}
	}
	fmt.Printf("Accepted and rendered %d actual full meters at generation%d revision%d; unchanged-state replay%dms\n",
		len(meters), state.Engine.Generation, state.Engine.AppliedRevision, (len(meters)-1)*500)
}

func TestContinuousCurrentMetersKeepUnchangedEngineObservationFresh(t *testing.T) {
	start := time.Now()
	m := completeMeter()
	progress := map[string]*Progress{}
	engineSeen := start
	check(t, !observeMeterProgress(progress, &m, ids(&m), start), "first advanced")
	// The service sends no redundant engine/state events. More than twice
	// the UI's 30-second engine window must remain confirmed by real work.
	for n := 1; n <= 700; n++ {
		now := start.Add(ms(n * 100))
		for i := range m.Targets {
			m.Targets[i].Callbacks += 40
			m.Targets[i].ProcessedFrames += 4480
		}
		check(t, observeMeterProgress(progress, &m, ids(&m), now), "not advanced at %d", n)
		engineSeen = now
		for id, p := range progress {
			check(t, p.Fresh(now), "%s not fresh at %d", id, n)
		}
	}
	check(t, engineSeen.Sub(start) == sec(70), "seen %v", engineSeen.Sub(start))
	// Repeated queued/cached receipts are not new observations, even while
	// the previous sample is still inside the meter freshness interval.
	for n := 1; n <= 310; n++ {
		now := engineSeen.Add(ms(n * 100))
		check(t, !observeMeterProgress(progress, &m, ids(&m), now), "repeat advanced at %d", n)
	}
	for id, p := range progress {
		check(t, !p.Fresh(engineSeen.Add(sec(31))), "%s still fresh", id)
	}
}

func TestMeterEngineRenewalRequiresEveryPathAndAStableBaseline(t *testing.T) {
	start := time.Now()
	baseline := completeMeter()
	for _, c := range []string{"stalled-path", "frame-only", "counter-reset", "revision", "gap", "failed"} {
		progress := map[string]*Progress{}
		next := cloneMeter(baseline)
		check(t, !observeMeterProgress(progress, &next, ids(&next), start), "%s: first advanced", c)
		for i := range next.Targets {
			next.Targets[i].Callbacks++
			next.Targets[i].ProcessedFrames += 112
		}
		now := start.Add(ms(100))
		switch c {
		case "stalled-path":
			next.Targets[1] = CloneTargetMeter(&baseline.Targets[1])
		case "frame-only":
			next.Targets[1].Callbacks = baseline.Targets[1].Callbacks
		case "counter-reset":
			next.Targets[1].Callbacks = 1
		case "revision":
			next.Revision++
		case "gap":
			now = start.Add(sec(1))
		case "failed":
			next.Targets[0].WorkerFailed = true
		}
		check(t, !observeMeterProgress(progress, &next, ids(&next), now), "%s", c)
	}
	empty := baseline
	empty.Targets = nil
	check(t, !observeMeterProgress(map[string]*Progress{}, &empty, nil, start), "empty advanced")
}

func TestOldFaultsAreNotNewButFailedWorkersRemainVisible(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	w, ok := warning(p, now)
	checkNoWarning(t, w, ok)
	m.ProcessedFrames += 112
	m.MissedDeadlines++
	p.ObserveMeter(1, 0, &m, now)
	w, ok = warning(p, now)
	checkWarning(t, w, ok, WarningSuppressionFallback, true)
	p.ObserveMeter(1, 0, &m, now.Add(sec(4)))
	w, ok = warning(p, now.Add(sec(5)))
	checkNoWarning(t, w, ok)
	m.WorkerFailed = true
	p.ObserveMeter(1, 0, &m, now.Add(sec(6)))
	w, ok = warning(p, now.Add(sec(60)))
	checkWarning(t, w, ok, WarningWorkerFailed, true)
	m.WorkerFailed = false
	p.ObserveMeter(2, 0, &m, now.Add(sec(61)))
	w, ok = warning(p, now.Add(sec(61)))
	checkNoWarning(t, w, ok)
}

func TestADiagnosticsResetIsANewBaselineNotAFault(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.ProcessedFrames += 112
	m.Callbacks++
	p.ObserveMeter(1, 0, &m, now.Add(ms(50)))
	check(t, p.Fresh(now.Add(ms(50))), "not fresh")
	// After the reset every counter restarts near zero, the faults included;
	// new ones since then are what count.
	m.ProcessedFrames = 224
	m.Callbacks = 2
	m.MissedDeadlines = 4
	reset := now.Add(ms(100))
	p.ObserveMeter(1, 1, &m, reset)
	w, ok := warning(p, reset)
	checkNoWarning(t, w, ok)
	m.ProcessedFrames += 112
	m.Callbacks++
	p.ObserveMeter(1, 1, &m, reset.Add(ms(50)))
	check(t, p.Fresh(reset.Add(ms(50))), "not fresh after reset")
	w, ok = warning(p, reset.Add(ms(50)))
	checkNoWarning(t, w, ok)
	m.MissedDeadlines++
	p.ObserveMeter(1, 1, &m, reset.Add(ms(100)))
	w, ok = warning(p, reset.Add(ms(100)))
	checkWarning(t, w, ok, WarningSuppressionFallback, true)
}

func TestResetAndRevisionChangeEstablishNewBaselines(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.SuppressionFrames = 0
	m.MissedDeadlines = 1
	m.StaleResults = 0
	p.ObserveMeter(1, 0, &m, now)
	w, ok := warning(p, now)
	checkNoWarning(t, w, ok)
	m.MissedDeadlines = 20
	p.ObserveMeter(2, 0, &m, now)
	w, ok = warning(p, now)
	checkNoWarning(t, w, ok)
	check(t, !p.Fresh(now), "fresh")
	m.ProcessedFrames = 0
	m.MissedDeadlines = 30
	p.ObserveMeter(2, 0, &m, now)
	w, ok = warning(p, now)
	checkNoWarning(t, w, ok)
	check(t, !p.Fresh(now), "fresh")
	m.ProcessedFrames = 112
	m.MissedDeadlines++
	p.ObserveMeter(2, 0, &m, now)
	w, ok = warning(p, now)
	checkWarning(t, w, ok, WarningSuppressionFallback, true)
}

func plainTiming(calls uint64) core.Timing {
	return core.Timing{Calls: calls, TotalNs: 100000, MaxNs: 1000}
}

// reading is a reading `seconds` of audio into generation, with xruns
// counted.
func readingAt(generation, epoch, seconds, xruns uint64) core.Meter {
	target := meter()
	target.ProcessedFrames = seconds * 48_000
	target.Performance = &core.Performance{
		CallbackBudgetNs: 2333333, WorkerBudgetNs: 10000000, Xruns: xruns,
		Callback:   plainTiming(100),
		WorkerWall: plainTiming(20),
		WorkerCPU:  plainTiming(20),
	}
	return core.Meter{
		MessageType:      core.MeterTypeMeter,
		Revision:         1,
		Generation:       generation,
		DiagnosticsEpoch: epoch,
		Targets:          []core.TargetMeter{target},
	}
}

// headline is the xruns the headline shows for m.
func headline(w *StartupWindow, m *core.Meter) uint64 {
	target := CloneTargetMeter(&m.Targets[0])
	w.Settle(&target)
	return target.Performance.Xruns
}

func TestAStartupBurstIsLeftOutOfTheHeadline(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	burst := readingAt(1, 0, 5, 12)
	w.Observe(&burst, 48_000, now)
	check(t, w.Open(), "not open")
	check(t, headline(w, &burst) == 0, "headline %d", headline(w, &burst))
	check(t, w.Faults.Xruns == 12, "faults %d", w.Faults.Xruns)
	later := readingAt(1, 0, 70, 12)
	w.Observe(&later, 48_000, now)
	check(t, !w.Open(), "still open")
	check(t, headline(w, &later) == 0, "headline %d", headline(w, &later))
	check(t, w.Faults.Xruns == 12, "faults %d", w.Faults.Xruns)
}

func TestAnXrunAfterTheWindowCountsNormally(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	first := readingAt(1, 0, 59, 12)
	w.Observe(&first, 48_000, now)
	later := readingAt(1, 0, 90, 13)
	w.Observe(&later, 48_000, now)
	check(t, headline(w, &later) == 1, "headline %d", headline(w, &later))
	check(t, w.Faults.Xruns == 12, "faults %d", w.Faults.Xruns)
}

func TestAMixerRestartOpensTheWindowAgain(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	a, b := readingAt(1, 0, 5, 12), readingAt(1, 0, 90, 13)
	w.Observe(&a, 48_000, now)
	w.Observe(&b, 48_000, now)
	restarted := readingAt(2, 0, 2, 3)
	w.Observe(&restarted, 48_000, now)
	check(t, w.Open(), "not open")
	check(t, headline(w, &restarted) == 0, "headline %d", headline(w, &restarted))
	check(t, w.Faults.Xruns == 3, "faults %d", w.Faults.Xruns)
}

func TestADiagnosticsResetIsNotARestart(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	first := readingAt(1, 0, 5, 12)
	w.Observe(&first, 48_000, now)
	// The frames restart with the counters, but the mixer did not.
	reset := readingAt(1, 1, 1, 1)
	w.Observe(&reset, 48_000, now)
	check(t, !w.Open(), "open")
	check(t, headline(w, &reset) == 1, "headline %d", headline(w, &reset))
	check(t, w.Faults.Xruns == 0, "faults %d", w.Faults.Xruns)
	// Reset before this app connected: its age says nothing about startup.
	w = &StartupWindow{}
	w.Observe(&reset, 48_000, now)
	check(t, !w.Open() && !w.WantsEvents(), "open or wants events")
	check(t, headline(w, &reset) == 1, "headline %d", headline(w, &reset))
}

func TestConnectingAfterTheWindowPlacesXrunsFromTheEventLog(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	m := readingAt(1, 0, 130, 15)
	w.Observe(&m, 48_000, now)
	check(t, w.WantsEvents(), "does not want events")
	check(t, headline(w, &m) == 15, "counted until the log says otherwise")
	w.ReadEvents(func(from, until time.Time) uint64 {
		check(t, from.Equal(now.Add(-sec(130))), "from %v", from)
		check(t, until.Equal(now.Add(-sec(70))), "until %v", until)
		return 12
	})
	check(t, !w.WantsEvents(), "still wants events")
	check(t, headline(w, &m) == 3, "headline %d", headline(w, &m))
	check(t, w.Faults.Xruns == 12, "faults %d", w.Faults.Xruns)
}

func TestAnUnreadableEventLogLeavesEveryFaultInTheHeadline(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	m := readingAt(1, 0, 130, 15)
	w.Observe(&m, 48_000, now)
	w.GiveUp()
	check(t, !w.WantsEvents(), "still wants events")
	check(t, headline(w, &m) == 15, "headline %d", headline(w, &m))
	check(t, w.Faults.Xruns == 0, "faults %d", w.Faults.Xruns)
}

func TestAStartupXrunDoesNotWarnButOtherFaultsDo(t *testing.T) {
	now := time.Now()
	r := readingAt(1, 0, 1, 0)
	m := r.Targets[0]
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.Performance.Xruns++
	p.ObserveMeter(1, 0, &m, now)
	w, ok := warning(p, now)
	checkWarning(t, w, ok, WarningAudioDeadline, true)
	p.ForgetStartupDeadline()
	w, ok = warning(p, now)
	checkNoWarning(t, w, ok)
	m.Performance.WorkerCPU.ClockErrors++
	p.ObserveMeter(1, 0, &m, now)
	p.ForgetStartupDeadline()
	w, ok = warning(p, now)
	checkWarning(t, w, ok, WarningClockFailure, true)
}

func TestOptionalPerformanceIsBaselinedAndNewEventsAreVisible(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	callback := plainTiming(100)
	callback.Overruns = 2
	m.Performance = &core.Performance{
		CallbackBudgetNs: 2333333, WorkerBudgetNs: 10000000, Xruns: 4,
		Callback:   callback,
		WorkerWall: plainTiming(20),
		WorkerCPU:  plainTiming(20),
	}
	p.ObserveMeter(1, 0, &m, now)
	w, ok := warning(p, now)
	checkNoWarning(t, w, ok)
	m.Performance.Xruns++
	p.ObserveMeter(1, 0, &m, now)
	w, ok = warning(p, now)
	checkWarning(t, w, ok, WarningAudioDeadline, true)
	m.Performance.WorkerCPU.ClockErrors++
	p.ObserveMeter(1, 0, &m, now)
	w, ok = warning(p, now)
	checkWarning(t, w, ok, WarningClockFailure, true)
}

func TestFirstMeterAfterAReceiptGapIsABaseline(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.ProcessedFrames += 112
	p.ObserveMeter(1, 0, &m, now.Add(ms(50)))
	check(t, p.Fresh(now.Add(ms(50))), "not fresh")

	resumed := now.Add(sec(2))
	m.ProcessedFrames += 48_000
	m.MissedDeadlines += 10
	p.ObserveMeter(1, 0, &m, resumed)
	check(t, !p.Fresh(resumed), "fresh after gap")
	w, ok := warning(p, resumed)
	checkNoWarning(t, w, ok)
	m.ProcessedFrames += 112
	p.ObserveMeter(1, 0, &m, resumed.Add(ms(50)))
	check(t, p.Fresh(resumed.Add(ms(50))), "not fresh after resume")
	w, ok = warning(p, resumed.Add(ms(50)))
	checkNoWarning(t, w, ok)
	m.MissedDeadlines++
	p.ObserveMeter(1, 0, &m, resumed.Add(ms(100)))
	w, ok = warning(p, resumed.Add(ms(100)))
	checkWarning(t, w, ok, WarningSuppressionFallback, true)

	m.WorkerFailed = true
	p.ObserveMeter(1, 0, &m, resumed.Add(sec(2)))
	w, ok = warning(p, resumed.Add(sec(2)))
	checkWarning(t, w, ok, WarningWorkerFailed, true)
}

func TestSuppressionResetClearsThePreviousWorkersRecentWarning(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.ProcessedFrames += 112
	m.MissedDeadlines++
	p.ObserveMeter(1, 0, &m, now.Add(ms(50)))
	w, ok := warning(p, now)
	checkWarning(t, w, ok, WarningSuppressionFallback, true)

	m.SuppressionFrames = 0
	m.MissedDeadlines = 0
	m.StaleResults = 0
	p.ObserveMeter(1, 0, &m, now.Add(ms(100)))
	w, ok = warning(p, now.Add(ms(100)))
	checkNoWarning(t, w, ok)
	m.SuppressionFrames = 1
	m.MissedDeadlines = 1
	p.ObserveMeter(1, 0, &m, now.Add(ms(150)))
	w, ok = warning(p, now.Add(ms(150)))
	checkWarning(t, w, ok, WarningSuppressionFallback, true)
}

func TestCallbackResetRebaselinesFaultsEvenIfFramesHaveCaughtUp(t *testing.T) {
	now := time.Now()
	m := meter()
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.Callbacks = 1
	m.ProcessedFrames += 112
	m.MissedDeadlines += 10
	p.ObserveMeter(1, 0, &m, now.Add(ms(50)))
	w, ok := warning(p, now.Add(ms(50)))
	checkNoWarning(t, w, ok)
	check(t, !p.Fresh(now.Add(ms(50))), "fresh after callback reset")
	m.Callbacks++
	m.ProcessedFrames += 112
	p.ObserveMeter(1, 0, &m, now.Add(ms(100)))
	check(t, p.Fresh(now.Add(ms(100))), "not fresh")
}

func TestInactiveSuppressionDoesNotMaskIndependentAudioWarnings(t *testing.T) {
	now := time.Now()
	m := meter()
	m.Performance = &core.Performance{
		CallbackBudgetNs: 2333333, WorkerBudgetNs: 10000000,
		Callback:   plainTiming(100),
		WorkerWall: plainTiming(20),
		WorkerCPU:  plainTiming(20),
	}
	p := NewProgress(m.ProcessedFrames)
	p.ObserveMeter(1, 0, &m, now)
	m.WorkerFailed = true
	m.Performance.Xruns++
	p.ObserveMeter(1, 0, &m, now.Add(ms(50)))
	w, ok := p.WarningFor(now, true)
	checkWarning(t, w, ok, WarningWorkerFailed, true)
	w, ok = p.WarningFor(now, false)
	checkWarning(t, w, ok, WarningAudioDeadline, true)

	// A later suppression-only event must not replace the recent callback
	// warning, even when that suppression event is not relevant to output.
	m.WorkerFailed = false
	m.MissedDeadlines++
	p.ObserveMeter(1, 0, &m, now.Add(ms(100)))
	w, ok = p.WarningFor(now, false)
	checkWarning(t, w, ok, WarningAudioDeadline, true)
	m.SuppressionFrames = 0
	m.MissedDeadlines = 0
	m.StaleResults = 0
	p.ObserveMeter(1, 0, &m, now.Add(ms(150)))
	w, ok = p.WarningFor(now, false)
	checkWarning(t, w, ok, WarningAudioDeadline, true)
	w, ok = p.WarningFor(now.Add(sec(6)), false)
	checkNoWarning(t, w, ok)
}

func TestStartupFaultsLeaveEveryPathCounter(t *testing.T) {
	now, w := time.Now(), &StartupWindow{}
	burst := readingAt(1, 0, 5, 2)
	burst.Targets[0].MissedDeadlines = 7
	burst.Targets[0].InputDrops = 1
	burst.Targets[0].Performance.Callback.ClockErrors = 3
	w.Observe(&burst, 48_000, now)
	later := cloneMeter(burst)
	later.Targets[0].ProcessedFrames = 90 * 48_000
	later.Targets[0].MissedDeadlines = 9
	w.Observe(&later, 48_000, now)
	shown := CloneTargetMeter(&later.Targets[0])
	w.Settle(&shown)
	check(t, shown.MissedDeadlines == 2 && shown.InputDrops == 0, "(%d, %d)", shown.MissedDeadlines, shown.InputDrops)
	check(t, shown.Performance.Callback.ClockErrors == 0, "clock errors %d", shown.Performance.Callback.ClockErrors)
	xruns, ok := w.SettledXruns(&later)
	check(t, ok && xruns == 0, "settled xruns %d %v", xruns, ok)
}

func TestTheEnginesRunningTimePlacesTheWindowNotTheFrames(t *testing.T) {
	// Paths began counting frames 12 s after the engine started.
	now := time.Now()
	m := readingAt(1, 0, 50, 4)
	m.RunningForNs = u64p(uint64(sec(62)))
	w := &StartupWindow{}
	w.Observe(&m, 48_000, now)
	check(t, !w.Open() && w.WantsEvents(), "open %v wants %v", w.Open(), w.WantsEvents())
	r := w.Range()
	check(t, r != nil, "no range")
	check(t, r[0].Equal(now.Add(-sec(62))), "from %v", r[0])
	check(t, r[1].Equal(now.Add(-sec(2))), "until %v", r[1])
	m.RunningForNs = u64p(uint64(sec(55)))
	w = &StartupWindow{}
	w.Observe(&m, 48_000, now)
	check(t, w.Open(), "not open")
}

func TestARecentCountIsTheRiseWithinTheLastMinute(t *testing.T) {
	now := time.Now()
	wall := time.Now()
	r := &RecentCount{}
	_, ok := r.Within()
	check(t, !ok, "within before any reading")
	within := func() uint64 {
		t.Helper()
		v, ok := r.Within()
		check(t, ok, "no within")
		return v
	}
	r.Observe(1, 0, 4, now, wall)
	// The first reading is a baseline, not a rise.
	check(t, r.Recent().Last == nil, "rose on the baseline")
	r.Observe(1, 0, 4, now.Add(sec(30)), wall)
	check(t, within() == 0, "within %d", within())
	r.Observe(1, 0, 6, now.Add(sec(50)), wall)
	check(t, within() == 2, "within %d", within())
	check(t, timeEq(r.Recent().Last, &wall), "last %v", r.Recent().Last)
	// The reading at 30 s is the baseline once the first ages out.
	r.Observe(1, 0, 7, now.Add(sec(95)), wall)
	check(t, within() == 3, "within %d", within())
	r.Observe(1, 0, 7, now.Add(sec(200)), wall)
	check(t, within() == 0, "within %d", within())
	// A restart or reset starts the counter, and the window, over.
	r.Observe(2, 0, 1, now.Add(sec(201)), wall)
	check(t, within() == 0, "within %d", within())
	check(t, r.Recent().Last == nil, "last after restart")
}
