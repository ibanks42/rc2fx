package core

import (
	"strconv"
	"strings"
)

// Supported EasyEffects IIR EQ interchange. This is data conversion only.

// MaxFileBytes is the largest EQ preset file read (easyeffects and APO).
const MaxFileBytes = 4 * 1024 * 1024

var easyEffectsFrequencies = [32]float64{
	22.4, 27.8, 34.51, 42.82, 53.14, 65.95, 81.83, 101.55, 126.0, 156.38, 194.06, 240.81, 298.834,
	370.834, 460.182, 571.057, 708.647, 879.387, 1091.26, 1354.19, 1680.47, 2085.35, 2587.79,
	3211.29, 3985.01, 4945.15, 6136.63, 7615.17, 9449.96, 11726.8, 14552.2, 18058.4,
}

type Pipeline uint8

const (
	PipelineInput Pipeline = iota
	PipelineOutput
)

func (p Pipeline) AsStr() string {
	if p == PipelineInput {
		return "input"
	}
	return "output"
}

func (p Pipeline) String() string { return p.AsStr() }

type ImportOptions struct {
	Stereo   bool
	Selected *string
	Channel  *Channel
}

type CandidateSummary struct {
	Key            string
	Pipeline       Pipeline
	Instance       string
	BandCount      int
	ChannelsDiffer bool
	Error          *string
}

type ImportResultKind uint8

const (
	// ImportResultReady: EQ, Source and OtherEffects are set.
	ImportResultReady ImportResultKind = iota
	// ImportResultChooseEqualizer: Candidates is set.
	ImportResultChooseEqualizer
	// ImportResultChooseChannel: Source is set.
	ImportResultChooseChannel
)

// ImportResult is an import's outcome; Kind says which fields mean
// something.
type ImportResult struct {
	Kind         ImportResultKind
	EQ           EQSettings
	Source       CandidateSummary
	OtherEffects int
	Candidates   []CandidateSummary
}

type easyEqualizer struct {
	enabled    bool
	inputGain  float64
	outputGain float64
	left       []Band
	right      []Band
}

type easyCandidate struct {
	summary CandidateSummary
	eq      easyEqualizer
	err     error
}

func easyObject(value Value, name string) ([]Member, error) {
	members, ok := value.AsObject()
	if !ok {
		return nil, CoreError(name + " must be an object")
	}
	return members, nil
}

func memberOf(members []Member, key string) (Value, bool) {
	for _, member := range members {
		if member.Key == key {
			return member.Value, true
		}
	}
	return Value{}, false
}

func easyNumber(members []Member, key string, fallback, min, max float64) (float64, error) {
	value := fallback
	if v, ok := memberOf(members, key); ok {
		number, ok := v.AsF64()
		if !ok {
			return 0, CoreError(key + " must be a number")
		}
		value = number
	}
	if err := rangeCheck(value, min, max, key); err != nil {
		return 0, err
	}
	return value, nil
}

func easyBool(members []Member, key string, fallback bool) (bool, error) {
	if v, ok := memberOf(members, key); ok {
		b, ok := v.AsBool()
		if !ok {
			return false, CoreError(key + " must be true or false")
		}
		return b, nil
	}
	return fallback, nil
}

func easyToken(members []Member, key, fallback string) (string, error) {
	if v, ok := memberOf(members, key); ok {
		text, ok := v.AsString()
		if !ok {
			return "", CoreError(key + " must be text")
		}
		return text, nil
	}
	return fallback, nil
}

func onlyKeys(members []Member, allowed []string, where string) error {
	for _, member := range members {
		known := false
		for _, key := range allowed {
			if member.Key == key {
				known = true
				break
			}
		}
		if !known {
			return CoreError("Unsupported " + where + " field: " + member.Key)
		}
	}
	return nil
}

func readBand(value Value, index int) (Band, error) {
	members, err := easyObject(value, "Band")
	if err != nil {
		return Band{}, err
	}
	if err := onlyKeys(members, []string{
		"type", "mode", "slope", "solo", "mute", "width", "frequency", "gain", "q",
	}, "band"); err != nil {
		return Band{}, err
	}
	typeName, err := easyToken(members, "type", "Bell")
	if err != nil {
		return Band{}, err
	}
	var bandType BandType
	switch typeName {
	case "Off":
		bandType = BandTypeOff
	case "Bell":
		bandType = BandTypeBell
	case "Lo-shelf":
		bandType = BandTypeLowShelf
	case "Hi-shelf":
		bandType = BandTypeHighShelf
	case "Lo-pass":
		bandType = BandTypeLowPass
	case "Hi-pass":
		bandType = BandTypeHighPass
	case "Notch":
		bandType = BandTypeNotch
	default:
		return Band{}, CoreError("Unsupported filter type: " + typeName)
	}
	modeName, err := easyToken(members, "mode", "RLC (BT)")
	if err != nil {
		return Band{}, err
	}
	var mode BandMode
	switch modeName {
	case "APO (DR)":
		mode = BandModeApoDr
	case "RLC (BT)":
		mode = BandModeRlcBt
	default:
		return Band{}, CoreError("Unsupported filter model: " + modeName)
	}
	// `slope` and `width` are accepted and ignored: RC2FX processes every band
	// as one filter section and none of its filters use a width, so a slope
	// above x1 sounds gentler here than in EasyEffects.
	mute, err := easyBool(members, "mute", false)
	if err != nil {
		return Band{}, err
	}
	solo, err := easyBool(members, "solo", false)
	if err != nil {
		return Band{}, err
	}
	frequency, err := easyNumber(members, "frequency", easyEffectsFrequencies[index], 10.0, 24000.0)
	if err != nil {
		return Band{}, err
	}
	gain, err := easyNumber(members, "gain", 0.0, -36.0, 36.0)
	if err != nil {
		return Band{}, err
	}
	q, err := easyNumber(members, "q", 4.36, 0.0, 100.0)
	if err != nil {
		return Band{}, err
	}
	band := Band{
		ID:        "band-" + strconv.Itoa(index+1),
		Enabled:   !mute,
		BandType:  bandType,
		Mode:      mode,
		Solo:      solo,
		Frequency: frequency,
		Gain:      gain,
		Q:         q,
	}
	if err := band.Validate(); err != nil {
		return Band{}, err
	}
	return band, nil
}

func readEqualizer(value Value) (easyEqualizer, error) {
	members, err := easyObject(value, "Equalizer")
	if err != nil {
		return easyEqualizer{}, err
	}
	if err := onlyKeys(members, []string{
		"mode", "decramp", "balance", "pitch-left", "pitch-right", "bypass", "split-channels",
		"input-gain", "output-gain", "num-bands", "left", "right",
	}, "equalizer"); err != nil {
		return easyEqualizer{}, err
	}
	mode, err := easyToken(members, "mode", "IIR")
	if err != nil {
		return easyEqualizer{}, err
	}
	if mode != "IIR" {
		return easyEqualizer{}, CoreError("Unsupported processing mode: " + mode + "; only IIR is supported")
	}
	decramp, err := easyToken(members, "decramp", "Off")
	if err != nil {
		return easyEqualizer{}, err
	}
	if decramp != "Off" {
		return easyEqualizer{}, CoreError("Decramp must be Off")
	}
	balance, err := easyNumber(members, "balance", 0.0, -100.0, 100.0)
	if err != nil {
		return easyEqualizer{}, err
	}
	if balance != 0.0 {
		return easyEqualizer{}, CoreError("Nonzero balance is unsupported")
	}
	for _, key := range []string{"pitch-left", "pitch-right"} {
		pitch, err := easyNumber(members, key, 0.0, -120.0, 120.0)
		if err != nil {
			return easyEqualizer{}, err
		}
		if pitch != 0.0 {
			return easyEqualizer{}, CoreError("Frequency pitch shifting is unsupported")
		}
	}
	bypass, err := easyBool(members, "bypass", false)
	if err != nil {
		return easyEqualizer{}, err
	}
	if _, err := easyBool(members, "split-channels", false); err != nil {
		return easyEqualizer{}, err
	}
	inputGain, err := easyNumber(members, "input-gain", 0.0, -36.0, 36.0)
	if err != nil {
		return easyEqualizer{}, err
	}
	outputGain, err := easyNumber(members, "output-gain", 0.0, -36.0, 36.0)
	if err != nil {
		return easyEqualizer{}, err
	}
	count := 32
	if v, ok := memberOf(members, "num-bands"); ok {
		n, ok := v.AsU64()
		if !ok || n < 1 || n > 32 {
			return easyEqualizer{}, CoreError("num-bands must be an integer from 1 to 32")
		}
		count = int(n)
	}
	var sides [2][]Band
	for s, side := range []string{"left", "right"} {
		channelValue, ok := memberOf(members, side)
		if !ok {
			return easyEqualizer{}, CoreError("Missing " + side + " channel")
		}
		channel, err := easyObject(channelValue, side)
		if err != nil {
			return easyEqualizer{}, err
		}
		for _, member := range channel {
			suffix, ok := strings.CutPrefix(member.Key, "band")
			n, parsed := parseRustUint(suffix, 64)
			if !ok || !parsed || n >= MaxBands {
				return easyEqualizer{}, CoreError("Unsupported channel field")
			}
		}
		bands := []Band{}
		for index := 0; index < count; index++ {
			key := "band" + strconv.Itoa(index)
			bandValue, ok := memberOf(channel, key)
			if !ok {
				return easyEqualizer{}, CoreError("Missing " + side + " " + key)
			}
			band, err := readBand(bandValue, index)
			if err != nil {
				return easyEqualizer{}, err
			}
			bands = append(bands, band)
		}
		sides[s] = bands
	}
	return easyEqualizer{
		enabled:    !bypass,
		inputGain:  inputGain,
		outputGain: outputGain,
		left:       sides[0],
		right:      sides[1],
	}, nil
}

func bandsEqual(a, b []Band) bool { return deepEqual(a, b) }

func readCandidates(text string) ([]easyCandidate, int, error) {
	rootValue, err := parseStrict([]byte(strings.TrimPrefix(text, "\ufeff")), MaxFileBytes, DefaultMaxJSONNodes)
	if err != nil {
		return nil, 0, err
	}
	root, err := easyObject(rootValue, "Preset")
	if err != nil {
		return nil, 0, err
	}
	candidates := []easyCandidate{}
	otherEffects := 0
	for _, pipeline := range []Pipeline{PipelineInput, PipelineOutput} {
		sectionValue, ok := memberOf(root, pipeline.AsStr())
		if !ok {
			continue
		}
		section, err := easyObject(sectionValue, "Pipeline")
		if err != nil {
			return nil, 0, err
		}
		orderValue, _ := memberOf(section, "plugins_order")
		order, ok := orderValue.AsArray()
		if !ok {
			return nil, 0, CoreError("Pipeline requires a plugins_order array")
		}
		if blocklist, ok := memberOf(section, "blocklist"); ok {
			items, isArray := blocklist.AsArray()
			allNames := isArray
			for _, item := range items {
				if !item.IsString() {
					allNames = false
				}
			}
			if !allNames {
				return nil, 0, CoreError("blocklist must contain names")
			}
		}
		seen := map[string]struct{}{}
		canonical := map[string]struct{}{}
		for _, value := range order {
			entry, ok := value.AsString()
			if !ok {
				return nil, 0, CoreError("plugins_order must contain names")
			}
			if _, duplicate := seen[entry]; duplicate {
				return nil, 0, CoreError("plugins_order contains a duplicate effect")
			}
			seen[entry] = struct{}{}
			if name, _, _ := strings.Cut(entry, "#"); name != "equalizer" {
				otherEffects++
				continue
			}
			instance := "equalizer#0"
			if entry != "equalizer" {
				suffix, ok := strings.CutPrefix(entry, "equalizer#")
				if !ok || suffix == "" || len(suffix) > 3 || !allASCIIDigits(suffix) {
					return nil, 0, CoreError("Invalid equalizer instance")
				}
				instance = entry
			}
			if _, duplicate := canonical[instance]; duplicate {
				return nil, 0, CoreError("Duplicate equalizer instance")
			}
			canonical[instance] = struct{}{}
			settings, found := memberOf(section, instance)
			if !found && entry == "equalizer" {
				settings, found = memberOf(section, entry)
			}
			candidate := easyCandidate{}
			if !found {
				candidate.err = CoreError("Equalizer settings are missing")
			} else {
				candidate.eq, candidate.err = readEqualizer(settings)
			}
			candidate.summary = CandidateSummary{
				Key:      pipeline.AsStr() + "/" + instance,
				Pipeline: pipeline,
				Instance: instance,
			}
			if candidate.err == nil {
				candidate.summary.BandCount = len(candidate.eq.left)
				candidate.summary.ChannelsDiffer = !bandsEqual(candidate.eq.left, candidate.eq.right)
			} else {
				message := candidate.err.Error()
				candidate.summary.Error = &message
			}
			candidates = append(candidates, candidate)
		}
	}
	if len(candidates) == 0 {
		return nil, 0, CoreError("No active equalizer was found in this EasyEffects preset")
	}
	return candidates, otherEffects, nil
}

func allASCIIDigits(text string) bool {
	for i := 0; i < len(text); i++ {
		if text[i] < '0' || text[i] > '9' {
			return false
		}
	}
	return true
}

func neutralBand() Band {
	band := NewBand("band-1")
	band.BandType = BandTypeOff
	return band
}

func isNeutral(band Band) bool {
	band.ID = "band-1"
	return band == neutralBand()
}

func stripPadding(bands []Band) []Band {
	for len(bands) > 0 && isNeutral(bands[len(bands)-1]) {
		bands = bands[:len(bands)-1]
	}
	return bands
}

// ImportEasyEffects reads an EasyEffects preset's equalizers: one ready to
// use, or the choice the user must make.
func ImportEasyEffects(text string, options ImportOptions) (ImportResult, error) {
	candidates, otherEffects, err := readCandidates(text)
	if err != nil {
		return ImportResult{}, err
	}
	selected := 0
	switch {
	case options.Selected != nil:
		selected = -1
		for i, candidate := range candidates {
			if candidate.summary.Key == *options.Selected {
				selected = i
				break
			}
		}
		if selected < 0 {
			return ImportResult{}, CoreError("Selected equalizer is absent from this file")
		}
	case len(candidates) > 1:
		allFailed := true
		for _, candidate := range candidates {
			if candidate.err == nil {
				allFailed = false
			}
		}
		if allFailed {
			return ImportResult{}, candidates[0].err
		}
		summaries := make([]CandidateSummary, len(candidates))
		for i, candidate := range candidates {
			summaries[i] = candidate.summary
		}
		return ImportResult{Kind: ImportResultChooseEqualizer, Candidates: summaries}, nil
	}
	chosen := candidates[selected]
	if chosen.err != nil {
		return ImportResult{}, chosen.err
	}
	candidate := chosen.eq
	differs := !bandsEqual(candidate.left, candidate.right)
	if differs && !options.Stereo && options.Channel == nil {
		return ImportResult{Kind: ImportResultChooseChannel, Source: chosen.summary}, nil
	}
	var bands []Band
	var rightBands *[]Band
	switch {
	case !differs:
		bands = candidate.left
		if len(candidate.left) == 1 && isNeutral(candidate.left[0]) {
			bands = []Band{}
		}
	case options.Stereo:
		bands = stripPadding(candidate.left)
		right := stripPadding(candidate.right)
		rightBands = &right
	default:
		side := candidate.left
		if options.Channel != nil && *options.Channel == ChannelRight {
			side = candidate.right
		}
		bands = stripPadding(side)
	}
	eq := EQSettings{
		Enabled:      candidate.enabled,
		InputGainDB:  candidate.inputGain,
		OutputGainDB: candidate.outputGain,
		Bands:        bands,
		RightBands:   rightBands,
	}
	channels := uint8(1)
	if options.Stereo {
		channels = 2
	}
	if err := eq.Validate(channels); err != nil {
		return ImportResult{}, err
	}
	return ImportResult{
		Kind:         ImportResultReady,
		EQ:           eq,
		Source:       chosen.summary,
		OtherEffects: otherEffects,
	}, nil
}

func easyTypeName(value BandType) string {
	switch value {
	case BandTypeOff:
		return "Off"
	case BandTypeBell:
		return "Bell"
	case BandTypeLowShelf:
		return "Lo-shelf"
	case BandTypeHighShelf:
		return "Hi-shelf"
	case BandTypeLowPass:
		return "Lo-pass"
	case BandTypeHighPass:
		return "Hi-pass"
	case BandTypeNotch:
		return "Notch"
	}
	return string(value)
}

func exportBand(band Band) Value {
	mode := "RLC (BT)"
	if band.Mode == BandModeApoDr {
		mode = "APO (DR)"
	}
	return ObjectValue(
		Member{"type", StringValue(easyTypeName(band.BandType))},
		Member{"mode", StringValue(mode)},
		Member{"solo", BoolValue(band.Solo)},
		Member{"mute", BoolValue(!band.Enabled)},
		Member{"gain", F64Value(band.Gain)},
		Member{"frequency", F64Value(band.Frequency)},
		Member{"q", F64Value(band.Q)},
	)
}

// ExportEasyEffects is an EasyEffects preset holding one IIR equalizer,
// pretty-printed with a final newline. Keys are written in the order they
// are listed here, the order earlier versions of the app wrote.
func ExportEasyEffects(eq EQSettings, pipeline Pipeline) (string, error) {
	if err := eq.Validate(eq.responseChannels()); err != nil {
		return "", err
	}
	right := eq.Bands
	if eq.RightBands != nil {
		right = *eq.RightBands
	}
	count := max(len(eq.Bands), len(right), 1)
	placeholder := neutralBand()
	channel := func(bands []Band) Value {
		object := ObjectValue()
		for i := 0; i < count; i++ {
			band := placeholder
			if i < len(bands) {
				band = bands[i]
			}
			object.Set("band"+strconv.Itoa(i), exportBand(band))
		}
		return object
	}
	equalizer := ObjectValue(
		Member{"bypass", BoolValue(!eq.Enabled)},
		Member{"input-gain", F64Value(eq.InputGainDB)},
		Member{"output-gain", F64Value(eq.OutputGainDB)},
		Member{"mode", StringValue("IIR")},
		Member{"decramp", StringValue("Off")},
		Member{"split-channels", BoolValue(eq.RightBands != nil)},
		Member{"balance", U64Value(0)},
		Member{"pitch-left", U64Value(0)},
		Member{"pitch-right", U64Value(0)},
		Member{"num-bands", U64Value(uint64(count))},
		Member{"left", channel(eq.Bands)},
		Member{"right", channel(right)},
	)
	value := ObjectValue(Member{pipeline.AsStr(), ObjectValue(
		Member{"blocklist", ArrayValue()},
		Member{"plugins_order", ArrayValue(StringValue("equalizer#0"))},
		Member{"equalizer#0", equalizer},
	)})
	return value.Pretty() + "\n", nil
}
