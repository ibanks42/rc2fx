package core

import "testing"

func TestLiveNeedsSave(t *testing.T) {
	base := func() []Target {
		return []Target{{ID: "mic-1", Kind: TargetKindMic, Channels: 1,
			Order: []Effect{EffectHighpass, EffectSuppression, EffectEQ}}}
	}
	cases := []struct {
		name string
		edit func(ts []Target)
		want bool
	}{
		{"same", func([]Target) {}, false},
		{"eq gain", func(ts []Target) { ts[0].Gain.DB = 3 }, false},
		{"order without suppression", func(ts []Target) {
			ts[0].Order = []Effect{EffectEQ, EffectHighpass, EffectSuppression}
		}, false},
		{"suppression on", func(ts []Target) { ts[0].Suppression.Enabled = true }, true},
		{"channels", func(ts []Target) { ts[0].Channels = 2 }, true},
	}
	for _, c := range cases {
		running, candidate := base(), base()
		c.edit(candidate)
		if got := LiveNeedsSave(running, candidate); got != c.want {
			t.Errorf("%s: got %v, want %v", c.name, got, c.want)
		}
	}
	// Suppression already on: its lookahead and the order need a save.
	running, candidate := base(), base()
	running[0].Suppression.Enabled, candidate[0].Suppression.Enabled = true, true
	candidate[0].Order = []Effect{EffectEQ, EffectHighpass, EffectSuppression}
	if !LiveNeedsSave(running, candidate) {
		t.Error("order with suppression on: want a save")
	}
	candidate = base()
	candidate[0].Suppression.Enabled, candidate[0].Suppression.Lookahead = true, 1
	if !LiveNeedsSave(running, candidate) {
		t.Error("lookahead with suppression on: want a save")
	}
}
