package core

import (
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

const (
	SchemaVersion uint32  = 9
	SampleRate    float64 = 48_000.0
	MaxTargets            = 32
	MaxBands              = 32
)

// TargetKind is serde `rename_all = "lowercase"`.
type TargetKind string

const (
	TargetKindMic      TargetKind = "mic"
	TargetKindPlayback TargetKind = "playback"
	TargetKindOutput   TargetKind = "output"
)

var targetKinds = []TargetKind{TargetKindMic, TargetKindPlayback, TargetKindOutput}

func (k *TargetKind) UnmarshalText(text []byte) error { return enumUnmarshal(k, text, targetKinds) }

// Effect is one processing stage, serde `rename_all = "lowercase"`.
type Effect string

const (
	EffectHighpass    Effect = "highpass"
	EffectSuppression Effect = "suppression"
	EffectEQ          Effect = "eq"
	EffectCompressor  Effect = "compressor"
	EffectGain        Effect = "gain"
	EffectLimiter     Effect = "limiter"
)

// EffectAll is every effect, in the default order.
var EffectAll = [6]Effect{
	EffectHighpass,
	EffectSuppression,
	EffectEQ,
	EffectCompressor,
	EffectGain,
	EffectLimiter,
}

func (e *Effect) UnmarshalText(text []byte) error { return enumUnmarshal(e, text, EffectAll[:]) }

func (e Effect) Label() string {
	switch e {
	case EffectHighpass:
		return "High-pass"
	case EffectSuppression:
		return "Noise suppression"
	case EffectEQ:
		return "Equalizer"
	case EffectCompressor:
		return "Compressor"
	case EffectGain:
		return "Gain"
	case EffectLimiter:
		return "Limiter"
	}
	return string(e)
}

// BandType is serde `rename_all = "kebab-case"`.
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

// BandTypeAll is every band type.
var BandTypeAll = [7]BandType{
	BandTypeOff,
	BandTypeBell,
	BandTypeLowShelf,
	BandTypeHighShelf,
	BandTypeLowPass,
	BandTypeHighPass,
	BandTypeNotch,
}

func (b *BandType) UnmarshalText(text []byte) error { return enumUnmarshal(b, text, BandTypeAll[:]) }

func (b BandType) Label() string {
	switch b {
	case BandTypeOff:
		return "Off"
	case BandTypeBell:
		return "Bell"
	case BandTypeLowShelf:
		return "Low shelf"
	case BandTypeHighShelf:
		return "High shelf"
	case BandTypeLowPass:
		return "Low-pass"
	case BandTypeHighPass:
		return "High-pass"
	case BandTypeNotch:
		return "Notch"
	}
	return string(b)
}

// BandMode is serde `rename_all = "kebab-case"`.
type BandMode string

const (
	BandModeApoDr BandMode = "apo-dr"
	BandModeRlcBt BandMode = "rlc-bt"
)

var bandModes = []BandMode{BandModeApoDr, BandModeRlcBt}

func (m *BandMode) UnmarshalText(text []byte) error { return enumUnmarshal(m, text, bandModes) }

// Channel is a side of a stereo EQ.
type Channel uint8

const (
	ChannelLeft Channel = iota
	ChannelRight
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
	// DeepFilterNet3's lookahead in hops: 0 (the default, and what settings
	// saved before this field existed mean) or 1, which adds exactly one hop
	// of latency.
	Lookahead uint8     `json:"lookahead,default"`
	Gate      VoiceGate `json:"gate"`
}

// VoiceGate turns the suppressed microphone down while the model hears no
// voice. It acts only while suppression is enabled, since the voice score
// comes from the suppression model.
type VoiceGate struct {
	// 0 (off) to 1 (strict); sets every parameter unless `advanced`.
	Strength float64 `json:"strength"`
	// Use the parameters below instead of the ones `strength` stands for.
	Advanced bool `json:"advanced"`
	// Voice score (0 to 1) at or above which the gate opens.
	Threshold float64 `json:"threshold"`
	// How far the closed gate turns the signal down, in dB; 0 is off.
	Reduction float64 `json:"reduction"`
	// Opening time constant, in ms.
	Attack float64 `json:"attack"`
	// How long the gate stays open after the score last reached the
	// threshold, in ms.
	Hold float64 `json:"hold"`
	// Closing time constant after the hold, in ms.
	Release float64 `json:"release"`
}

// GateShape is the parameters the gate runs with. Its smoothing runs in dB.
type GateShape struct {
	// Voice score that opens the gate.
	Threshold float64
	// Voice score an open gate stays open down to (`threshold` minus
	// `GATE_HYSTERESIS`), so a score hovering near the threshold, as in soft
	// consonants, does not flutter it.
	CloseThreshold float64
	ReductionDB    float64
	AttackMs       float64
	HoldMs         float64
	ReleaseMs      float64
}

const GateHysteresis = 0.15

// log2OnePointFive is 1.5f64.log2() correctly rounded (what glibc's log2
// returns); Go's math.Log2 can be an ulp away.
const log2OnePointFive = 0.58496250072115618145373894394781650876

// GateShapeForStrength is what a strength from 0 (off) through gentle (keeps
// breaths) to 1 (strict) stands for. Depth grows from nothing, like a blend,
// reaching 20 dB at half strength. Strictness comes mostly from the threshold
// and depth, so the gate never becomes a hard mute: the residual after neural
// suppression is already low, and deeper reduction only makes the switching
// audible. Hold and release shrink less; the release moves geometrically so
// equal slider steps sound even. Opening is always 5 ms, inside the model's
// 10-30 ms lookahead.
func GateShapeForStrength(strength float64) GateShape {
	s := num.Clamp(strength, 0.0, 1.0)
	// The explicit float64 conversions keep Go from fusing a multiply-add,
	// so results match the mixer's bit for bit on every architecture.
	linear := func(gentle, strict float64) float64 { return gentle + float64((strict-gentle)*s) }
	geometric := func(gentle, strict float64) float64 { return float64(gentle * math.Pow(strict/gentle, s)) }
	return newGateShape(
		linear(0.3, 0.7),
		float64(30.0*math.Pow(s, log2OnePointFive)),
		5.0,
		linear(350.0, 150.0),
		geometric(300.0, 90.0),
	)
}

func newGateShape(threshold, reductionDB, attackMs, holdMs, releaseMs float64) GateShape {
	return GateShape{
		Threshold:      threshold,
		CloseThreshold: num.Max(threshold-GateHysteresis, 0.0),
		ReductionDB:    reductionDB,
		AttackMs:       attackMs,
		HoldMs:         holdMs,
		ReleaseMs:      releaseMs,
	}
}

// VoiceGateWithStrength is a gate at `strength`, its advanced parameters
// matching it.
func VoiceGateWithStrength(strength float64) VoiceGate {
	gate := VoiceGate{Strength: strength}
	gate.SetAdvanced(true)
	gate.Advanced = false
	return gate
}

// Active reports whether the gate turns anything down.
func (g VoiceGate) Active() bool { return g.Shape().ReductionDB > 0.0 }

func (g VoiceGate) Shape() GateShape {
	if g.Advanced {
		return newGateShape(g.Threshold, g.Reduction, g.Attack, g.Hold, g.Release)
	}
	return GateShapeForStrength(g.Strength)
}

// SetAdvanced switches between the strength slider and the separate
// parameters. Entering advanced mode starts the parameters from the current
// strength, so the sound does not jump; leaving keeps them for later.
func (g *VoiceGate) SetAdvanced(advanced bool) {
	if advanced && !g.Advanced {
		shape := GateShapeForStrength(g.Strength)
		g.Threshold = shape.Threshold
		g.Reduction = shape.ReductionDB
		g.Attack = shape.AttackMs
		g.Hold = shape.HoldMs
		g.Release = shape.ReleaseMs
	}
	g.Advanced = advanced
}

// SelectLookahead: 0 (lowest latency) or 1 (more context, one hop more).
func (s *Suppression) SelectLookahead(lookahead uint8) { s.Lookahead = lookahead }

// Geometry is the geometry the saved settings stand for, and hence the
// latency they add, without a worker scheduling allowance.
func (s Suppression) Geometry() (SuppressionGeometry, bool) {
	return SuppressionAdmittedGeometry(s.Lookahead)
}

func (s Suppression) validateLookahead() error {
	if s.Lookahead > 1 {
		return CoreError("The lookahead must be 0 or 1")
	}
	return nil
}

// DFNHopSamples is DeepFilterNet3's STFT hop, in samples: one lookahead step
// adds exactly this much latency (5.0 ms at 48 kHz), in both the one- and
// two-hop builds.
const DFNHopSamples uint32 = 240

// Lookaheads is the lookahead values DeepFilterNet3 admits, lowest latency
// first.
var Lookaheads = [2]uint8{0, 1}

// ModelGeometry is model-defined buffering, independent of any worker
// scheduling allowance.
type ModelGeometry struct {
	FrameSamples          uint32
	IntrinsicDelaySamples uint32
}

// SuppressionGeometry is a resolved processing geometry: the model's
// buffering plus the worker's scheduling allowance. Factories use
// SuppressionAdmittedGeometry.
type SuppressionGeometry struct {
	SampleRate            uint32
	FrameSamples          uint32
	IntrinsicDelaySamples uint32
	WorkerLeadSamples     uint32
}

func (g SuppressionGeometry) LatencySamples() uint32 {
	return g.FrameSamples + g.IntrinsicDelaySamples + g.WorkerLeadSamples
}

func SuppressionModelGeometryWithLookahead(lookahead uint8) ModelGeometry {
	return ModelGeometry{
		FrameSamples:          DFNHopSamples,
		IntrinsicDelaySamples: 120 + DFNHopSamples*uint32(lookahead),
	}
}

func SuppressionAdmittedGeometry(lookahead uint8) (SuppressionGeometry, bool) {
	if lookahead > 1 {
		return SuppressionGeometry{}, false
	}
	model := SuppressionModelGeometryWithLookahead(lookahead)
	return SuppressionGeometry{
		SampleRate:            48_000,
		FrameSamples:          model.FrameSamples,
		IntrinsicDelaySamples: model.IntrinsicDelaySamples,
		WorkerLeadSamples:     320,
	}, true
}

type EQSettings struct {
	Enabled      bool    `json:"enabled"`
	InputGainDB  float64 `json:"inputGainDb"`
	OutputGainDB float64 `json:"outputGainDb"`
	Bands        []Band  `json:"bands"`
	// Nil: the right channel follows the left. May be absent but not null.
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

// MaxStatusText is the longest status sentence a mixer may send. It leaves
// room for 512 of the widest characters and the context around them.
const MaxStatusText = 4096

// ValidDisplayText reports whether `text` from the mixer is fit to show: at
// most `maximum` bytes, one line, and no bidirectional controls that would
// make it read as something else.
func ValidDisplayText(text string, maximum int) bool {
	if len(text) > maximum {
		return false
	}
	for _, c := range text {
		if unicode.IsControl(c) || IsBidiControl(c) {
			return false
		}
	}
	return true
}

// IsBidiControl reports Unicode's embedding, override and isolate controls.
func IsBidiControl(c rune) bool {
	return (c >= '\u202a' && c <= '\u202e') || (c >= '\u2066' && c <= '\u2069')
}

func ValidID(id string) bool {
	if id == "" || len(id) > 48 {
		return false
	}
	for i := 0; i < len(id); i++ {
		c := id[i]
		if !(c >= 'a' && c <= 'z') && !(c >= '0' && c <= '9') && c != '-' {
			return false
		}
	}
	return true
}

func ValidateBands(bands []Band) error {
	if len(bands) > MaxBands {
		return CoreError("At most 32 bands are supported per channel")
	}
	ids := make(map[string]struct{}, len(bands))
	for _, band := range bands {
		if err := band.Validate(); err != nil {
			return err
		}
		if _, duplicate := ids[band.ID]; duplicate {
			return CoreError("Duplicate band ID: " + band.ID)
		}
		ids[band.ID] = struct{}{}
	}
	return nil
}

func NewBand(id string) Band {
	return Band{
		ID:        id,
		Enabled:   true,
		BandType:  BandTypeBell,
		Mode:      BandModeApoDr,
		Solo:      false,
		Frequency: 1000.0,
		Gain:      0.0,
		Q:         1.0,
	}
}

func (b Band) Validate() error {
	if !ValidID(b.ID) {
		return CoreError("Invalid band ID")
	}
	if err := rangeCheck(b.Frequency, 10.0, 24000.0, "Frequency"); err != nil {
		return err
	}
	if err := rangeCheck(b.Gain, -36.0, 36.0, "Band gain"); err != nil {
		return err
	}
	return rangeCheck(b.Q, 0.0, 100.0, "Q")
}

func DefaultEQSettings() EQSettings {
	return EQSettings{Enabled: true, Bands: []Band{}}
}

func (e EQSettings) Validate(channels uint8) error {
	if channels < 1 || channels > 2 {
		return CoreError("Channels must be 1 or 2")
	}
	if err := rangeCheck(e.InputGainDB, -36.0, 36.0, "EQ input gain"); err != nil {
		return err
	}
	if err := rangeCheck(e.OutputGainDB, -36.0, 36.0, "EQ output gain"); err != nil {
		return err
	}
	if err := ValidateBands(e.Bands); err != nil {
		return err
	}
	if e.RightBands != nil {
		if channels != 2 {
			return CoreError("Independent right EQ requires a stereo target")
		}
		if err := ValidateBands(*e.RightBands); err != nil {
			return err
		}
	}
	return nil
}

// Channel is the bands a side plays: the right follows the left unless it
// has its own.
func (e EQSettings) Channel(side Channel) []Band {
	if side == ChannelRight && e.RightBands != nil {
		return *e.RightBands
	}
	return e.Bands
}

// ChannelMut is the band list a side edits.
func (e *EQSettings) ChannelMut(side Channel) *[]Band {
	if side == ChannelRight && e.RightBands != nil {
		return e.RightBands
	}
	return &e.Bands
}

// AddBand appends `band-N`, N one above the highest numbered band. The
// pointer is valid until the band list next changes.
func (e *EQSettings) AddBand(side Channel) (*Band, error) {
	bands := e.ChannelMut(side)
	if len(*bands) >= MaxBands {
		return nil, CoreError("At most 32 bands are supported per channel")
	}
	var highest uint64
	for _, band := range *bands {
		if suffix, ok := strings.CutPrefix(band.ID, "band-"); ok {
			if n, ok := parseRustUint(suffix, 64); ok && n > highest {
				highest = n
			}
		}
	}
	if highest == math.MaxUint64 {
		return nil, CoreError("Band ID space exhausted")
	}
	*bands = append(*bands, NewBand("band-"+strconv.FormatUint(highest+1, 10)))
	return &(*bands)[len(*bands)-1], nil
}

func (e *EQSettings) RemoveBand(side Channel, id string) bool {
	bands := e.ChannelMut(side)
	for i, band := range *bands {
		if band.ID == id {
			*bands = slices.Delete(*bands, i, i+1)
			return true
		}
	}
	return false
}

// Clone is a deep copy.
func (e EQSettings) Clone() EQSettings {
	e.Bands = slices.Clone(e.Bands)
	if e.RightBands != nil {
		right := slices.Clone(*e.RightBands)
		if right == nil {
			right = []Band{}
		}
		e.RightBands = &right
	}
	return e
}

// Equal compares field by field (deepEqual).
func (e EQSettings) Equal(other EQSettings) bool { return deepEqual(e, other) }

func NewTarget(id, name string, kind TargetKind, channels uint8) Target {
	eq := DefaultEQSettings()
	// Processing is opt-in: a new path passes audio through unchanged
	// until each stage is explicitly enabled.
	eq.Enabled = false
	return Target{
		ID:       id,
		Name:     name,
		Kind:     kind,
		Channels: channels,
		Bypass:   false,
		Order:    slices.Clone(EffectAll[:]),
		Highpass: Highpass{Enabled: false, Frequency: 80.0},
		Suppression: Suppression{
			Enabled:   false,
			Wet:       1.0,
			Lookahead: 0,
			Gate:      VoiceGateWithStrength(0.0),
		},
		EQ: eq,
		Compressor: Compressor{
			Enabled:   false,
			Threshold: -18.0,
			Ratio:     3.0,
			Attack:    10.0,
			Release:   120.0,
			Makeup:    0.0,
		},
		Gain:    Gain{Enabled: false, DB: 0.0},
		Limiter: Limiter{Enabled: false, Ceiling: -1.0, Release: 100.0},
	}
}

func (t Target) Validate() error {
	if err := t.Suppression.validateLookahead(); err != nil {
		return err
	}
	return t.validateFields()
}

// validateFields is the field validation shared by every settings entry
// point.
func (t Target) validateFields() error {
	if !ValidID(t.ID) || t.Name == "" || len(t.Name) > 128 || t.Channels < 1 || t.Channels > 2 {
		return CoreError("Invalid target identity: " + t.ID)
	}
	if len(t.Order) != 6 || !distinctEffects(t.Order) {
		return CoreError("Effects must contain each of the six stages once")
	}
	if err := t.EQ.Validate(t.Channels); err != nil {
		return err
	}
	if t.Suppression.Enabled && !t.SupportsSuppression() {
		return CoreError("Noise suppression is supported on a mono microphone or the Chat voice mix only")
	}
	gate := t.Suppression.Gate
	checks := []struct {
		value, min, max float64
		name            string
	}{
		{t.Highpass.Frequency, 20.0, 1000.0, "High-pass frequency"},
		{t.Suppression.Wet, 0.0, 1.0, "Suppression blend"},
		{gate.Strength, 0.0, 1.0, "Voice gate strength"},
		{gate.Threshold, 0.0, 1.0, "Voice gate threshold"},
		{gate.Reduction, 0.0, 60.0, "Voice gate reduction"},
		{gate.Attack, 0.5, 50.0, "Voice gate attack"},
		{gate.Hold, 0.0, 2000.0, "Voice gate hold"},
		{gate.Release, 30.0, 2000.0, "Voice gate release"},
		{t.Compressor.Threshold, -60.0, 0.0, "Compressor threshold"},
		{t.Compressor.Ratio, 1.0, 20.0, "Compressor ratio"},
		{t.Compressor.Attack, 0.1, 200.0, "Compressor attack"},
		{t.Compressor.Release, 10.0, 2000.0, "Compressor release"},
		{t.Compressor.Makeup, 0.0, 24.0, "Compressor makeup"},
		{t.Gain.DB, -60.0, 12.0, "Gain"},
		{t.Limiter.Ceiling, -24.0, 0.0, "Limiter ceiling"},
		{t.Limiter.Release, 10.0, 2000.0, "Limiter release"},
	}
	for _, check := range checks {
		if err := rangeCheck(check.value, check.min, check.max, check.name); err != nil {
			return err
		}
	}
	return nil
}

func distinctEffects(order []Effect) bool {
	seen := make(map[Effect]struct{}, len(order))
	for _, effect := range order {
		seen[effect] = struct{}{}
	}
	return len(seen) == 6
}

// IsChatOutput: Chat suppression acts on a separate mono microphone stem,
// never on the completed stereo mix (which also carries pads and playback).
func (t Target) IsChatOutput() bool {
	return t.Kind == TargetKindOutput && t.Channels == 2 && (t.ID == "send-chat" || t.ID == "duo-send-chat")
}

func (t Target) SupportsSuppression() bool {
	return (t.Kind == TargetKindMic && t.Channels == 1) || t.IsChatOutput()
}

func (t Target) EffectEnabled(effect Effect) bool {
	switch effect {
	case EffectHighpass:
		return t.Highpass.Enabled
	case EffectSuppression:
		return t.Suppression.Enabled
	case EffectEQ:
		return t.EQ.Enabled
	case EffectCompressor:
		return t.Compressor.Enabled
	case EffectGain:
		return t.Gain.Enabled
	case EffectLimiter:
		return t.Limiter.Enabled
	}
	return false
}

func (t *Target) SetEffectEnabled(effect Effect, enabled bool) {
	switch effect {
	case EffectHighpass:
		t.Highpass.Enabled = enabled
	case EffectSuppression:
		t.Suppression.Enabled = enabled
	case EffectEQ:
		t.EQ.Enabled = enabled
	case EffectCompressor:
		t.Compressor.Enabled = enabled
	case EffectGain:
		t.Gain.Enabled = enabled
	case EffectLimiter:
		t.Limiter.Enabled = enabled
	}
}

// Clone is a deep copy.
func (t Target) Clone() Target {
	t.Order = slices.Clone(t.Order)
	t.EQ = t.EQ.Clone()
	return t
}

// Equal compares field by field (deepEqual).
func (t Target) Equal(other Target) bool { return deepEqual(t, other) }

// CloneTargets deep-copies a target list.
func CloneTargets(targets []Target) []Target {
	if targets == nil {
		return nil
	}
	copied := make([]Target, len(targets))
	for i, target := range targets {
		copied[i] = target.Clone()
	}
	return copied
}

// TargetsEqual compares two target lists element by element (deepEqual).
func TargetsEqual(a, b []Target) bool { return deepEqual(a, b) }

func (c Config) Validate() error { return c.validateWith(Target.Validate) }

func (c Config) validateWith(validateTarget func(Target) error) error {
	if c.SchemaVersion != SchemaVersion || c.Revision == 0 {
		return CoreError("Expected schema version " + strconv.FormatUint(uint64(SchemaVersion), 10) + " and a positive revision")
	}
	if len(c.Targets) == 0 || len(c.Targets) > MaxTargets {
		return CoreError("Expected 1 to " + strconv.Itoa(MaxTargets) + " targets")
	}
	suppressed := 0
	for _, target := range c.Targets {
		if target.Suppression.Enabled {
			suppressed++
		}
	}
	if suppressed > 1 {
		return CoreError("Noise suppression can be enabled on only one microphone or the Chat voice mix at a time; turn it off on the other target first")
	}
	ids := make(map[string]struct{}, len(c.Targets))
	for _, target := range c.Targets {
		if err := validateTarget(target); err != nil {
			return err
		}
		if _, duplicate := ids[target.ID]; duplicate {
			return CoreError("Duplicate target ID")
		}
		ids[target.ID] = struct{}{}
	}
	return c.ValidateSelection(c.SelectedTargets)
}

// ValidateSelection: selection is distinct from saved effect settings.
// Physical eligibility is supplied by the connected controller, not inferred
// from this catalog.
func (c Config) ValidateSelection(selected []string) error {
	ids := make(map[string]struct{}, len(selected))
	for _, id := range selected {
		if !c.hasTarget(id) {
			return CoreError("Unknown processing path: " + id)
		}
		if _, duplicate := ids[id]; duplicate {
			return CoreError("Duplicate processing path: " + id)
		}
		ids[id] = struct{}{}
	}
	return nil
}

func (c Config) hasTarget(id string) bool {
	for _, target := range c.Targets {
		if target.ID == id {
			return true
		}
	}
	return false
}

func (c Config) IsSelected(id string) bool { return slices.Contains(c.SelectedTargets, id) }

func (c Config) SelectedChannels() uint32 {
	var channels uint32
	for _, target := range c.Targets {
		if c.IsSelected(target.ID) {
			channels += uint32(target.Channels)
		}
	}
	return channels
}

// CanonicalSelection is the selection in catalog order.
func (c Config) CanonicalSelection(selected []string) ([]string, error) {
	if err := c.ValidateSelection(selected); err != nil {
		return nil, err
	}
	ids := []string{}
	for _, target := range c.Targets {
		if slices.Contains(selected, target.ID) {
			ids = append(ids, target.ID)
		}
	}
	return ids, nil
}

// ConfigFromJSON is strict parsing of saved settings in the current schema.
func ConfigFromJSON(text string) (Config, error) {
	config, err := FromJSON[Config](text)
	if err != nil {
		return Config{}, err
	}
	if err := config.Validate(); err != nil {
		return Config{}, err
	}
	return config, nil
}

func (c Config) ValidateUpdate(targets []Target) error {
	if len(targets) != len(c.Targets) {
		return CoreError("Effects updates must retain the complete saved target catalog")
	}
	for _, target := range targets {
		if !c.hasTarget(target.ID) {
			return CoreError("Effects updates must retain the complete saved target catalog")
		}
	}
	updated := c
	updated.Targets = targets
	if err := updated.Validate(); err != nil {
		return err
	}
	for _, target := range targets {
		for _, old := range c.Targets {
			if old.ID == target.ID {
				if old.Kind != target.Kind || old.Channels != target.Channels {
					return CoreError("Kind and channels cannot change for " + target.ID)
				}
				break
			}
		}
	}
	return nil
}

// Clone is a deep copy.
func (c Config) Clone() Config {
	c.SelectedTargets = slices.Clone(c.SelectedTargets)
	c.Targets = CloneTargets(c.Targets)
	return c
}

// Equal compares field by field (deepEqual).
func (c Config) Equal(other Config) bool { return deepEqual(c, other) }

// DefaultConfig is Config::default(): every path the mixer can process, none
// selected, revision 1.
func DefaultConfig() Config {
	targets := []Target{}
	// This existing saved chain owns physical combo input 4. Do not move
	// its effects to input 1 merely because its durable ID says mic-1.
	targets = append(targets, NewTarget("mic-1", "Mic 4", TargetKindMic, 1))
	for i := 1; i <= 3; i++ {
		n := strconv.Itoa(i)
		targets = append(targets, NewTarget("analog-"+n, "Mic "+n, TargetKindMic, 1))
	}
	for _, name := range []string{"main", "chat", "secondary", "game", "music", "a", "b"} {
		targets = append(targets, NewTarget("usb-"+name, "USB "+name, TargetKindPlayback, 2))
	}
	for i := 1; i <= 4; i++ {
		n := strconv.Itoa(i)
		targets = append(targets, NewTarget("headphones-"+n, "Headphones "+n, TargetKindOutput, 2))
	}
	for _, pair := range [][2]string{
		{"monitors", "Monitors"},
		{"send-main", "Main Mix Out"},
		{"send-chat", "Chat Mix Out"},
		{"send-secondary", "Secondary Mix Out"},
	} {
		targets = append(targets, NewTarget(pair[0], pair[1], TargetKindOutput, 2))
	}
	for i := 1; i <= 3; i++ {
		n := strconv.Itoa(i)
		targets = append(targets, NewTarget("callme-"+n, "CallMe "+n, TargetKindPlayback, 2))
	}
	for i := 1; i <= 2; i++ {
		n := strconv.Itoa(i)
		targets = append(targets, NewTarget("wireless-"+n, "Wireless "+n, TargetKindMic, 1))
	}
	targets = append(targets, NewTarget("bluetooth", "Bluetooth", TargetKindPlayback, 2))
	return Config{
		SchemaVersion:   SchemaVersion,
		Revision:        1,
		SelectedTargets: []string{},
		Targets:         targets,
	}
}

// parseRustUint is Rust's `str::parse` for an unsigned integer of `bits`
// bits: an optional `+`, then one or more ASCII digits, in range.
func parseRustUint(text string, bits int) (uint64, bool) {
	text = strings.TrimPrefix(text, "+")
	if text == "" {
		return 0, false
	}
	for i := 0; i < len(text); i++ {
		if text[i] < '0' || text[i] > '9' {
			return 0, false
		}
	}
	value, err := strconv.ParseUint(text, 10, bits)
	return value, err == nil
}
