package packaging

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// macOS

func TestBundleMovesTheExecutableIntoAnApp(t *testing.T) {
	files := Files{"rc2fx": {Data: []byte("binary"), Mode: 0o755}}
	files, err := bundle(files, "1.2.3")
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := files["rc2fx"]; ok {
		t.Fatal("the executable stayed outside the bundle")
	}
	executable := files["RC2FX.app/Contents/MacOS/rc2fx"]
	if string(executable.Data) != "binary" || executable.Mode != 0o755 {
		t.Fatalf("%+v", executable)
	}
	plist := string(files["RC2FX.app/Contents/Info.plist"].Data)
	if !strings.Contains(plist, "<key>CFBundleShortVersionString</key><string>1.2.3</string>") ||
		!strings.Contains(plist, "<key>CFBundleExecutable</key><string>rc2fx</string>") {
		t.Fatal(plist)
	}
	if !bytes.HasPrefix(files["RC2FX.app/Contents/Resources/rc2fx.icns"].Data, []byte("icns")) {
		t.Fatal("icon")
	}
}

func TestBundleNeedsTheExecutable(t *testing.T) {
	if _, err := bundle(Files{}, "1.2.3"); err == nil {
		t.Fatal("bundled nothing")
	}
}

// Velopack

func testTarget(system, arch string) target {
	return target{os: system, arch: arch, version: "1.2.3", name: "rc2fx-x"}
}

func TestPublishedNamesFollowTheProjectAndTheFeedKeepsVpkNames(t *testing.T) {
	var windows []string
	for _, o := range testTarget("windows", "x64").outputs() {
		windows = append(windows, o.publishAs)
	}
	if !slices.Equal(windows, []string{"rc2fx-x-setup.exe", "rc2fx-x.zip", "io.github.ibanks42.rc2fx-1.2.3-full.nupkg", "releases.win.json"}) {
		t.Fatalf("%v", windows)
	}
	macos := testTarget("macos", "universal").outputs()
	if macos[0] != (output{"io.github.ibanks42.rc2fx-osx-Setup.pkg", "rc2fx-x.pkg"}) ||
		macos[2].publishAs != "io.github.ibanks42.rc2fx-1.2.3-osx-full.nupkg" || macos[3].publishAs != "releases.osx.json" {
		t.Fatalf("%v", macos)
	}
	if r := testTarget("macos", "universal").runtime(); r != "osx" {
		t.Fatal(r)
	}
	linux := testTarget("linux", "x64").outputs()
	if !slices.Equal(linux, []output{
		{"io.github.ibanks42.rc2fx.AppImage", "rc2fx-x.AppImage"},
		{"io.github.ibanks42.rc2fx-1.2.3-linux-full.nupkg", "io.github.ibanks42.rc2fx-1.2.3-linux-full.nupkg"},
		{"releases.linux.json", "releases.linux.json"},
	}) {
		t.Fatalf("%v", linux)
	}
	if testTarget("linux", "x64").runtime() != "linux-x64" || testTarget("windows", "x64").runtime() != "win-x64" {
		t.Fatal("runtimes")
	}
}

func TestTheFeedMustDescribeExactlyThisPackage(t *testing.T) {
	feed := func(version, sha string) []byte {
		return []byte(`{"Assets":[{"PackageId":"` + PackID + `","Version":"` + version + `","Type":"Full","FileName":"p.nupkg","SHA1":"x","SHA256":"` + sha + `","Size":1}]}`)
	}
	if err := checkFeed(feed("1.2.3", "ABCD"), "1.2.3", "p.nupkg", "abcd"); err != nil {
		t.Fatal(err)
	}
	if checkFeed(feed("1.2.4", "ABCD"), "1.2.3", "p.nupkg", "abcd") == nil ||
		checkFeed(feed("1.2.3", "ABCE"), "1.2.3", "p.nupkg", "abcd") == nil ||
		checkFeed([]byte(`{"Assets":[]}`), "1.2.3", "p.nupkg", "abcd") == nil {
		t.Fatal("a feed for another package was accepted")
	}
}

func TestUnsignedMacOSBuildsAreStillSealedAdHoc(t *testing.T) {
	// Skip if macOS signing env vars are configured on this machine.
	for _, name := range []string{"RC2FX_MACOS_IDENTITY", "RC2FX_MACOS_INSTALLER_IDENTITY", "RC2FX_NOTARY_PROFILE", "RC2FX_MACOS_KEYCHAIN"} {
		if _, ok := setting(name); ok {
			return
		}
	}
	args, err := signingArgs("macos")
	if err != nil || !slices.Equal(args, []string{"--signAppIdentity", "-"}) {
		t.Fatalf("%v %v", args, err)
	}
}

func TestLinuxPacksTheAppDirUnsignedWithNoDeltas(t *testing.T) {
	if args, err := signingArgs("linux"); err != nil || len(args) != 0 {
		t.Fatalf("%v %v", args, err)
	}
	args := packArgs("/r", testTarget("linux", "x64"), "/w/rc2fx.AppDir", "/s")
	joined := strings.Join(args, " ")
	if !strings.Contains(joined, "--packDir /w/rc2fx.AppDir") || !strings.Contains(joined, "--runtime linux-x64") ||
		!strings.Contains(joined, "--delta None") || strings.Contains(joined, "--icon") || strings.Contains(joined, "--sign") {
		t.Fatal(joined)
	}
	if !strings.Contains(joined, "--mainExe rc2fx") || strings.Contains(joined, "--mainExe rc2fx.exe") {
		t.Fatal(joined)
	}
}

func TestWindowsPacksAnExeWithAStartMenuShortcutAndNoDeltas(t *testing.T) {
	joined := strings.Join(packArgs("/r", testTarget("windows", "x64"), "/p", "/s"), " ")
	for _, want := range []string{"--packId io.github.ibanks42.rc2fx", "--mainExe rc2fx.exe", "--shortcuts StartMenuRoot", "--delta None", "--runtime win-x64"} {
		if !strings.Contains(joined, want) {
			t.Errorf("%s missing: %s", want, joined)
		}
	}
	if strings.Contains(joined, "--signEntitlements") {
		t.Fatal(joined)
	}
}

// AppImage

func fakeTools(t *testing.T, dir string) tools {
	t.Helper()
	linuxdeploy := filepath.Join(dir, "linuxdeploy")
	script := "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"$0.args\"\nprintf '%s\\n' \"${NO_STRIP:-}\" > \"$0.env\"\nappdir=\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --appdir ]; then appdir=$2; fi\n  shift\ndone\nmkdir -p \"$appdir/usr/lib\"\nprintf host > \"$appdir/usr/lib/libXau.so.6\"\n"
	if err := os.WriteFile(linuxdeploy, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	patchelf := filepath.Join(dir, "patchelf")
	if err := os.WriteFile(patchelf, []byte("pinned ELF patcher"), 0o644); err != nil {
		t.Fatal(err)
	}
	return tools{linuxdeploy, patchelf}
}

func TestUnexpectedBundledLibrariesNeedReviewedNotices(t *testing.T) {
	for _, name := range []string{"libxkbcommon-x11.so.0", "libxcb-xkb.so.1.0.0", "libXdmcp.so.6", "libbsd.so.0", "libmd.so.0"} {
		if !coveredLibrary(name) {
			t.Errorf("%s", name)
		}
	}
	for _, name := range []string{"libnew.so.1", "libXau.something", "libbsd-ctor.so.0", "libgpuikit.so"} {
		if coveredLibrary(name) {
			t.Errorf("%s", name)
		}
	}
}

func walk(t *testing.T, base string) map[string][]byte {
	t.Helper()
	seen := map[string][]byte{}
	err := filepath.Walk(base, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, _ := filepath.Rel(base, path)
		if rel == "." {
			return nil
		}
		if info.IsDir() {
			seen[filepath.ToSlash(rel)+"/"] = nil
			return nil
		}
		data, err := os.ReadFile(path)
		seen[filepath.ToSlash(rel)] = data
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return seen
}

func TestAppDirHasOnlyTheRuntimeUIAndLicenses(t *testing.T) {
	appdir := filepath.Join(t.TempDir(), "AppDir")
	exe := []byte("ELF executable")
	if err := stage(appdir, exe, []licenseFile{{"rust-crates.html", []byte("<html>")}}); err != nil {
		t.Fatal(err)
	}
	files := walk(t, appdir)
	if !bytes.Equal(files["usr/bin/rc2fx"], exe) {
		t.Fatal("executable")
	}
	for _, path := range []string{"usr/share/applications/rc2fx.desktop", "usr/share/icons/hicolor/scalable/apps/rc2fx.svg"} {
		if _, ok := files[path]; !ok {
			t.Errorf("%s missing", path)
		}
	}
	if string(files["usr/share/licenses/rc2fx/rust-crates.html"]) != "<html>" {
		t.Fatal("license")
	}
	apprun := string(files["AppRun"])
	if !strings.Contains(apprun, "$APPDIR/usr/bin/rc2fx") || !strings.Contains(apprun, "LD_LIBRARY_PATH") || strings.Contains(apprun, "APPIMAGE=") {
		t.Fatal(apprun)
	}
	info, err := os.Lstat(filepath.Join(appdir, "AppRun"))
	if err != nil || !info.Mode().IsRegular() {
		t.Fatalf("%v %v", info, err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o111 == 0 {
		t.Fatal("AppRun is not executable")
	}
	for path := range files {
		for _, forbidden := range []string{"docs", "crates", "experiments", "config", "runtime", "xtask", "mixer", "target", "README", "rc2fxd", "librc2fx_native"} {
			if strings.HasPrefix(path, forbidden) {
				t.Errorf("%s leaked into %s", forbidden, path)
			}
		}
	}
}

func TestAppRunUsesItsOwnLocationAndPreservesAppImage(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("linux only")
	}
	appdir := filepath.Join(t.TempDir(), "AppDir")
	script := []byte("#!/bin/sh\nprintf '%s\\n' \"$APPDIR\" \"$APPIMAGE\" \"$LD_LIBRARY_PATH\" \"$1\" \"$2\"\n")
	if err := stage(appdir, script, nil); err != nil {
		t.Fatal(err)
	}
	command := exec.Command(filepath.Join(appdir, "AppRun"), "one", "two words")
	command.Env = append(os.Environ(), "APPDIR=/tmp/inherited-appdir", "APPIMAGE=/tmp/rc2fx.AppImage", "LD_LIBRARY_PATH=/usr/local/lib")
	output, err := command.Output()
	if err != nil {
		t.Fatal(err)
	}
	want := appdir + "\n/tmp/rc2fx.AppImage\n" + appdir + "/usr/lib:/usr/local/lib\none\ntwo words\n"
	if string(output) != want {
		t.Fatalf("%q", output)
	}
}

func TestDeployKeepsThePayloadAndTheAppDirTheHelperNeeds(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("linux only")
	}
	root := t.TempDir()
	appdir := filepath.Join(root, "work", "rc2fx.AppDir")
	exe := []byte("registry-pinned executable bytes")
	tools := fakeTools(t, root)
	if err := stage(appdir, exe, []licenseFile{{"rust-crates.html", []byte("license")}}); err != nil {
		t.Fatal(err)
	}
	if err := deploy(appdir, "x64", tools); err != nil {
		t.Fatal(err)
	}
	seen := walk(t, appdir)
	if !bytes.Equal(seen["usr/bin/rc2fx"], exe) || string(seen["usr/lib/libXau.so.6"]) != "host" {
		t.Fatal("deployed files")
	}
	// The helper copies the AppDir and runs its regular AppRun.
	apprun, err := os.ReadFile(filepath.Join(task.Root(), filepath.FromSlash(appRunPath)))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(seen["AppRun"], apprun) {
		t.Fatal("AppRun changed")
	}
	if info, _ := os.Lstat(filepath.Join(appdir, "AppRun")); info.Mode()&os.ModeSymlink != 0 {
		t.Fatal("AppRun is a link")
	}
	if _, ok := seen["rc2fx.desktop"]; !ok {
		t.Fatal("rc2fx.desktop")
	}
	if _, ok := seen["rc2fx.svg"]; !ok {
		t.Fatal("rc2fx.svg")
	}
	for path := range seen {
		for _, forbidden := range []string{"docs/", "crates/", "experiments/", "config/", "runtime/", "xtask/", "mixer/scripts", "rc2fxd", "librc2fx_native", ".tar.gz", ".zip"} {
			if strings.Contains(path, forbidden) {
				t.Errorf("%s leaked into the AppDir", forbidden)
			}
		}
	}
	args, err := os.ReadFile(filepath.Join(root, "linuxdeploy.args"))
	if err != nil {
		t.Fatal(err)
	}
	for _, arg := range []string{"--appdir", "--executable", "--desktop-file", "--icon-file"} {
		if !strings.Contains(string(args), arg) {
			t.Errorf("%s missing from %q", arg, args)
		}
	}
	if strings.Contains(string(args), "--custom-apprun") {
		t.Fatal("passing the staged AppRun as its own copy source makes linuxdeploy delete it")
	}
	if strings.Contains(string(args), "--output") {
		t.Fatalf("linuxdeploy must only deploy, not package: %q", args)
	}
	if env, _ := os.ReadFile(filepath.Join(root, "linuxdeploy.env")); string(env) != "1\n" {
		t.Fatalf("%q", env)
	}
}

func TestTheAppImageMustStartWithTheRuntimeItsNoticesCover(t *testing.T) {
	runtimeBytes := []byte("\x7fELF vendored runtime")
	notices := core.ObjectValue(
		core.Member{Key: "schemaVersion", Value: core.U64Value(2)},
		core.Member{Key: "runtimeSha256", Value: core.StringValue(task.Sha256(runtimeBytes))},
		core.Member{Key: "runtimeSize", Value: core.U64Value(uint64(len(runtimeBytes)))},
	)
	appimage := append(bytes.Clone(runtimeBytes), []byte("hsqs squashfs image")...)
	if err := checkRuntime(appimage, notices); err != nil {
		t.Fatal(err)
	}
	appimage[1] ^= 1
	if checkRuntime(appimage, notices) == nil || checkRuntime(runtimeBytes[:4], notices) == nil ||
		checkRuntime(runtimeBytes, core.ObjectValue(core.Member{Key: "runtimeSha256", Value: core.StringValue("x")})) == nil {
		t.Fatal("a different runtime was accepted")
	}
}

func TestNoticesFromBeforeVpkAreRefused(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, filepath.FromSlash(notices))
	put(t, root, notices, []byte(`{"schemaVersion":1,"runtimeSha256":"x","sections":[]}`))
	if _, err := readNotices(root); err == nil || !strings.Contains(err.Error(), "schema") {
		t.Fatalf("%v", err)
	}
	if err := os.WriteFile(path, []byte(`{"schemaVersion":2,"runtimeSha256":"x","sections":[]}`), 0o666); err != nil {
		t.Fatal(err)
	}
	if _, err := readNotices(root); err != nil {
		t.Fatal(err)
	}
}
