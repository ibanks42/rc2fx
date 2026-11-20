package installer

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestAppVersionMatchesConfigAppRelease(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "config", "app-release.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cfg struct {
		SchemaVersion int    `json:"schemaVersion"`
		Version       string `json:"version"`
	}
	if err := json.Unmarshal(data, &cfg); err != nil {
		t.Fatal(err)
	}
	if cfg.SchemaVersion != 1 {
		t.Fatalf("schemaVersion = %d, want 1", cfg.SchemaVersion)
	}
	if cfg.Version != AppVersion {
		t.Fatalf("installer.AppVersion = %q, config/app-release.json has %q", AppVersion, cfg.Version)
	}
}
