package app

// The desk's helpers that do not draw: effect names, signal order and stage
// summaries. This file does not import gk: its
// functions are tested with plain `go test`.

import (
	"fmt"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// shortEffect is a short effect name for the channel list.
func shortEffect(effect core.Effect) string {
	switch effect {
	case core.EffectHighpass:
		return "HPF"
	case core.EffectSuppression:
		return "Noise"
	case core.EffectEQ:
		return "EQ"
	case core.EffectCompressor:
		return "Comp"
	case core.EffectGain:
		return "Gain"
	case core.EffectLimiter:
		return "Limiter"
	}
	return string(effect)
}

// effectIndex is `effect as usize`: the effect's place in core.EffectAll, for
// element ids.
func effectIndex(effect core.Effect) int {
	if i := slices.Index(core.EffectAll[:], effect); i >= 0 {
		return i
	}
	return len(core.EffectAll)
}

// visibleEffects is the effects in signal order, without suppression on
// targets that cannot use it.
func visibleEffects(target *core.Target) []core.Effect {
	effects := make([]core.Effect, 0, len(target.Order)+1)
	for _, effect := range target.Order {
		if target.SupportsSuppression() || effect != core.EffectSuppression {
			effects = append(effects, effect)
		}
	}
	if target.IsChatOutput() {
		// The voice branch is before the stereo sum, not a movable output stage.
		effects = slices.DeleteFunc(effects, func(e core.Effect) bool { return e == core.EffectSuppression })
		effects = slices.Insert(effects, 0, core.EffectSuppression)
	}
	return effects
}

// stageSummary is a stage's key value in the flow lane.
func stageSummary(target *core.Target, effect core.Effect, channel core.Channel) string {
	switch effect {
	case core.EffectSuppression:
		gate := ""
		if target.Suppression.Gate.Active() {
			gate = " · gate"
		}
		return fmt.Sprintf("%.0f%% blend%s", target.Suppression.Wet*100, gate)
	case core.EffectGain:
		return fmt.Sprintf("%+.2f dB", target.Gain.DB)
	case core.EffectHighpass:
		return formatHz(target.Highpass.Frequency)
	case core.EffectEQ:
		count := len(target.EQ.Channel(channel))
		plural := "s"
		if count == 1 {
			plural = ""
		}
		return fmt.Sprintf("%d band%s", count, plural)
	case core.EffectCompressor:
		return fmt.Sprintf("%.0f dB · %.1f:1", target.Compressor.Threshold, target.Compressor.Ratio)
	case core.EffectLimiter:
		return fmt.Sprintf("%.1f dBFS", target.Limiter.Ceiling)
	}
	return ""
}
