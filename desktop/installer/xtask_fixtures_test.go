package installer

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/tuf"
)

func generateLiveTUFRepos(t *testing.T) (string, []string) {
	t.Helper()
	dir := t.TempDir()
	args := []string{"run", ".", "tuf-test-repos", dir}
	repos := []string{"ed25519", "rsa"}
	if runtime.GOOS == "windows" {
		args = append(args, "--no-rsa")
		repos = []string{"ed25519"}
	}
	cmd := exec.Command("go", args...)
	cmd.Dir = filepath.Join("..", "..", "tools", "xtask")
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("generate live TUF repos: %v\n%s", err, out)
	}
	return dir, repos
}

// Verify that live TUF repositories signed by Go xtask pass the desktop verifier
// as well as the mixer's (crates/rc2fx-device/tests/xtask_tuf_fixtures.rs).
func TestTheAppAcceptsEveryRepositoryTheGoSignerWrote(t *testing.T) {
	dir, repos := generateLiveTUFRepos(t)
	targets := filepath.Join(dir, "targets")
	for _, repo := range repos {
		root, err := os.ReadFile(filepath.Join(dir, repo, "root.json"))
		if err != nil {
			t.Fatal(err)
		}
		for _, metadata := range []string{"metadata", "refreshed"} {
			name := repo + "/" + metadata
			folders := tuf.Folders{Metadata: filepath.Join(dir, repo, metadata), Targets: targets}
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
	dir, repos := generateLiveTUFRepos(t)
	root, err := os.ReadFile(filepath.Join(dir, "ed25519", "root.json"))
	if err != nil {
		t.Fatal(err)
	}
	altered := t.TempDir()
	source := filepath.Join(dir, "ed25519", "metadata")
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
				t.Fatal("targets.json no longer names 1.0.1; update this test")
			}
			data = changed
		}
		if err := os.WriteFile(filepath.Join(altered, entry.Name()), data, 0o644); err != nil {
			t.Fatal(err)
		}
	}
	folders := tuf.Folders{Metadata: altered, Targets: filepath.Join(dir, "targets")}
	if _, err := openMixerRepository(context.Background(), folders, root, t.TempDir()); err == nil {
		t.Fatal("an altered targets.json was accepted")
	}
	if slices.Contains(repos, "rsa") {
		other, err := os.ReadFile(filepath.Join(dir, "rsa", "root.json"))
		if err != nil {
			t.Fatal(err)
		}
		folders.Metadata = source
		if _, err := openMixerRepository(context.Background(), folders, other, t.TempDir()); err == nil {
			t.Fatal("metadata was accepted under another repository's root")
		}
	}
}
