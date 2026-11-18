package app

import (
	"os"
	"path/filepath"
	"testing"
)

// connection.rs tests.

func TestOnlyARefusedOpenOffersTheUSBRule(t *testing.T) {
	if !usbPermission("USB permission required: Permission denied (os error 13)") {
		t.Fatal("a refused open does not offer the rule")
	}
	for _, other := range []string{
		"",
		"Couldn't open the mixer's USB control: No such file or directory (os error 2)",
		"The mixer is connected, but RC2FX isn't running on it",
	} {
		if usbPermission(other) {
			t.Errorf("offered the rule for %q", other)
		}
	}
}

func TestFoundCopyRequiresTheSpecificDeviceDiscoveryOutcome(t *testing.T) {
	if !mixerFound("The mixer is connected, but RC2FX isn't running on it") {
		t.Fatal("the discovery outcome is not found")
	}
	for _, other := range []string{
		"",
		"timed out",
		"No RØDECaster Pro II connected",
		"More than one RØDECaster Pro II is connected; connect only one",
	} {
		if mixerFound(other) {
			t.Errorf("found for %q", other)
		}
	}
}

func viewString(t *testing.T, view viewPrefs, key string) string {
	t.Helper()
	v, ok := view[key]
	if !ok {
		t.Fatalf("%s missing", key)
	}
	s, ok := v.AsString()
	if !ok {
		t.Fatalf("%s is not a string", key)
	}
	return s
}

func TestExperimentalDuoIsOffByDefaultAndPersistsWithoutLosingViewSettings(t *testing.T) {
	path := filepath.Join(t.TempDir(), "RC2FX/view.json")
	if duoEnabled(readView(path)) {
		t.Fatal("on by default")
	}
	if err := saveDuo(path, true); err != nil {
		t.Fatal(err)
	}
	if !duoEnabled(readView(path)) {
		t.Fatal("not saved")
	}
	if err := os.WriteFile(path, []byte(`{"channelFilter":"outputs","showTelemetry":true,"experimentalDuoFlash":true}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := saveDuo(path, false); err != nil {
		t.Fatal(err)
	}
	view := readView(path)
	if duoEnabled(view) {
		t.Fatal("not turned off")
	}
	if got := viewString(t, view, "channelFilter"); got != "outputs" {
		t.Fatalf("channelFilter = %q", got)
	}
	if !view.boolean("showTelemetry") {
		t.Fatal("showTelemetry lost")
	}
	for _, invalid := range []string{
		`{"experimentalDuoFlash":"true"}`,
		`{"experimentalDuoFlash":1}`,
		"broken",
		"[]",
	} {
		if err := os.WriteFile(path, []byte(invalid), 0o644); err != nil {
			t.Fatal(err)
		}
		if duoEnabled(readView(path)) {
			t.Errorf("on for %s", invalid)
		}
	}
}

func TestDeveloperModeIsOffByDefaultAndPersistsAlone(t *testing.T) {
	path := filepath.Join(t.TempDir(), "RC2FX/view.json")
	if developerEnabled(readView(path)) {
		t.Fatal("on by default")
	}
	if err := saveDuo(path, true); err != nil {
		t.Fatal(err)
	}
	if err := saveFlag(path, "developerMode", true); err != nil {
		t.Fatal(err)
	}
	view := readView(path)
	if !developerEnabled(view) || !duoEnabled(view) {
		t.Fatal("not both on")
	}
	if err := saveFlag(path, "developerMode", false); err != nil {
		t.Fatal(err)
	}
	if developerEnabled(readView(path)) {
		t.Fatal("not turned off")
	}
	if err := os.WriteFile(path, []byte(`{"developerMode":"yes"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if developerEnabled(readView(path)) {
		t.Fatal(`on for "yes"`)
	}
}

func TestAFailedDeveloperSaveIsReported(t *testing.T) {
	blocker := filepath.Join(t.TempDir(), "file")
	if err := os.WriteFile(blocker, nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if saveFlag(filepath.Join(blocker, "view.json"), "developerMode", true) == nil {
		t.Fatal("saved under a file")
	}
}

// The Rust wrote view.json through serde_json's BTreeMap: keys sorted,
// compact. The file must stay byte-identical so either app reads the other's.
func TestTheViewFileIsWrittenWithSortedKeys(t *testing.T) {
	path := filepath.Join(t.TempDir(), "RC2FX/view.json")
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(`{"showTelemetry":true,"appearance":"black"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := saveFlag(path, "developerMode", true); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"appearance":"black","developerMode":true,"showTelemetry":true}`; string(got) != want {
		t.Fatalf("%s, want %s", got, want)
	}
}
