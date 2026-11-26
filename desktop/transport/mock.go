//go:build mockmixer

package transport

// A stand-in mixer for working on the app away from the hardware, built only
// with the mockmixer tag. It replaces the HID client under the same `api/v1`
// requests, so everything above it is the real app.
//
// It behaves as a running native engine does: saves check their revision, a
// change is acknowledged first and settles a moment later, and it only
// reports meters while running. Settings live for one run of the app; each
// start is the same demo setup. No audio exists: meters are generated, and
// marked synthetic so the channel editor labels its live readings as
// simulated.
//
// It manages SSH access as a current mixer does, but reports no host keys, so
// the app never pins a made-up host key among the real ones.

import (
	"encoding/json"
	"fmt"
	"math"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

// mockSerial is what the app sees as the mixer's USB serial.
const mockSerial = "MOCK-MIXER"

// settleTime is how long a change takes to reach "running" again.
const settleTime = 300 * time.Millisecond

const meterPeriod = 100 * time.Millisecond

const usbPeriod = time.Second

// xrunPeriod is often enough that the diagnostics events have something to
// show.
const xrunPeriod = 47 * time.Second

const blockSize uint32 = 112

const mockFirmware = "1.7.7"

const mockFirmwareProfile = "rodecaster-pro-ii-1.7.7"

// mockRuntime is the mixer runtime version the simulated mixer reports
// (tracking config/mixer-release.json, independent of the desktop app version).
const mockRuntime = "0.2.1"

type showFader struct {
	sourceID uint16
	name     string
	color    string
	target   string
}

// show is the show's faders: source id, name, color and the path it feeds.
// These paths carry a signal; every other input is silent, like an unused
// socket.
var show = [5]showFader{
	{3, "Host", "fffdba0e", "mic-1"},
	{0, "Guest", "ff5ce1e6", "analog-1"},
	{7, "Music", "ffa78bfa", "usb-main"},
	{8, "Call", "fff17e7e", "usb-chat"},
	{9, "Phone", "ff929eaf", "bluetooth"},
}

// The shared mixer outlives connections, so settings survive Disconnect and
// Connect.
var (
	sharedOnce  sync.Once
	sharedLock  sync.Mutex
	sharedMixer *mockMixer
)

// withMixer runs f with the shared mixer locked.
func withMixer[T any](f func(*mockMixer) T) T {
	sharedOnce.Do(func() { sharedMixer = newMockMixer(time.Now()) })
	sharedLock.Lock()
	defer sharedLock.Unlock()
	return f(sharedMixer)
}

// connection is one app connection to the shared mixer.
type connection struct{}

func openConnection() connection {
	withMixer(func(m *mockMixer) struct{} {
		m.connected()
		return struct{}{}
	})
	return connection{}
}

func (connection) Request(request *usbproto.Request) (usbproto.Reply, error) {
	return withMixer(func(m *mockMixer) usbproto.Reply {
		return m.handle(request, time.Now())
	}), nil
}

func (connection) IsOpen() bool {
	return true
}

func (connection) Reopen() error {
	return nil
}

func (connection) IsCompact() bool {
	return true
}

// openSession stands the mock mixer in for USB 1.
func openSession() (Session, string, error) {
	return openConnection(), mockSerial, nil
}

func mockReply(status uint16, body any) usbproto.Reply {
	value, err := marshal(body)
	if err != nil {
		panic("mock replies serialize")
	}
	return usbproto.Reply{Status: status, Body: value}
}

func mockFailure(status uint16, message string) usbproto.Reply {
	return usbproto.Reply{Status: status, Body: object(member{"error", jsonString(message)})}
}

// refused carries a reply out of a handler that stopped early.
type refused struct {
	reply usbproto.Reply
}

func parseBody(body json.RawMessage, v any) *refused {
	if body == nil {
		return &refused{mockFailure(400, "A JSON body is required")}
	}
	if err := decode(body, v); err != nil {
		return &refused{mockFailure(400, err.Error())}
	}
	return nil
}

func expectedRevision(body json.RawMessage) (uint64, *refused) {
	if revision, ok := uintField(body, "expectedRevision"); ok {
		return revision, nil
	}
	return 0, &refused{mockFailure(400, "expectedRevision is required")}
}

// demoConfig is the demo's starting point: a treated host mic, so the
// editors have something to show, and two more paths switched on.
func demoConfig() core.Config {
	config := core.DefaultConfig()
	config.SelectedTargets = []string{"mic-1", "analog-1", "usb-chat"}
	index := slices.IndexFunc(config.Targets, func(target core.Target) bool { return target.ID == "mic-1" })
	if index < 0 {
		panic("the default catalog has mic-1")
	}
	host := &config.Targets[index]
	host.Highpass.Enabled = true
	host.Suppression.Enabled = true
	host.Suppression.Wet = 0.9
	host.Suppression.Gate = core.VoiceGateWithStrength(0.4)
	host.EQ.Enabled = true
	for _, b := range []struct {
		bandType  core.BandType
		frequency float64
		gain      float64
	}{
		{core.BandTypeLowShelf, 120.0, -2.0},
		{core.BandTypeBell, 3_000.0, 2.5},
		{core.BandTypeHighShelf, 10_000.0, 1.5},
	} {
		band, err := host.EQ.AddBand(core.ChannelLeft)
		if err != nil {
			panic("the demo stays under the band limit")
		}
		band.BandType = b.bandType
		band.Frequency = b.frequency
		band.Gain = b.gain
	}
	host.Compressor.Enabled = true
	host.Limiter.Enabled = true
	return config
}

// signal is a level and whether it is speech, for one path at seconds.
func signal(target *core.Target, seconds float64) (float64, bool) {
	// A silent mixer, to see what the app costs with nothing to show.
	if _, silent := os.LookupEnv("RC2FX_MOCK_SILENT"); silent {
		return 0.0, false
	}
	seed := 0.0
	for i := 0; i < len(target.ID); i++ {
		seed += float64(target.ID[i])
	}
	wave := func(rate, phase float64) float64 { return math.Sin(seconds*rate + seed*phase) }
	inShow := slices.ContainsFunc(show[:], func(fader showFader) bool { return fader.target == target.ID })
	switch {
	case target.Kind == core.TargetKindMic && inShow:
		if wave(0.9, 1.0)+wave(0.37, 3.0) > -0.3 {
			return 0.12 + 0.2*(1.0+wave(9.0, 2.0)*wave(3.1, 5.0)), true
		}
		return 0.006 * (1.5 + wave(2.3, 7.0)), false
	case target.Kind == core.TargetKindPlayback && inShow:
		return 0.25 + 0.1*wave(1.4, 1.0) + 0.08*wave(8.0, 2.0), false
	case target.Kind == core.TargetKindOutput:
		return 0.3 + 0.1*wave(1.1, 1.0) + 0.1*wave(6.0, 2.0), false
	default:
		return 0.0, false
	}
}

func toDecibels(level float64) float64 {
	return 20.0 * math.Log10(math.Max(level, 1e-6))
}

func toLevel(decibels float64) float64 {
	return math.Pow(10, decibels/20.0)
}

// process is what the chain would roughly do to input: enough for each
// control to visibly move the meters, not a model of the DSP.
func process(target *core.Target, input float64, speaking bool, meter *core.TargetMeter) {
	output := input
	if target.Suppression.Enabled {
		gate := &target.Suppression.Gate
		meter.Voice = 0.04
		if speaking {
			meter.Voice = 0.92
		}
		meter.GateOpen = 0.0
		if speaking || !gate.Active() {
			meter.GateOpen = 1.0
		}
		if !speaking {
			output *= 1.0 - 0.9*target.Suppression.Wet
			output *= toLevel(-gate.Shape().ReductionDB)
		}
	}
	if target.EQ.Enabled {
		output *= toLevel(target.EQ.InputGainDB + target.EQ.OutputGainDB)
	}
	if target.Compressor.Enabled {
		compressor := &target.Compressor
		over := math.Max(toDecibels(output)-compressor.Threshold, 0.0)
		meter.GainReduction = over * (1.0 - 1.0/compressor.Ratio)
		output *= toLevel(compressor.Makeup - meter.GainReduction)
	}
	if target.Gain.Enabled {
		output *= toLevel(target.Gain.DB)
	}
	if target.Limiter.Enabled {
		output = math.Min(output, toLevel(target.Limiter.Ceiling))
	}
	meter.OutputPeak = output
	meter.RMS = output * 0.4
}

type savedPreset struct {
	summary core.PresetSummary
	targets []core.Target
}

type mockMixer struct {
	config    core.Config
	engine    core.EngineStatus
	routing   core.RoutingStatus
	selection core.SelectionStatus
	// Global processing, independent of which paths are selected.
	processing bool
	settleAt   *time.Time
	// liveTargets is what the audio plays when a live edit differs from the
	// saved settings, nil otherwise.
	liveTargets []core.Target
	presets     []savedPreset
	nextPreset  uint64
	// sshFingerprint is the managed developer key's, nil while SSH is off.
	sshFingerprint *string
	// State changes waiting for the next event poll.
	events           []json.RawMessage
	log              []core.DiagnosticsEvent
	nextSeq          uint64
	diagnosticsEpoch uint64
	started          time.Time
	lastMeter        time.Time
	lastUSB          *time.Time
	lastXrun         time.Time
	// Frames since the last diagnostics reset; every counter derives from it.
	frames uint64
	xruns  uint64
}

// since is now - earlier, or zero when earlier is later.
func since(now, earlier time.Time) time.Duration {
	if now.Before(earlier) {
		return 0
	}
	return now.Sub(earlier)
}

// orEmpty keeps an empty list a list: a nil slice would encode as null.
func orEmpty(ids []string) []string {
	if ids == nil {
		return []string{}
	}
	return ids
}

func newMockMixer(now time.Time) *mockMixer {
	m := &mockMixer{
		engine: core.EngineStatus{
			Generation:      1,
			RestartAllowed:  false,
			Mode:            core.EngineModeNative,
			Status:          core.EnginePhaseStarting,
			AppliedRevision: 0,
			SampleRate:      uint32(core.SampleRate),
			BlockSize:       blockSize,
			Message:         "",
			AudioAdvancing:  false,
		},
		routing: core.RoutingStatus{
			Status:    core.RoutingPhaseUnmanaged,
			Channels:  0,
			Recovered: 0,
			Message:   "",
		},
		selection: core.SelectionStatus{
			ChannelCapacity:    nil,
			Status:             core.SelectionPhaseIdle,
			Allowed:            false,
			QualifiedTargetIDs: []string{},
			ActiveTargetIDs:    []string{},
			PendingTargetIDs:   []string{},
			Message:            "",
		},
		config:     demoConfig(),
		processing: true,
		nextPreset: 1,
		nextSeq:    1,
		started:    now,
		lastMeter:  now,
		lastXrun:   now,
	}
	m.settle()
	return m
}

// connected starts a new client's event stream from the current state.
func (m *mockMixer) connected() {
	m.events = nil
	m.publish()
}

func (m *mockMixer) state() core.DeviceState {
	canonical := func(ids []string) []string {
		selected, err := m.config.CanonicalSelection(ids)
		if err != nil {
			return []string{}
		}
		return orEmpty(selected)
	}
	selection := m.selection
	// Saved effects can reorder the catalog; these lists follow it.
	selection.QualifiedTargetIDs = make([]string, 0, len(m.config.Targets))
	for _, target := range m.config.Targets {
		selection.QualifiedTargetIDs = append(selection.QualifiedTargetIDs, target.ID)
	}
	selection.ActiveTargetIDs = canonical(selection.ActiveTargetIDs)
	selection.PendingTargetIDs = canonical(selection.PendingTargetIDs)
	return core.DeviceState{
		Config:    m.config,
		Engine:    m.engine,
		Routing:   m.routing,
		Selection: selection,
	}
}

func (m *mockMixer) publish() {
	data, err := marshal(m.state())
	if err != nil {
		panic("state serializes")
	}
	m.events = append(m.events, object(member{"event", jsonString("state")}, member{"data", data}))
}

// beginChange: a change was accepted; audio follows after settleTime.
func (m *mockMixer) beginChange(now time.Time) {
	m.engine.Status = core.EnginePhaseStarting
	m.engine.AudioAdvancing = false
	m.engine.AppliedRevision = 0
	m.engine.Message = "Settings saved; waiting for native audio acknowledgement"
	m.selection.Allowed = false
	m.settleAt = new(now.Add(settleTime))
}

// settle: the engine has caught up with the saved settings.
func (m *mockMixer) settle() {
	m.settleAt = nil
	m.engine.Status = core.EnginePhaseRunning
	m.engine.AudioAdvancing = true
	m.engine.AppliedRevision = m.config.Revision
	m.engine.Message = "Mock mixer: simulated audio"
	m.selection.Status = core.SelectionPhaseIdle
	m.selection.Allowed = true
	m.selection.PendingTargetIDs = []string{}
	if m.processing {
		m.selection.ActiveTargetIDs = slices.Clone(orEmpty(m.config.SelectedTargets))
		m.routing.Status = core.RoutingPhaseActive
		m.routing.Channels = m.config.SelectedChannels()
	} else {
		m.selection.ActiveTargetIDs = []string{}
		m.routing.Status = core.RoutingPhaseBypassed
		m.routing.Channels = 0
	}
}

func (m *mockMixer) record(kind core.DiagnosticsKind, message string, details core.EventDetails, now time.Time) {
	m.log = append(m.log, core.DiagnosticsEvent{
		Seq:     m.nextSeq,
		Kind:    kind,
		Path:    nil,
		AtNs:    m.nowNs(now),
		Message: message,
		Details: details,
	})
	m.nextSeq++
}

func (m *mockMixer) nowNs(now time.Time) uint64 {
	return uint64(since(now, m.started).Nanoseconds())
}

// advance catches up on everything that happens with time alone, on each
// request.
func (m *mockMixer) advance(now time.Time) {
	if m.settleAt != nil && !now.Before(*m.settleAt) {
		m.settle()
		m.publish()
	}
	if m.engine.Status == core.EnginePhaseRunning && since(now, m.lastXrun) >= xrunPeriod {
		m.lastXrun = now
		m.xruns++
		details := core.EventDetails{DelayedUs: new(uint64(180))}
		m.record(core.DiagnosticsKindXrun, "Simulated xrun", details, now)
	}
}

func (m *mockMixer) handle(request *usbproto.Request, now time.Time) usbproto.Reply {
	m.advance(now)
	body := request.Body
	path := request.Path
	var result *refused
	var reply usbproto.Reply
	switch method := request.Method; {
	case method == "GET" && path == "api/v1/events":
		reply = mockReply(200, m.nextEvent(now))
	case method == "GET" && path == "api/v1/state":
		reply = mockReply(200, m.state())
	case method == "GET" && path == "api/v1/version":
		reply = mockReply(200, versions())
	case method == "GET" && path == "api/v1/inputs":
		reply = mockReply(200, inventory())
	case method == "GET" && path == "api/v1/diagnostics":
		state, err := marshal(m.state())
		if err != nil {
			panic("state serializes")
		}
		reply = usbproto.Reply{Status: 200, Body: object(member{"mock", jsonBool(true)}, member{"state", state})}
	case method == "POST" && path == "api/v1/diagnostics/reset":
		reply = m.resetDiagnostics(now)
	case method == "GET" && strings.HasPrefix(path, "api/v1/diagnostics/events"):
		after := uint64(0)
		if _, text, found := strings.Cut(path, "after="); found {
			// The mixer's u64 parse also takes a leading plus.
			if parsed, err := strconv.ParseUint(strings.TrimPrefix(text, "+"), 10, 64); err == nil {
				after = parsed
			}
		}
		reply = mockReply(200, m.eventsPage(after, now))
	case method == "PUT" && path == "api/v1/config":
		var update core.ConfigUpdate
		if result = parseBody(body, &update); result == nil {
			reply, result = m.save(update, now)
		}
	case method == "PUT" && path == "api/v1/live":
		var update core.ConfigUpdate
		if result = parseBody(body, &update); result == nil {
			reply, result = m.applyLive(update)
		}
	case method == "PUT" && path == "api/v1/routing":
		reply, result = m.setRouting(body, now)
	case method == "PUT" && path == "api/v1/selection":
		reply, result = m.selectTargets(body, now)
	case method == "POST" && path == "api/v1/engine/restart":
		result = &refused{mockFailure(422, "Native per-path updates do not restart the vendor audio engine")}
	case method == "GET" && path == "api/v1/presets":
		presets := make([]core.PresetSummary, 0, len(m.presets))
		for _, preset := range m.presets {
			presets = append(presets, preset.summary)
		}
		list, err := marshal(presets)
		if err != nil {
			panic("mock replies serialize")
		}
		reply = usbproto.Reply{Status: 200, Body: object(member{"presets", list})}
	case method == "POST" && path == "api/v1/presets":
		reply, result = m.createPreset(body)
	case method == "POST" && strings.HasSuffix(path, "/apply"):
		reply, result = m.applyPreset(path, body, now)
	case method == "GET" && path == "api/v1/ssh":
		reply = mockReply(200, m.sshStatus())
	case method == "PUT" && path == "api/v1/ssh":
		reply, result = m.setSSH(body)
	case method == "DELETE":
		reply, result = m.deletePreset(path)
	default:
		result = &refused{mockFailure(404, "Unknown endpoint")}
	}
	if result != nil {
		return result.reply
	}
	return reply
}

func (m *mockMixer) checkRevision(revision uint64) *refused {
	if revision == m.config.Revision {
		return nil
	}
	return &refused{mockFailure(409, "revision conflict")}
}

// checkTransition is the checks every processing transition shares.
func (m *mockMixer) checkTransition(revision, generation uint64) *refused {
	if failed := m.checkRevision(revision); failed != nil {
		return failed
	}
	if generation != m.engine.Generation || !m.selection.Allowed {
		return &refused{mockFailure(409, "a transition is already pending")}
	}
	return nil
}

func (m *mockMixer) save(update core.ConfigUpdate, now time.Time) (usbproto.Reply, *refused) {
	if failed := m.checkRevision(update.ExpectedRevision); failed != nil {
		return usbproto.Reply{}, failed
	}
	if err := m.config.ValidateUpdate(update.Targets); err != nil {
		return usbproto.Reply{}, &refused{mockFailure(400, fmt.Sprintf("invalid request: %s", err))}
	}
	// Saving what a live edit already plays changes no audio, so the engine
	// stays running, as the mixer's does.
	alreadyRunning := m.engine.Status == core.EnginePhaseRunning && core.TargetsEqual(m.running(), update.Targets)
	m.config.Revision++
	m.config.Targets = update.Targets
	m.liveTargets = nil
	if alreadyRunning {
		m.engine.AppliedRevision = m.config.Revision
	} else {
		m.beginChange(now)
	}
	m.publish()
	return mockReply(200, m.state()), nil
}

// running is what the audio plays: the live edit, or else the saved settings.
func (m *mockMixer) running() []core.Target {
	if m.liveTargets != nil {
		return m.liveTargets
	}
	return m.config.Targets
}

func (m *mockMixer) sshStatus() core.SSHStatus {
	return core.SSHStatus{
		Managed:            m.sshFingerprint != nil,
		ManagedFingerprint: m.sshFingerprint,
		HostKeys:           []string{},
		ServerRunning:      new(true),
	}
}

// setSSH is PUT api/v1/ssh: it remembers the key's fingerprint and nothing
// else, since no SSH server exists.
func (m *mockMixer) setSSH(body json.RawMessage) (usbproto.Reply, *refused) {
	var request core.SSHRequest
	if failed := parseBody(body, &request); failed != nil {
		return usbproto.Reply{}, failed
	}
	if !request.Enabled {
		m.sshFingerprint = nil
		return mockReply(200, m.sshStatus()), nil
	}
	if request.PublicKey == nil {
		return usbproto.Reply{}, &refused{mockFailure(400, "publicKey is required to enable SSH access")}
	}
	key, err := core.ParseEd25519PublicKey(*request.PublicKey)
	if err != nil {
		return usbproto.Reply{}, &refused{mockFailure(400, err.Error())}
	}
	m.sshFingerprint = new(key.Fingerprint())
	return mockReply(200, m.sshStatus()), nil
}

// applyLive is PUT api/v1/live: the targets go on the running audio at
// once, unless they need the engine restarted. The app saves them once edits
// stop; the mixer keeps what plays if the app goes away first (not modeled
// here).
func (m *mockMixer) applyLive(update core.ConfigUpdate) (usbproto.Reply, *refused) {
	if failed := m.checkRevision(update.ExpectedRevision); failed != nil {
		return usbproto.Reply{}, failed
	}
	merged, err := m.config.ResolveLiveUpdate(update.Targets)
	if err != nil {
		return usbproto.Reply{}, &refused{mockFailure(400, fmt.Sprintf("invalid request: %s", err))}
	}
	if m.engine.Status != core.EnginePhaseRunning {
		return usbproto.Reply{}, &refused{mockFailure(503, "the engine is not running")}
	}
	if core.LiveNeedsSave(m.running(), merged) {
		return usbproto.Reply{}, &refused{mockFailure(422, "liveRequiresSave: this change needs a save")}
	}
	if core.TargetsEqual(m.config.Targets, merged) {
		m.liveTargets = nil
	} else {
		m.liveTargets = core.CloneTargets(merged)
	}
	return mockReply(200, object(member{"accepted", json.RawMessage("true")},
		member{"baseRevision", json.RawMessage(strconv.FormatUint(m.config.Revision, 10))})), nil
}

func (m *mockMixer) setRouting(body json.RawMessage, now time.Time) (usbproto.Reply, *refused) {
	var request core.RoutingRequest
	if failed := parseBody(body, &request); failed != nil {
		return usbproto.Reply{}, failed
	}
	if failed := m.checkTransition(request.ExpectedRevision, request.ExpectedGeneration); failed != nil {
		return usbproto.Reply{}, failed
	}
	m.processing = request.Enabled
	m.beginChange(now)
	if request.Enabled {
		m.routing.Status = core.RoutingPhaseStarting
	} else {
		m.routing.Status = core.RoutingPhaseBypassing
	}
	m.routing.Channels = 0
	m.publish()
	return mockReply(202, m.state()), nil
}

func (m *mockMixer) selectTargets(body json.RawMessage, now time.Time) (usbproto.Reply, *refused) {
	var request core.SelectionRequest
	if failed := parseBody(body, &request); failed != nil {
		return usbproto.Reply{}, failed
	}
	if failed := m.checkTransition(request.ExpectedRevision, request.ExpectedGeneration); failed != nil {
		return usbproto.Reply{}, failed
	}
	ids, err := m.config.CanonicalSelection(request.TargetIDs)
	if err != nil {
		return usbproto.Reply{}, &refused{mockFailure(400, fmt.Sprintf("invalid request: %s", err))}
	}
	ids = orEmpty(ids)
	m.config.Revision++
	m.config.SelectedTargets = slices.Clone(ids)
	m.beginChange(now)
	m.selection.Status = core.SelectionPhaseChanging
	m.selection.PendingTargetIDs = ids
	if m.processing {
		m.routing.Status = core.RoutingPhaseStarting
	} else {
		m.routing.Status = core.RoutingPhaseBypassing
	}
	m.routing.Channels = 0
	m.publish()
	return mockReply(202, m.state()), nil
}

func (m *mockMixer) createPreset(body json.RawMessage) (usbproto.Reply, *refused) {
	revision, failed := expectedRevision(body)
	if failed != nil {
		return usbproto.Reply{}, failed
	}
	if failed := m.checkRevision(revision); failed != nil {
		return usbproto.Reply{}, failed
	}
	name, ok := stringField(body, "name")
	if !ok {
		return usbproto.Reply{}, &refused{mockFailure(400, "name is required")}
	}
	summary := core.PresetSummary{
		ID:   fmt.Sprintf("preset-%d", m.nextPreset),
		Name: name,
	}
	m.nextPreset++
	m.presets = append(m.presets, savedPreset{summary, core.CloneTargets(m.config.Targets)})
	return mockReply(201, summary), nil
}

func (m *mockMixer) applyPreset(path string, body json.RawMessage, now time.Time) (usbproto.Reply, *refused) {
	id, ok := strings.CutPrefix(path, "api/v1/presets/")
	if ok {
		id, ok = strings.CutSuffix(id, "/apply")
	}
	index := slices.IndexFunc(m.presets, func(preset savedPreset) bool { return ok && preset.summary.ID == id })
	if index < 0 {
		return usbproto.Reply{}, &refused{mockFailure(404, "not found")}
	}
	revision, failed := expectedRevision(body)
	if failed != nil {
		return usbproto.Reply{}, failed
	}
	update := core.ConfigUpdate{
		ExpectedRevision: revision,
		Targets:          core.CloneTargets(m.presets[index].targets),
	}
	return m.save(update, now)
}

func (m *mockMixer) deletePreset(path string) (usbproto.Reply, *refused) {
	id, ok := strings.CutPrefix(path, "api/v1/presets/")
	if !ok {
		return usbproto.Reply{}, &refused{mockFailure(404, "Unknown endpoint")}
	}
	index := slices.IndexFunc(m.presets, func(preset savedPreset) bool { return preset.summary.ID == id })
	if index < 0 {
		return usbproto.Reply{}, &refused{mockFailure(404, "not found")}
	}
	m.presets = slices.Delete(m.presets, index, index+1)
	return usbproto.Reply{Status: 200, Body: object(member{"deleted", jsonString(id)})}, nil
}

func (m *mockMixer) resetDiagnostics(now time.Time) usbproto.Reply {
	m.diagnosticsEpoch++
	m.frames = 0
	m.xruns = 0
	m.log = nil
	m.record(core.DiagnosticsKindReset, "Diagnostics reset", core.EventDetails{}, now)
	return mockReply(200, m.state())
}

func (m *mockMixer) eventsPage(after uint64, now time.Time) core.DiagnosticsEvents {
	events := []core.DiagnosticsEvent{}
	for _, event := range m.log {
		if len(events) == core.EventsPage {
			break
		}
		if event.Seq > after {
			events = append(events, event)
		}
	}
	nextSeq := max(m.nextSeq, after)
	more := false
	if len(events) > 0 {
		last := events[len(events)-1]
		nextSeq = last.Seq + 1
		more = last.Seq+1 < m.nextSeq
	}
	return core.DiagnosticsEvents{
		NowNs:   m.nowNs(now),
		NextSeq: nextSeq,
		Lost:    0,
		More:    more,
		Events:  events,
	}
}

// nextEvent is state changes first, then whichever periodic report is due.
func (m *mockMixer) nextEvent(now time.Time) json.RawMessage {
	if len(m.events) > 0 {
		event := m.events[0]
		m.events = m.events[1:]
		return event
	}
	if m.lastUSB == nil || since(now, *m.lastUSB) >= usbPeriod {
		m.lastUSB = new(now)
		usb, err := marshal(core.USBStatus{USB1: core.USBLinkStateConfigured})
		if err != nil {
			panic("mock replies serialize")
		}
		return object(member{"event", jsonString("usb")}, member{"data", usb})
	}
	elapsed := since(now, m.lastMeter)
	if m.engine.Status == core.EnginePhaseRunning && elapsed >= meterPeriod {
		m.lastMeter = now
		m.frames += uint64(elapsed.Seconds() * core.SampleRate)
		meter, err := marshal(m.meter(now))
		if err != nil {
			panic("mock replies serialize")
		}
		return object(member{"event", jsonString("meter")}, member{"data", meter})
	}
	return json.RawMessage("null")
}

func (m *mockMixer) meter(now time.Time) core.Meter {
	seconds := since(now, m.started).Seconds()
	targets := make([]core.TargetMeter, 0, len(m.config.Targets))
	for i := range m.config.Targets {
		targets = append(targets, m.targetMeter(&m.config.Targets[i], seconds))
	}
	return core.Meter{
		MessageType:      core.MeterTypeMeter,
		Revision:         m.engine.AppliedRevision,
		Generation:       m.engine.Generation,
		Synthetic:        true,
		DiagnosticsEpoch: m.diagnosticsEpoch,
		// The mock has no engine-generation clock of its own; a reader
		// falls back to counting frames.
		RunningForNs: nil,
		Targets:      targets,
	}
}

func (m *mockMixer) targetMeter(target *core.Target, seconds float64) core.TargetMeter {
	active := slices.Contains(m.selection.ActiveTargetIDs, target.ID)
	suppressing := active && target.Suppression.Enabled
	input, speaking := signal(target, seconds)
	blocks := uint64(blockSize)
	suppressionFrames := uint64(0)
	if suppressing {
		suppressionFrames = m.frames / uint64(core.DFNHopSamples)
	}
	latency := uint32(0)
	if suppressing {
		if geometry, ok := target.Suppression.Geometry(); ok {
			latency = geometry.LatencySamples()
		}
	}
	var performance *core.Performance
	if active {
		performance = new(m.performance(suppressionFrames))
	}
	meter := core.TargetMeter{
		ID:                target.ID,
		InputPeak:         input,
		OutputPeak:        input,
		RMS:               input * 0.4,
		GainReduction:     0.0,
		Voice:             0.0,
		GateOpen:          0.0,
		Clipped:           0,
		Spectrum:          []float64{},
		Callbacks:         m.frames / blocks,
		ProcessedFrames:   m.frames,
		SuppressionFrames: suppressionFrames,
		MissedDeadlines:   0,
		StaleResults:      0,
		InputDrops:        0,
		OutputDrops:       0,
		LatencySamples:    latency,
		WorkerFailed:      false,
		Performance:       performance,
	}
	if active && !target.Bypass {
		process(target, input, speaking, &meter)
	}
	return meter
}

func (m *mockMixer) performance(workerFrames uint64) core.Performance {
	timing := func(calls, meanNs, maxNs uint64) core.Timing {
		if calls == 0 {
			maxNs = 0
		}
		return core.Timing{
			Calls:       calls,
			TotalNs:     calls * meanNs,
			MaxNs:       maxNs,
			Overruns:    0,
			ClockErrors: 0,
			Histogram:   [8]uint64{calls, 0, 0, 0, 0, 0, 0, 0},
		}
	}
	// This mixer's peak is constant, so its recent window carries the same
	// value; nothing timed is absent, never a zero cost.
	callbacks := m.frames / uint64(blockSize)
	var callbackRecent, workerRecent *uint64
	if callbacks > 0 {
		callbackRecent = new(uint64(910_000))
	}
	if workerFrames > 0 {
		workerRecent = new(uint64(3_800_000))
	}
	return core.Performance{
		CallbackBudgetNs:    2_333_333,
		WorkerBudgetNs:      9_333_333,
		Xruns:               m.xruns,
		Callback:            timing(callbacks, 420_000, 910_000),
		WorkerWall:          timing(workerFrames, 2_100_000, 3_800_000),
		WorkerCPU:           timing(workerFrames, 1_900_000, 3_400_000),
		HelperLate:          new(uint64(0)),
		HelperStalls:        new(uint64(0)),
		CallbackRecentMaxNs: callbackRecent,
		WorkerRecentMaxNs:   workerRecent,
	}
}

func versions() core.DeviceVersions {
	return core.DeviceVersions{
		Runtime:         new(mockRuntime),
		Protocol:        new(uint32(core.MixerProtocolVersion)),
		NativeProtocol:  nil,
		Firmware:        new(mockFirmware),
		FirmwareProfile: new(mockFirmwareProfile),
		InPlaceUpdates:  nil,
	}
}

func inventory() core.StockInventory {
	inputs := make([]core.StockSource, 0, len(show))
	for i, fader := range show {
		inputs = append(inputs, core.StockSource{
			ID:       fmt.Sprintf("input-%d", fader.sourceID),
			SourceID: fader.sourceID,
			Fader:    new(uint8(i + 1)),
			Name:     fader.name,
			Color:    fader.color,
			TargetID: fader.target,
		})
	}
	return core.StockInventory{
		Available: true,
		ShowName:  "Mock Show",
		Revision:  strings.Repeat("0", 64),
		Message:   "",
		Inputs:    inputs,
	}
}
