package model

import (
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func findTarget(targets []core.Target, id string) *core.Target {
	for i := range targets {
		if targets[i].ID == id {
			return &targets[i]
		}
	}
	return nil
}

func TestHiddenOwnerNeedsExplicitReleaseAndSavedAckBeforeReassignment(t *testing.T) {
	config := core.DefaultConfig()
	config.SelectedTargets = []string{"mic-2"}
	config.Targets = append(config.Targets, core.NewTarget("mic-2", "Mic 1", core.TargetKindMic, 1))
	owner := findTarget(config.Targets, "mic-1")
	owner.Suppression.Enabled = true
	saved := deepCopy(config.Targets)
	before := deepCopy(*findTarget(saved, "mic-1"))
	draft := deepCopy(saved)
	// Identity, not the original index or a fader position, locates the owner.
	slices.Reverse(draft)
	next := slices.IndexFunc(draft, func(t core.Target) bool { return t.ID == "mic-2" })
	if config.IsSelected("mic-1") {
		t.Fatal("mic-1 is selected")
	}
	if blocker := SuppressionBlocker(draft, saved, "mic-2"); blocker == nil || blocker.ID != "mic-1" {
		t.Fatalf("blocker %v", blocker)
	}
	if !ReleaseSuppression(draft, "mic-1") {
		t.Fatal("not released")
	}
	expected := before
	expected.Suppression.Enabled = false
	if !equal(*findTarget(draft, "mic-1"), expected) {
		t.Fatal("release changed more than suppression")
	}
	if ReleaseSuppression(draft, "mic-1") {
		t.Fatal("released twice")
	}
	if ReleaseSuppression(draft, "missing") {
		t.Fatal("released a missing target")
	}
	if EnableEffect(draft, saved, next, core.EffectSuppression, true) {
		t.Fatal("enabled while the saved owner holds it")
	}
	// A successful Save acknowledges the disabled owner.
	acknowledged := deepCopy(draft)
	if !EnableEffect(draft, acknowledged, next, core.EffectSuppression, true) {
		t.Fatal("not enabled after the acknowledgement")
	}
}

func TestSuppressionDraftHasOneOwnerAndChatVoiceIsBeforeTheStereoChain(t *testing.T) {
	targets := []core.Target{
		core.NewTarget("mic-1", "Mic", core.TargetKindMic, 1),
		core.NewTarget("send-chat", "Chat", core.TargetKindOutput, 2),
		core.NewTarget("headphones-1", "Phones", core.TargetKindOutput, 2),
	}
	if !EnableEffect(targets, nil, 0, core.EffectSuppression, true) {
		t.Fatal("mic-1 not enabled")
	}
	saved := deepCopy(targets)
	if EnableEffect(targets, nil, 1, core.EffectSuppression, true) {
		t.Fatal("a second owner")
	}
	if !EnableEffect(targets, saved, 0, core.EffectSuppression, false) {
		t.Fatal("not disabled")
	}
	if EnableEffect(targets, saved, 1, core.EffectSuppression, true) {
		t.Fatal("apply the disable before claiming a new live destination")
	}
	saved = deepCopy(targets)
	if !EnableEffect(targets, saved, 1, core.EffectSuppression, true) {
		t.Fatal("send-chat not enabled")
	}
	if EnableEffect(targets, nil, 2, core.EffectSuppression, true) {
		t.Fatal("headphones enabled")
	}
	before := slices.Clone(targets[1].Order)
	PlaceEffect(&targets[1], core.EffectSuppression, core.EffectLimiter)
	if !slices.Equal(targets[1].Order, before) {
		t.Fatal("a pre-mix voice stage cannot be dragged into stereo output effects")
	}
}

func bandIDs(t *core.Target) []string {
	var ids []string
	for _, b := range t.EQ.Bands {
		ids = append(ids, b.ID)
	}
	return ids
}

func TestSplitEditReorderLinkPreservesSelectedChannelAndIDs(t *testing.T) {
	target := deepCopy(core.DefaultConfig().Targets[4])
	for range 3 {
		if _, err := target.EQ.AddBand(core.ChannelLeft); err != nil {
			t.Fatal(err)
		}
	}
	original := deepCopy(target.EQ.Bands)
	Split(&target)
	(*target.EQ.ChannelMut(core.ChannelRight))[0].Gain = -7.5
	selected := PlaceBand(&target.EQ, core.ChannelRight, 0, 1)
	if selected != 1 {
		t.Fatalf("selected %d", selected)
	}
	if got := target.EQ.Channel(core.ChannelRight)[1].ID; got != original[0].ID {
		t.Fatalf("id %q", got)
	}
	if !equal(target.EQ.Bands, original) {
		t.Fatal("left bands changed")
	}
	Link(&target, core.ChannelRight)
	if target.EQ.RightBands != nil {
		t.Fatal("still split")
	}
	if got := target.EQ.Bands[1].Gain; got != -7.5 {
		t.Fatalf("gain %v", got)
	}
	if got := ActiveChannel(&target, core.ChannelRight); got != core.ChannelLeft {
		t.Fatalf("channel %v", got)
	}
	// Dropping across several bands shifts the ones between.
	order := bandIDs(&target)
	if got := PlaceBand(&target.EQ, core.ChannelLeft, 0, 2); got != 2 {
		t.Fatalf("placed at %d", got)
	}
	if got := bandIDs(&target); !slices.Equal(got, []string{order[1], order[2], order[0]}) {
		t.Fatalf("ids %q", got)
	}
	if got := PlaceBand(&target.EQ, core.ChannelLeft, 2, 0); got != 0 {
		t.Fatalf("placed at %d", got)
	}
	if got := bandIDs(&target); !slices.Equal(got, order) {
		t.Fatalf("and back: %q", got)
	}
	if got := PlaceBand(&target.EQ, core.ChannelLeft, 0, 9); got != 0 {
		t.Fatalf("placed at %d", got)
	}
	if got := bandIDs(&target); !slices.Equal(got, order) {
		t.Fatalf("out of range is ignored: %q", got)
	}
}

func TestModelChangeAndVisibleEffectOrderStayValid(t *testing.T) {
	target := deepCopy(core.DefaultConfig().Targets[4])
	band, err := target.EQ.AddBand(core.ChannelLeft)
	if err != nil {
		t.Fatal(err)
	}
	band.BandType = core.BandTypeHighShelf
	SetModel(band, core.BandModeRlcBt)
	if band.Mode != core.BandModeRlcBt {
		t.Fatalf("mode %v", band.Mode)
	}
	PlaceEffect(&target, core.EffectEQ, core.EffectHighpass)
	if target.Order[0] != core.EffectEQ || target.Order[1] != core.EffectSuppression || target.Order[2] != core.EffectHighpass {
		t.Fatalf("order %v", target.Order)
	}
	if err := target.Validate(); err != nil {
		t.Fatal(err)
	}
	// Dropping across several stages shifts the ones between.
	before := slices.Clone(target.Order)
	PlaceEffect(&target, core.EffectLimiter, core.EffectEQ)
	if target.Order[0] != core.EffectLimiter {
		t.Fatalf("order %v", target.Order)
	}
	if target.Order[1] != core.EffectSuppression {
		t.Fatalf("hidden stage stays put: %v", target.Order)
	}
	if target.Order[2] != core.EffectEQ {
		t.Fatalf("order %v", target.Order)
	}
	last := target.Order[len(target.Order)-1]
	PlaceEffect(&target, core.EffectLimiter, last)
	if !slices.Equal(target.Order, before) {
		t.Fatalf("and back: %v", target.Order)
	}
	if err := target.Validate(); err != nil {
		t.Fatal(err)
	}
	mono := deepCopy(core.DefaultConfig().Targets[0])
	Split(&mono)
	if mono.EQ.RightBands != nil {
		t.Fatal("a mono target split")
	}
}
