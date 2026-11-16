package app

import "testing"

// lib.rs message_tests.
func TestProgressStepsAreNotToastedButOutcomesAre(t *testing.T) {
	for _, step := range []string{
		"Saving settings to mixer…",
		"Saved to mixer • waiting for applied telemetry",
		"Path change accepted • waiting for the mixer's result",
		"Requesting processing paths. Local effect edits are kept; saved effects will be used.",
		"Loading preset on the mixer…",
		"Collecting mixer diagnostics…",
	} {
		if !progressMessage(step) {
			t.Errorf("%s", step)
		}
	}
	for _, outcome := range []string{
		"Could not import EQ: Unsupported filter type: Allpass",
		"Saved settings file to /tmp/a.json",
		"Preset ‘Podcast’ saved on the mixer",
		"Enter a preset name of 1–128 bytes.",
		"Mixer settings changed elsewhere. Keep your draft or reload the mixer settings.",
	} {
		if progressMessage(outcome) {
			t.Errorf("%s", outcome)
		}
	}
}
