package edit

import (
	"fmt"
	"math"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Parameter identifies an editable DSP control.
type Parameter int

const (
	ParameterCutoff Parameter = iota
	ParameterBlend
	ParameterGateStrength
	ParameterGateThreshold
	ParameterGateReduction
	ParameterGateAttack
	ParameterGateHold
	ParameterGateRelease
	ParameterGain
	ParameterThreshold
	ParameterRatio
	ParameterAttack
	ParameterRelease
	ParameterMakeup
	ParameterCeiling
	ParameterLimiterRelease
	ParameterEQInput
	ParameterEQOutput
	ParameterFrequency
	ParameterBandGain
	ParameterQ
)

// Description holds UI metadata for a Parameter.
type Description struct {
	Label, Unit    string
	Min, Max, Step float64
	Log            bool
}

// Describe returns the label, unit, bounds, step size, and slider scale for p.
func (p Parameter) Describe() Description {
	d := func(label, unit string, min, max, step float64, log bool) Description {
		return Description{label, unit, min, max, step, log}
	}
	switch p {
	case ParameterCutoff:
		return d("Cutoff", "Hz", 20., 1000., 1., true)
	case ParameterBlend:
		return d("Blend", "%", 0., 100., 1., false)
	case ParameterGateStrength:
		return d("Strength", "%", 0., 100., 1., false)
	case ParameterGateThreshold:
		return d("Threshold", "%", 0., 100., 1., false)
	case ParameterGateReduction:
		return d("Reduction", "dB", 0., 60., 0.5, false)
	case ParameterGateAttack:
		return d("Attack", "ms", 0.5, 50., 0.1, true)
	case ParameterGateHold:
		return d("Hold", "ms", 0., 2000., 1., false)
	case ParameterGateRelease:
		return d("Release", "ms", 30., 2000., 1., true)
	case ParameterGain:
		return d("Output gain", "dB", -60., 12., 0.01, false)
	case ParameterThreshold:
		return d("Threshold", "dB", -60., 0., 0.5, false)
	case ParameterRatio:
		return d("Ratio", ":1", 1., 20., 0.1, false)
	case ParameterAttack:
		return d("Attack", "ms", 0.1, 200., 0.1, true)
	case ParameterRelease:
		return d("Release", "ms", 10., 2000., 1., true)
	case ParameterMakeup:
		return d("Makeup", "dB", 0., 24., 0.01, false)
	case ParameterCeiling:
		return d("Ceiling", "dBFS", -24., 0., 0.1, false)
	case ParameterLimiterRelease:
		return d("Release", "ms", 10., 2000., 1., true)
	case ParameterEQInput:
		return d("Input gain", "dB", -36., 36., 0.01, false)
	case ParameterEQOutput:
		return d("Output gain", "dB", -36., 36., 0.01, false)
	case ParameterFrequency:
		return d("Frequency", "Hz", 10., 24000., 1., true)
	case ParameterBandGain:
		return d("Gain", "dB", -36., 36., 0.01, false)
	default: // ParameterQ
		return d("Q", "", 0.1, 100., 0.001, true)
	}
}

// Decimals returns the number of decimal places needed to display step.
func Decimals(step float64) int {
	switch {
	case step >= 1.:
		return 0
	case step >= 0.1:
		return 1
	case step >= 0.01:
		return 2
	default:
		return 3
	}
}

// Format formats value with fixed decimal places for text fields.
func Format(value float64, decimals int) string {
	return strconv.FormatFloat(value, 'f', decimals, 64)
}

// Fraction maps value into [0, 1] along a linear or logarithmic slider range.
func Fraction(value, min, max float64, log bool) float32 {
	value = math.Min(math.Max(value, min), max)
	var span float64
	if log && min > 0 {
		span = math.Log(value/min) / math.Log(max/min)
	} else {
		span = (value - min) / (max - min)
	}
	if math.IsInf(span, 0) || math.IsNaN(span) {
		return 0
	}
	return float32(span)
}

// Hz formats a frequency for display (e.g. "80 Hz", "3.5 kHz", "12.5 kHz").
func Hz(value float64) string {
	if value >= 1000. {
		k := value / 1000.
		var text string
		if k >= 10. {
			text = fmt.Sprintf("%.1f", k)
		} else {
			text = fmt.Sprintf("%.2f", k)
		}
		return strings.TrimRight(strings.TrimRight(text, "0"), ".") + " kHz"
	}
	return fmt.Sprintf("%.0f Hz", value)
}

// Display formats value with its unit and a typographic minus sign.
func (p Parameter) Display(value float64) string {
	desc := p.Describe()
	var text string
	switch p {
	case ParameterCutoff, ParameterFrequency:
		text = Hz(value)
	case ParameterRatio:
		text = fmt.Sprintf("%.1f:1", value)
	case ParameterQ:
		text = Format(value, Decimals(desc.Step))
	default:
		number := Format(value, Decimals(desc.Step))
		signed := number
		if desc.Unit == "dB" && value > 0 {
			signed = "+" + number
		}
		switch desc.Unit {
		case "%":
			text = signed + "%"
		case "":
			text = signed
		default:
			text = signed + " " + desc.Unit
		}
	}
	return strings.ReplaceAll(text, "-", "\u2212")
}

// DefaultValue returns the reset value for p on target. Individual gate
// parameters reset to the shape derived from the target's current gate strength.
func (p Parameter) DefaultValue(target *core.Target) float64 {
	fresh := core.NewTarget("default", "default", target.Kind, target.Channels)
	band := core.NewBand("default")
	gate := core.GateShapeForStrength(target.Suppression.Gate.Strength)
	switch p {
	case ParameterCutoff:
		return fresh.Highpass.Frequency
	case ParameterBlend:
		return fresh.Suppression.Wet * 100.
	case ParameterGateStrength:
		return fresh.Suppression.Gate.Strength * 100.
	case ParameterGateThreshold:
		return gate.Threshold * 100.
	case ParameterGateReduction:
		return gate.ReductionDB
	case ParameterGateAttack:
		return gate.AttackMs
	case ParameterGateHold:
		return gate.HoldMs
	case ParameterGateRelease:
		return gate.ReleaseMs
	case ParameterGain:
		return fresh.Gain.DB
	case ParameterThreshold:
		return fresh.Compressor.Threshold
	case ParameterRatio:
		return fresh.Compressor.Ratio
	case ParameterAttack:
		return fresh.Compressor.Attack
	case ParameterRelease:
		return fresh.Compressor.Release
	case ParameterMakeup:
		return fresh.Compressor.Makeup
	case ParameterCeiling:
		return fresh.Limiter.Ceiling
	case ParameterLimiterRelease:
		return fresh.Limiter.Release
	case ParameterEQInput:
		return fresh.EQ.InputGainDB
	case ParameterEQOutput:
		return fresh.EQ.OutputGainDB
	case ParameterFrequency:
		return band.Frequency
	case ParameterBandGain:
		return band.Gain
	default: // ParameterQ
		return band.Q
	}
}

func bandAt(target *core.Target, channel core.Channel, band int) *core.Band {
	bands := target.EQ.ChannelMut(channel)
	if band < 0 || band >= len(*bands) {
		return nil
	}
	return &(*bands)[band]
}

// Get reads the parameter's current value from target (using channel and band for EQ band params).
func (p Parameter) Get(target *core.Target, channel core.Channel, band int) float64 {
	switch p {
	case ParameterCutoff:
		return target.Highpass.Frequency
	case ParameterBlend:
		return target.Suppression.Wet * 100.
	case ParameterGateStrength:
		return target.Suppression.Gate.Strength * 100.
	case ParameterGateThreshold:
		return target.Suppression.Gate.Threshold * 100.
	case ParameterGateReduction:
		return target.Suppression.Gate.Reduction
	case ParameterGateAttack:
		return target.Suppression.Gate.Attack
	case ParameterGateHold:
		return target.Suppression.Gate.Hold
	case ParameterGateRelease:
		return target.Suppression.Gate.Release
	case ParameterGain:
		return target.Gain.DB
	case ParameterThreshold:
		return target.Compressor.Threshold
	case ParameterRatio:
		return target.Compressor.Ratio
	case ParameterAttack:
		return target.Compressor.Attack
	case ParameterRelease:
		return target.Compressor.Release
	case ParameterMakeup:
		return target.Compressor.Makeup
	case ParameterCeiling:
		return target.Limiter.Ceiling
	case ParameterLimiterRelease:
		return target.Limiter.Release
	case ParameterEQInput:
		return target.EQ.InputGainDB
	case ParameterEQOutput:
		return target.EQ.OutputGainDB
	}
	bands := target.EQ.Channel(channel)
	in := band >= 0 && band < len(bands)
	switch p {
	case ParameterFrequency:
		if in {
			return bands[band].Frequency
		}
		return 1000.
	case ParameterBandGain:
		if in {
			return bands[band].Gain
		}
		return 0.
	default: // ParameterQ
		if in {
			return bands[band].Q
		}
		return 1.
	}
}

// Set writes value into target; out-of-range EQ band indices are ignored.
func (p Parameter) Set(target *core.Target, channel core.Channel, band int, value float64) {
	switch p {
	case ParameterCutoff:
		target.Highpass.Frequency = value
	case ParameterBlend:
		target.Suppression.Wet = value / 100.
	case ParameterGateStrength:
		target.Suppression.Gate.Strength = value / 100.
	case ParameterGateThreshold:
		target.Suppression.Gate.Threshold = value / 100.
	case ParameterGateReduction:
		target.Suppression.Gate.Reduction = value
	case ParameterGateAttack:
		target.Suppression.Gate.Attack = value
	case ParameterGateHold:
		target.Suppression.Gate.Hold = value
	case ParameterGateRelease:
		target.Suppression.Gate.Release = value
	case ParameterGain:
		target.Gain.DB = value
	case ParameterThreshold:
		target.Compressor.Threshold = value
	case ParameterRatio:
		target.Compressor.Ratio = value
	case ParameterAttack:
		target.Compressor.Attack = value
	case ParameterRelease:
		target.Compressor.Release = value
	case ParameterMakeup:
		target.Compressor.Makeup = value
	case ParameterCeiling:
		target.Limiter.Ceiling = value
	case ParameterLimiterRelease:
		target.Limiter.Release = value
	case ParameterEQInput:
		target.EQ.InputGainDB = value
	case ParameterEQOutput:
		target.EQ.OutputGainDB = value
	case ParameterFrequency:
		if b := bandAt(target, channel, band); b != nil {
			b.Frequency = value
		}
	case ParameterBandGain:
		if b := bandAt(target, channel, band); b != nil {
			b.Gain = value
		}
	case ParameterQ:
		if b := bandAt(target, channel, band); b != nil {
			b.Q = value
		}
	}
}

// ParseValue parses a decimal float from text, rejecting Go's hex float and
// digit-underscore syntax. Callers are responsible for rejecting non-finite values.
func ParseValue(text string) (float64, bool) {
	if strings.ContainsAny(text, "xX_") {
		return 0, false
	}
	value, err := strconv.ParseFloat(text, 64)
	if err != nil {
		// Overflow returns ±Inf, which the caller rejects as non-finite.
		if ne, ok := err.(*strconv.NumError); ok && ne.Err == strconv.ErrRange {
			return value, true
		}
		return 0, false
	}
	return value, true
}

// Parameters returns the ordered list of controls shown for effect.
func Parameters(effect core.Effect, advancedGate, hasBands bool) []Parameter {
	switch effect {
	case core.EffectHighpass:
		return []Parameter{ParameterCutoff}
	case core.EffectSuppression:
		if advancedGate {
			return []Parameter{ParameterBlend, ParameterGateThreshold, ParameterGateReduction,
				ParameterGateAttack, ParameterGateHold, ParameterGateRelease}
		}
		return []Parameter{ParameterBlend, ParameterGateStrength}
	case core.EffectGain:
		return []Parameter{ParameterGain}
	case core.EffectCompressor:
		return []Parameter{ParameterThreshold, ParameterRatio, ParameterAttack, ParameterRelease, ParameterMakeup}
	case core.EffectLimiter:
		return []Parameter{ParameterCeiling, ParameterLimiterRelease}
	default: // core.EffectEQ
		if !hasBands {
			return []Parameter{ParameterEQInput, ParameterEQOutput}
		}
		return []Parameter{ParameterFrequency, ParameterBandGain, ParameterQ, ParameterEQInput, ParameterEQOutput}
	}
}

// Reported holds the mixer's reported latency and active lookahead for a path.
// Either field is nil when no live report is available.
type Reported struct {
	LatencySamples *uint32
	LiveLookahead  *uint8
}

// AfterApplying projects the latency in samples for choice by shifting the
// mixer's reported latency by the hop difference from the active lookahead.
func (r Reported) AfterApplying(choice uint8) (uint32, bool) {
	if r.LiveLookahead == nil || r.LatencySamples == nil {
		return 0, false
	}
	steps := int64(choice) - int64(*r.LiveLookahead)
	return uint32(max(int64(*r.LatencySamples)+steps*int64(core.DFNHopSamples), 0)), true
}

// Relative returns the sample delta between choice and reference (±240 per hop).
func (r Reported) Relative(choice, reference uint8) int32 {
	return int32(int64(choice)-int64(reference)) * int32(core.DFNHopSamples)
}

// LookaheadFigure formats the absolute latency figure next to the lookahead
// selector, or "" when no live report is available.
func LookaheadFigure(reported Reported, draft uint8) string {
	after, ok := reported.AfterApplying(draft)
	if !ok {
		return ""
	}
	ms := float64(after) / 48.
	if reported.LiveLookahead != nil && *reported.LiveLookahead == draft {
		return fmt.Sprintf("%.1f ms", ms)
	}
	return fmt.Sprintf("%.1f ms after applying", ms)
}

// LookaheadCost formats the relative latency label for a lookahead card
// ("now", "+5.0 ms", or "−5.0 ms") against the live setting or draft.
func LookaheadCost(reported Reported, choice, draft uint8) string {
	reference := draft
	if reported.LiveLookahead != nil {
		reference = *reported.LiveLookahead
	}
	relative := reported.Relative(choice, reference)
	if relative == 0 {
		return "now"
	}
	ms := float64(relative) / 48.
	return strings.ReplaceAll(fmt.Sprintf("%+.1f ms", ms), "-", "\u2212")
}

// CompressorOut evaluates the compressor's static transfer curve in dBFS.
func CompressorOut(compressor core.Compressor, db float64) float64 {
	threshold, ratio, makeup := compressor.Threshold, math.Max(compressor.Ratio, 1.), compressor.Makeup
	out := db
	if db >= threshold {
		out = threshold + (db-threshold)/ratio
	}
	return math.Min(out+makeup, 0.)
}

// LiveInputDB converts a linear peak value into a clamped [-60, 0] dBFS level.
func LiveInputDB(peak float64) float64 {
	return math.Min(math.Max(20.*math.Log10(math.Max(peak, 1e-9)), -60.), 0.)
}
