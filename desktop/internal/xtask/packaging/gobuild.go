package packaging

// Building the Go desktop app for packaging (cargo build -p rc2fx-app
// before): cgo against the gpuikit library in desktop/third_party/gpuikit,
// Velopack's C library (`velopack` tag, desktop/third_party/velopack), the
// generated licence notices compiled in through `go generate ./app/licenses`,
// and with --embedded the mixer payload staged by
// installer/embed/cmd/stage-payload (`embedded` tag). The executable and the
// libraries it loads land together in target/[TRIPLE/]release.
//
// The macOS app is universal: built once for arm64 and once for amd64, each
// compiled and linked for macOSMinimum against the universal gpuikit library
// and that architecture's static velopack_libc, then joined by `lipo
// -create` (RC2FX_LIPO, or lipo on the PATH) into one executable.

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// library is a shared library shipped beside the executable.
type library struct {
	name string
	data []byte
}

// gpuikitLibrary is the gpuikit library's file name on goos.
func gpuikitLibrary(goos string) string {
	switch goos {
	case "windows":
		return "gpuikit.dll"
	case "darwin":
		return "libgpuikit.dylib"
	}
	return "libgpuikit.so"
}

// shippedNames is every library the app loads from beside itself on goos.
func shippedNames(goos string) []string {
	names := []string{gpuikitLibrary(goos)}
	if goos == "windows" {
		// MSVC-built; linked through its plain C exports (vpkc.go).
		names = append(names, "velopack_libc.dll")
	}
	// Linux and macOS link velopack_libc's static library into the
	// executable, on macOS each slice its architecture's: Velopack's
	// universal velopack_libc_osx.dylib has its build machine's path as its
	// install name, so shipping it would need rewriting and signing another
	// file.
	return names
}

// shippedLibraries reads the libraries the build put beside the executable.
func shippedLibraries(root, goos, goarch, release string) ([]library, error) {
	_ = root
	_ = goarch
	var out []library
	for _, name := range shippedNames(goos) {
		data, err := read(filepath.Join(release, name))
		if err != nil {
			return nil, task.Errorf("%s\nBuild the app or drop --skip-build.", err)
		}
		out = append(out, library{name, data})
	}
	return out, nil
}

// gpuikitDir is the unpacked gpuikit download desktop/third_party/gpuikit
// links to.
func gpuikitDir(root string) string {
	return filepath.Join(root, "desktop", "third_party", "gpuikit")
}

// findGpuikit is the gpuikit library file for goos in the download.
func findGpuikit(root, goos string) (string, error) {
	name := gpuikitLibrary(goos)
	for _, dir := range []string{"lib", "bin"} {
		path := filepath.Join(gpuikitDir(root), dir, name)
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
			return path, nil
		}
	}
	return "", task.Errorf("%s is not in %s; link desktop/third_party/gpuikit to the gpuikit download for this platform (desktop/README.md)",
		name, gpuikitDir(root))
}

// goArches is the GOARCH of each build one package for goos/goarch needs:
// one per slice of the universal macOS app, otherwise just goarch.
func goArches(goos, goarch string) []string {
	if goos == "darwin" {
		return darwinGoArches
	}
	return []string{goarch}
}

// fetchTarget is fetch-velopack.sh's and fetch-gpuikit.sh's target for a
// package for goos/goarch.
func fetchTarget(goos, goarch string) string {
	if goos == "darwin" {
		return "darwin-universal"
	}
	return goos + "-" + goarch
}

// velopackDir is velopack_libc for one build's goos/goarch, fetched by
// desktop/scripts/fetch-velopack.sh, which must be the version vpk packs
// with. fetch is the script's target.
func velopackDir(root, goos, goarch, fetch string) (string, error) {
	base := filepath.Join(root, "desktop", "third_party", "velopack")
	version, err := os.ReadFile(filepath.Join(base, "VERSION"))
	if err != nil {
		return "", task.Errorf("%s: %s; run desktop/scripts/fetch-velopack.sh %s",
			filepath.Join(base, "VERSION"), task.IOError(err), fetch)
	}
	if got := strings.TrimSpace(string(version)); got != licenses.VPKVersion {
		return "", task.Errorf("desktop/third_party/velopack is velopack_libc %s, not %s; run desktop/scripts/fetch-velopack.sh %s",
			got, licenses.VPKVersion, fetch)
	}
	dir := filepath.Join(base, goos+"-"+goarch)
	if _, err := os.Stat(dir); err != nil {
		return "", task.Errorf("velopack_libc has no %s-%s build in %s; run desktop/scripts/fetch-velopack.sh %s",
			goos, goarch, base, fetch)
	}
	return dir, nil
}

// buildEnv is the environment `go build` of the app runs with for one
// goos/goarch, from environ (os.Environ()): cgo on and CGO_LDFLAGS as
// appLinkFlags gave them. On macOS the C compiler (CC, clang unless set),
// CGO_CFLAGS and CGO_LDFLAGS also name goarch's -arch and macOSMinimum, and
// MACOSX_DEPLOYMENT_TARGET is macOSMinimum, so cgo's objects, the static
// libraries and the external link all target the same slice and the same
// oldest macOS; without them clang targets the build machine's macOS.
func buildEnv(environ []string, goos, goarch, cgoLDFlags string) []string {
	lookup := func(name string) string {
		value := ""
		for _, entry := range environ {
			if k, v, ok := strings.Cut(entry, "="); ok && k == name {
				value = v
			}
		}
		return value
	}
	env := append(slices.Clone(environ), "GOOS="+goos, "GOARCH="+goarch, "CGO_ENABLED=1")
	if goos != "darwin" {
		return append(env, "CGO_LDFLAGS="+cgoLDFlags)
	}
	target := "-arch " + clangArch(goarch) + " -mmacosx-version-min=" + macOSMinimum
	cc := lookup("CC")
	if cc == "" {
		cc = "clang"
	}
	cflags := lookup("CGO_CFLAGS")
	if cflags == "" {
		// go env's default, which setting CGO_CFLAGS replaces.
		cflags = "-O2 -g"
	}
	return append(env, "CC="+cc, "CGO_CFLAGS="+cflags+" "+target, "CGO_LDFLAGS="+cgoLDFlags+" "+target,
		"MACOSX_DEPLOYMENT_TARGET="+macOSMinimum)
}

// lipo joins the slices into one universal executable at out.
func lipo(out string, slices []string) error {
	tool, ok := setting("RC2FX_LIPO")
	if !ok {
		tool = "lipo"
	}
	return run(tool, append([]string{"-create", "-output", out}, slices...))
}

// BuildApp builds the app for goos/goarch into target/[TRIPLE/]release with
// the notices in documentPath compiled in, and copies the libraries it
// loads beside it. On macOS goarch is "universal": one build per
// architecture, joined by lipo.
func BuildApp(root string, target *string, goos, goarch, documentPath string, embedded bool) error {
	desktop := filepath.Join(root, "desktop")
	gpuikit, err := findGpuikit(root, goos)
	if err != nil {
		return err
	}
	if goos == "darwin" {
		data, err := read(gpuikit)
		if err != nil {
			return err
		}
		if err := checkLibrary(gpuikit, data); err != nil {
			return task.Errorf("%s\nFetch the universal download: desktop/scripts/fetch-gpuikit.sh darwin-universal", err)
		}
	}
	arches := goArches(goos, goarch)
	for _, arch := range arches {
		if _, err := velopackDir(root, goos, arch, fetchTarget(goos, goarch)); err != nil {
			return err
		}
	}
	host := func(args ...string) *exec.Cmd {
		command := exec.Command("go", args...)
		command.Dir = desktop
		return command
	}
	if embedded {
		// The payload the `embedded` tag compiles in, checked as the Rust
		// build script did; RC2FX_RELEASE_BUILD=1 applies the release build
		// line.
		if err := task.Run(host("run", "./installer/embed/cmd/stage-payload", "-root", root)); err != nil {
			return err
		}
	}
	// go generate writes app/licenses/licenses.json, which the build embeds;
	// the checkout's copy (the development placeholder) comes back
	// afterwards.
	licensesFile := filepath.Join(desktop, "app", "licenses", "licenses.json")
	original, err := os.ReadFile(licensesFile)
	if err != nil {
		return task.Errorf("%s: %s", licensesFile, task.IOError(err))
	}
	defer func() { _ = os.WriteFile(licensesFile, original, 0o644) }()
	generate := host("generate", "./app/licenses")
	generate.Env = append(os.Environ(), "RC2FX_RELEASE_BUILD=1", "RC2FX_LICENSES_JSON="+documentPath)
	if err := task.Run(generate); err != nil {
		return err
	}
	if goos == "windows" {
		syso, err := windowsResources(root)
		if err != nil {
			return err
		}
		if syso != "" {
			defer func() { _ = os.Remove(syso) }()
		}
	}
	release := releaseDir(root, target)
	if err := os.MkdirAll(release, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	exe := filepath.Join(release, "rc2fx")
	if goos == "windows" {
		exe += ".exe"
	}
	tags := "velopack"
	if embedded {
		tags += ",embedded"
	}
	// strip = "debuginfo" in the Rust release profile.
	ldflags, cgoLDFlags := appLinkFlags(goos, filepath.Dir(gpuikit), "-w")
	var built []string
	for _, arch := range arches {
		out := exe
		if goos == "darwin" {
			out = exe + "-" + arch
			defer func() { _ = os.Remove(out) }()
		}
		build := host("build", "-trimpath", "-tags", tags, "-ldflags", ldflags, "-o", out, "./cmd/rc2fx")
		build.Env = buildEnv(os.Environ(), goos, arch, cgoLDFlags)
		if err := task.Run(build); err != nil {
			return err
		}
		built = append(built, out)
	}
	if goos == "darwin" {
		_ = os.Remove(exe)
		if err := lipo(exe, built); err != nil {
			return err
		}
		data, err := read(exe)
		if err != nil {
			return err
		}
		if _, err := checkUniversal(exe, data, macOSMinimum); err != nil {
			return err
		}
	}
	if goos == "windows" {
		if err := checkWindowsImports(exe); err != nil {
			return err
		}
		if err := checkWindowsManifest(exe); err != nil {
			return err
		}
	}
	if err := copyLibrary(gpuikit, filepath.Join(release, gpuikitLibrary(goos))); err != nil {
		return err
	}
	if goos == "windows" {
		velopack, err := velopackDir(root, goos, goarch, fetchTarget(goos, goarch))
		if err != nil {
			return err
		}
		if err := copyLibrary(filepath.Join(velopack, "velopack_libc.dll"), filepath.Join(release, "velopack_libc.dll")); err != nil {
			return err
		}
	}
	return nil
}

func copyLibrary(from, to string) error {
	data, err := read(from)
	if err != nil {
		return err
	}
	if err := os.WriteFile(to, data, 0o755); err != nil {
		return task.Errorf("%s: %s", to, task.IOError(err))
	}
	return os.Chmod(to, 0o755)
}

// windowsResources gives rc2fx.exe its icon, version details and the
// application manifest (the Rust app's build.rs; gpui-fast embedded the
// manifest there, gpuikit leaves it to the program): Explorer, the Start
// menu, Installed apps and the taskbar read the details, the window loads
// icon 1, and without the manifest's common controls 6 the program does not
// start (gpuikit.dll imports TaskDialogIndirect). It compiles them with windres
// (RC2FX_WINDRES, or x86_64-w64-mingw32-windres or windres on PATH) into
// desktop/cmd/rc2fx/rsrc_windows_amd64.syso, which the caller removes after
// the build. Development machines without a resource compiler still build;
// releases must carry the icon.
func windowsResources(root string) (string, error) {
	icon := filepath.Join(root, filepath.FromSlash(task.AssetsDir), "rc2fx.ico")
	script := resourceScript(icon, windowsManifest(root), installer.AppVersion)
	dir, err := os.MkdirTemp("", "rc2fx-resources")
	if err != nil {
		return "", task.Error(task.IOError(err))
	}
	defer os.RemoveAll(dir)
	rc := filepath.Join(dir, "rc2fx.rc")
	if err := os.WriteFile(rc, []byte(script), 0o644); err != nil {
		return "", task.Error(task.IOError(err))
	}
	windres, ok := setting("RC2FX_WINDRES")
	if !ok {
		for _, candidate := range []string{"x86_64-w64-mingw32-windres", "windres"} {
			if path, err := exec.LookPath(candidate); err == nil {
				windres, ok = path, true
				break
			}
		}
	}
	missing := func(reason string) (string, error) {
		if releaseBuild() {
			return "", task.Errorf("\n\ncannot embed the Windows icon, version and manifest: %s\n\n", reason)
		}
		fmt.Printf("warning: cannot embed the Windows icon, version and manifest (a copy goes beside rc2fx.exe): %s\n", reason)
		return "", nil
	}
	if !ok {
		return missing("no resource compiler (set RC2FX_WINDRES or install mingw-w64 windres)")
	}
	syso := filepath.Join(root, "desktop", "cmd", "rc2fx", "rsrc_windows_amd64.syso")
	if _, err := os.Stat(syso); err == nil || !errors.Is(err, fs.ErrNotExist) {
		return "", task.Errorf("%s already exists; remove it before packaging", syso)
	}
	command := exec.Command(windres, "-i", rc, "-O", "coff", "-o", syso)
	var output bytes.Buffer
	command.Stdout = &output
	command.Stderr = &output
	if err := command.Run(); err != nil {
		_ = os.Remove(syso)
		return missing(strings.TrimSpace(output.String()))
	}
	return syso, nil
}

// windowsManifest is the gpuikit download's application manifest: common
// controls 6 and per-monitor DPI awareness, which every gpuikit program on
// Windows needs.
func windowsManifest(root string) string {
	return filepath.Join(gpuikitDir(root), "windows", "gpuikit.manifest")
}

// besideManifest puts the manifest beside exe as exe.manifest, for a build
// that could not embed it (no resource compiler): Windows reads it when the
// program has none of its own.
func besideManifest(root, exe string) error {
	return copyLibrary(windowsManifest(root), exe+".manifest")
}

// resourceScript is build.rs's rc2fx.rc for version, with the manifest as
// resource 1 of type 24 (RT_MANIFEST), which Windows reads at process start.
func resourceScript(icon, manifest, version string) string {
	parts := strings.FieldsFunc(version, func(r rune) bool { return r == '.' || r == '-' || r == '+' })
	numbers := make([]string, 4)
	for i := range numbers {
		numbers[i] = "0"
		if i < 3 && i < len(parts) {
			if n, ok := task.ParseUint(parts[i], 16); ok {
				numbers[i] = strconv.FormatUint(n, 10)
			}
		}
	}
	numeric := strings.Join(numbers, ",")
	quote := func(text string) string {
		return `"` + strings.ReplaceAll(strings.ReplaceAll(text, `\`, `\\`), `"`, `""`) + `"`
	}
	return fmt.Sprintf(`#pragma code_page(65001)
1 24 %s
1 ICON %s
1 VERSIONINFO
FILEVERSION %s
PRODUCTVERSION %s
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Isaiah Banks"
      VALUE "FileDescription", "RC2FX"
      VALUE "FileVersion", %s
      VALUE "InternalName", "rc2fx"
      VALUE "LegalCopyright", "Copyright (c) 2026 Isaiah Banks. MIT License."
      VALUE "OriginalFilename", "rc2fx.exe"
      VALUE "ProductName", "RC2FX"
      VALUE "ProductVersion", %s
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
`, quote(manifest), quote(icon), numeric, numeric, quote(version), quote(version))
}
