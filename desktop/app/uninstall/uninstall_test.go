package uninstall

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestOnlyRC2FXDataAndItsOwnKeyPairAreTargeted(t *testing.T) {
	for _, target := range targets() {
		if !strings.HasSuffix(target, "RC2FX") &&
			!strings.HasSuffix(target, "rc2fx_rodecaster_pro_ii") &&
			!strings.HasSuffix(target, "rc2fx_rodecaster_pro_ii.pub") {
			t.Fatalf("unexpected target %s", target)
		}
	}
}

func TestRemovingWhatIsAlreadyGoneSucceeds(t *testing.T) {
	dir := t.TempDir()
	if err := remove(filepath.Join(dir, "missing")); err != nil {
		t.Fatal(err)
	}
	folder := filepath.Join(dir, "RC2FX")
	if err := os.MkdirAll(filepath.Join(folder, "installer"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(folder, "view.json"), []byte("{}"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := remove(folder); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(folder); !os.IsNotExist(err) {
		t.Fatal("folder still there")
	}
}
