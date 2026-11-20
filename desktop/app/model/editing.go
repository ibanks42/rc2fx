package model

import (
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// ActiveChannel returns channel when target uses split stereo EQ, or ChannelLeft otherwise.
func ActiveChannel(target *core.Target, channel core.Channel) core.Channel {
	if target.Channels == 2 && target.EQ.RightBands != nil {
		return channel
	}
	return core.ChannelLeft
}

// Split enables independent left/right EQ on a stereo target by cloning the left bands.
func Split(target *core.Target) {
	if target.Channels == 2 && target.EQ.RightBands == nil {
		right := slices.Clone(target.EQ.Bands)
		target.EQ.RightBands = &right
	}
}

// Link collapses a split EQ back into a shared one, keeping keep's bands.
func Link(target *core.Target, keep core.Channel) {
	if keep == core.ChannelRight && target.EQ.RightBands != nil {
		target.EQ.Bands = *target.EQ.RightBands
	}
	target.EQ.RightBands = nil
}

// PlaceBand moves the band at index from to index to, shifting intermediate bands,
// and returns the resulting index.
func PlaceBand(eq *core.EQSettings, channel core.Channel, from, to int) int {
	bands := eq.ChannelMut(channel)
	if from < 0 || to < 0 || from >= len(*bands) || to >= len(*bands) {
		return from
	}
	band := (*bands)[from]
	*bands = slices.Delete(*bands, from, from+1)
	*bands = slices.Insert(*bands, to, band)
	return to
}

// PlaceEffect reorders effect to onto's position among the target's visible
// stages, leaving unsupported/hidden stages in their existing slots.
func PlaceEffect(target *core.Target, effect, onto core.Effect) {
	if target.IsChatOutput() && (effect == core.EffectSuppression || onto == core.EffectSuppression) {
		return
	}
	var slots []int
	for i, e := range target.Order {
		if (target.SupportsSuppression() && !target.IsChatOutput()) || e != core.EffectSuppression {
			slots = append(slots, i)
		}
	}
	visible := make([]core.Effect, 0, len(slots))
	for _, i := range slots {
		visible = append(visible, target.Order[i])
	}
	from := slices.Index(visible, effect)
	to := slices.Index(visible, onto)
	if from < 0 || to < 0 {
		return
	}
	moved := visible[from]
	visible = slices.Delete(visible, from, from+1)
	visible = slices.Insert(visible, to, moved)
	for k, slot := range slots {
		target.Order[slot] = visible[k]
	}
}

// SuppressionBlocker returns another target (in draft targets, then saved) that
// currently owns voice suppression, enforcing the single-destination limit.
func SuppressionBlocker(targets, saved []core.Target, id string) *core.Target {
	for _, list := range [][]core.Target{targets, saved} {
		for i := range list {
			if list[i].ID != id && list[i].Suppression.Enabled {
				return &list[i]
			}
		}
	}
	return nil
}

// ReleaseSuppression disables suppression for ownerID in the local draft.
// Until applied, the saved state still blocks enabling suppression elsewhere.
func ReleaseSuppression(targets []core.Target, ownerID string) bool {
	for i := range targets {
		if targets[i].ID == ownerID {
			changed := targets[i].Suppression.Enabled
			targets[i].Suppression.Enabled = false
			return changed
		}
	}
	return false
}

// EnableEffect toggles effect on targets[selected], returning false if the
// change violates suppression ownership or target capabilities.
func EnableEffect(targets, saved []core.Target, selected int, effect core.Effect, enabled bool) bool {
	if selected < 0 || selected >= len(targets) {
		return false
	}
	target := &targets[selected]
	if effect == core.EffectSuppression &&
		enabled &&
		(!target.SupportsSuppression() || SuppressionBlocker(targets, saved, target.ID) != nil) {
		return false
	}
	targets[selected].SetEffectEnabled(effect, enabled)
	return true
}
