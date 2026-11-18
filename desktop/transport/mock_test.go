//go:build mockmixer

package transport

import (
	"bytes"
	"encoding/json"
	"slices"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

func mockRequest(method, path string, body json.RawMessage) *usbproto.Request {
	return &usbproto.Request{
		Method: method,
		Path:   path,
		Body:   body,
	}
}

func mustMarshal(t *testing.T, v any) json.RawMessage {
	t.Helper()
	body, err := marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return body
}

// connectedMixer is a mixer and the app's own session model, connected the
// way the app is.
func connectedMixer(t *testing.T) (*mockMixer, *core.Session, time.Time) {
	t.Helper()
	now := time.Now()
	m := newMockMixer(now)
	m.connected()
	session := core.DefaultSession()
	if _, err := session.Receive(readState(t, m, now)); err != nil {
		t.Fatal(err)
	}
	return m, &session, now
}

// readState reads state through the app's strict parser.
func readState(t *testing.T, m *mockMixer, now time.Time) core.DeviceState {
	t.Helper()
	reply := m.handle(mockRequest("GET", "api/v1/state", nil), now)
	if reply.Status != 200 {
		t.Fatalf("state: %d", reply.Status)
	}
	return parseState(t, reply.Body)
}

func parseState(t *testing.T, body json.RawMessage) core.DeviceState {
	t.Helper()
	text, err := compact(body)
	if err != nil {
		t.Fatal(err)
	}
	state, err := core.DeviceStateFromJSON(string(text))
	if err != nil {
		t.Fatal(err)
	}
	return state
}

func settled(t *testing.T, m *mockMixer, now time.Time) (core.DeviceState, time.Time) {
	t.Helper()
	later := now.Add(settleTime)
	return readState(t, m, later), later
}

// nextMeter polls until a meter arrives, as the app's event stream does.
func nextMeter(t *testing.T, m *mockMixer, now time.Time) (core.Meter, time.Time) {
	t.Helper()
	for range 100 {
		now = now.Add(50 * time.Millisecond)
		reply := m.handle(mockRequest("GET", "api/v1/events", nil), now)
		if name, _ := stringField(reply.Body, "event"); name == "meter" {
			data, _ := field(reply.Body, "data")
			var meter core.Meter
			if err := decode(data, &meter); err != nil {
				t.Fatal(err)
			}
			return meter, now
		}
	}
	t.Fatal("no meter arrived")
	return core.Meter{}, now
}

func assertMeterAccepted(t *testing.T, meter *core.Meter, state *core.DeviceState) {
	t.Helper()
	if meter.Generation != state.Engine.Generation {
		t.Fatalf("generation %d, want %d", meter.Generation, state.Engine.Generation)
	}
	if meter.Revision != state.Engine.AppliedRevision {
		t.Fatalf("revision %d, want %d", meter.Revision, state.Engine.AppliedRevision)
	}
	if !meter.Synthetic {
		t.Fatal("the app must label this as a simulation")
	}
	ids := make([]string, 0, len(meter.Targets))
	for _, target := range meter.Targets {
		ids = append(ids, target.ID)
	}
	if !slices.Equal(ids, state.Selection.QualifiedTargetIDs) {
		t.Fatalf("meter targets %v, qualified %v", ids, state.Selection.QualifiedTargetIDs)
	}
	for i := range meter.Targets {
		if err := meter.Targets[i].Validate(); err != nil {
			t.Fatal(err)
		}
	}
}

// The whole way through Transport, as the app sends it. The only test using
// the shared mixer; the others each build their own.
func TestASaveGoesThroughTheTransportAndComesBackAsMeters(t *testing.T) {
	transport, err := New()
	if err != nil {
		t.Fatal(err)
	}
	defer transport.Close()
	wait := func(found func(EventKind) bool) {
		t.Helper()
		deadline := time.Now().Add(5 * time.Second)
		for time.Now().Before(deadline) {
			if event, ok := transport.TryRecv(); ok {
				if found(event.Kind) {
					return
				}
			} else {
				time.Sleep(5 * time.Millisecond)
			}
		}
		t.Fatal("the expected event did not arrive")
	}
	if err := transport.Send(CommandConnect{Generation: 1}); err != nil {
		t.Fatal(err)
	}
	var mixer string
	var raw json.RawMessage
	wait(func(kind EventKind) bool {
		connected, ok := kind.(EventKindConnected)
		mixer, raw = connected.Mixer, connected.State
		return ok
	})
	if mixer != mockSerial {
		t.Fatalf("serial %q", mixer)
	}
	state := parseState(t, raw)
	update := core.ConfigUpdate{
		ExpectedRevision: state.Config.Revision,
		Targets:          state.Config.Targets,
	}
	if err := transport.Send(CommandApply{Generation: 1, Update: update}); err != nil {
		t.Fatal(err)
	}
	revision := state.Config.Revision + 1
	wait(func(kind EventKind) bool {
		applied, ok := kind.(EventKindApplied)
		if ok {
			if got := parseState(t, applied.State).Config.Revision; got != revision {
				t.Fatalf("applied revision %d, want %d", got, revision)
			}
		}
		return ok
	})
	wait(func(kind EventKind) bool {
		meter, ok := kind.(EventKindMeter)
		if !ok {
			return false
		}
		got, ok := uintField(meter.Meter, "revision")
		return ok && got == revision
	})
}

func TestTheAppAcceptsTheStartingStateAsAppliedAndReady(t *testing.T) {
	m, session, now := connectedMixer(t)
	state := readState(t, m, now)
	if !state.IsApplied() {
		t.Fatal("not applied")
	}
	if !state.CanDisableProcessing() {
		t.Fatal("cannot disable processing")
	}
	if !session.IsApplied() {
		t.Fatal("the session is not applied")
	}
	meter, _ := nextMeter(t, m, now)
	assertMeterAccepted(t, &meter, &state)
}

func TestANewConnectionReceivesTheStateFirst(t *testing.T) {
	m, _, now := connectedMixer(t)
	reply := m.handle(mockRequest("GET", "api/v1/events", nil), now)
	if name, _ := stringField(reply.Body, "event"); name != "state" {
		t.Fatalf("first event %q", name)
	}
	data, _ := field(reply.Body, "data")
	parseState(t, data)
}

func TestASaveIsAcknowledgedThenSettlesAndAStaleRevisionConflicts(t *testing.T) {
	m, session, now := connectedMixer(t)
	session.Draft[0].Gain.Enabled = true
	session.Draft[0].Gain.DB = 6.0
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	body := mustMarshal(t, pending.Request)
	reply := m.handle(mockRequest("PUT", "api/v1/config", body), now)
	if reply.Status != 200 {
		t.Fatalf("save: %d %s", reply.Status, reply.Body)
	}
	state := parseState(t, reply.Body)
	if !state.ProcessingTransitionPending() {
		t.Fatal("no transition pending")
	}
	if err := session.Acknowledge(pending, state); err != nil {
		t.Fatal(err)
	}

	state, later := settled(t, m, now)
	if !state.IsApplied() {
		t.Fatal("not applied")
	}
	if got := state.Config.Targets[0].Gain.DB; got != 6.0 {
		t.Fatalf("gain %v", got)
	}
	conflict := m.handle(mockRequest("PUT", "api/v1/config", body), later)
	if conflict.Status != 409 {
		t.Fatalf("stale save: %d", conflict.Status)
	}
	if detail, _ := stringField(conflict.Body, "error"); detail != "revision conflict" {
		t.Fatalf("error %q", detail)
	}
}

func TestASaveTheRealMixerWouldRefuseIsRefused(t *testing.T) {
	m, session, now := connectedMixer(t)
	targets := slices.Clone(session.Draft)
	targets[0].Gain.DB = 99.0
	update := core.ConfigUpdate{
		ExpectedRevision: m.config.Revision,
		Targets:          targets,
	}
	reply := m.handle(mockRequest("PUT", "api/v1/config", mustMarshal(t, update)), now)
	if reply.Status != 400 {
		t.Fatalf("status %d", reply.Status)
	}
}

func TestAPathChangePassesTheAppsAcknowledgementCheckAndSettles(t *testing.T) {
	m, session, now := connectedMixer(t)
	session.SelectionDraft = append(session.SelectionDraft, "usb-main")
	pending, err := session.BeginSelection()
	if err != nil {
		t.Fatal(err)
	}
	reply := m.handle(mockRequest("PUT", "api/v1/selection", mustMarshal(t, pending.Request)), now)
	if reply.Status != 202 {
		t.Fatalf("status %d %s", reply.Status, reply.Body)
	}
	state := parseState(t, reply.Body)
	if err := pending.ValidateAcknowledgement(state); err != nil {
		t.Fatal(err)
	}

	state, later := settled(t, m, now)
	if !state.IsApplied() {
		t.Fatal("not applied")
	}
	if !slices.Contains(state.Selection.ActiveTargetIDs, "usb-main") {
		t.Fatalf("active %v", state.Selection.ActiveTargetIDs)
	}
	meter, _ := nextMeter(t, m, later)
	assertMeterAccepted(t, &meter, &state)
}

func TestProcessingTurnsOffAndOnThroughTheAppsTransitions(t *testing.T) {
	m, session, now := connectedMixer(t)
	pending, err := session.BeginRouting(false)
	if err != nil {
		t.Fatal(err)
	}
	reply := m.handle(mockRequest("PUT", "api/v1/routing", mustMarshal(t, pending.Request)), now)
	if reply.Status != 202 {
		t.Fatalf("status %d %s", reply.Status, reply.Body)
	}
	state := parseState(t, reply.Body)
	if err := pending.ValidateAcknowledgement(state); err != nil {
		t.Fatal(err)
	}

	state, later := settled(t, m, now)
	if len(state.Selection.ActiveTargetIDs) != 0 {
		t.Fatalf("active %v", state.Selection.ActiveTargetIDs)
	}
	if !state.CanEnableProcessing() {
		t.Fatal("cannot enable processing")
	}
	if _, err := session.Receive(state); err != nil {
		t.Fatal(err)
	}
	pending, err = session.BeginRouting(true)
	if err != nil {
		t.Fatal(err)
	}
	reply = m.handle(mockRequest("PUT", "api/v1/routing", mustMarshal(t, pending.Request)), later)
	state = parseState(t, reply.Body)
	if err := pending.ValidateAcknowledgement(state); err != nil {
		t.Fatal(err)
	}
	state, _ = settled(t, m, later)
	if !state.IsApplied() {
		t.Fatal("not applied")
	}
}

func TestASecondTransitionWaitsForTheFirst(t *testing.T) {
	m, session, now := connectedMixer(t)
	saved := session.Saved
	if saved == nil {
		t.Fatal("nothing saved")
	}
	body := func(enabled bool) json.RawMessage {
		return object(
			member{"enabled", jsonBool(enabled)},
			member{"expectedRevision", jsonUint(saved.Config.Revision)},
			member{"expectedGeneration", jsonUint(saved.Engine.Generation)},
		)
	}
	first := m.handle(mockRequest("PUT", "api/v1/routing", body(false)), now)
	if first.Status != 202 {
		t.Fatalf("first: %d", first.Status)
	}
	second := m.handle(mockRequest("PUT", "api/v1/routing", body(true)), now)
	if second.Status != 409 {
		t.Fatalf("second: %d", second.Status)
	}
}

func TestMetersStopWhileAChangeSettles(t *testing.T) {
	m, session, now := connectedMixer(t)
	update := core.ConfigUpdate{
		ExpectedRevision: m.config.Revision,
		Targets:          session.Draft,
	}
	m.handle(mockRequest("PUT", "api/v1/config", mustMarshal(t, update)), now)
	m.events = nil
	m.lastUSB = new(now)
	during := now.Add(meterPeriod)
	event := m.handle(mockRequest("GET", "api/v1/events", nil), during)
	if !isNull(event.Body) {
		t.Fatalf("got %s", event.Body)
	}
}

func TestTheControlsMoveTheMeters(t *testing.T) {
	m, session, now := connectedMixer(t)
	index := func(m *mockMixer) int {
		i := slices.IndexFunc(m.config.Targets, func(target core.Target) bool { return target.ID == "analog-1" })
		if i < 0 {
			t.Fatal("no analog-1")
		}
		return i
	}
	peak := func(meter *core.Meter) (float64, float64) {
		i := slices.IndexFunc(meter.Targets, func(target core.TargetMeter) bool { return target.ID == "analog-1" })
		if i < 0 {
			t.Fatal("no analog-1 meter")
		}
		return meter.Targets[i].InputPeak, meter.Targets[i].OutputPeak
	}
	meter, now := nextMeter(t, m, now)
	if input, output := peak(&meter); input != output {
		t.Fatalf("no effects enabled yet: %v from %v", output, input)
	}
	target := index(m)
	session.Draft[target].Gain.Enabled = true
	session.Draft[target].Gain.DB = -12.0
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	m.handle(mockRequest("PUT", "api/v1/config", mustMarshal(t, pending.Request)), now)
	_, now = settled(t, m, now)
	meter, _ = nextMeter(t, m, now)
	if input, output := peak(&meter); !(output < input*0.3) {
		t.Fatalf("%v from %v", output, input)
	}
}

func TestPresetsSaveApplyAndDelete(t *testing.T) {
	m, _, now := connectedMixer(t)
	revision := m.config.Revision
	created := m.handle(mockRequest("POST", "api/v1/presets",
		object(member{"name", jsonString("Voice")}, member{"expectedRevision", jsonUint(revision)})), now)
	if created.Status != 201 {
		t.Fatalf("create: %d", created.Status)
	}
	id, _ := stringField(created.Body, "id")
	if !core.ValidID(id) {
		t.Fatalf("id %q", id)
	}
	list := m.handle(mockRequest("GET", "api/v1/presets", nil), now)
	presets, _ := field(list.Body, "presets")
	items, _ := elements(presets)
	if len(items) == 0 {
		t.Fatalf("list %s", list.Body)
	}
	if name, _ := stringField(items[0], "name"); name != "Voice" {
		t.Fatalf("name %q", name)
	}

	apply := "api/v1/presets/" + id + "/apply"
	applied := m.handle(mockRequest("POST", apply, object(member{"expectedRevision", jsonUint(revision)})), now)
	if applied.Status != 200 {
		t.Fatalf("apply: %d %s", applied.Status, applied.Body)
	}
	parseState(t, applied.Body)
	if m.config.Revision != revision+1 {
		t.Fatalf("revision %d", m.config.Revision)
	}

	path := "api/v1/presets/" + id
	deleted := m.handle(mockRequest("DELETE", path, nil), now)
	if want := object(member{"deleted", jsonString(id)}); !bytes.Equal(deleted.Body, want) {
		t.Fatalf("got %s, want %s", deleted.Body, want)
	}
	missing := m.handle(mockRequest("DELETE", path, nil), now)
	if missing.Status != 404 {
		t.Fatalf("missing: %d", missing.Status)
	}
}

func TestInputsAndVersionsPassTheAppsChecks(t *testing.T) {
	m, _, now := connectedMixer(t)
	reply := m.handle(mockRequest("GET", "api/v1/inputs", nil), now)
	var inventory core.StockInventory
	if err := decode(reply.Body, &inventory); err != nil {
		t.Fatal(err)
	}
	if err := inventory.Validate(); err != nil {
		t.Fatal(err)
	}
	reply = m.handle(mockRequest("GET", "api/v1/version", nil), now)
	var versions core.DeviceVersions
	if err := decode(reply.Body, &versions); err != nil {
		t.Fatal(err)
	}
	if versions.FirmwareProfile == nil || *versions.FirmwareProfile != mockFirmwareProfile {
		t.Fatalf("profile %v", versions.FirmwareProfile)
	}
}

func TestSSHTurnsOnAndOffWithoutHostKeys(t *testing.T) {
	m, _, now := connectedMixer(t)
	read := func(reply usbproto.Reply) core.SSHStatus {
		t.Helper()
		if reply.Status != 200 {
			t.Fatalf("status %d", reply.Status)
		}
		var status core.SSHStatus
		if err := decode(reply.Body, &status); err != nil {
			t.Fatal(err)
		}
		if len(status.HostKeys) != 0 {
			t.Fatalf("host keys %v", status.HostKeys)
		}
		return status
	}
	if read(m.handle(mockRequest("GET", "api/v1/ssh", nil), now)).Managed {
		t.Fatal("on at the start")
	}
	key := "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f"
	on := read(m.handle(mockRequest("PUT", "api/v1/ssh", mustMarshal(t, core.SSHRequest{Enabled: true, PublicKey: &key})), now))
	if !on.Managed || on.ManagedFingerprint == nil {
		t.Fatalf("not on: %+v", on)
	}
	if !read(m.handle(mockRequest("GET", "api/v1/ssh", nil), now)).Managed {
		t.Fatal("forgot it was on")
	}
	if missing := m.handle(mockRequest("PUT", "api/v1/ssh", mustMarshal(t, core.SSHRequest{Enabled: true})), now); missing.Status != 400 {
		t.Fatalf("no key: %d", missing.Status)
	}
	if read(m.handle(mockRequest("PUT", "api/v1/ssh", mustMarshal(t, core.SSHRequest{Enabled: false})), now)).Managed {
		t.Fatal("still on")
	}
}

func TestADiagnosticsResetRestartsTheCountersUnderANewEpoch(t *testing.T) {
	m, _, now := connectedMixer(t)
	before, now := nextMeter(t, m, now)
	if before.Targets[0].ProcessedFrames == 0 {
		t.Fatal("no frames before the reset")
	}
	reply := m.handle(mockRequest("POST", "api/v1/diagnostics/reset", nil), now)
	if reply.Status != 200 {
		t.Fatalf("reset: %d", reply.Status)
	}
	after, now := nextMeter(t, m, now)
	if after.DiagnosticsEpoch != before.DiagnosticsEpoch+1 {
		t.Fatalf("epoch %d after %d", after.DiagnosticsEpoch, before.DiagnosticsEpoch)
	}
	if after.Targets[0].ProcessedFrames > before.Targets[0].ProcessedFrames {
		t.Fatalf("frames %d after %d", after.Targets[0].ProcessedFrames, before.Targets[0].ProcessedFrames)
	}
	page := m.handle(mockRequest("GET", "api/v1/diagnostics/events?after=0", nil), now)
	var events core.DiagnosticsEvents
	if err := decode(page.Body, &events); err != nil {
		t.Fatal(err)
	}
	if len(events.Events) != 1 || events.Events[0].Kind != core.DiagnosticsKindReset {
		t.Fatalf("events %+v", events.Events)
	}
}

func TestXrunsAppearInTheEventLogAndTheMeter(t *testing.T) {
	m, _, now := connectedMixer(t)
	later := now.Add(xrunPeriod)
	page := m.handle(mockRequest("GET", "api/v1/diagnostics/events", nil), later)
	var events core.DiagnosticsEvents
	if err := decode(page.Body, &events); err != nil {
		t.Fatal(err)
	}
	if len(events.Events) == 0 || events.Events[0].Kind != core.DiagnosticsKindXrun {
		t.Fatalf("events %+v", events.Events)
	}
	meter, _ := nextMeter(t, m, later)
	i := slices.IndexFunc(meter.Targets, func(target core.TargetMeter) bool { return target.ID == "mic-1" })
	if i < 0 || meter.Targets[i].Performance == nil || meter.Targets[i].Performance.Xruns != 1 {
		t.Fatalf("host meter %+v", meter.Targets)
	}
}
