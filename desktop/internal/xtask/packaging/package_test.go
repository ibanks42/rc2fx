package packaging

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

var scripts = []string{"rc2fx.sh", "rc2fx-shim.sh", "rc2fx-gui-shim.sh", "install.sh"}

func put(t *testing.T, root, name string, data []byte) {
	t.Helper()
	path := filepath.Join(root, filepath.FromSlash(name))
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o666); err != nil {
		t.Fatal(err)
	}
}

func sha(data []byte) string { return task.Sha256(data) }

// fixture creates a temp checkout with a fake mixer build, scripts, and a
// synthetic executable embedding that payload.
func fixture(t *testing.T) (string, []byte) {
	t.Helper()
	root := t.TempDir()
	catalog := []byte(`{"schemaVersion":1,"profiles":[]}`)
	elf := make([]byte, 64)
	copy(elf, []byte{127, 'E', 'L', 'F', 2, 1})
	elf[18] = 0xb7
	var sums strings.Builder
	for _, name := range []string{"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware"} {
		put(t, root, "target/mixer/"+name, elf)
		fmt.Fprintf(&sums, "%s  %s\n", sha(elf), name)
	}
	put(t, root, "target/mixer/firmware-profiles.json", catalog)
	fmt.Fprintf(&sums, "%s  firmware-profiles.json\n", sha(catalog))
	for _, name := range []string{"librc2fx_gui.so", "librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"} {
		put(t, root, "target/mixer/"+name, elf)
		fmt.Fprintf(&sums, "%s  %s\n", sha(elf), name)
	}
	release := []byte(`{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}`)
	put(t, root, "target/mixer/mixer-release.json", release)
	fmt.Fprintf(&sums, "%s  mixer-release.json\n", sha(release))
	put(t, root, "target/mixer/SHA256SUMS", []byte(sums.String()))
	for _, name := range scripts {
		script := []byte("#!/bin/sh\r\nexit 0\r\n")
		if name == "rc2fx.sh" {
			script = []byte("#!/bin/bash\r\nrc2fx_build=development\r\nexit 0\r\n")
		}
		put(t, root, "mixer/scripts/"+name, script)
	}
	put(t, root, "config/firmware-profiles.json", catalog)
	put(t, root, "config/mixer-release.json", release)
	return root, syntheticExecutable(t, root, false)
}

// syntheticExecutable builds a dummy binary wrapping the expected embedded
// payload so packaging tests don't need a full GUI build.
func syntheticExecutable(t *testing.T, root string, release bool) []byte {
	t.Helper()
	envelope, err := embed.Expected(root, release)
	if err != nil {
		t.Fatal(err)
	}
	exe := []byte("executable prefix\x00")
	exe = append(exe, envelope.AsBytes()...)
	return append(exe, []byte("\x00executable suffix")...)
}

func TestDownloadOnlyPackageNeedsNoRuntimeAndRejectsABundledPayload(t *testing.T) {
	exe := []byte("executable\x00RC2FX-PAYLOAD-MODE:download:1\x00")
	files, err := collectWith(t.TempDir(), "windows", exe, nil, true)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(files["rc2fx.exe"].Data, exe) {
		t.Fatal("the executable changed")
	}
	root, embedded := fixture(t)
	embedded = append(embedded, exe...)
	if err := validateExecutable(root, embedded, true); err == nil || !strings.Contains(err.Error(), "unexpectedly") {
		t.Fatalf("%v", err)
	}
}

func TestReleaseAndDevelopmentExecutablesKeepTheirEmbeddedPolicy(t *testing.T) {
	for _, release := range []bool{false, true} {
		root, _ := fixture(t)
		exe := syntheticExecutable(t, root, release)
		files, err := collectWith(root, "windows", exe, nil, release)
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(files["rc2fx.exe"].Data, exe) {
			t.Fatal("packaging must not rewrite the executable")
		}
		envelope, err := embed.Find(files["rc2fx.exe"].Data)
		if err != nil {
			t.Fatal(err)
		}
		bundle, err := envelope.Bundle()
		if err != nil {
			t.Fatal(err)
		}
		kind := "development"
		if release {
			kind = "release"
		}
		if !strings.Contains(string(bundle["rc2fx.sh"]), "rc2fx_build="+kind+"\n") {
			t.Fatalf("%s: %q", kind, bundle["rc2fx.sh"])
		}
	}
}

func TestPackagingRefusesOldCorruptStaleAndWrongPolicyExecutables(t *testing.T) {
	root, exe := fixture(t)
	if _, err := collectWith(root, "windows", []byte("old app"), nil, false); err == nil || !strings.Contains(err.Error(), "embed") {
		t.Fatalf("%v", err)
	}
	corrupt := bytes.Clone(exe)
	offset := bytes.Index(corrupt, []byte("rc2fx_build=development"))
	corrupt[offset] ^= 1
	if _, err := collectWith(root, "windows", corrupt, nil, false); err == nil {
		t.Fatal("a corrupt executable was packaged")
	}
	if _, err := collectWith(root, "windows", exe, nil, true); err == nil || !strings.Contains(err.Error(), "rc2fx.sh") {
		t.Fatalf("%v", err)
	}
	scriptPath := filepath.Join(root, "mixer", "scripts", "rc2fx.sh")
	script, _ := os.ReadFile(scriptPath)
	script = append(script, []byte("\n# changed after desktop compilation\n")...)
	if err := os.WriteFile(scriptPath, script, 0o666); err != nil {
		t.Fatal(err)
	}
	if _, err := collectWith(root, "windows", exe, nil, false); err == nil || !strings.Contains(err.Error(), "rc2fx.sh") {
		t.Fatalf("%v", err)
	}
}

func packageFor(t *testing.T, system string) (string, Files) {
	t.Helper()
	root, exe := fixture(t)
	files, err := collectWith(root, system, exe, []licenseFile{
		{"rust-crates.html", []byte("<html>")},
		{"LICENSE-MIT", []byte("MIT terms")},
	}, false)
	if err != nil {
		t.Fatal(err)
	}
	return root, files
}

func TestPackageHasSelfContainedExecutableAndLicenses(t *testing.T) {
	root, files := packageFor(t, "windows")
	if files["rc2fx.exe"].Mode != 0o755 {
		t.Fatal("mode")
	}
	if _, ok := files["licenses/rust-crates.html"]; !ok {
		t.Fatal("licenses")
	}
	if string(files["licenses/LICENSE-MIT"].Data) != "MIT terms" {
		t.Fatal("license text")
	}
	// Installer inputs must stay embedded, never shipped as sidecar files.
	for _, forbidden := range []string{"config/", "crates/", "docs/", "experiments/", "runtime/", "xtask/", "mixer/", "desktop/", "README"} {
		for name := range files {
			if strings.HasPrefix(name, forbidden) {
				t.Errorf("%s leaked into archive: %s", forbidden, name)
			}
		}
	}
	envelope, err := embed.Find(files["rc2fx.exe"].Data)
	if err != nil {
		t.Fatal(err)
	}
	bundle, err := envelope.Bundle()
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range installer.PayloadNames {
		if _, ok := bundle[name]; !ok {
			t.Errorf("%s", name)
		}
	}
	folder := filepath.Join(root, "out", top)
	if err := writeFolder(folder, files); err != nil {
		t.Fatal(err)
	}
	copied, err := os.ReadFile(filepath.Join(folder, "rc2fx.exe"))
	if err != nil {
		t.Fatal(err)
	}
	found, err := embed.Find(copied)
	if err != nil {
		t.Fatal(err)
	}
	if err := found.RequireMatches(envelope); err != nil {
		t.Fatal(err)
	}
	// Missing source payload files must fail rather than reusing a stale binary.
	if err := os.Remove(filepath.Join(root, "target", "mixer", "rc2fxd")); err != nil {
		t.Fatal(err)
	}
	if _, err := collectWith(root, "windows", copied, nil, false); err == nil {
		t.Fatal("a stale payload was packaged")
	}
}

func TestPlatformNames(t *testing.T) {
	for target, want := range map[string][2]string{
		"x86_64-pc-windows-msvc":   {"windows", "x64"},
		"aarch64-apple-darwin":     {"macos", "universal"},
		"x86_64-unknown-linux-gnu": {"linux", "x64"},
	} {
		system, arch, err := platform(target)
		if err != nil || system != want[0] || arch != want[1] {
			t.Errorf("%s: %s %s %v", target, system, arch, err)
		}
	}
	if _, _, err := platform("riscv64gc-unknown-linux-gnu"); err == nil {
		t.Fatal("riscv64 accepted")
	}
}

// The test binary itself lacks -tags velopack, so checkGoBuild must reject it.
func TestPackagesRefuseGoBuildsWithoutVelopack(t *testing.T) {
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(self)
	if err != nil {
		t.Fatal(err)
	}
	if err := checkGoBuild(data); err == nil || !strings.Contains(err.Error(), "-tags") {
		t.Fatalf("%v", err)
	}
	if err := checkGoBuild([]byte("not a Go program")); err == nil {
		t.Fatal("a non-Go executable was accepted")
	}
}

func TestTheAppShipsOnlyVelopackOnWindowsBesideIt(t *testing.T) {
	for goos, want := range map[string]string{
		"linux":   "",
		"darwin":  "",
		"windows": "velopack_libc.dll",
	} {
		if got := strings.Join(shippedNames(goos), ","); got != want {
			t.Errorf("%s: %s", goos, got)
		}
	}
	// Resource script must omit type-24 manifest because package gk embeds its own.
	script := resourceScript(`C:\r\rc2fx.ico`, "1.2.3")
	if !strings.Contains(script, "FILEVERSION 1,2,3,0") || !strings.Contains(script, `1 ICON "C:\\r\\rc2fx.ico"`) ||
		strings.Contains(script, "1 24 ") ||
		!strings.Contains(script, `VALUE "ProductVersion", "1.2.3"`) {
		t.Fatal(script)
	}
}
