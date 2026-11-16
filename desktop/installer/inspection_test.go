package installer

import (
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/firmware"
)

func TestUnknownArchiveIsRejectedWithoutExtractingAnything(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "unknown.tar.gz")
	put(t, file, []byte("not firmware"))
	catalog, err := firmware.ParseCatalog(string(readConfig(t, "firmware-profiles.json")))
	must(t, err)
	for _, model := range firmware.ModelAll {
		_, err := InspectArchiveWithCatalog(model, file, catalog)
		contains(t, err, "not pinned")
	}
	entries, err := os.ReadDir(dir)
	must(t, err)
	if len(entries) != 1 {
		t.Fatal("something was extracted")
	}
}

// Set RC2FX_DUO_ARCHIVE to an inspected official Duo archive; no device
// needed.
func TestPinnedDuoArchivePassesOfflineOnlyAndFailsAsProIi(t *testing.T) {
	file := realEnv(t, "RC2FX_DUO_ARCHIVE")
	report, err := InspectArchive(firmware.ModelDuo, file)
	must(t, err)
	if !slices.Contains([]string{"rodecaster-duo-1.7.6", "rodecaster-duo-1.7.7"}, report.Profile) || !report.ExecutableVerified || report.RuntimeQualified {
		t.Fatalf("%+v", report)
	}
	if _, err := InspectArchive(firmware.ModelProIi, file); err == nil {
		t.Fatal("a Duo archive passed as Pro II")
	}
}
