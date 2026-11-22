package release

// Verifies that the Go TUF release signer produces deterministic Ed25519
// repositories and fresh RSA-3072 repositories that load cleanly (initial and
// refreshed) without relying on committed fixtures that expire over time.

import (
	"bytes"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

func TestXtaskSigningRepositories(t *testing.T) {
	dir := t.TempDir()
	includeRSA := runtime.GOOS != "windows"
	if err := GenerateTestRepos(dir, includeRSA); err != nil {
		t.Fatal(err)
	}

	// Re-signing the same release with fixed Ed25519 keys must match byte-for-byte.
	again := t.TempDir()
	if err := SignTestEd25519(dir, again); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{
		"root.json",
		"metadata/1.root.json", "metadata/targets.json", "metadata/snapshot.json", "metadata/timestamp.json",
		"refreshed/targets.json", "refreshed/snapshot.json", "refreshed/timestamp.json",
	} {
		want := mustRead(t, filepath.Join(dir, "ed25519", filepath.FromSlash(name)))
		got := mustRead(t, filepath.Join(again, filepath.FromSlash(name)))
		if !bytes.Equal(got, want) {
			t.Errorf("ed25519/%s: signer output is not deterministic", name)
		}
	}

	repos := []string{"ed25519"}
	if includeRSA {
		repos = append(repos, "rsa")
	}
	const payloadRel = "mixer-v1.2.3/rc2fx-mixer-1.2.3.payload"
	wantPayload := mustRead(t, filepath.Join(dir, "targets", filepath.FromSlash(payloadRel)))
	for _, repo := range repos {
		rootBytes := mustRead(t, filepath.Join(dir, repo, "root.json"))
		for _, metadata := range []string{"metadata", "refreshed"} {
			loaded, err := loadRepository(rootBytes, filepath.Join(dir, repo, metadata), filepath.Join(dir, "targets"), time.Now())
			if err != nil {
				t.Fatalf("%s/%s: %v", repo, metadata, err)
			}
			gotPayload, err := loaded.ReadTarget(payloadRel)
			if err != nil {
				t.Fatalf("%s/%s: %v", repo, metadata, err)
			}
			if !bytes.Equal(gotPayload, wantPayload) {
				t.Fatalf("%s/%s: payload mismatch", repo, metadata)
			}
			wantSeq := map[string]uint64{"metadata": 1, "refreshed": 2}[metadata]
			if loaded.Timestamp.Version != wantSeq || loaded.Targets.Version != 1 {
				t.Fatalf("%s/%s: timestamp %d targets %d", repo, metadata, loaded.Timestamp.Version, loaded.Targets.Version)
			}
		}
	}
	if !bytes.Equal(
		mustRead(t, filepath.Join(dir, "ed25519", "metadata", "targets.json")),
		mustRead(t, filepath.Join(dir, "ed25519", "refreshed", "targets.json")),
	) {
		t.Fatal("renewal changed targets")
	}
}
