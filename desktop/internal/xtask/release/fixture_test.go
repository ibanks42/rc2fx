package release

// Maintains fixtures/xtask, which the Rust TUF verifier tests against
// (crates/rc2fx-device/tests/xtask_tuf_fixtures.rs). Both repositories sign
// targets/mixer-v1.2.3 with metadata/ (seq 1) and refreshed/ (seq 2):
//   - ed25519/: deterministic keys/expiries; re-signed on every test run to
//     catch serialization drift
//   - rsa/: production RSA-3072 / RSA-PSS keys from mixer-keys (randomized sigs)
//
// Regenerate from desktop/ with:
//
//	RC2FX_WRITE_XTASK_FIXTURES=1 go test ./internal/xtask/release -run TestXtaskFixtures

import (
	"bytes"
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/x509"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

func fixtureDir() string { return filepath.Join(task.Root(), "fixtures", "xtask") }

const (
	fixtureRelease = "targets/mixer-v1.2.3"
	fixturePayload = "mixer-v1.2.3/rc2fx-mixer-1.2.3.payload"
	ed25519Root    = "2099-01-01T00:00:00Z"
	ed25519Expiry  = "2098-01-01T00:00:00Z"
	ed25519Renewal = "2097-06-01T00:00:00Z"
)

// fixtureKeys writes deterministic test-only Ed25519 keys seeded by role name.
func fixtureKeys(t *testing.T, dir string) map[string]tufsign.Signer {
	t.Helper()
	signers := map[string]tufsign.Signer{}
	for _, role := range allRoles {
		seed := sha256.Sum256([]byte("rc2fx xtask fixture " + role))
		der, err := x509.MarshalPKCS8PrivateKey(ed25519.NewKeyFromSeed(seed[:]))
		if err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(dir, role+".pk8")
		mustWrite(t, path, der)
		signer, err := tufsign.LoadKey(path)
		if err != nil {
			t.Fatal(err)
		}
		signers[role] = signer
	}
	return signers
}

func signEd25519(t *testing.T, targets, out string) {
	t.Helper()
	keys := t.TempDir()
	signers := fixtureKeys(t, keys)
	var public []tufsign.Key
	for _, role := range allRoles {
		public = append(public, signers[role].Key())
	}
	signed, err := tufsign.NewRoot(mustTime(t, ed25519Root), allRoles, public)
	if err != nil {
		t.Fatal(err)
	}
	root, err := tufsign.RootFromSigned(signed)
	if err != nil {
		t.Fatal(err)
	}
	rootFile, err := root.Sign(tufsign.RoleRoot, signed, []tufsign.Signer{signers[tufsign.RoleRoot]})
	if err != nil {
		t.Fatal(err)
	}
	mustWrite(t, filepath.Join(out, "root.json"), rootFile)
	key := func(role string) string { return filepath.Join(keys, role+".pk8") }
	if err := Sign([]string{
		filepath.Join(out, "root.json"), filepath.Join(targets, fixtureRelease), filepath.Join(out, "metadata"),
		"1", ed25519Expiry, key(tufsign.RoleTargets), key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		t.Fatal(err)
	}
	if err := Refresh([]string{
		filepath.Join(out, "root.json"), filepath.Join(out, "metadata"), filepath.Join(out, "refreshed"),
		"2", ed25519Renewal, key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		t.Fatal(err)
	}
}

func signRSA(t *testing.T, targets, out string) {
	t.Helper()
	keys := filepath.Join(t.TempDir(), "keys")
	if err := createKeys(keys, task.Root()); err != nil {
		t.Fatal(err)
	}
	mustWrite(t, filepath.Join(out, "root.json"), mustRead(t, filepath.Join(keys, "public", "root.json")))
	key := func(role string) string { return filepath.Join(keys, "release", role+".pem") }
	if err := Sign([]string{
		filepath.Join(out, "root.json"), filepath.Join(targets, fixtureRelease), filepath.Join(out, "metadata"),
		"1", tufsign.DisplayTimestamp(time.Now().Add(7 * 86400 * time.Second)),
		key(tufsign.RoleTargets), key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		t.Fatal(err)
	}
	if err := Refresh([]string{
		filepath.Join(out, "root.json"), filepath.Join(out, "metadata"), filepath.Join(out, "refreshed"),
		"2", tufsign.DisplayTimestamp(time.Now().Add(6 * 86400 * time.Second)),
		key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		t.Fatal(err)
	}
}

func writeFixtures(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("mixer-keys needs Unix permissions")
	}
	dir := fixtureDir()
	base := t.TempDir()
	fixturePackage(t, base)
	for _, name := range []string{"rc2fx-mixer-1.2.3.payload", "firmware-profiles.json", "mixer-compatibility.json", "THIRD-PARTY-NOTICES.html"} {
		mustWrite(t, filepath.Join(dir, fixtureRelease, name), mustRead(t, filepath.Join(base, fixtureRelease, name)))
	}
	for _, repo := range []string{"ed25519", "rsa"} {
		if err := os.RemoveAll(filepath.Join(dir, repo)); err != nil {
			t.Fatal(err)
		}
	}
	signEd25519(t, dir, filepath.Join(dir, "ed25519"))
	signRSA(t, dir, filepath.Join(dir, "rsa"))
}

func loadFixture(t *testing.T, repo, metadata string, now time.Time) *loadedRepository {
	t.Helper()
	dir := fixtureDir()
	loaded, err := loadRepository(mustRead(t, filepath.Join(dir, repo, "root.json")),
		filepath.Join(dir, repo, metadata), filepath.Join(dir, "targets"), now)
	if err != nil {
		t.Fatalf("%s/%s: %v", repo, metadata, err)
	}
	payload, err := loaded.ReadTarget(fixturePayload)
	if err != nil {
		t.Fatalf("%s/%s: %v", repo, metadata, err)
	}
	if !bytes.Equal(payload, mustRead(t, filepath.Join(dir, "targets", fixturePayload))) {
		t.Fatal("payload")
	}
	return loaded
}

func TestXtaskFixtures(t *testing.T) {
	if os.Getenv("RC2FX_WRITE_XTASK_FIXTURES") == "1" {
		writeFixtures(t)
	}
	dir := fixtureDir()
	if _, err := os.Stat(filepath.Join(dir, "ed25519", "metadata", "targets.json")); err != nil {
		t.Fatalf("fixtures/xtask is not generated yet: run RC2FX_WRITE_XTASK_FIXTURES=1 go test ./internal/xtask/release -run TestXtaskFixtures (%v)", err)
	}
	// Re-signing the committed release with the fixed Ed25519 keys must match byte-for-byte.
	again := t.TempDir()
	signEd25519(t, dir, again)
	for _, name := range []string{
		"root.json",
		"metadata/1.root.json", "metadata/targets.json", "metadata/snapshot.json", "metadata/timestamp.json",
		"refreshed/targets.json", "refreshed/snapshot.json", "refreshed/timestamp.json",
	} {
		want := mustRead(t, filepath.Join(dir, "ed25519", filepath.FromSlash(name)))
		got := mustRead(t, filepath.Join(again, filepath.FromSlash(name)))
		if !bytes.Equal(got, want) {
			t.Errorf("ed25519/%s: the signer no longer writes the committed bytes", name)
		}
	}
	for _, metadata := range []string{"metadata", "refreshed"} {
		loaded := loadFixture(t, "ed25519", metadata, time.Now())
		if want := map[string]uint64{"metadata": 1, "refreshed": 2}[metadata]; loaded.Timestamp.Version != want || loaded.Targets.Version != 1 {
			t.Fatalf("ed25519/%s: timestamp %d targets %d", metadata, loaded.Timestamp.Version, loaded.Targets.Version)
		}
		// Use epoch 0 for the committed RSA fixture since its discovery metadata
		// only lives for a week after generation.
		loadFixture(t, "rsa", metadata, time.Unix(0, 0))
	}
	if !bytes.Equal(mustRead(t, filepath.Join(dir, "ed25519", "metadata", "targets.json")),
		mustRead(t, filepath.Join(dir, "ed25519", "refreshed", "targets.json"))) {
		t.Fatal("renewal changed targets")
	}
}
