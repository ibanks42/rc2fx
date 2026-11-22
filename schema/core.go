// Package schema is the single source of truth for RC2FX's shared wire types
// and constants across Rust (crates/rc2fx-core) and Go (desktop/core).
//
// It is written in standard Go syntax so normal Go tooling (gofmt, editors,
// go/parser) works directly with zero custom language or external compilers.
// Regenerate both sides with `./xtask schema` (or `just schema`).
package schema

const (
	SchemaVersion uint32  = 9
	SampleRate    float64 = 48_000.0
	MaxTargets            = 32
	MaxBands              = 32

	GateHysteresis = 0.15

	// DFNHopSamples is DeepFilterNet3's STFT hop size in samples (5.0 ms at 48 kHz per lookahead step).
	DFNHopSamples uint32 = 240

	// EventsPage is the max diagnostics entries per page, sized to fit inside the USB control frame limit.
	EventsPage = 256

	// SSHManagedMarker is the comment marker identifying the single authorized_keys line managed by RC2FX.
	SSHManagedMarker = "rc2fx-managed-developer-key"

	// MaxSSHPublicKey is the maximum accepted public key line length (well above an Ed25519 key).
	MaxSSHPublicKey = 512

	// MixerProtocolVersion is the USB 1 control protocol version. Bump when older desktop apps can no longer talk to the mixer.
	MixerProtocolVersion uint32 = 1

	MixerReleaseSchema uint32 = 1

	// MaxListed is the max file paths included in an image-check report; overflow is counted in unlisted.
	MaxListed = 32
)

type TargetKind string

const (
	TargetKindMic      TargetKind = "mic"
	TargetKindPlayback TargetKind = "playback"
	TargetKindOutput   TargetKind = "output"
)

//schema:all
type Effect string

const (
	EffectHighpass    Effect = "highpass"
	EffectSuppression Effect = "suppression"
	EffectEQ          Effect = "eq"
	EffectCompressor  Effect = "compressor"
	EffectGain        Effect = "gain"
	EffectLimiter     Effect = "limiter"
)

//schema:all
type BandType string

const (
	BandTypeOff       BandType = "off"
	BandTypeBell      BandType = "bell"
	BandTypeLowShelf  BandType = "low-shelf"
	BandTypeHighShelf BandType = "high-shelf"
	BandTypeLowPass   BandType = "low-pass"
	BandTypeHighPass  BandType = "high-pass"
	BandTypeNotch     BandType = "notch"
)

type BandMode string

const (
	BandModeApoDr BandMode = "apo-dr"
	BandModeRlcBt BandMode = "rlc-bt"
)

type Config struct {
	SchemaVersion   uint32   `json:"schemaVersion"`
	Revision        uint64   `json:"revision"`
	SelectedTargets []string `json:"selectedTargets"`
	Targets         []Target `json:"targets"`
}

type Target struct {
	ID          string      `json:"id"`
	Name        string      `json:"name"`
	Kind        TargetKind  `json:"kind"`
	Channels    uint8       `json:"channels"`
	Bypass      bool        `json:"bypass"`
	Order       []Effect    `json:"order"`
	Highpass    Highpass    `json:"highpass"`
	Suppression Suppression `json:"suppression"`
	EQ          EQSettings  `json:"eq"`
	Compressor  Compressor  `json:"compressor"`
	Gain        Gain        `json:"gain"`
	Limiter     Limiter     `json:"limiter"`
}

type Highpass struct {
	Enabled   bool    `json:"enabled"`
	Frequency float64 `json:"frequency"`
}

type Suppression struct {
	Enabled bool    `json:"enabled"`
	Wet     float64 `json:"wet"`
	// DeepFilterNet3 lookahead in hops: 0 (default, also used for legacy configs) or 1 (adds one hop of latency).
	Lookahead uint8     `json:"lookahead,default"`
	Gate      VoiceGate `json:"gate"`
}

// VoiceGate attenuates the suppressed microphone when the model detects no voice.
// Only active while suppression is enabled, since it reads the model's voice score.
type VoiceGate struct {
	// 0 (off) to 1 (strict); derives all parameters unless advanced is set.
	Strength float64 `json:"strength"`
	// Use the explicit fields below instead of deriving them from strength.
	Advanced bool `json:"advanced"`
	// Voice score (0..=1) at or above which the gate opens.
	Threshold float64 `json:"threshold"`
	// Attenuation when closed, in dB (0 disables the gate).
	Reduction float64 `json:"reduction"`
	// Opening time constant in ms.
	Attack float64 `json:"attack"`
	// Time in ms the gate stays open after the voice score drops below threshold.
	Hold float64 `json:"hold"`
	// Closing time constant after hold, in ms.
	Release float64 `json:"release"`
}

type EQSettings struct {
	Enabled      bool    `json:"enabled"`
	InputGainDB  float64 `json:"inputGainDb"`
	OutputGainDB float64 `json:"outputGainDb"`
	Bands        []Band  `json:"bands"`
	// Nil/None means the right channel follows the left; omitted on the wire (never null).
	RightBands *[]Band `json:"rightBands,omitempty,nonnull"`
}

type Band struct {
	ID        string   `json:"id"`
	Enabled   bool     `json:"enabled"`
	BandType  BandType `json:"type"`
	Mode      BandMode `json:"mode"`
	Solo      bool     `json:"solo"`
	Frequency float64  `json:"frequency"`
	Gain      float64  `json:"gain"`
	Q         float64  `json:"q"`
}

type Compressor struct {
	Enabled   bool    `json:"enabled"`
	Threshold float64 `json:"threshold"`
	Ratio     float64 `json:"ratio"`
	Attack    float64 `json:"attack"`
	Release   float64 `json:"release"`
	Makeup    float64 `json:"makeup"`
}

type Gain struct {
	Enabled bool    `json:"enabled"`
	DB      float64 `json:"db"`
}

type Limiter struct {
	Enabled bool    `json:"enabled"`
	Ceiling float64 `json:"ceiling"`
	Release float64 `json:"release"`
}

type EngineMode string

const (
	EngineModeSimulation EngineMode = "simulation"
	EngineModeJack       EngineMode = "jack"
	EngineModeNative     EngineMode = "native"
	EngineModeOffline    EngineMode = "offline"
)

type EnginePhase string

const (
	EnginePhaseStarting   EnginePhase = "starting"
	EnginePhaseRestarting EnginePhase = "restarting"
	EnginePhaseRunning    EnginePhase = "running"
	EnginePhaseOffline    EnginePhase = "offline"
	EnginePhaseError      EnginePhase = "error"
)

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

type EngineRestartRequest struct {
	ExpectedRevision   uint64 `json:"expectedRevision"`
	ExpectedGeneration uint64 `json:"expectedGeneration"`
}

// RoutingRequest toggles global processing. Generation checks distinguish this from
// per-channel effect bypass and reject requests from a retired audio owner.
type RoutingRequest struct {
	Enabled            bool   `json:"enabled"`
	ExpectedRevision   uint64 `json:"expectedRevision"`
	ExpectedGeneration uint64 `json:"expectedGeneration"`
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

type RoutingStatus struct {
	Status    RoutingPhase `json:"status"`
	Channels  uint32       `json:"channels"`
	Recovered uint32       `json:"recovered"`
	Message   string       `json:"message"`
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
	// Peak voice score (0..=1) from the suppression model in this window, or 0 when suppression is off.
	Voice float64 `json:"voice"`
	// Peak voice gate openness (0 = closed, 1 = open) in this window. 1 when the gate is off; 0 when suppression is off.
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

//schema:default
type Timing struct {
	Calls       uint64    `json:"calls"`
	TotalNs     uint64    `json:"totalNs"`
	MaxNs       uint64    `json:"maxNs"`
	Overruns    uint64    `json:"overruns"`
	ClockErrors uint64    `json:"clockErrors"`
	Histogram   [8]uint64 `json:"histogram"`
}

// Performance allows unknown fields for forward compatibility with newer mixers.
//
//schema:allow_unknown
type Performance struct {
	CallbackBudgetNs uint64 `json:"callbackBudgetNs"`
	WorkerBudgetNs   uint64 `json:"workerBudgetNs"`
	Xruns            uint64 `json:"xruns"`
	// Calls == 0 means callback timing wasn't sampled in this build, not zero duration.
	Callback Timing `json:"callback"`
	// Suppression worker wall and CPU time (all zeros if no worker). Cumulative across worker rebuilds within the current engine generation.
	WorkerWall Timing `json:"workerWall"`
	WorkerCPU  Timing `json:"workerCpu"`
	// Helper routines that finished past their cycle deadline, and callbacks that found the helper still busy. Omitted by older firmware and on the JACK path.
	HelperLate   *uint64 `json:"helperLate,omitempty"`
	HelperStalls *uint64 `json:"helperStalls,omitempty"`
	// Slowest audio callback and slowest suppression worker wall-time turnaround over roughly the last 10 s of audio. Omitted by older firmware or before any samples.
	CallbackRecentMaxNs *uint64 `json:"callbackRecentMaxNs,omitempty"`
	WorkerRecentMaxNs   *uint64 `json:"workerRecentMaxNs,omitempty"`
}

// Meter allows unknown fields for forward compatibility with newer mixers.
//
//schema:allow_unknown
type Meter struct {
	MessageType MeterType `json:"type"`
	Revision    uint64    `json:"revision"`
	Generation  uint64    `json:"generation"`
	Synthetic   bool      `json:"synthetic"`
	// Incremented on each diagnostics reset so clients know to rebaseline their counter deltas. 0 means never reset (or older firmware).
	DiagnosticsEpoch uint64 `json:"diagnosticsEpoch,default"`
	// Uptime of this engine generation on the daemon's monotonic clock. Omitted by older firmware.
	RunningForNs *uint64       `json:"runningForNs,omitempty"`
	Targets      []TargetMeter `json:"targets"`
}

type MeterType string

const (
	MeterTypeMeter MeterType = "meter"
)

type SelectionPhase string

const (
	SelectionPhaseIdle     SelectionPhase = "idle"
	SelectionPhaseChanging SelectionPhase = "changing"
	SelectionPhaseError    SelectionPhase = "error"
)

type SelectionStatus struct {
	// Audio-channel capacity from the last verified stock graph, not a CPU guarantee or reservation. None/nil means it has not been observed.
	ChannelCapacity    *uint32        `json:"channelCapacity"`
	Status             SelectionPhase `json:"status"`
	Allowed            bool           `json:"allowed"`
	QualifiedTargetIDs []string       `json:"qualifiedTargetIds"`
	// Allocated by the actual engine. Routing independently reports wet edges.
	ActiveTargetIDs  []string `json:"activeTargetIds"`
	PendingTargetIDs []string `json:"pendingTargetIds"`
	Message          string   `json:"message"`
}

type SelectionRequest struct {
	ExpectedRevision   uint64   `json:"expectedRevision"`
	ExpectedGeneration uint64   `json:"expectedGeneration"`
	TargetIDs          []string `json:"targetIds"`
}

type StockSource struct {
	ID       string `json:"id"`
	SourceID uint16 `json:"sourceId"`
	// None/nil for a known source that is not assigned to a show fader.
	Fader *uint8 `json:"fader"`
	Name  string `json:"name"`
	Color string `json:"color"`
	// Empty when the installed route profile does not process this source.
	TargetID string `json:"targetId"`
}

type StockInventory struct {
	Available bool          `json:"available"`
	ShowName  string        `json:"showName"`
	Revision  string        `json:"revision"`
	Message   string        `json:"message"`
	Inputs    []StockSource `json:"inputs"`
}

// DiagnosticsEvent is a single diagnostics log entry. Seq increments by 1, never
// repeats, and continues across a diagnostics reset (which appends a reset entry).
type DiagnosticsEvent struct {
	Seq  uint64          `json:"seq"`
	Kind DiagnosticsKind `json:"kind"`
	// Target path, or None/nil for engine-wide events.
	Path *string `json:"path,omitempty"`
	// Mixer CLOCK_MONOTONIC timestamp in nanoseconds.
	AtNs uint64 `json:"atNs"`
	// Human-readable summary for the UI.
	Message string `json:"message"`
	// Measured values for this event, if any.
	Details EventDetails `json:"details,omitempty,default"`
}

// DiagnosticsKind is a closed set of event kinds; unknown kinds from newer firmware deserialize as Other.
//
//schema:fallback=Other
type DiagnosticsKind string

const (
	// JACK xrun notification.
	DiagnosticsKindXrun DiagnosticsKind = "xrun"
	// Audio callback found the helper still busy with the previous frame.
	DiagnosticsKindHelperBusy DiagnosticsKind = "helperBusy"
	// Audio callback gave up waiting for the helper and passed the frame through.
	DiagnosticsKindHelperStall DiagnosticsKind = "helperStall"
	// Helper finished after its frame's cycle had already ended.
	DiagnosticsKindHelperLateFinish DiagnosticsKind = "helperLateFinish"
	// Audio callback finished late in its cycle.
	DiagnosticsKindLateEnd DiagnosticsKind = "lateEnd"
	// Audio callback took longer than one period.
	DiagnosticsKindOverrun DiagnosticsKind = "overrun"
	// Audio callback started late in its cycle.
	DiagnosticsKindLateStart DiagnosticsKind = "lateStart"
	// Gap between consecutive callbacks exceeded 1.5 periods.
	DiagnosticsKindGap DiagnosticsKind = "gap"
	// Estimated cycle start jumped by more than 1.5 periods.
	DiagnosticsKindCycleSkip DiagnosticsKind = "cycleSkip"
	// Vendor job finished after the next callback had started.
	DiagnosticsKindVendorJobLate DiagnosticsKind = "vendorJobLate"
	// Mixer rebuilt its USB gadget (USB mode change).
	DiagnosticsKindUSBGadget DiagnosticsKind = "usbGadget"
	// Suppression worker missed a frame deadline (listener heard delayed dry audio).
	DiagnosticsKindMissedDeadline DiagnosticsKind = "missedDeadline"
	// Worker result arrived past its deadline (still used, but late).
	DiagnosticsKindLateFrame DiagnosticsKind = "lateFrame"
	// Stale suppression results observed between readings.
	DiagnosticsKindStaleResults DiagnosticsKind = "staleResults"
	// Dropped input frames observed between readings.
	DiagnosticsKindInputDrops DiagnosticsKind = "inputDrops"
	// Dropped output frames observed between readings.
	DiagnosticsKindOutputDrops DiagnosticsKind = "outputDrops"
	// Clipped samples observed between readings.
	DiagnosticsKindClipped DiagnosticsKind = "clipped"
	// Events dropped from the engine ring before they were read.
	DiagnosticsKindLost DiagnosticsKind = "lost"
	// Diagnostics reset marker; UI counters restart from here.
	DiagnosticsKindReset DiagnosticsKind = "reset"
	// Unknown event kind from a newer mixer build.
	DiagnosticsKindOther DiagnosticsKind = "other"
)

// EventDetails holds optional numeric payload for a diagnostics event.
//
//schema:copy,default,empty_check
type EventDetails struct {
	// Measured duration or timestamp offset (cycle position, gap, turnaround).
	ValueNs *uint64 `json:"valueNs,omitempty"`
	// Xrun delay reported by jackd, in microseconds.
	DelayedUs *uint64 `json:"delayedUs,omitempty"`
	// Counter increase since the previous reading (drops, clips, lost events).
	Delta *uint64 `json:"delta,omitempty"`
	// Elapsed time over which delta accumulated.
	IntervalNs *uint64 `json:"intervalNs,omitempty"`
}

// DiagnosticsEvents is a paged diagnostics log response.
type DiagnosticsEvents struct {
	// Mixer CLOCK_MONOTONIC at reply time; clients add (local_recv - now_ns) to atNs.
	NowNs uint64 `json:"nowNs"`
	// Next sequence number; pass as after to fetch subsequent entries.
	NextSeq uint64 `json:"nextSeq"`
	// Entries evicted after after before the client read them (0 if caught up).
	Lost uint64 `json:"lost"`
	// True if more entries are available starting at nextSeq.
	More   bool               `json:"more"`
	Events []DiagnosticsEvent `json:"events"`
}

// SSHStatus is the mixer SSH access state. Managed, UnmanagedKeys, and HostKeys
// are required so a malformed reply fails rather than reading as "off"; unknown
// extra fields are ignored.
//
//schema:allow_unknown,default
type SSHStatus struct {
	// Whether the RC2FX-managed key is installed.
	Managed bool `json:"managed"`
	// SHA256:... fingerprint of the managed key, if installed.
	ManagedFingerprint *string `json:"managedFingerprint,omitempty"`
	// Count of other authorized_keys lines (e.g. baked into an earlier image).
	UnmanagedKeys uint32 `json:"unmanagedKeys"`
	// Mixer SSH server public host keys for host pinning.
	HostKeys []string `json:"hostKeys"`
	// Whether sshd was observed running, or None/nil if unknown.
	ServerRunning *bool   `json:"serverRunning,omitempty"`
	Detail        *string `json:"detail,omitempty"`
}

// SSHRequest is the payload for PUT api/v1/ssh.
type SSHRequest struct {
	Enabled bool `json:"enabled"`
	// Required when enabling: a single ssh-ed25519 public key line.
	PublicKey *string `json:"publicKey,omitempty"`
}

// MixerRelease models mixer-release.json.
type MixerRelease struct {
	SchemaVersion   uint32 `json:"schemaVersion"`
	Version         string `json:"version"`
	ProtocolVersion uint32 `json:"protocolVersion"`
}

// DeviceVersions is the response payload for GET api/v1/version. Unknown fields
// are ignored for forward compatibility, and any version the mixer can't
// determine is omitted.
//
//schema:allow_unknown,default
type DeviceVersions struct {
	// Installed RC2FX mixer release version.
	Runtime *string `json:"runtime,omitempty"`
	// Installed runtime's control protocol version.
	Protocol *uint32 `json:"protocol,omitempty"`
	// Native library's control protocol version, when older than the daemon's.
	NativeProtocol *uint32 `json:"nativeProtocol,omitempty"`
	// RØDE firmware version from the launcher-validated profile.
	Firmware *string `json:"firmware,omitempty"`
	// Validated firmware profile ID.
	FirmwareProfile *string `json:"firmwareProfile,omitempty"`
	// In-place USB 1 update capability, omitted if unsupported. 1 supports full uploads; 2 adds file listing and delta uploads.
	InPlaceUpdates *uint32 `json:"inPlaceUpdates,omitempty"`
}

type USBLinkState string

const (
	USBLinkStateUnavailable     USBLinkState = "unavailable"
	USBLinkStateNotAttached     USBLinkState = "notAttached"
	USBLinkStateAttached        USBLinkState = "attached"
	USBLinkStatePowered         USBLinkState = "powered"
	USBLinkStateReconnecting    USBLinkState = "reconnecting"
	USBLinkStateUnauthenticated USBLinkState = "unauthenticated"
	USBLinkStateDefault         USBLinkState = "default"
	USBLinkStateAddressed       USBLinkState = "addressed"
	USBLinkStateConfigured      USBLinkState = "configured"
	USBLinkStateSuspended       USBLinkState = "suspended"
)

//schema:copy
type USBStatus struct {
	// Pro II USB 1 only. This does not describe USB 2, network connectivity,
	// host playback activity, or whether a configured link carries audio.
	USB1 USBLinkState `json:"usb1"`
}

//schema:int_enum
type ImageStatus string

const (
	// No manifest in this image (pre-check install or in-place update).
	ImageStatusNone ImageStatus = "none"
	// Manifest exists, but verification hasn't completed yet.
	ImageStatusPending ImageStatus = "pending"
	// All manifest files matched.
	ImageStatusOk ImageStatus = "ok"
	// Damaged files were restored from the inactive root; a reboot is needed since RØDE's boot already loaded the damaged copies.
	ImageStatusRepaired ImageStatus = "repaired"
	// At least one file is damaged and couldn't be restored.
	ImageStatusDamaged ImageStatus = "damaged"
)

// ImageCheck allows unknown fields for forward compatibility with newer mixers.
//
//schema:allow_unknown,default
type ImageCheck struct {
	Status   ImageStatus `json:"status,default"`
	Checked  *string     `json:"checked,omitempty"`
	Repaired []string    `json:"repaired,omitempty,default"`
	Damaged  []string    `json:"damaged,omitempty,default"`
	// Count of repaired/damaged files omitted due to MaxListed.
	Unlisted uint `json:"unlisted,omitempty,default"`
}
