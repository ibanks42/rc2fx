package installer

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/internal/tuf"
)

// The release metadata the Go xtask signs (fixtures/xtask) must pass the
// app's own verifier, not only the mixer's (crates/rc2fx-device/tests/
// xtask_tuf_fixtures.rs): the app checks the same repository before it
// offers an in-place update.
func TestTheAppAcceptsEveryRepositoryTheGoSignerWrote(t *testing.T) {
	fixtures := filepath.Join("..", "..", "fixtures", "xtask")
	targets := filepath.Join(fixtures, "targets")
	for _, repo := range []string{"ed25519", "rsa"} {
		root, err := os.ReadFile(filepath.Join(fixtures, repo, "root.json"))
		if err != nil {
			t.Fatal(err)
		}
		for _, metadata := range []string{"metadata", "refreshed"} {
			name := repo + "/" + metadata
			folders := tuf.Folders{Metadata: filepath.Join(fixtures, repo, metadata), Targets: targets}
			r, err := openMixerRepository(context.Background(), folders, root, t.TempDir())
			if err != nil {
				t.Fatalf("%s: %v", name, err)
			}
			signed, err := r.targets()
			if err != nil {
				t.Fatalf("%s: %v", name, err)
			}
			var names []string
			for target := range signed.Targets {
				names = append(names, target)
			}
			slices.Sort(names)
			want := []string{
				"mixer-v1.2.3/THIRD-PARTY-NOTICES.html",
				"mixer-v1.2.3/firmware-profiles.json",
				"mixer-v1.2.3/rc2fx-mixer-1.2.3.payload",
			}
			if !slices.Equal(names, want) {
				t.Fatalf("%s: targets %v, want %v", name, names, want)
			}
		}
	}
}

func TestTheAppRefusesAnAlteredGoSignedRepository(t *testing.T) {
	fixtures := filepath.Join("..", "..", "fixtures", "xtask")
	root, err := os.ReadFile(filepath.Join(fixtures, "ed25519", "root.json"))
	if err != nil {
		t.Fatal(err)
	}
	altered := t.TempDir()
	source := filepath.Join(fixtures, "ed25519", "metadata")
	entries, err := os.ReadDir(source)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		data, err := os.ReadFile(filepath.Join(source, entry.Name()))
		if err != nil {
			t.Fatal(err)
		}
		if entry.Name() == "targets.json" {
			changed := bytes.Replace(data, []byte(`"1.0.1"`), []byte(`"1.0.2"`), 1)
			if bytes.Equal(changed, data) {
				t.Fatal("the fixture's targets.json no longer names 1.0.1; update this test")
			}
			data = changed
		}
		if err := os.WriteFile(filepath.Join(altered, entry.Name()), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	folders := tuf.Folders{Metadata: altered, Targets: filepath.Join(fixtures, "targets")}
	if _, err := openMixerRepository(context.Background(), folders, root, t.TempDir()); err == nil {
		t.Fatal("an altered targets.json was accepted")
	}
	// The other repository's root signs none of these.
	other, err := os.ReadFile(filepath.Join(fixtures, "rsa", "root.json"))
	if err != nil {
		t.Fatal(err)
	}
	folders.Metadata = source
	if _, err := openMixerRepository(context.Background(), folders, other, t.TempDir()); err == nil {
		t.Fatal("metadata was accepted under another repository's root")
	}
}
