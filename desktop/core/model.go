package core

import (
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/num"
)

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

type Channel uint8

const (
	ChannelLeft Channel = iota
	ChannelRight
)

// GateShape holds the resolved runtime gate parameters (smoothed in dB).
type GateShape struct {
	Threshold      float64
	CloseThreshold float64 // Threshold - GateHysteresis to avoid chatter on soft consonants
	ReductionDB    float64
	AttackMs       float64
	HoldMs         float64
	ReleaseMs      float64
}

// Correctly rounded log2(1.5) matching glibc; Go's math.Log2 can be 1 ULP off.
const log2OnePointFive = 0.58496250072115618145373894394781650876

// GateShapeForStrength maps strength in [0, 1] to gate parameters. Reduction
// scales smoothly up to 30 dB (20 dB at 0.5) rather than hard-muting; release
// scales geometrically so equal slider steps sound even; attack stays at 5 ms
// inside the model's lookahead.
func GateShapeForStrength(strength float64) GateShape {
	s := num.Clamp(strength, 0.0, 1.0)
	// Explicit float64() conversions inhibit FMA so threshold/hold match the
	// mixer bit-for-bit. math.Pow may differ from glibc pow by up to 2 ULPs.
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

func VoiceGateWithStrength(strength float64) VoiceGate {
	gate := VoiceGate{Strength: strength}
	gate.SetAdvanced(true)
	gate.Advanced = false
	return gate
}

func (g VoiceGate) Active() bool { return g.Shape().ReductionDB > 0.0 }

func (g VoiceGate) Shape() GateShape {
	if g.Advanced {
		return newGateShape(g.Threshold, g.Reduction, g.Attack, g.Hold, g.Release)
	}
	return GateShapeForStrength(g.Strength)
}

// SetAdvanced toggles manual gate parameters. Entering advanced mode seeds
// them from the current strength so there is no audible jump; leaving keeps
// the custom values intact.
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

func (s *Suppression) SelectLookahead(lookahead uint8) { s.Lookahead = lookahead }

func (s Suppression) Geometry() (SuppressionGeometry, bool) {
	return SuppressionAdmittedGeometry(s.Lookahead)
}

func (s Suppression) validateLookahead() error {
	if s.Lookahead > 1 {
		return CoreError("The lookahead must be 0 or 1")
	}
	return nil
}

var Lookaheads = [2]uint8{0, 1}

// ModelGeometry is model-inherent buffering, excluding worker scheduling lead.
type ModelGeometry struct {
	FrameSamples          uint32
	IntrinsicDelaySamples uint32
}

// SuppressionGeometry is model buffering plus the worker scheduling allowance.
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

const MaxStatusText = 4096

// ValidDisplayText checks that mixer-provided text is within maximum bytes and
// contains no control or bidi override characters.
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

// IsBidiControl reports whether c is a Unicode bidi embedding, override, or isolate control.
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

// Channel returns the bands active for side (right falls back to left when linked).
func (e EQSettings) Channel(side Channel) []Band {
	if side == ChannelRight && e.RightBands != nil {
		return *e.RightBands
	}
	return e.Bands
}

func (e *EQSettings) ChannelMut(side Channel) *[]Band {
	if side == ChannelRight && e.RightBands != nil {
		return e.RightBands
	}
	return &e.Bands
}

// AddBand appends "band-N" using one past the highest existing band number.
func (e *EQSettings) AddBand(side Channel) (*Band, error) {
	bands := e.ChannelMut(side)
	if len(*bands) >= MaxBands {
		return nil, CoreError("At most 32 bands are supported per channel")
	}
	var highest uint64
	for _, band := range *bands {
		if suffix, ok := strings.CutPrefix(band.ID, "band-"); ok {
			if n, ok := common.ParseUint(suffix, 64); ok && n > highest {
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

func (e EQSettings) Equal(other EQSettings) bool { return deepEqual(e, other) }

func NewTarget(id, name string, kind TargetKind, channels uint8) Target {
	eq := DefaultEQSettings()
	// Stages start disabled so a new path passes audio through unchanged.
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

// IsChatOutput reports whether t is a Chat mix output, which suppresses a
// dedicated mono mic stem rather than the full stereo mix.
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

func (t Target) Clone() Target {
	t.Order = slices.Clone(t.Order)
	t.EQ = t.EQ.Clone()
	return t
}

func (t Target) Equal(other Target) bool { return deepEqual(t, other) }

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

// ValidateSelection checks IDs against the catalog. Physical eligibility is
// reported by the mixer, not inferred here.
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

// CanonicalSelection validates selected and returns it in catalog order.
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

// ResolveLiveUpdate merges a full or partial live-edit target list onto
// c.Targets, rejecting unknown or duplicate IDs, and validates the merged
// catalog with ValidateUpdate.
func (c Config) ResolveLiveUpdate(targets []Target) ([]Target, error) {
	if len(targets) > len(c.Targets) {
		return nil, CoreError("Live update contains unknown or duplicate targets")
	}
	seen := make(map[string]struct{}, len(targets))
	merged := CloneTargets(c.Targets)
	for _, target := range targets {
		if _, dup := seen[target.ID]; dup {
			return nil, CoreError("Duplicate target id: " + target.ID)
		}
		seen[target.ID] = struct{}{}
		found := false
		for i := range merged {
			if merged[i].ID == target.ID {
				merged[i] = target.Clone()
				found = true
				break
			}
		}
		if !found {
			return nil, CoreError("Unknown target id: " + target.ID)
		}
	}
	if err := c.ValidateUpdate(merged); err != nil {
		return nil, err
	}
	return merged, nil
}

func (c Config) Clone() Config {
	c.SelectedTargets = slices.Clone(c.SelectedTargets)
	c.Targets = CloneTargets(c.Targets)
	return c
}

func (c Config) Equal(other Config) bool { return deepEqual(c, other) }

// DefaultConfig returns the full target catalog with nothing selected at revision 1.
func DefaultConfig() Config {
	targets := []Target{}
	// Legacy ID "mic-1" maps to physical combo input 4; keep the ID stable.
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
