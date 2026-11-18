package core

import (
	"math"
	"slices"
)

// Desired settings, local edits and observed applied state are kept separate.

const maxUint64 = math.MaxUint64

// optU64 is an Option<u64>.
type optU64 struct {
	value uint64
	ok    bool
}

func someU64(value uint64) optU64 { return optU64{value: value, ok: true} }

// is: Some(value) == Some(other).
func (o optU64) is(other uint64) bool { return o.ok && o.value == other }

// checkedAddOne is value.checked_add(1).
func checkedAddOne(value uint64) optU64 {
	if value == maxUint64 {
		return optU64{}
	}
	return someU64(value + 1)
}

// EngineMode is serde `rename_all = "lowercase"`.
type EngineMode string

const (
	EngineModeSimulation EngineMode = "simulation"
	EngineModeJack       EngineMode = "jack"
	EngineModeNative     EngineMode = "native"
	EngineModeOffline    EngineMode = "offline"
)

var engineModes = []EngineMode{EngineModeSimulation, EngineModeJack, EngineModeNative, EngineModeOffline}

func (m *EngineMode) UnmarshalText(text []byte) error { return enumUnmarshal(m, text, engineModes) }

func (m EngineMode) HasProcessingControls() bool {
	return m == EngineModeJack || m == EngineModeNative
}

// EnginePhase is serde `rename_all = "lowercase"`.
type EnginePhase string

const (
	EnginePhaseStarting   EnginePhase = "starting"
	EnginePhaseRestarting EnginePhase = "restarting"
	EnginePhaseRunning    EnginePhase = "running"
	EnginePhaseOffline    EnginePhase = "offline"
	EnginePhaseError      EnginePhase = "error"
)

var enginePhases = []EnginePhase{
	EnginePhaseStarting, EnginePhaseRestarting, EnginePhaseRunning, EnginePhaseOffline, EnginePhaseError,
}

func (p *EnginePhase) UnmarshalText(text []byte) error { return enumUnmarshal(p, text, enginePhases) }

type EngineStatus struct {
	Generation      uint64      `json:"generation"`
	RestartAllowed  bool        `json:"restartAllowed"`
	Mode            EngineMode  `json:"mode"`
	Status          EnginePhase `json:"status"`
	AppliedRevision uint64      `json:"appliedRevision"`
	SampleRate      uint32      `json:"sampleRate"`
	BlockSize       uint32      `json:"blockSize"`
	Message         string      `json:"message"`
	AudioAdvancing  bool        `json:"audioAdvancing"`
}

func (e EngineStatus) Validate() error {
	if !ValidDisplayText(e.Message, MaxStatusText) {
		return CoreError("Invalid engine status message")
	}
	return nil
}

func (e EngineStatus) RestartEligible() bool {
	return e.RestartAllowed && e.Mode == EngineModeJack && e.Status == EnginePhaseError
}

type EngineRestartRequest struct {
	ExpectedRevision   uint64 `json:"expectedRevision"`
	ExpectedGeneration uint64 `json:"expectedGeneration"`
}

// RoutingRequest is the global processing enable/disable. Generation checks
// distinguish this from per-channel effect bypass and reject requests from a
// retired audio owner.
type RoutingRequest struct {
	Enabled            bool   `json:"enabled"`
	ExpectedRevision   uint64 `json:"expectedRevision"`
	ExpectedGeneration uint64 `json:"expectedGeneration"`
}

type routingRuntime struct {
	engine    EngineStatus
	routing   RoutingStatus
	selection SelectionStatus
}

type PendingRouting struct {
	Request RoutingRequest
	config  Config
	runtime routingRuntime
}

func (p PendingRouting) ValidateAcknowledgement(state DeviceState) error {
	if err := state.Validate(); err != nil {
		return err
	}
	phase := RoutingPhaseBypassing
	if p.Request.Enabled {
		phase = RoutingPhaseStarting
	}
	native := p.runtime.engine.Mode == EngineModeNative
	expectedGeneration := someU64(p.Request.ExpectedGeneration)
	expectedStatus := EnginePhaseStarting
	if !native {
		expectedGeneration = checkedAddOne(p.Request.ExpectedGeneration)
		expectedStatus = EnginePhaseRestarting
	}
	if !state.Config.Equal(p.config) ||
		!expectedGeneration.is(state.Engine.Generation) ||
		state.Engine.Mode != p.runtime.engine.Mode ||
		state.Engine.Status != expectedStatus ||
		state.Engine.RestartAllowed ||
		state.Engine.AudioAdvancing ||
		state.Engine.AppliedRevision != 0 ||
		state.Routing.Status != phase ||
		state.Selection.Status != SelectionPhaseIdle ||
		len(state.Selection.PendingTargetIDs) != 0 {
		return CoreError("Processing response does not match the requested transition and saved settings")
	}
	return nil
}

type PendingRestart struct {
	Request EngineRestartRequest
	config  Config
}

func (p PendingRestart) ValidateAcknowledgement(state DeviceState) error {
	if err := state.Validate(); err != nil {
		return err
	}
	if !state.Config.Equal(p.config) ||
		!checkedAddOne(p.Request.ExpectedGeneration).is(state.Engine.Generation) ||
		state.Engine.Mode != EngineModeJack ||
		state.Engine.Status != EnginePhaseRestarting ||
		state.Engine.RestartAllowed ||
		state.Engine.AudioAdvancing {
		return CoreError("Restart response does not match the requested engine and saved settings")
	}
	return nil
}

type DeviceState struct {
	Config    Config          `json:"config"`
	Engine    EngineStatus    `json:"engine"`
	Routing   RoutingStatus   `json:"routing"`
	Selection SelectionStatus `json:"selection"`
}

// RoutingPhase is serde `rename_all = "lowercase"`.
type RoutingPhase string

const (
	RoutingPhaseUnmanaged RoutingPhase = "unmanaged"
	RoutingPhaseStarting  RoutingPhase = "starting"
	RoutingPhaseActive    RoutingPhase = "active"
	RoutingPhaseBypassing RoutingPhase = "bypassing"
	RoutingPhaseBypassed  RoutingPhase = "bypassed"
	RoutingPhaseError     RoutingPhase = "error"
)

var routingPhases = []RoutingPhase{
	RoutingPhaseUnmanaged, RoutingPhaseStarting, RoutingPhaseActive,
	RoutingPhaseBypassing, RoutingPhaseBypassed, RoutingPhaseError,
}

func (p *RoutingPhase) UnmarshalText(text []byte) error { return enumUnmarshal(p, text, routingPhases) }

type RoutingStatus struct {
	Status    RoutingPhase `json:"status"`
	Channels  uint32       `json:"channels"`
	Recovered uint32       `json:"recovered"`
	Message   string       `json:"message"`
}

func (r RoutingStatus) Validate() error {
	if !ValidDisplayText(r.Message, MaxStatusText) {
		return CoreError("Invalid routing status message")
	}
	return nil
}

type ConfigUpdate struct {
	ExpectedRevision uint64   `json:"expectedRevision"`
	Targets          []Target `json:"targets"`
}

type PresetSummary struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

type TargetMeter struct {
	ID            string  `json:"id"`
	InputPeak     float64 `json:"inputPeak"`
	OutputPeak    float64 `json:"outputPeak"`
	RMS           float64 `json:"rms"`
	GainReduction float64 `json:"gainReduction"`
	// Highest suppression-model voice score in the window, 0 to 1 (0 without
	// suppression).
	Voice float64 `json:"voice"`
	// How far open the voice gate was at its most open in the window, 0
	// (fully closed) to 1. 1 while the gate is off; 0 without suppression.
	GateOpen          float64      `json:"gateOpen"`
	Clipped           uint64       `json:"clipped"`
	Spectrum          []float64    `json:"spectrum"`
	Callbacks         uint64       `json:"callbacks"`
	ProcessedFrames   uint64       `json:"processedFrames"`
	SuppressionFrames uint64       `json:"suppressionFrames"`
	MissedDeadlines   uint64       `json:"missedDeadlines"`
	StaleResults      uint64       `json:"staleResults"`
	InputDrops        uint64       `json:"inputDrops"`
	OutputDrops       uint64       `json:"outputDrops"`
	LatencySamples    uint32       `json:"latencySamples"`
	WorkerFailed      bool         `json:"workerFailed"`
	Performance       *Performance `json:"performance"`
}

type Timing struct {
	Calls       uint64    `json:"calls"`
	TotalNs     uint64    `json:"totalNs"`
	MaxNs       uint64    `json:"maxNs"`
	Overruns    uint64    `json:"overruns"`
	ClockErrors uint64    `json:"clockErrors"`
	Histogram   [8]uint64 `json:"histogram"`
}

func (t Timing) MeanMs() float64 {
	if t.Calls == 0 {
		return 0.
	}
	return float64(t.TotalNs) / float64(t.Calls) / 1_000_000.
}

// Performance accepts unknown fields: a newer mixer may report more.
type Performance struct {
	CallbackBudgetNs uint64 `json:"callbackBudgetNs"`
	WorkerBudgetNs   uint64 `json:"workerBudgetNs"`
	Xruns            uint64 `json:"xruns"`
	// `calls == 0` means callback timing was not sampled in this build, not
	// that callbacks take no time.
	Callback Timing `json:"callback"`
	// The suppression worker's wall and CPU time; zeros when the path has no
	// worker. Cumulative for the engine generation, across worker rebuilds.
	WorkerWall Timing `json:"workerWall"`
	WorkerCPU  Timing `json:"workerCpu"`
	// Helper routines that finished after their frame's cycle had ended, and
	// callbacks that found the helper still busy. Absent from a mixer that
	// predates them (and from the JACK path).
	HelperLate   *uint64 `json:"helperLate,omitempty"`
	HelperStalls *uint64 `json:"helperStalls,omitempty"`
	// Slowest audio callback, and slowest suppression worker turnaround
	// (wall time), within roughly the last 10 s of audio. Absent from a
	// mixer that predates them, or before any timing in the window.
	CallbackRecentMaxNs *uint64 `json:"callbackRecentMaxNs,omitempty"`
	WorkerRecentMaxNs   *uint64 `json:"workerRecentMaxNs,omitempty"`
}

func (Performance) AllowUnknownJSONFields() {}

// Meter accepts unknown fields: a newer mixer may report more.
type Meter struct {
	MessageType MeterType `json:"type"`
	Revision    uint64    `json:"revision"`
	Generation  uint64    `json:"generation"`
	Synthetic   bool      `json:"synthetic"`
	// Bumped by each diagnostics reset. The counters below are relative to
	// that reset, so a reader rebaselines its fault tracking when this
	// changes. 0 means "never reset" (and is what an older mixer sends).
	DiagnosticsEpoch uint64 `json:"diagnosticsEpoch,default"`
	// How long this engine generation has been running when the reading
	// was made, on the daemon's monotonic clock: from the start of the audio
	// process that runs the engine (the mixer's own process), or from the
	// moment it was first reported Running when that start cannot be read.
	// Lets a reader place the startup window. Absent from older mixers.
	RunningForNs *uint64       `json:"runningForNs,omitempty"`
	Targets      []TargetMeter `json:"targets"`
}

func (Meter) AllowUnknownJSONFields() {}

// MeterType is serde `rename_all = "lowercase"`.
type MeterType string

const MeterTypeMeter MeterType = "meter"

func (m *MeterType) UnmarshalText(text []byte) error {
	return enumUnmarshal(m, text, []MeterType{MeterTypeMeter})
}

func (d DeviceState) ProcessingTransitionPending() bool {
	return d.Engine.Status == EnginePhaseRestarting ||
		(d.Engine.Mode == EngineModeNative && d.Engine.Status == EnginePhaseStarting)
}

func (d DeviceState) nativeControlsReady() bool {
	return d.Engine.Mode == EngineModeNative &&
		d.Engine.Status == EnginePhaseRunning &&
		d.Engine.AudioAdvancing &&
		d.Engine.AppliedRevision == d.Config.Revision &&
		!d.Engine.RestartAllowed &&
		d.Selection.Status == SelectionPhaseIdle &&
		d.Selection.Allowed &&
		len(d.Selection.PendingTargetIDs) == 0 &&
		d.Routing.Recovered == 0
}

// CanEnableProcessing: eligibility is a device claim following complete
// stock-only recovery. An offline engine produces no meters; stale meters
// cannot prove readiness.
func (d DeviceState) CanEnableProcessing() bool {
	if d.Engine.Mode == EngineModeNative {
		return d.nativeControlsReady() &&
			d.Routing.Status == RoutingPhaseBypassed &&
			d.Routing.Channels == 0 &&
			len(d.Selection.ActiveTargetIDs) == 0
	}
	return d.Engine.Mode == EngineModeJack &&
		d.Engine.Status == EnginePhaseOffline &&
		d.Engine.RestartAllowed &&
		d.Engine.Generation < 9_007_199_254_740_991 &&
		!d.Engine.AudioAdvancing &&
		d.Routing.Status == RoutingPhaseBypassed &&
		d.Routing.Channels == 0 &&
		d.Routing.Recovered == 0 &&
		len(d.Config.SelectedTargets) != 0 &&
		d.Selection.Status == SelectionPhaseIdle &&
		len(d.Selection.ActiveTargetIDs) == 0 &&
		len(d.Selection.PendingTargetIDs) == 0
}

func (d DeviceState) CanDisableProcessing() bool {
	if d.Engine.Mode == EngineModeNative {
		return (d.nativeControlsReady() && d.Routing.Status == RoutingPhaseActive) ||
			(d.Engine.Status == EnginePhaseError &&
				d.Routing.Status == RoutingPhaseError &&
				d.Selection.Status != SelectionPhaseChanging &&
				len(d.Selection.PendingTargetIDs) == 0)
	}
	return d.Engine.Mode == EngineModeJack &&
		d.Engine.Generation < 9_007_199_254_740_991 &&
		d.Engine.Status == EnginePhaseRunning &&
		d.Routing.Status == RoutingPhaseActive &&
		d.Selection.Status == SelectionPhaseIdle &&
		len(d.Selection.PendingTargetIDs) == 0
}

// DeviceStateFromJSON is strict current-schema parsing.
func DeviceStateFromJSON(text string) (DeviceState, error) {
	state, err := FromJSON[DeviceState](text)
	if err != nil {
		return DeviceState{}, err
	}
	if err := state.Validate(); err != nil {
		return DeviceState{}, err
	}
	return state, nil
}

func (d DeviceState) Validate() error {
	if err := d.Config.Validate(); err != nil {
		return err
	}
	if err := d.Engine.Validate(); err != nil {
		return err
	}
	if err := d.Routing.Validate(); err != nil {
		return err
	}
	return d.Selection.Validate(d.Config)
}

func (d DeviceState) StockSelected() bool {
	if d.Engine.Mode == EngineModeNative {
		return d.nativeControlsReady() &&
			len(d.Config.SelectedTargets) == 0 &&
			len(d.Selection.ActiveTargetIDs) == 0 &&
			(d.Routing.Status == RoutingPhaseActive || d.Routing.Status == RoutingPhaseBypassed) &&
			d.Routing.Channels == 0
	}
	return d.Selection.Status == SelectionPhaseIdle &&
		len(d.Config.SelectedTargets) == 0 &&
		len(d.Selection.ActiveTargetIDs) == 0 &&
		d.Engine.Status == EnginePhaseOffline &&
		!d.Engine.AudioAdvancing &&
		d.Routing.Status == RoutingPhaseBypassed &&
		d.Routing.Channels == 0 &&
		d.Routing.Recovered == 0
}

func (d DeviceState) IsApplied() bool {
	if d.Selection.Status != SelectionPhaseIdle {
		return false
	}
	ids, err := d.Config.CanonicalSelection(d.Config.SelectedTargets)
	if err != nil || !slices.Equal(d.Selection.ActiveTargetIDs, ids) {
		return false
	}
	return len(d.Config.SelectedTargets) != 0 &&
		d.Engine.Status == EnginePhaseRunning &&
		d.Engine.AudioAdvancing &&
		d.Engine.AppliedRevision == d.Config.Revision &&
		(d.Engine.Mode == EngineModeSimulation ||
			(d.Routing.Status == RoutingPhaseActive &&
				d.Routing.Channels == d.Config.SelectedChannels()))
}

// Clone is a deep copy.
func (d DeviceState) Clone() DeviceState {
	d.Config = d.Config.Clone()
	d.Selection = d.Selection.Clone()
	return d
}

// Equal is Rust's derived PartialEq.
func (d DeviceState) Equal(other DeviceState) bool { return deepEqual(d, other) }

func (m TargetMeter) Validate() error {
	invalid := !ValidID(m.ID)
	for _, value := range []float64{m.InputPeak, m.OutputPeak, m.RMS, m.GainReduction} {
		if math.IsNaN(value) || math.IsInf(value, 0) || value < 0.0 {
			invalid = true
		}
	}
	for _, value := range []float64{m.Voice, m.GateOpen} {
		if !(value >= 0.0 && value <= 1.0) {
			invalid = true
		}
	}
	if len(m.Spectrum) > 64 {
		invalid = true
	}
	for _, value := range m.Spectrum {
		if math.IsNaN(value) || math.IsInf(value, 0) {
			invalid = true
		}
	}
	if invalid {
		return CoreError("Invalid meter values")
	}
	return nil
}

// PendingApply is a request snapshot: it prevents a late save response from
// deleting newer edits.
type PendingApply struct {
	Request         ConfigUpdate
	selectedTargets []string
}

type RemoteUpdate uint8

const (
	RemoteUpdateLoaded RemoteUpdate = iota
	RemoteUpdateUnchanged
	RemoteUpdateConflict
	RemoteUpdateStale
)

func (r RemoteUpdate) String() string {
	switch r {
	case RemoteUpdateLoaded:
		return "Loaded"
	case RemoteUpdateUnchanged:
		return "Unchanged"
	case RemoteUpdateConflict:
		return "Conflict"
	case RemoteUpdateStale:
		return "Stale"
	}
	return "RemoteUpdate(?)"
}

// Session is the editor's view of one mixer. Methods that take a
// DeviceState take ownership of it: do not change its slices afterwards.
type Session struct {
	Saved                    *DeviceState
	Draft                    []Target
	SelectionDraft           []string
	Conflict                 bool
	Connected                bool
	baseRevision             optU64
	observedEngineGeneration optU64
}

// DefaultSession is Session::default(): no mixer read yet, the default
// catalog as the draft.
func DefaultSession() Session {
	defaults := DefaultConfig()
	return Session{
		Draft:          defaults.Targets,
		SelectionDraft: defaults.SelectedTargets,
	}
}

func (s *Session) SelectionDirty() bool {
	if s.Saved == nil {
		return len(s.SelectionDraft) != 0
	}
	return !slices.Equal(s.Saved.Config.SelectedTargets, s.SelectionDraft)
}

func (s *Session) AnyDirty() bool { return s.Dirty() || s.SelectionDirty() }

func (s *Session) Dirty() bool {
	if s.Saved == nil {
		return !TargetsEqual(s.Draft, DefaultConfig().Targets)
	}
	return !TargetsEqual(s.Saved.Config.Targets, s.Draft)
}

func (s *Session) IsApplied() bool {
	return s.Connected && !s.AnyDirty() && s.Saved != nil && s.Saved.IsApplied()
}

func (s *Session) Disconnect() {
	s.Connected = false
	s.resetEngineObservation()
}

// resetEngineObservation: a deliberate new connection may address a
// different device sequence.
func (s *Session) resetEngineObservation() { s.observedEngineGeneration = optU64{} }

// observedNewer: the observed generation is Some(g) with generation < g.
func (s *Session) observedNewer(generation uint64) bool {
	return s.observedEngineGeneration.ok && generation < s.observedEngineGeneration.value
}

// unobservedOrOlder: observed generation is None or older than generation.
func (s *Session) unobservedOrOlder(generation uint64) bool {
	return !s.observedEngineGeneration.ok || s.observedEngineGeneration.value < generation
}

func (s *Session) ReceiveEngine(engine EngineStatus) bool {
	if s.observedNewer(engine.Generation) {
		return false
	}
	if s.Saved == nil {
		return false
	}
	s.observedEngineGeneration = someU64(engine.Generation)
	s.Saved.Engine = engine
	return true
}

// ReceiveRuntime observes the live portion of our pending settings write's
// stream echo. The caller identifies the exact request/revision; the
// request's reply will acknowledge its settings. Keep all saved settings and
// local drafts here.
func (s *Session) ReceiveRuntime(state DeviceState) (RemoteUpdate, error) {
	if err := state.Validate(); err != nil {
		return 0, err
	}
	if s.Saved == nil {
		return 0, CoreError("Read device settings before observing runtime")
	}
	saved := s.Saved
	if state.Config.Revision < saved.Config.Revision || s.observedNewer(state.Engine.Generation) {
		return RemoteUpdateStale, nil
	}
	if err := saved.Config.ValidateUpdate(state.Config.Targets); err != nil {
		return 0, err
	}
	if !slices.Equal(state.Config.SelectedTargets, saved.Config.SelectedTargets) {
		return 0, CoreError("A settings echo cannot change processing-path selection")
	}
	s.observedEngineGeneration = someU64(state.Engine.Generation)
	saved.Engine = state.Engine
	saved.Routing = state.Routing
	saved.Selection = state.Selection
	return RemoteUpdateUnchanged, nil
}

func (s *Session) Receive(state DeviceState) (RemoteUpdate, error) {
	if err := state.Validate(); err != nil {
		return 0, err
	}
	if s.observedNewer(state.Engine.Generation) {
		return RemoteUpdateStale, nil
	}
	if s.Saved != nil && state.Config.Revision < s.Saved.Config.Revision {
		return RemoteUpdateStale, nil
	}
	dirty := s.Dirty()
	selectionDirty := s.SelectionDirty()
	topologyOnly := false
	if saved := s.Saved; saved != nil && TargetsEqual(saved.Config.Targets, state.Config.Targets) {
		if !slices.Equal(saved.Config.SelectedTargets, state.Config.SelectedTargets) {
			topologyOnly = true
		} else if saved.Selection.Status == SelectionPhaseChanging &&
			state.Selection.Status == SelectionPhaseIdle &&
			state.Engine.Generation == saved.Engine.Generation &&
			checkedAddOne(saved.Config.Revision).is(state.Config.Revision) {
			ids, err := state.Config.CanonicalSelection(state.Config.SelectedTargets)
			topologyOnly = err == nil && slices.Equal(ids, saved.Selection.PendingTargetIDs)
		}
	}
	s.observedEngineGeneration = someU64(state.Engine.Generation)
	revision := state.Config.Revision
	changed := s.Saved == nil || s.Saved.Config.Revision != revision
	if !selectionDirty {
		s.SelectionDraft = slices.Clone(state.Config.SelectedTargets)
	}
	if dirty && changed && !topologyOnly {
		s.Conflict = true
		s.Saved = &state
		s.Connected = true
		return RemoteUpdateConflict, nil
	}
	if !dirty {
		s.Draft = CloneTargets(state.Config.Targets)
		s.baseRevision = someU64(revision)
		s.Conflict = false
	} else if changed && topologyOnly {
		// Selection commits preserve all effects. Rebase local effect
		// edits without treating that topology-only revision as a conflict.
		s.baseRevision = someU64(revision)
	}
	s.Saved = &state
	s.Connected = true
	if changed {
		return RemoteUpdateLoaded, nil
	}
	return RemoteUpdateUnchanged, nil
}

// ReceiveSnapshot: a request's snapshot can predate a later stream event at
// the same engine generation. Call only while the stream's runtime
// observation is fresh.
func (s *Session) ReceiveSnapshot(state DeviceState) (RemoteUpdate, error) {
	if saved := s.Saved; saved != nil && s.observedEngineGeneration.is(state.Engine.Generation) {
		state.Engine = saved.Engine
		state.Routing = saved.Routing
		// Do not let an older same-generation reply regress a
		// completed selection after its streamed state has committed.
		if state.Config.Revision == saved.Config.Revision {
			state.Selection = saved.Selection.Clone()
		}
	}
	return s.Receive(state)
}

func (s *Session) ReloadSaved() {
	if saved := s.Saved; saved != nil {
		s.Draft = CloneTargets(saved.Config.Targets)
		s.SelectionDraft = slices.Clone(saved.Config.SelectedTargets)
		s.baseRevision = someU64(saved.Config.Revision)
		s.Conflict = false
	}
}

func (s *Session) BeginApply() (PendingApply, error) {
	if !s.Connected {
		return PendingApply{}, CoreError("Connect to the mixer before applying settings")
	}
	if s.Conflict {
		return PendingApply{}, CoreError("Settings changed on another controller; reload or explicitly rebase your edits")
	}
	saved := s.Saved
	if saved == nil {
		return PendingApply{}, CoreError("Read device settings before applying")
	}
	if saved.ProcessingTransitionPending() || saved.Selection.Status == SelectionPhaseChanging {
		return PendingApply{}, CoreError("Wait for the processing change before applying settings")
	}
	if err := saved.Config.ValidateUpdate(s.Draft); err != nil {
		return PendingApply{}, err
	}
	expected := saved.Config.Revision
	if s.baseRevision.ok {
		expected = s.baseRevision.value
	}
	return PendingApply{
		selectedTargets: slices.Clone(saved.Config.SelectedTargets),
		Request: ConfigUpdate{
			ExpectedRevision: expected,
			Targets:          CloneTargets(s.Draft),
		},
	}, nil
}

func (s *Session) BeginSelection() (PendingSelection, error) {
	if s.Saved == nil || !s.Connected {
		return PendingSelection{}, CoreError("Connect to the mixer before changing processing paths")
	}
	saved := s.Saved
	if !saved.Selection.Allowed ||
		saved.Selection.Status == SelectionPhaseChanging ||
		saved.ProcessingTransitionPending() ||
		saved.Engine.Generation == maxUint64 ||
		saved.Config.Revision == maxUint64 {
		return PendingSelection{}, CoreError("The mixer is not ready to change processing paths")
	}
	targetIDs, err := saved.Config.CanonicalSelection(s.SelectionDraft)
	if err != nil {
		return PendingSelection{}, err
	}
	if _, err := saved.Selection.CheckCapacity(saved.Config, targetIDs); err != nil {
		return PendingSelection{}, err
	}
	current, err := saved.Config.CanonicalSelection(saved.Config.SelectedTargets)
	if err != nil {
		return PendingSelection{}, err
	}
	unqualified := false
	for _, id := range targetIDs {
		if !slices.Contains(saved.Selection.QualifiedTargetIDs, id) {
			unqualified = true
		}
	}
	if (slices.Equal(targetIDs, current) && saved.Selection.Status != SelectionPhaseError) || unqualified {
		return PendingSelection{}, CoreError("Choose a changed selection from the mixer's qualified paths")
	}
	return PendingSelection{
		Request: SelectionRequest{
			ExpectedRevision:   saved.Config.Revision,
			ExpectedGeneration: saved.Engine.Generation,
			TargetIDs:          targetIDs,
		},
		config: saved.Config.Clone(),
		mode:   saved.Engine.Mode,
	}, nil
}

func (s *Session) AcknowledgeSelection(pending PendingSelection, state DeviceState) error {
	if err := pending.ValidateAcknowledgement(state); err != nil {
		return err
	}
	nativeRevision := state.Engine.Mode == EngineModeNative &&
		s.observedEngineGeneration.is(state.Engine.Generation) &&
		s.Saved != nil && s.Saved.Config.Revision < state.Config.Revision
	if nativeRevision || s.unobservedOrOlder(state.Engine.Generation) {
		if _, err := s.Receive(state); err != nil {
			return err
		}
	}
	return nil
}

func (s *Session) BeginRouting(enabled bool) (PendingRouting, error) {
	if s.Saved == nil || !s.Connected {
		return PendingRouting{}, CoreError("Connect to the mixer before changing processing")
	}
	saved := s.Saved
	ready := saved.CanDisableProcessing()
	if enabled {
		ready = saved.CanEnableProcessing()
	}
	if !ready {
		return PendingRouting{}, CoreError("The mixer is not ready for this processing transition")
	}
	if enabled && (s.Dirty() || s.Conflict) {
		return PendingRouting{}, CoreError("Apply or reload effect changes before enabling processing")
	}
	return PendingRouting{
		Request: RoutingRequest{
			Enabled:            enabled,
			ExpectedRevision:   saved.Config.Revision,
			ExpectedGeneration: saved.Engine.Generation,
		},
		config: saved.Config.Clone(),
		runtime: routingRuntime{
			engine:    saved.Engine,
			routing:   saved.Routing,
			selection: saved.Selection.Clone(),
		},
	}, nil
}

func (s *Session) AcknowledgeRouting(pending PendingRouting, state DeviceState) error {
	if err := pending.ValidateAcknowledgement(state); err != nil {
		return err
	}
	nativeTransition := state.Engine.Mode == EngineModeNative &&
		s.observedEngineGeneration.is(state.Engine.Generation) &&
		s.Saved != nil &&
		s.Saved.Config.Equal(pending.config) &&
		s.Saved.Engine == pending.runtime.engine &&
		s.Saved.Routing == pending.runtime.routing &&
		s.Saved.Selection.Equal(pending.runtime.selection)
	if nativeTransition || s.unobservedOrOlder(state.Engine.Generation) {
		if _, err := s.Receive(state); err != nil {
			return err
		}
	}
	return nil
}

func (s *Session) BeginRestart() (PendingRestart, error) {
	if s.Saved == nil || !s.Connected {
		return PendingRestart{}, CoreError("Connect to the mixer before restarting processing")
	}
	saved := s.Saved
	if !saved.Engine.RestartEligible() || saved.Engine.Generation == maxUint64 {
		return PendingRestart{}, CoreError("The mixer is not ready for an engine restart")
	}
	return PendingRestart{
		Request: EngineRestartRequest{
			ExpectedRevision:   saved.Config.Revision,
			ExpectedGeneration: saved.Engine.Generation,
		},
		config: saved.Config.Clone(),
	}, nil
}

// AcknowledgeRestart: the stream may have already reported the result of
// this reservation. Preserve newer runtime state and all local edits in that
// case.
func (s *Session) AcknowledgeRestart(pending PendingRestart, state DeviceState) error {
	if err := pending.ValidateAcknowledgement(state); err != nil {
		return err
	}
	if s.unobservedOrOlder(state.Engine.Generation) {
		if _, err := s.Receive(state); err != nil {
			return err
		}
	}
	return nil
}

// Acknowledge: only call with the successful response belonging to this
// request. The response's targets are what the mixer stored for this save,
// and they are taken as they are: the mixer refuses a save whose expected
// revision is not current, so no other write can be in this response, but
// the stored values may differ from the request's (a mixer before 0.2.2
// parses some numbers one unit in the last place off). Edits made since the
// request was sent are kept.
func (s *Session) Acknowledge(pending PendingApply, state DeviceState) error {
	if err := state.Validate(); err != nil {
		return err
	}
	if state.Config.Revision <= pending.Request.ExpectedRevision ||
		!slices.Equal(state.Config.SelectedTargets, pending.selectedTargets) {
		return CoreError("Save response does not match the requested settings")
	}
	if s.Saved != nil && s.Saved.Config.Revision > state.Config.Revision {
		return CoreError("Save response is older than the known device state")
	}
	s.preserveObservedRuntime(&state)
	if TargetsEqual(s.Draft, pending.Request.Targets) {
		s.Draft = CloneTargets(state.Config.Targets)
	}
	// The device-wide screen setting is not a per-target edit: the
	// acknowledged state is what the mixer now runs.
	s.baseRevision = someU64(state.Config.Revision)
	s.observedEngineGeneration = someU64(state.Engine.Generation)
	s.Saved = &state
	s.Connected = true
	s.Conflict = false
	return nil
}

// AcknowledgePreset accepts a preset response while preserving edits made
// during the request.
func (s *Session) AcknowledgePreset(expectedRevision uint64, originalDraft []Target, state DeviceState) error {
	if err := state.Validate(); err != nil {
		return err
	}
	if !checkedAddOne(expectedRevision).is(state.Config.Revision) ||
		(s.Saved != nil && !slices.Equal(s.Saved.Config.SelectedTargets, state.Config.SelectedTargets)) ||
		(s.Saved != nil && s.Saved.Config.Revision > state.Config.Revision) {
		return CoreError("Preset response does not match the requested revision")
	}
	s.preserveObservedRuntime(&state)
	edited := !TargetsEqual(s.Draft, originalDraft)
	if !edited {
		s.Draft = CloneTargets(state.Config.Targets)
	}
	s.baseRevision = someU64(state.Config.Revision)
	s.observedEngineGeneration = someU64(state.Engine.Generation)
	s.Saved = &state
	s.Connected = true
	// A preset can replace every target. Keep edits made during the request
	// but require an explicit choice before overwriting its result.
	s.Conflict = edited
	return nil
}

// preserveObservedRuntime: a successful effects write confirms its settings,
// not current audio. The ordered stream may already have reported failure,
// recovery or a new generation after the server captured this reply. Call
// only after validating the response and its unchanged processing-path
// selection.
func (s *Session) preserveObservedRuntime(state *DeviceState) {
	if saved := s.Saved; saved != nil &&
		s.observedEngineGeneration.ok && s.observedEngineGeneration.value >= state.Engine.Generation {
		state.Engine = saved.Engine
		state.Routing = saved.Routing
		state.Selection = saved.Selection.Clone()
	}
}

// RebaseEdits is a deliberate user choice: keep local edits against the
// latest revision.
func (s *Session) RebaseEdits() error {
	if s.Saved == nil {
		return CoreError("No device state is available")
	}
	if err := s.Saved.Config.ValidateUpdate(s.Draft); err != nil {
		return err
	}
	s.baseRevision = someU64(s.Saved.Config.Revision)
	s.Conflict = false
	return nil
}
