package model

import (
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// ActiveChannel is the channel edits go to: channel when target has an
// independent right EQ, else the left one.
func ActiveChannel(target *core.Target, channel core.Channel) core.Channel {
	if target.Channels == 2 && target.EQ.RightBands != nil {
		return channel
	}
	return core.ChannelLeft
}

// Split gives a stereo target an independent right EQ, starting as a copy of
// the left.
func Split(target *core.Target) {
	if target.Channels == 2 && target.EQ.RightBands == nil {
		right := slices.Clone(target.EQ.Bands)
		target.EQ.RightBands = &right
	}
}

// Link makes both channels share one EQ, keeping keep's bands.
func Link(target *core.Target, keep core.Channel) {
	if keep == core.ChannelRight && target.EQ.RightBands != nil {
		target.EQ.Bands = *target.EQ.RightBands
	}
	target.EQ.RightBands = nil
}

// PlaceBand moves the band at from to to, shifting the bands between, as
// dropping one band chip on another does; returns where it landed.
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

// PlaceEffect moves effect to where onto is in the visible chain, shifting
// the stages between them, as dropping one stage card on another does.
// Stages this target cannot use keep their places.
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

// SuppressionBlocker keeps the single enabled destination invariant in the
// draft as well as the controller: the target other than id, in targets and
// then saved (nil when there is none), that has suppression enabled.
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

// ReleaseSuppression explicitly releases a saved or hidden owner's slot in
// the local draft only. The saved owner still blocks a new enable until this
// edit is applied.
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

// EnableEffect turns effect on or off for targets[selected]; false when it
// may not be changed. saved is nil when nothing is saved.
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
