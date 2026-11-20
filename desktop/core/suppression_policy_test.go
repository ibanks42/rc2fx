package core

import (
	"strings"
	"testing"
)

// Suppression eligibility and single-target budget tests (mirrors
// crates/rc2fx-core/tests/suppression_policy.rs).

func enabledTarget(id string, kind TargetKind, channels uint8) Target {
	target := NewTarget(id, id, kind, channels)
	target.Suppression.Enabled = true
	return target
}

func TestOneMonoMicOrChatVoiceMixIsEligible(t *testing.T) {
	for _, target := range []Target{
		enabledTarget("analog-1", TargetKindMic, 1),
		enabledTarget("mic-1", TargetKindMic, 1),
		enabledTarget("send-chat", TargetKindOutput, 2),
		enabledTarget("duo-send-chat", TargetKindOutput, 2),
	} {
		check(t, target.SupportsSuppression(), target.ID)
		ok(t, target.Validate())
	}
	for _, target := range []Target{
		enabledTarget("headphones-1", TargetKindOutput, 2),
		enabledTarget("usb-chat", TargetKindPlayback, 2),
		enabledTarget("send-chat", TargetKindOutput, 1),
		enabledTarget("stereo-mic", TargetKindMic, 2),
	} {
		check(t, !target.SupportsSuppression(), target.ID)
		fails(t, target.Validate(), target.ID)
	}
}

func TestSavedEnableBudgetIsGlobalEvenWhenBypassedOrDeselected(t *testing.T) {
	first := enabledTarget("analog-1", TargetKindMic, 1)
	for _, second := range []Target{
		enabledTarget("mic-1", TargetKindMic, 1),
		enabledTarget("send-chat", TargetKindOutput, 2),
	} {
		config := DefaultConfig()
		config.Targets = []Target{first.Clone(), second}
		config.SelectedTargets = []string{}
		config.Targets[1].Bypass = true
		err := config.Validate()
		check(t, err != nil && strings.Contains(err.Error(), "only one"), err)
		config.Targets[0].Suppression.Enabled = false
		ok(t, config.Validate())
	}
}
