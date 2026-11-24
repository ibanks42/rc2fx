package appupdate

import (
	"bytes"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/tuf"
	"github.com/ibanks42/rc2fx/desktop/tuf/tuftest"
)

const testPackage = "io.github.ibanks42.rc2fx-1.2.3-full.nupkg"

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func put(t *testing.T, path string, data []byte) {
	t.Helper()
	must(t, os.MkdirAll(filepath.Dir(path), 0o755))
	must(t, os.WriteFile(path, data, 0o644))
}

type appFixture struct {
	t    *testing.T
	dir  string
	repo *tuftest.Repo
}

func newAppFixture(t *testing.T) *appFixture {
	dir := t.TempDir()
	repo := tuftest.NewRepo(t, dir)
	put(t, filepath.Join(dir, testPackage), []byte("signed update package"))
	return &appFixture{t: t, dir: dir, repo: repo}
}

func (f *appFixture) signWith(name, version string, versions [3]int64, expires string) {
	f.repo.Sign([]tuftest.Target{{Name: name, File: filepath.Join(f.dir, testPackage), Custom: map[string]any{"version": version}}}, versions, expires)
}

func (f *appFixture) sign(versions [3]int64) {
	f.signWith("v1.2.3/"+testPackage, "1.2.3", versions, "2099-01-01T00:00:00Z")
}

// Separate file from the repo target so rejected downloads can be deleted.
func (f *appFixture) download(data []byte) string {
	path := filepath.Join(f.dir, "downloaded.nupkg")
	put(f.t, path, data)
	return path
}

func (f *appFixture) verify(version, asset, file string) *VerifyError {
	return verifyAppPackage(f.repo.Trusted, tuf.Folders{Metadata: filepath.Join(f.dir, "metadata"), Targets: f.dir}, version, asset, file)
}

func signedApp(t *testing.T) *appFixture {
	f := newAppFixture(t)
	f.sign([3]int64{2, 2, 2})
	return f
}

func TestConfiguredProductionRootIsValid(t *testing.T) {
	if !AppUpdatesConfigured() {
		return
	}
	trusted, err := tuf.NewTrusted(trustedRoot)
	must(t, err)
	// Release assets keep fixed names under their tags.
	if trusted.Root().ConsistentSnapshot {
		t.Fatal("consistent snapshots")
	}
}

func TestACorrectlySignedPackageIsAccepted(t *testing.T) {
	f := signedApp(t)
	file := f.download([]byte("signed update package"))
	if err := f.verify("1.2.3", testPackage, file); err != nil {
		t.Fatal(err)
	}
	if !common.PathExists(file) {
		t.Fatal("removed")
	}
}

func TestAPackageWithTheWrongBytesOrLengthIsRejectedAndRemoved(t *testing.T) {
	f := signedApp(t)
	for _, data := range [][]byte{[]byte("signed update packagf"), []byte("signed update package!"), {}} {
		file := f.download(data)
		err := f.verify("1.2.3", testPackage, file)
		if err == nil || !err.Rejected || err.Message != updateMismatch {
			t.Fatalf("%q: %+v", data, err)
		}
		if common.PathExists(file) {
			t.Fatal("a rejected download is discarded")
		}
	}
}

func TestAnUnsignedReleaseOrAssetIsRejectedAndSaysToTryAgainLater(t *testing.T) {
	f := signedApp(t)
	file := f.download([]byte("signed update package"))
	for _, c := range [][2]string{
		{"1.2.4", testPackage},
		{"1.2.3", "releases.osx.json"},
		{"1.2.3", "io.github.ibanks42.rc2fx-1.2.4-full.nupkg"},
	} {
		err := f.verify(c[0], c[1], file)
		if err == nil || !err.Rejected || err.Message != updateUnsigned {
			t.Fatalf("%v: %+v", c, err)
		}
	}
	if !common.PathExists(file) {
		t.Fatal("nothing was compared, so the caller decides")
	}
}

func TestAnOlderDownloadThanTheSignedReleaseSaysItWasReplaced(t *testing.T) {
	f := signedApp(t)
	file := f.download([]byte("signed update package"))
	err := f.verify("1.2.2", testPackage, file)
	if err == nil || !err.Rejected || err.Message != updateSuperseded {
		t.Fatalf("%+v", err)
	}
}

func TestATargetWhoseSignedVersionDiffersFromItsNameIsRejected(t *testing.T) {
	f := newAppFixture(t)
	f.signWith("v1.2.3/"+testPackage, "1.2.4", [3]int64{2, 2, 2}, "2099-01-01T00:00:00Z")
	err := f.verify("1.2.3", testPackage, f.download([]byte("signed update package")))
	if err == nil || !err.Rejected || !strings.Contains(err.Message, "does not describe") {
		t.Fatalf("%+v", err)
	}
}

func TestMetadataSignedByAKeyOutsideTheRootIsRejected(t *testing.T) {
	f := signedApp(t)
	file := f.download([]byte("signed update package"))
	if err := f.verify("1.2.3", testPackage, file); err != nil {
		t.Fatal(err)
	}
	// Swap in newer metadata signed by an untrusted key.
	impostor := newAppFixture(t)
	impostor.sign([3]int64{3, 3, 3})
	for _, role := range []string{"targets.json", "snapshot.json", "timestamp.json"} {
		data, err := os.ReadFile(filepath.Join(impostor.dir, "metadata", role))
		must(t, err)
		put(t, filepath.Join(f.dir, "metadata", role), data)
	}
	err := f.verify("1.2.3", testPackage, file)
	if err == nil || err.Rejected || !strings.HasPrefix(err.Message, "Could not check") {
		t.Fatalf("%+v", err)
	}
	if !common.PathExists(file) {
		t.Fatal("removed")
	}
}

func TestExpiredMetadataIsNotTrusted(t *testing.T) {
	f := newAppFixture(t)
	f.signWith("v1.2.3/"+testPackage, "1.2.3", [3]int64{2, 2, 2}, "2001-01-01T00:00:00Z")
	file := f.download([]byte("signed update package"))
	if err := f.verify("1.2.3", testPackage, file); err == nil || err.Rejected {
		t.Fatalf("%+v", err)
	}
	if !common.PathExists(file) {
		t.Fatal("removed")
	}
}

func TestOlderRolesMixedIntoNewerMetadataAreRefused(t *testing.T) {
	f := newAppFixture(t)
	meta := filepath.Join(f.dir, "metadata")
	check := func() *VerifyError {
		return f.verify("1.2.3", testPackage, f.download([]byte("signed update package")))
	}
	f.sign([3]int64{5, 5, 5})
	old := func(role string) []byte {
		data, err := os.ReadFile(filepath.Join(meta, role))
		must(t, err)
		return data
	}
	oldTargets, oldSnapshot := old("targets.json"), old("snapshot.json")
	f.sign([3]int64{6, 6, 6})
	if err := check(); err != nil {
		t.Fatal(err)
	}
	// Timestamp and snapshot pin downstream role versions within a single load.
	put(t, filepath.Join(meta, "targets.json"), oldTargets)
	if err := check(); err == nil || err.Rejected {
		t.Fatalf("%+v", err)
	}
	f.sign([3]int64{6, 6, 6})
	put(t, filepath.Join(meta, "snapshot.json"), oldSnapshot)
	if err := check(); err == nil || err.Rejected {
		t.Fatalf("%+v", err)
	}
}

func TestEveryCheckStartsFreshAndLeavesNoState(t *testing.T) {
	f := newAppFixture(t)
	check := func() *VerifyError {
		return f.verify("1.2.3", testPackage, f.download([]byte("signed update package")))
	}
	f.sign([3]int64{5, 5, 5})
	if err := check(); err != nil {
		t.Fatal(err)
	}
	// Stateless checks accept lower metadata versions (see package comment).
	f.sign([3]int64{4, 4, 4})
	if err := check(); err != nil {
		t.Fatal(err)
	}
}

func TestInvalidVersionsAndAssetNamesAreRefusedBeforeAnyFetch(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "package")
	put(t, file, []byte("x"))
	for _, c := range [][2]string{
		{"1.2.3", ""},
		{"1.2.3", "../SHA256SUMS"},
		{"1.2.3", "a/b"},
		{"1.2.3", `a\b`},
		{"1.2.3", ".hidden"},
		{"one", testPackage},
	} {
		err := verifyAppPackage([]byte("null"), tuf.Folders{Metadata: filepath.Join(dir, "nonexistent"), Targets: filepath.Join(dir, "nonexistent")}, c[0], c[1], file)
		if err == nil || !err.Rejected || !strings.Contains(err.Message, "is not an app update package") {
			t.Fatalf("%v: %+v", c, err)
		}
	}
	if common.PathExists(filepath.Join(dir, "nonexistent")) {
		t.Fatal("fetched")
	}
}

func TestAppMetadataComesOnlyFromItsOwnBranchOnTheOfficialHosts(t *testing.T) {
	metadataBase, err := url.Parse(appMetadataURL)
	must(t, err)
	targetsBase, err := url.Parse(appTargetsURL)
	must(t, err)
	must(t, checkAppRepositoryURL(metadataBase, appMetadataHost))
	must(t, checkAppRepositoryURL(targetsBase, appTargetsHost))
	// File names are joined onto the base, so the branch path must survive.
	joined, err := tuf.Join(metadataBase, "timestamp.json")
	must(t, err)
	if joined.String() != "https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/timestamp.json" {
		t.Fatal(joined)
	}
	joined, err = tuf.Join(targetsBase, "v1.2.3/rc2fx-linux-x64.AppImage")
	must(t, err)
	if joined.String() != "https://github.com/ibanks42/rc2fx/releases/download/v1.2.3/rc2fx-linux-x64.AppImage" {
		t.Fatal(joined)
	}
	for _, bad := range []string{
		"http://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/",
		"https://raw.githubusercontent.com.example.com/x/",
		"https://raw.githubusercontent.com:8443/x/",
		"https://user@raw.githubusercontent.com/x/",
	} {
		u, err := url.Parse(bad)
		must(t, err)
		if checkAppRepositoryURL(u, appMetadataHost) == nil {
			t.Fatalf("%s accepted", bad)
		}
	}
}

func TestVerifyUpdatePackageReturnsAVerifyError(t *testing.T) {
	file := filepath.Join(t.TempDir(), "x")
	put(t, file, []byte("x"))
	err := VerifyUpdatePackage("one", testPackage, file)
	var verifyError *VerifyError
	if !errors.As(err, &verifyError) || !verifyError.Rejected {
		t.Fatalf("%v", err)
	}
	if !bytes.Contains([]byte(err.Error()), []byte("not an app update package")) {
		t.Fatal(err)
	}
}
