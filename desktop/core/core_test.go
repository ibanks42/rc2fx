package core

import (
	"fmt"
	"math"
	"slices"
	"strings"
	"testing"
)

// The settings model and device state: wire round trips and defaults, strict
// validation, suppression latency and the voice gate, meters and diagnostics,
// EQ coefficients, response curves and EasyEffects interchange, and the
// editor session (restarts, conflicts, late replies). Rust's
// crates/rc2fx-core/tests/core.rs checks the mixer's side of the model; the
// curves, interchange and session are Go only.

func coreState(revision uint64) DeviceState {
	selected := []string{"mic-1", "usb-main", "send-chat"}
	config := DefaultConfig()
	config.Revision = revision
	config.SelectedTargets = slices.Clone(selected)
	return DeviceState{
		Selection: SelectionStatus{
			Status:             SelectionPhaseIdle,
			Allowed:            true,
			QualifiedTargetIDs: slices.Clone(selected),
			ActiveTargetIDs:    slices.Clone(selected),
			PendingTargetIDs:   []string{},
		},
		Routing: RoutingStatus{Status: RoutingPhaseActive, Channels: 5},
		Config:  config,
		Engine: EngineStatus{
			Generation:      1,
			Mode:            EngineModeJack,
			Status:          EnginePhaseRunning,
			AppliedRevision: revision,
			SampleRate:      48000,
			BlockSize:       112,
			AudioAdvancing:  true,
		},
	}
}

func restartableState() DeviceState {
	state := coreState(7)
	state.Engine.Status = EnginePhaseError
	state.Engine.Generation = 8
	state.Engine.RestartAllowed = true
	state.Engine.AudioAdvancing = false
	return state
}

func restartAck(state DeviceState) DeviceState {
	ack := state.Clone()
	ack.Engine.Generation++
	ack.Engine.Status = EnginePhaseRestarting
	ack.Engine.RestartAllowed = false
	return ack
}

func TestRestartWireRequiresGenerationAndExplicitEligibility(t *testing.T) {
	original := restartableState()
	value := toValue(t, original)
	check(t, must(DeviceStateFromJSON(value.String())).Equal(original))
	for _, key := range []string{"generation", "restartAllowed"} {
		value := value.Clone()
		value.GetMut("engine").Remove(key)
		fails(t, must2(DeviceStateFromJSON(value.String())), key)
	}
	request := EngineRestartRequest{ExpectedRevision: 7, ExpectedGeneration: 8}
	check(t, toValue(t, request).Equal(parse(t, `{"expectedRevision":7,"expectedGeneration":8}`)))
	meter := parse(t, `{"type":"meter","revision":7,"generation":8,"synthetic":false,"targets":[]}`)
	ok(t, must2(FromJSON[Meter](meter.String())))
	old := meter.Clone()
	old.Remove("generation")
	fails(t, must2(FromJSON[Meter](old.String())))
}

// must2 keeps only the error of a (value, error) pair.
func must2[T any](_ T, err error) error { return err }

func TestRestartPreservesSavedRevisionSettingsAndUnsavedDraft(t *testing.T) {
	saved := restartableState()
	session := DefaultSession()
	receive(t, &session, saved.Clone())
	session.Draft[0].Gain.DB = -9.
	draft := CloneTargets(session.Draft)
	pending := must(session.BeginRestart())
	check(t, pending.Request.ExpectedRevision == 7)
	check(t, pending.Request.ExpectedGeneration == 8)
	ok(t, session.AcknowledgeRestart(pending, restartAck(saved)))
	check(t, session.Saved.Config.Equal(saved.Config))
	check(t, TargetsEqual(session.Draft, draft))
	check(t, !session.IsApplied())
	fails(t, must2(session.BeginApply()))
	fails(t, must2(session.BeginRestart()))
}

func TestLateRestartAckCannotOverwriteTerminalStreamState(t *testing.T) {
	for _, phase := range []EnginePhase{EnginePhaseRunning, EnginePhaseError} {
		saved := restartableState()
		session := DefaultSession()
		receive(t, &session, saved.Clone())
		pending := must(session.BeginRestart())
		ack := restartAck(saved)
		terminal := ack.Clone()
		terminal.Engine.Status = phase
		terminal.Engine.Message = "Stream result"
		receive(t, &session, terminal.Clone())
		ok(t, session.AcknowledgeRestart(pending, ack))
		check(t, session.Saved.Equal(terminal))
		must(session.ReceiveSnapshot(restartAck(saved)))
		check(t, session.Saved.Equal(terminal))
		check(t, receive(t, &session, saved.Clone()) == RemoteUpdateStale)
		check(t, !session.ReceiveEngine(saved.Engine))
		// A deliberate new connection can target a different mixer with its
		// own generation sequence. A transient event-stream reconnect does not reset it.
		session.Disconnect()
		check(t, receive(t, &session, saved) != RemoteUpdateStale)
	}
}

func TestRestartRejectsWrongGenerationMutatedSettingsOrUnreservedResponse(t *testing.T) {
	saved := restartableState()
	session := DefaultSession()
	receive(t, &session, saved.Clone())
	pending := must(session.BeginRestart())
	for mutation := 0; mutation < 6; mutation++ {
		ack := restartAck(saved)
		switch mutation {
		case 0:
			ack.Engine.Generation++
		case 1:
			ack.Config.Revision++
		case 2:
			ack.Config.Targets[0].Gain.DB = -5.
		case 3:
			ack.Engine.Status = EnginePhaseRunning
		case 4:
			ack.Engine.RestartAllowed = true
		default:
			ack.Engine.AudioAdvancing = true
		}
		fails(t, session.AcknowledgeRestart(pending, ack), mutation)
		check(t, session.Saved.Equal(saved))
	}
	session.Disconnect()
	fails(t, must2(session.BeginRestart()))
}

func TestAppliedRequiresAllHardwareChannelsRouted(t *testing.T) {
	value := coreState(1)
	check(t, value.IsApplied())
	value.Routing.Channels--
	check(t, !value.IsApplied())
	for _, phase := range []RoutingPhase{
		RoutingPhaseUnmanaged, RoutingPhaseStarting, RoutingPhaseBypassing, RoutingPhaseBypassed, RoutingPhaseError,
	} {
		value.Routing.Status = phase
		check(t, !value.IsApplied())
	}
	value.Engine.Mode = EngineModeSimulation
	check(t, value.IsApplied())
}

func TestCurrentWireRoundtripAndDefaults(t *testing.T) {
	config := DefaultConfig()
	ok(t, config.Validate())
	check(t, len(config.Targets) == 25)
	check(t, len(config.SelectedTargets) == 0)
	text := encode(t, config)
	check(t, must(ConfigFromJSON(text)).Equal(config))
	check(t, strings.Contains(text, `"schemaVersion":9`))
	check(t, !strings.Contains(text, "algorithm"))
	check(t, !strings.Contains(text, "screenConversion"))
}

func TestRemovedSuppressionOptionsAreRejectedAtEveryConfigEntry(t *testing.T) {
	config := toValue(t, DefaultConfig())
	for _, version := range []uint64{5, 6, 7, 8, 10} {
		invalid := config.Clone()
		invalid.Set("schemaVersion", U64Value(version))
		fails(t, must2(ConfigFromJSON(invalid.String())), version)
		snapshot := toValue(t, coreState(1))
		snapshot.Set("config", invalid)
		fails(t, must2(DeviceStateFromJSON(snapshot.String())), version)
	}
	obsolete := config.Clone()
	obsolete.Set("screenConversion", StringValue("retired"))
	fails(t, must2(ConfigFromJSON(obsolete.String())))
	targets, _ := config.Get("targets")
	update := ObjectValue(
		Member{"expectedRevision", U64Value(1)},
		Member{"targets", targets},
		Member{"screenConversion", StringValue("retired")},
	)
	fails(t, must2(FromJSON[ConfigUpdate](update.String())))
	for _, field := range []string{"algorithm", "strength"} {
		invalid := config.Clone()
		invalid.Pointer("/targets/0/suppression").Set(field, StringValue("retired"))
		fails(t, must2(ConfigFromJSON(invalid.String())), field)
		targets, _ := invalid.Get("targets")
		update := ObjectValue(Member{"expectedRevision", U64Value(1)}, Member{"targets", targets})
		fails(t, must2(FromJSON[ConfigUpdate](update.String())), field)
	}
}

func TestFixedSuppressionRoundtripsWithoutModelIdentity(t *testing.T) {
	config := DefaultConfig()
	config.Targets[0].Suppression.Enabled = true
	text := encode(t, config)
	check(t, must(ConfigFromJSON(text)).Equal(config))
}

func TestSuppressionLookaheadRoundtripsAndRejectsUnavailableGeometry(t *testing.T) {
	config := DefaultConfig()
	config.Targets[0].Suppression.SelectLookahead(1)
	text := encode(t, config)
	check(t, must(ConfigFromJSON(text)).Equal(config))
	for _, lookahead := range []uint8{2, 240} {
		config.Targets[0].Suppression.Lookahead = lookahead
		fails(t, config.Validate(), lookahead)
	}
}

func TestVoiceGateRoundtripsAndOneStrengthSetsEveryParameter(t *testing.T) {
	config := DefaultConfig()
	config.Targets[0].Suppression.Enabled = true
	gate := &config.Targets[0].Suppression.Gate
	gate.Strength = 0.7
	text := encode(t, config)
	check(t, strings.Contains(text, `"gate":{"strength":0.7,"advanced":false,`), text)
	check(t, must(ConfigFromJSON(text)).Equal(config))
	// The default is off, its advanced parameters what 0 stands for.
	defaultGate := DefaultConfig().Targets[0].Suppression.Gate
	check(t, !defaultGate.Active())
	advanced := defaultGate
	advanced.SetAdvanced(true)
	want := defaultGate
	want.Advanced = true
	check(t, advanced == want)
	shape := GateShapeForStrength
	off, middle, strict := shape(0.), shape(0.5), shape(1.)
	check(t, off.Threshold == 0.3 && off.ReductionDB == 0. && off.HoldMs == 350. && off.ReleaseMs == 300., off)
	check(t, strict.Threshold == 0.7 && strict.ReductionDB == 30. && strict.HoldMs == 150., strict)
	check(t, approx(strict.ReleaseMs, 90., 1e-9))
	check(t, middle.Threshold == 0.5 && middle.AttackMs == 5. && middle.HoldMs == 250., middle)
	check(t, approx(middle.ReductionDB, 20., 1e-9))
	check(t, approx(middle.CloseThreshold, 0.35, 1e-12), "hysteresis below the threshold")
	check(t, GateShapeForStrength(0.).AttackMs == GateShapeForStrength(1.).AttackMs)
	// Stricter: surer voice to open, deeper, shorter hold, faster close.
	previous := off
	for step := 1; step <= 10; step++ {
		next := shape(float64(step) / 10.)
		check(t, next.Threshold > previous.Threshold && next.ReductionDB > previous.ReductionDB, step)
		check(t, next.HoldMs < previous.HoldMs && next.ReleaseMs < previous.ReleaseMs, step)
		previous = next
	}
	for _, strength := range []float64{-0.01, 1.01, math.NaN()} {
		config.Targets[0].Suppression.Gate.Strength = strength
		fails(t, config.Validate(), strength)
	}
}

func TestEasyEffectsWidthAndSlopeAreIgnoredOnImportAndNotExported(t *testing.T) {
	var setEverywhere func(value *Value)
	setEverywhere = func(value *Value) {
		switch value.Kind() {
		case ValueObject:
			if _, ok := value.Get("frequency"); ok {
				value.Set("slope", StringValue("x4"))
				value.Set("width", F64Value(11.0))
			}
			for i := range value.members {
				setEverywhere(&value.members[i].Value)
			}
		case ValueArray:
			for i := range value.items {
				setEverywhere(&value.items[i])
			}
		}
	}
	text := easyFixture(t, "output-linked-apo.json")
	plain := imported(t, text, true)
	steep := parse(t, text)
	setEverywhere(&steep)
	check(t, imported(t, steep.String(), true).Equal(plain))
	exported := must(ExportEasyEffects(plain, PipelineOutput))
	check(t, !strings.Contains(exported, `"slope"`) && !strings.Contains(exported, `"width"`))
}

func TestAdvancedVoiceGateStartsFromStrengthAndKeepsItsOwnValues(t *testing.T) {
	gate := DefaultConfig().Targets[0].Suppression.Gate
	gate.Strength = 1.0
	gate.SetAdvanced(true)
	check(t, gate.Shape() == GateShapeForStrength(1.0), "no jump when switching")
	gate.Threshold = 0.3
	gate.Hold = 900.0
	gate.Strength = 0.0
	check(t, gate.Shape().Threshold == 0.3 && gate.Shape().HoldMs == 900.0, "decoupled from strength")
	gate.SetAdvanced(false)
	check(t, gate.Shape() == GateShapeForStrength(0.0))
	check(t, gate.Threshold == 0.3 && gate.Hold == 900.0, "custom values are kept")
	config := DefaultConfig()
	for _, change := range []struct {
		field string
		value float64
	}{
		{"threshold", 1.5},
		{"reduction", 61.0},
		{"attack", 0.1},
		{"hold", 2001.0},
		{"release", 29.0},
	} {
		wire := toValue(t, config)
		wire.Pointer("/targets/0/suppression/gate").Set(change.field, F64Value(change.value))
		fails(t, must2(ConfigFromJSON(wire.String())), change.field)
	}
	config.Targets[0].Suppression.Gate = gate
	ok(t, config.Validate())
}

func TestSuppressionLatencyIncludesModelFrameAndWorkerAllowance(t *testing.T) {
	geometry, admitted := SuppressionAdmittedGeometry(0)
	check(t, admitted)
	check(t, geometry.FrameSamples == 240)
	check(t, geometry.IntrinsicDelaySamples == 120)
	check(t, geometry.WorkerLeadSamples == 320)
	check(t, geometry.LatencySamples() == 680)
}

func TestLookaheadOneAddsExactlyOneHopToTheAdmittedLatency(t *testing.T) {
	la0, _ := SuppressionAdmittedGeometry(0)
	la1, _ := SuppressionAdmittedGeometry(1)
	check(t, la1.LatencySamples()-la0.LatencySamples() == DFNHopSamples)
	_, admitted := SuppressionAdmittedGeometry(2)
	check(t, !admitted)
}

func TestTheWorkerLeadAndTheLatencyAddUp(t *testing.T) {
	geometry, _ := SuppressionAdmittedGeometry(0)
	check(t, geometry.WithWorkerLead(112).LatencySamples() == 240+120+112)
}

func TestPerformanceReportsHelperCountersAndAcceptsPayloadsWithoutThem(t *testing.T) {
	timing := Timing{Calls: 9, TotalNs: 900, MaxNs: 200, Overruns: 1, ClockErrors: 0, Histogram: [8]uint64{1, 2, 3, 4, 5, 6, 7, 8}}
	performance := Performance{
		CallbackBudgetNs:    2_333_333,
		WorkerBudgetNs:      9_333_333,
		Xruns:               3,
		Callback:            timing,
		WorkerWall:          timing,
		WorkerCPU:           timing,
		HelperLate:          new(uint64(2)),
		HelperStalls:        new(uint64(1)),
		CallbackRecentMaxNs: new(uint64(180)),
		WorkerRecentMaxNs:   new(uint64(900)),
	}
	target := TargetMeter{
		ID:                "mic-1",
		Spectrum:          []float64{},
		Callbacks:         9,
		ProcessedFrames:   900,
		SuppressionFrames: 9,
		LatencySamples:    808,
		Performance:       &performance,
	}
	meter := Meter{MessageType: MeterTypeMeter, Revision: 1, Generation: 1, Targets: []TargetMeter{target}}
	text := encode(t, meter)
	check(t, strings.Contains(text, `"helperLate":2`) && strings.Contains(text, `"helperStalls":1`), text)
	check(t, strings.Contains(text, `"callbackRecentMaxNs":180`) && strings.Contains(text, `"workerRecentMaxNs":900`), text)
	back := must(FromJSON[Meter](text))
	check(t, deepEqual(back, meter))
	// A mixer that predates the fields sends none: they are absent rather than
	// zero, so "not reported" stays distinguishable from "none".
	old := toValue(t, meter)
	object := old.Pointer("/targets/0/performance")
	for _, key := range []string{"helperLate", "helperStalls", "callbackRecentMaxNs", "workerRecentMaxNs"} {
		object.Remove(key)
	}
	var oldMeter Meter
	ok(t, DecodeValue(old, &oldMeter))
	p := oldMeter.Targets[0].Performance
	check(t, p.HelperLate == nil && p.HelperStalls == nil)
	check(t, p.CallbackRecentMaxNs == nil && p.WorkerRecentMaxNs == nil)
	check(t, p.Xruns == 3)
}

func TestMetersWithoutADiagnosticsEpochLoadAsZeroAndEventPagesRoundTrip(t *testing.T) {
	// An older mixer sends no epoch: the app reads 0 and keeps working.
	value := parse(t, `{"type": "meter", "revision": 1, "generation": 1, "synthetic": false, "targets": []}`)
	var meter Meter
	ok(t, DecodeValue(value, &meter))
	check(t, meter.DiagnosticsEpoch == 0)
	value.Set("diagnosticsEpoch", U64Value(7))
	ok(t, DecodeValue(value, &meter))
	check(t, meter.DiagnosticsEpoch == 7)

	page := DiagnosticsEvents{
		NowNs:   10,
		NextSeq: 3,
		Lost:    1,
		More:    false,
		Events: []DiagnosticsEvent{
			{
				Seq:     1,
				Kind:    DiagnosticsKindXrun,
				AtNs:    5,
				Message: "JACK xrun",
				Details: EventDetails{ValueNs: new(uint64(1120)), DelayedUs: new(uint64(240))},
			},
			{
				Seq:     2,
				Kind:    DiagnosticsKindOutputDrops,
				Path:    new("mic-1"),
				AtNs:    6,
				Message: "3 output drops on mic-1",
				Details: EventDetails{Delta: new(uint64(3)), IntervalNs: new(uint64(20_000_000))},
			},
		},
	}
	text := encode(t, page)
	check(t, strings.Contains(text, `"kind":"xrun"`), text)
	check(t, strings.Contains(text, `"kind":"outputDrops"`), text)
	check(t, deepEqual(must(FromJSON[DiagnosticsEvents](text)), page))
	// Empty details are omitted rather than sent as an object of nulls.
	plain := DiagnosticsEvent{Seq: 3, Kind: DiagnosticsKindReset, AtNs: 7, Message: "Diagnostics reset"}
	text = encode(t, plain)
	check(t, !strings.Contains(text, "details"), text)
	check(t, deepEqual(must(FromJSON[DiagnosticsEvent](text)), plain))
}

func TestDeviceStatusTextMustBeFitToShow(t *testing.T) {
	good := DeviceState{
		Config: DefaultConfig(),
		Engine: EngineStatus{
			Generation:      1,
			Mode:            EngineModeJack,
			Status:          EnginePhaseRunning,
			AppliedRevision: 1,
			SampleRate:      48_000,
			BlockSize:       112,
			Message:         "Running",
			AudioAdvancing:  true,
		},
		Routing: RoutingStatus{Status: RoutingPhaseActive, Channels: 1, Message: "Active"},
		Selection: SelectionStatus{
			Status:             SelectionPhaseIdle,
			Allowed:            true,
			QualifiedTargetIDs: []string{},
			ActiveTargetIDs:    []string{},
			PendingTargetIDs:   []string{},
			Message:            "Ready",
		},
	}
	ok(t, good.Validate())
	for _, bad := range []string{
		"line\nbreak",
		"escape\x1b[2J",
		"reversed\u202etxt",
		"isolate\u2066txt",
		strings.Repeat("x", MaxStatusText+1),
	} {
		state := good.Clone()
		state.Engine.Message = bad
		fails(t, state.Engine.Validate(), "engine", bad)
		fails(t, state.Validate(), "engine", bad)
		state = good.Clone()
		state.Routing.Message = bad
		fails(t, state.Validate(), "routing", bad)
		state = good.Clone()
		state.Selection.Message = bad
		fails(t, state.Validate(), "selection", bad)
		versions := DeviceVersions{Firmware: new(bad)}
		fails(t, versions.Validate(), "version", bad)
	}
	versions := DeviceVersions{
		Runtime:         new("0.1.2"),
		Protocol:        new(uint32(1)),
		Firmware:        new("1.7.7"),
		FirmwareProfile: new("rodecaster-pro-ii-1.7.7"),
	}
	ok(t, versions.Validate())
}

func TestDiagnosticsEventsAreMadeSafeToShow(t *testing.T) {
	event := func(path *string, message string) DiagnosticsEvent {
		return DiagnosticsEvent{Seq: 1, Kind: DiagnosticsKindReset, Path: path, AtNs: 1, Message: message}
	}
	for _, path := range []string{"", "Not An Id", "../x", strings.Repeat("a", 49)} {
		_, shown := event(new(path), "x").ForDisplay()
		check(t, !shown, path)
	}
	shown, _ := event(new("mic-1"), "a\nb\x1bc\u202ed\u2066e").ForDisplay()
	check(t, shown.Message == "abcde", shown.Message)
	// Cut on a character boundary, never past the cap.
	long, _ := event(nil, strings.Repeat("\u00e9", MaxEventMessage)).ForDisplay()
	check(t, len(long.Message) == MaxEventMessage, len(long.Message))
	check(t, strings.Trim(long.Message, "\u00e9") == "")
}

func TestStrictConfigRejectsAmbiguousOrInvalidData(t *testing.T) {
	base := toValue(t, DefaultConfig())
	var changes []Value
	for _, change := range []struct {
		pointer string
		value   string
	}{
		{"/schemaVersion", `1`},
		{"/schemaVersion", `2`},
		{"/schemaVersion", `3`},
		{"/schemaVersion", `4`},
		{"/schemaVersion", `10`},
		{"/targets/0/suppression/wet", `1.01`},
		{"/targets/0/suppression/gate/strength", `1.5`},
		{"/targets/0/suppression/gate", `{"enabled": true, "strength": 0.5}`},
		{"/revision", `0`},
		{"/targets/0/channels", `1.5`},
		{"/targets/0/channels", `256`},
		{"/targets/0/eq/bands", `null`},
		{"/targets/0/gain/db", `13`},
	} {
		next := base.Clone()
		*next.Pointer(change.pointer) = parse(t, change.value)
		changes = append(changes, next)
	}
	nullRight := base.Clone()
	nullRight.Pointer("/targets/0/eq").Set("rightBands", NullValue())
	changes = append(changes, nullRight)
	unknown := base.Clone()
	unknown.Pointer("/targets/0/eq").Set("legacy", BoolValue(true))
	changes = append(changes, unknown)
	duplicatedTarget := base.Clone()
	*duplicatedTarget.Pointer("/targets/1/id") = StringValue("mic-1")
	changes = append(changes, duplicatedTarget)
	badOrder := base.Clone()
	*badOrder.Pointer("/targets/0/order/1") = StringValue("eq")
	changes = append(changes, badOrder)
	for _, value := range changes {
		fails(t, must2(ConfigFromJSON(value.String())), value.String())
	}
	text := encode(t, DefaultConfig())
	fails(t, must2(ConfigFromJSON(strings.Replace(text, `"schemaVersion":9`, `"schemaVersion":9,"schemaVersion":9`, 1))))
	check(t, strings.Contains(text, `"frequency":80.0`), "floats keep their .0")
	fails(t, must2(ConfigFromJSON(strings.Replace(text, `"frequency":80.0`, `"frequency":80.0,"frequency":90.0`, 1))))
	fails(t, must2(ConfigFromJSON(text+" {}")))
	fails(t, must2(FromJSON[Value](strings.Repeat("[", 66)+"0"+strings.Repeat("]", 66))))
	fails(t, must2(FromJSON[Value](`{"x":1e400}`)))
	fails(t, must2(FromJSON[Value](`{"x":"\ud800"}`)))
}

func TestTypedValidationAndBandOperations(t *testing.T) {
	config := DefaultConfig()
	config.Targets[4].Suppression.Enabled = true
	fails(t, config.Validate())
	config.Targets[4].Suppression.Enabled = false
	config.Targets[0].Gain.DB = math.NaN()
	fails(t, config.Validate())
	config.Targets[0].Gain.DB = 0.0
	targets := CloneTargets(config.Targets)
	targets[0].Channels = 2
	fails(t, config.ValidateUpdate(targets))
	eq := &config.Targets[0].EQ
	for n := 1; n <= 32; n++ {
		check(t, must(eq.AddBand(ChannelLeft)).ID == fmt.Sprintf("band-%d", n), n)
	}
	fails(t, must2(eq.AddBand(ChannelLeft)))
	check(t, eq.RemoveBand(ChannelLeft, "band-2"))
	check(t, must(eq.AddBand(ChannelLeft)).ID == "band-33")
	eq.Bands[0].Mode = BandModeRlcBt
	ok(t, eq.Validate(1))
	eq.Bands[0].Mode = BandModeApoDr
	eq.RightBands = &[]Band{}
	fails(t, eq.Validate(1))
	ok(t, eq.Validate(2))
}

func TestSharedRlcReferenceCoefficientsMatch(t *testing.T) {
	vectors := parse(t, easyFixture(t, "rlc-bt-x1-mapping-vectors.json"))
	list, _ := vectors.Pointer("/vectors").AsArray()
	check(t, len(list) > 0)
	for _, vector := range list {
		name, _ := vector.Pointer("/type").AsString()
		kinds := map[string]BandType{
			"Bell": BandTypeBell, "Lo-shelf": BandTypeLowShelf, "Hi-shelf": BandTypeHighShelf,
			"Lo-pass": BandTypeLowPass, "Hi-pass": BandTypeHighPass, "Notch": BandTypeNotch,
		}
		kind, known := kinds[name]
		if !known {
			t.Fatalf("Unknown fixture type %s", name)
		}
		number := func(key string) float64 {
			value, ok := vector.Pointer("/" + key).AsF64()
			if !ok {
				t.Fatalf("%s is not a number", key)
			}
			return value
		}
		band := NewBand("band-1")
		band.BandType = kind
		band.Mode = BandModeRlcBt
		band.Frequency = number("frequency")
		band.Gain = number("gain")
		band.Q = number("easyeffectsQ")
		check(t, approx(DesignQ(band), number("rbjQ"), 1e-12), vector.String())
		coefficients := must(DesignCoefficients(band)).AsArray()
		wants, _ := vector.Pointer("/coefficients").AsArray()
		for i := 0; i < min(len(coefficients), len(wants)); i++ {
			want, _ := wants[i].AsF64()
			check(t, approx(coefficients[i], want, 2e-12), vector.String(), coefficients[i])
		}
	}
}

func TestResponseCurveMatchesPerPointResponse(t *testing.T) {
	eq := DefaultEQSettings()
	shelf := NewBand("band-1")
	shelf.Gain, shelf.Frequency, shelf.BandType = 4.0, 120.0, BandTypeLowShelf
	bell := NewBand("band-2")
	bell.Gain, bell.Frequency, bell.Q = -3.0, 2500.0, 2.0
	highPass := NewBand("band-3")
	highPass.Frequency, highPass.BandType = 60.0, BandTypeHighPass
	eq.Bands = []Band{shelf, bell, highPass}
	eq.InputGainDB = -1.5
	frequencies := LogFrequencies(64, 10.0, 24000.0)
	curve := ResponseCurve(eq, ChannelLeft, frequencies, true)
	for i, frequency := range frequencies {
		expected := ResponseDB(eq, ChannelLeft, frequency, true)
		check(t, approx(curve[i], expected, 1e-12), frequency)
	}
}

func TestResponseGainsSoloBypassAndChannelIsolation(t *testing.T) {
	eq := DefaultEQSettings()
	band := NewBand("band-1")
	band.Gain = 6.0
	eq.Bands = []Band{band}
	eq.InputGainDB = -2.0
	eq.OutputGainDB = 1.0
	check(t, approx(ResponseDB(eq, ChannelLeft, 1000.0, false), 6.0, 1e-9))
	check(t, approx(ResponseDB(eq, ChannelRight, 1000.0, true), 5.0, 1e-9))
	eq.RightBands = &[]Band{}
	check(t, ResponseDB(eq, ChannelRight, 1000.0, true) == -1.0)
	solo := NewBand("band-2")
	solo.Solo = true
	solo.Enabled = false
	eq.Bands = append(eq.Bands, solo)
	check(t, ResponseDB(eq, ChannelLeft, 1000.0, false) == 0.0)
	frequencies := []float64{10.0, 1000.0, 30000.0}
	for _, channel := range []Channel{ChannelLeft, ChannelRight} {
		for _, gains := range []bool{false, true} {
			curve := ResponseCurve(eq, channel, frequencies, gains)
			for i, frequency := range frequencies {
				check(t, curve[i] == ResponseDB(eq, channel, frequency, gains), channel, gains, frequency)
			}
		}
	}
	eq.Enabled = false
	check(t, slices.Equal(ResponseCurve(eq, ChannelLeft, frequencies, true), []float64{0, 0, 0}))
	check(t, ResponseDB(eq, ChannelLeft, 1000.0, true) == 0.0)
	check(t, slices.Equal(LogFrequencies(1, 10.0, 24000.0), []float64{10.0}))
	check(t, len(LogFrequencies(0, 10.0, 24000.0)) == 0)
	high := NewBand("band-1")
	high.Frequency = 24000.0
	clamped := must(DesignCoefficients(high))
	high.Frequency = 23520.0
	check(t, clamped == must(DesignCoefficients(high)))
}

func TestAllSupportedSharedPresetsImportAndRoundtrip(t *testing.T) {
	for _, name := range []string{
		"community-loudness-eq-extract.json",
		"output-linked-apo.json",
		"output-apo-common-types.json",
		"output-split-different.json",
		"input-v6-unindexed.json",
		"input-empty-as-off.json",
	} {
		eq := imported(t, easyFixture(t, name), true)
		ok(t, eq.Validate(2))
		roundtrip := imported(t, must(ExportEasyEffects(eq, PipelineOutput)), true)
		check(t, roundtrip.Equal(eq), "Roundtrip", name)
	}
}

func TestEmptyAndUnequalChannelExportNeverEmitZeroBands(t *testing.T) {
	empty := DefaultEQSettings()
	text := must(ExportEasyEffects(empty, PipelineInput))
	tree := parse(t, text)
	count, _ := tree.Pointer("/input/equalizer#0/num-bands").AsI64()
	check(t, count == 1, text)
	check(t, imported(t, text, true).Equal(empty))
	split := DefaultEQSettings()
	split.Bands = []Band{NewBand("band-1")}
	split.RightBands = &[]Band{}
	check(t, imported(t, must(ExportEasyEffects(split, PipelineOutput)), true).Equal(split))
	nonNeutral := NewBand("band-1")
	nonNeutral.BandType = BandTypeOff
	nonNeutral.Gain = 5.0
	nonNeutral.Enabled = false
	eq := DefaultEQSettings()
	eq.Bands = []Band{nonNeutral}
	check(t, imported(t, must(ExportEasyEffects(eq, PipelineOutput)), true).Equal(eq))
}

func TestExplicitEqualizerAndMonoChannelSelection(t *testing.T) {
	text := easyFixture(t, "output-split-different.json")
	check(t, must(ImportEasyEffects(text, ImportOptions{})).Kind == ImportResultChooseChannel)
	left := must(ImportEasyEffects(text, ImportOptions{Channel: new(ChannelLeft)}))
	right := must(ImportEasyEffects(text, ImportOptions{Channel: new(ChannelRight)}))
	check(t, !left.Equal(right))
	multiple := parse(t, text)
	output, _ := multiple.Get("output")
	multiple.Set("input", output.Clone())
	choice := must(ImportEasyEffects(multiple.String(), ImportOptions{}))
	check(t, choice.Kind == ImportResultChooseEqualizer && len(choice.Candidates) == 2, choice)
	chosen := must(ImportEasyEffects(multiple.String(), ImportOptions{Stereo: true, Selected: new("input/equalizer#0")}))
	check(t, chosen.Kind == ImportResultReady, chosen)
}

func TestUnsupportedImportsAndStrictJSONLeaveCallerUnchanged(t *testing.T) {
	base := parse(t, easyFixture(t, "output-linked-apo.json"))
	for _, change := range []struct {
		pointer string
		value   string
	}{
		{"/output/equalizer#0/mode", `"FIR"`},
		{"/output/equalizer#0/decramp", `"x2"`},
		{"/output/equalizer#0/balance", `1`},
		{"/output/equalizer#0/pitch-left", `2`},
		{"/output/equalizer#0/left/band0/type", `"Allpass"`},
		{"/output/equalizer#0/left/band0/mode", `"BWC (BT)"`},
		{"/output/equalizer#0/left/band0/gain", `37`},
		{"/output/equalizer#0/num-bands", `0`},
		{"/output/equalizer#0/num-bands", `33`},
		{"/output/equalizer#0/left/band0/mute", `"false"`},
	} {
		tree := base.Clone()
		*tree.Pointer(change.pointer) = parse(t, change.value)
		fails(t, must2(ImportEasyEffects(tree.String(), ImportOptions{Stereo: true})), "Accepted", change.pointer)
	}
	fails(t, must2(ImportEasyEffects(`{"output":{},"output":{}}`, ImportOptions{})))
	unknown := base.Clone()
	unknown.Pointer("/output/equalizer#0").Set("oversampling", U64Value(2))
	fails(t, must2(ImportEasyEffects(unknown.String(), ImportOptions{})))
	before := DefaultEQSettings()
	unchanged := before.Clone()
	_, _ = ImportEasyEffects("garbage", ImportOptions{})
	check(t, before.Equal(unchanged))
}

func TestMutedSoloAndModelDefaultsArePreserved(t *testing.T) {
	value := parse(t, easyFixture(t, "output-linked-apo.json"))
	for _, side := range []string{"left", "right"} {
		band := value.Pointer("/output/equalizer#0/" + side + "/band0")
		band.Remove("mode")
		band.Set("solo", BoolValue(true))
		band.Set("mute", BoolValue(true))
	}
	eq := imported(t, value.String(), true)
	check(t, eq.Bands[0].Mode == BandModeRlcBt)
	check(t, eq.Bands[0].Solo && !eq.Bands[0].Enabled)
	check(t, ResponseDB(eq, ChannelLeft, 1000.0, false) == 0.0)
}

func TestSessionConflictPreservesEditsAndRequiresChoice(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, coreState(1))
	session.Draft[0].Gain.DB = -4.0
	check(t, receive(t, &session, coreState(2)) == RemoteUpdateConflict)
	check(t, session.Draft[0].Gain.DB == -4.0)
	fails(t, must2(session.BeginApply()))
	ok(t, session.RebaseEdits())
	check(t, must(session.BeginApply()).Request.ExpectedRevision == 2)
	session.ReloadSaved()
	check(t, !session.Dirty())
	check(t, session.Draft[0].Gain.DB == 0.0)
	check(t, receive(t, &session, coreState(1)) == RemoteUpdateStale)
	check(t, session.Saved.Config.Revision == 2)
}

func TestLateSaveKeepsNewerEditAndAppliedIsNotJustSaved(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, coreState(1))
	session.Draft[0].Gain.DB = -4.0
	pending := must(session.BeginApply())
	session.Draft[0].Gain.DB = -5.0
	response := coreState(2)
	response.Config.Targets = CloneTargets(pending.Request.Targets)
	response.Engine.AppliedRevision = 1
	ok(t, session.Acknowledge(pending, response))
	check(t, session.Draft[0].Gain.DB == -5.0)
	check(t, session.Dirty())
	check(t, !session.IsApplied())
	session.ReloadSaved()
	check(t, !session.IsApplied())
	active := session.Saved.Clone()
	active.Engine.AppliedRevision = 2
	receive(t, &session, active)
	check(t, session.IsApplied())
	stalled := session.Saved.Clone()
	stalled.Engine.AudioAdvancing = false
	receive(t, &session, stalled)
	check(t, !session.IsApplied())
	failed := session.Saved.Clone()
	failed.Engine.Status = EnginePhaseError
	receive(t, &session, failed)
	check(t, !session.IsApplied())
	session.Disconnect()
	check(t, !session.IsApplied())
}

func TestOfflineDraftNotSilentlyOverwrittenOnConnection(t *testing.T) {
	session := DefaultSession()
	session.Draft[0].Gain.DB = -6.0
	check(t, session.Dirty())
	check(t, receive(t, &session, coreState(1)) == RemoteUpdateConflict)
	check(t, session.Draft[0].Gain.DB == -6.0)
}

func TestPresetReplyPreservesEditsMadeWhileLoading(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, coreState(1))
	original := CloneTargets(session.Draft)
	loaded := coreState(2)
	loaded.Config.Targets[0].Gain.DB = -6.0
	ok(t, session.AcknowledgePreset(1, original, loaded.Clone()))
	check(t, session.Draft[0].Gain.DB == -6.0)
	check(t, !session.Conflict)
	check(t, !session.Dirty())

	edited := DefaultSession()
	receive(t, &edited, coreState(1))
	edited.Draft[0].Gain.DB = -3.0
	ok(t, edited.AcknowledgePreset(1, original, loaded))
	check(t, edited.Draft[0].Gain.DB == -3.0)
	check(t, edited.Conflict)
	before := CloneTargets(edited.Draft)
	fails(t, edited.AcknowledgePreset(2, before, coreState(4)))
	check(t, TargetsEqual(edited.Draft, before))
}

func TestMetersRequireCurrentCountersAndFiniteValues(t *testing.T) {
	target := parse(t, `{"id":"mic-1","inputPeak":0.1,"outputPeak":0.05,"rms":0.02,"gainReduction":0,"voice":0.4,"gateOpen":1,"clipped":0,"spectrum":[],"callbacks":1,"processedFrames":112,"suppressionFrames":0,"missedDeadlines":0,"staleResults":0,"inputDrops":0,"outputDrops":0,"latencySamples":1440,"workerFailed":false}`)
	decoded := must(FromJSON[TargetMeter](target.String()))
	ok(t, decoded.Validate())
	target.Remove("workerFailed")
	fails(t, must2(FromJSON[TargetMeter](target.String())))
	invalid := decoded
	invalid.RMS = math.NaN()
	fails(t, invalid.Validate())
	// Voice score and gate position are fractions.
	for _, values := range [][2]float64{{1.01, 1.}, {-0.01, 1.}, {0.5, 1.5}, {0.5, math.NaN()}} {
		invalid := decoded
		invalid.Voice, invalid.GateOpen = values[0], values[1]
		fails(t, invalid.Validate(), values)
	}
}
