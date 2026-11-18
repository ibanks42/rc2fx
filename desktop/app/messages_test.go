package app

import (
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Which messages are progress steps and which are outcomes.
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
		mixerChangedElsewhere,
	} {
		if progressMessage(outcome) {
			t.Errorf("%s", outcome)
		}
	}
}

// A failed save is an outcome, so it is toasted whatever its reason says,
// and says it failed.
func TestAFailedSaveIsToasted(t *testing.T) {
	for _, reason := range []string{"", "HTTP 500: config write failed", "timed out waiting for the mixer…"} {
		message := saveFailedMessage(reason)
		if progressMessage(message) {
			t.Errorf("%q reads as progress", message)
		}
		if reason != "" && message != "Couldn't save: "+reason {
			t.Errorf("saveFailedMessage(%q) = %q", reason, message)
		}
	}
}

// Settings save on their own once edits stop: no text in the app may send the
// user to the "Save to mixer" button, which is gone.
func TestNoTextPointsToTheRemovedSaveButton(t *testing.T) {
	err := filepath.WalkDir(".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() || !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		source, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if strings.Contains(string(source), "Save to mixer") {
			t.Errorf("%s mentions the removed Save to mixer button", path)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}
