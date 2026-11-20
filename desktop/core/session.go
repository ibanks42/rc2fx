package core

import (
	"math"
	"slices"
)

const maxUint64 = math.MaxUint64

type optU64 struct {
	value uint64
	ok    bool
}

func someU64(value uint64) optU64 { return optU64{value: value, ok: true} }

func (o optU64) is(other uint64) bool { return o.ok && o.value == other }

func checkedAddOne(value uint64) optU64 {
	if value == maxUint64 {
		return optU64{}
	}
	return someU64(value + 1)
}

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
	return m == EngineModeNative
}

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

// RoutingRequest toggles global processing. ExpectedGeneration rejects
// requests sent against a retired audio owner.
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

type DeviceState struct {
	Config    Config          `json:"config"`
	Engine    EngineStatus    `json:"engine"`
	Routing   RoutingStatus   `json:"routing"`
	Selection SelectionStatus `json:"selection"`
}

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
	// Peak suppression voice score in the window [0, 1] (0 when suppression is off).
	Voice float64 `json:"voice"`
	// Peak gate openness in the window [0, 1] (1 when gate is off, 0 without suppression).
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

// Performance allows unknown fields for forward compatibility with newer mixers.
type Performance struct {
	CallbackBudgetNs uint64 `json:"callbackBudgetNs"`
	WorkerBudgetNs   uint64 `json:"workerBudgetNs"`
	Xruns            uint64 `json:"xruns"`
	// Calls == 0 means timing wasn't sampled, not zero duration.
	Callback Timing `json:"callback"`
	// Cumulative over the engine generation across worker rebuilds.
	WorkerWall Timing `json:"workerWall"`
	WorkerCPU  Timing `json:"workerCpu"`
	// Nil on older mixers and on the JACK path.
	HelperLate   *uint64 `json:"helperLate,omitempty"`
	HelperStalls *uint64 `json:"helperStalls,omitempty"`
	// Slowest callback and worker turnaround over roughly the last 10 s.
	CallbackRecentMaxNs *uint64 `json:"callbackRecentMaxNs,omitempty"`
	WorkerRecentMaxNs   *uint64 `json:"workerRecentMaxNs,omitempty"`
}

func (Performance) AllowUnknownJSONFields() {}

// Meter allows unknown fields for forward compatibility with newer mixers.
type Meter struct {
	MessageType MeterType `json:"type"`
	Revision    uint64    `json:"revision"`
	Generation  uint64    `json:"generation"`
	Synthetic   bool      `json:"synthetic"`
	// Incremented on diagnostics reset so clients rebaseline fault counters (0 on older mixers).
	DiagnosticsEpoch uint64 `json:"diagnosticsEpoch,default"`
	// Engine generation uptime on the daemon's monotonic clock (nil on older mixers).
	RunningForNs *uint64       `json:"runningForNs,omitempty"`
	Targets      []TargetMeter `json:"targets"`
}

func (Meter) AllowUnknownJSONFields() {}

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

// CanEnableProcessing reports whether the native engine is bypassed and ready
// to enable processing.
func (d DeviceState) CanEnableProcessing() bool {
	return d.nativeControlsReady() &&
		d.Routing.Status == RoutingPhaseBypassed &&
		d.Routing.Channels == 0 &&
		len(d.Selection.ActiveTargetIDs) == 0
}

func (d DeviceState) CanDisableProcessing() bool {
	return d.Engine.Mode == EngineModeNative &&
		((d.nativeControlsReady() && d.Routing.Status == RoutingPhaseActive) ||
			(d.Engine.Status == EnginePhaseError &&
				d.Routing.Status == RoutingPhaseError &&
				d.Selection.Status != SelectionPhaseChanging &&
				len(d.Selection.PendingTargetIDs) == 0))
}

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

func (d DeviceState) Clone() DeviceState {
	d.Config = d.Config.Clone()
	d.Selection = d.Selection.Clone()
	return d
}

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

// PendingApply snapshots an in-flight save so a late reply won't clobber newer edits.
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

// Session tracks the editor's view of one mixer. Methods taking a DeviceState
// assume ownership of its slices.
type Session struct {
	Saved                    *DeviceState
	Draft                    []Target
	SelectionDraft           []string
	Conflict                 bool
	Connected                bool
	baseRevision             optU64
	observedEngineGeneration optU64
}

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

func (s *Session) resetEngineObservation() { s.observedEngineGeneration = optU64{} }

func (s *Session) observedNewer(generation uint64) bool {
	return s.observedEngineGeneration.ok && generation < s.observedEngineGeneration.value
}

func (s *Session) unobservedOrOlder(generation uint64) bool {
	return !s.observedEngineGeneration.ok || s.observedEngineGeneration.value < generation
}

// ReceiveRuntime updates only the live runtime fields from a stream echo of
// our own in-flight settings write, leaving saved settings and local drafts
// untouched until the write reply arrives.
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
		// Selection commits leave effect settings intact, so rebase silently.
		s.baseRevision = someU64(revision)
	}
	s.Saved = &state
	s.Connected = true
	if changed {
		return RemoteUpdateLoaded, nil
	}
	return RemoteUpdateUnchanged, nil
}

// ReceiveSnapshot applies a request snapshot without letting it overwrite newer
// runtime state already received from the stream at the same engine generation.
func (s *Session) ReceiveSnapshot(state DeviceState) (RemoteUpdate, error) {
	if saved := s.Saved; saved != nil && s.observedEngineGeneration.is(state.Engine.Generation) {
		state.Engine = saved.Engine
		state.Routing = saved.Routing
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

// Acknowledge applies the response for pending. Stored targets are accepted as
// returned (older mixers before 0.2.2 may round floats by 1 ULP), while any
// local edits made after pending was sent are preserved.
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
	s.baseRevision = someU64(state.Config.Revision)
	s.observedEngineGeneration = someU64(state.Engine.Generation)
	s.Saved = &state
	s.Connected = true
	s.Conflict = false
	return nil
}

// AcknowledgePreset applies a preset load response, flagging a conflict if the
// user edited Draft while the request was in flight.
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
	s.Conflict = edited
	return nil
}

// Keep already-observed stream runtime state so a save reply can't roll back
// a newer engine failure, recovery, or generation. Call only after validating
// state and confirming selection is unchanged.
func (s *Session) preserveObservedRuntime(state *DeviceState) {
	if saved := s.Saved; saved != nil &&
		s.observedEngineGeneration.ok && s.observedEngineGeneration.value >= state.Engine.Generation {
		state.Engine = saved.Engine
		state.Routing = saved.Routing
		state.Selection = saved.Selection.Clone()
	}
}

// RebaseEdits clears a conflict by rebasing local edits onto the latest saved revision.
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
