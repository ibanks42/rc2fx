package release

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

// testRelease is a downloaded release: two assets and their SHA256SUMS.
func testRelease(t *testing.T, dir string) string {
	t.Helper()
	assets := filepath.Join(dir, "assets")
	var list strings.Builder
	for _, a := range []struct{ name, data string }{
		{"io.github.ibanks42.rc2fx-1.2.3-full.nupkg", "package"},
		{"rc2fx-linux-x64.AppImage", "appimage"},
	} {
		mustWrite(t, filepath.Join(assets, a.name), []byte(a.data))
		fmt.Fprintf(&list, "%s  %s\n", task.Sha256([]byte(a.data)), a.name)
	}
	mustWrite(t, filepath.Join(assets, sums), []byte(list.String()))
	return assets
}

func signInto(dir, out, version, targetsKey string) error {
	path := func(name string) string { return filepath.Join(dir, name) }
	return AppSign([]string{
		path("root.json"), path("assets"), version, path(out), "2", weekAhead(),
		path(targetsKey), path("snapshot.pk8"), path("timestamp.pk8"),
	})
}

func TestEveryAssetButTheSumsBecomesAVersionedTarget(t *testing.T) {
	dir := t.TempDir()
	testRoot(t, dir)
	testRelease(t, dir)
	if err := signInto(dir, "signed", "1.2.3", "targets.pk8"); err != nil {
		t.Fatal(err)
	}
	repository, err := tufsign.Load(mustRead(t, filepath.Join(dir, "root.json")), filepath.Join(dir, "signed"), dir, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for name := range repository.Targets.Entries {
		names = append(names, name)
	}
	sort.Strings(names)
	want := []string{"v1.2.3/io.github.ibanks42.rc2fx-1.2.3-full.nupkg", "v1.2.3/rc2fx-linux-x64.AppImage"}
	if strings.Join(names, ",") != strings.Join(want, ",") {
		t.Fatalf("%v", names)
	}
	for name, target := range repository.Targets.Entries {
		if v, _ := target.Custom.Get("version"); !v.Equal(core.StringValue("1.2.3")) {
			t.Fatalf("%s: version %s", name, v.String())
		}
	}
	appimage := repository.Targets.Entries["v1.2.3/rc2fx-linux-x64.AppImage"]
	if appimage.Length != 8 || task.Sha256([]byte("appimage")) != hex.EncodeToString(appimage.SHA256) {
		t.Fatalf("%+v", appimage)
	}
	if !repository.Targets.Expires.Equal(repository.Root.Expires) {
		t.Fatal("targets must last as long as the root")
	}
	if err := signInto(dir, "signed", "1.2.3", "targets.pk8"); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("%v", err)
	}
}

func TestSigningRefusesAssetsTheSumsDoNotVouchFor(t *testing.T) {
	dir := t.TempDir()
	testRoot(t, dir)
	assets := testRelease(t, dir)
	refused := func(out string) string {
		err := signInto(dir, out, "1.2.3", "targets.pk8")
		if err == nil {
			t.Fatalf("%s: signed", out)
		}
		return err.Error()
	}
	mustWrite(t, filepath.Join(assets, "rc2fx-linux-x64.AppImage"), []byte("replaced"))
	if e := refused("a"); !strings.Contains(e, "does not match") {
		t.Fatal(e)
	}
	mustWrite(t, filepath.Join(assets, "rc2fx-linux-x64.AppImage"), []byte("appimage"))
	mustWrite(t, filepath.Join(assets, "extra.zip"), []byte("extra"))
	if e := refused("b"); !strings.Contains(e, "not listed") {
		t.Fatal(e)
	}
	_ = os.Remove(filepath.Join(assets, "extra.zip"))
	_ = os.Remove(filepath.Join(assets, "rc2fx-linux-x64.AppImage"))
	if e := refused("c"); !strings.Contains(e, "missing") {
		t.Fatal(e)
	}
	_ = os.Remove(filepath.Join(assets, sums))
	if e := refused("d"); !strings.Contains(e, sums) {
		t.Fatal(e)
	}
	for _, out := range []string{"a", "b", "c", "d"} {
		if exists(filepath.Join(dir, out)) {
			t.Fatalf("%s written", out)
		}
	}
}

func TestSigningRefusesRootOrMisassignedKeysAndUnstableVersions(t *testing.T) {
	dir := t.TempDir()
	testRoot(t, dir)
	testRelease(t, dir)
	if err := signInto(dir, "a", "1.2.3", "root.pk8"); err == nil || !strings.Contains(err.Error(), "root private keys") {
		t.Fatalf("%v", err)
	}
	if err := signInto(dir, "b", "1.2.3", "snapshot.pk8"); err == nil || !strings.Contains(err.Error(), "not authorized") {
		t.Fatalf("%v", err)
	}
	for _, version := range []string{"1.2", "1.2.3-rc1", "01.2.3", "v1.2.3", "1.2.3/x"} {
		if err := signInto(dir, "c", version, "targets.pk8"); err == nil || !strings.Contains(err.Error(), "not a stable app version") {
			t.Fatalf("%s: %v", version, err)
		}
	}
}

func TestRenewalKeepsTheSignedAppTargets(t *testing.T) {
	dir := t.TempDir()
	testRoot(t, dir)
	testRelease(t, dir)
	if err := signInto(dir, "signed", "1.2.3", "targets.pk8"); err != nil {
		t.Fatal(err)
	}
	path := func(name string) string { return filepath.Join(dir, name) }
	args := []string{path("root.json"), path("signed"), path("renewed"), "3", weekAhead(), path("snapshot.pk8"), path("timestamp.pk8")}
	if err := AppRefresh(args[:6]); err == nil || !strings.Contains(err.Error(), "app-refresh") {
		t.Fatalf("%v", err)
	}
	if err := AppRefresh(args); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(mustRead(t, path("signed/targets.json")), mustRead(t, path("renewed/targets.json"))) {
		t.Fatal("targets changed")
	}
}

func TestAssetNamesStayOnePathSegment(t *testing.T) {
	for _, name := range []string{"rc2fx-linux-x64.AppImage", "releases.linux.json", "io.github.ibanks42.rc2fx-1.2.3-full.nupkg"} {
		if !plain(name) {
			t.Errorf("%s", name)
		}
	}
	for _, name := range []string{"", ".hidden", "a/b", `a\b`, "..", "a b"} {
		if plain(name) {
			t.Errorf("%q", name)
		}
	}
}
