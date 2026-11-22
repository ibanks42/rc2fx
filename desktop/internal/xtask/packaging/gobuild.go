package packaging

// Build the Go desktop app for packaging: links via cgo against the gpuikit
// static library (desktop/third_party/gpuikit) and Velopack's C library
// (`velopack` tag, desktop/third_party/velopack), embeds generated license
// notices via a Go -overlay, and with --embedded includes the mixer payload
// staged by cmd/stage-payload. Outputs land in target/[TRIPLE/]release (with
// velopack_libc.dll beside the executable on Windows).
//
// macOS builds are universal: arm64 and amd64 are built separately against
// macOSMinimum and merged with `lipo -create` (RC2FX_LIPO or lipo on PATH).

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"

	applicenses "github.com/ibanks42/rc2fx/desktop/app/licenses"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// library is a shared library shipped beside the executable.
type library struct {
	name string
	data []byte
}

// gpuikitLibrary is the gpuikit static library's file name, in the
// download's lib folder on every system (on Windows, MinGW's archive).
const gpuikitLibrary = "libgpuikit_static.a"

// shippedNames is every library the app loads from beside itself on goos.
// gpuikit is linked into the executable everywhere.
func shippedNames(goos string) []string {
	if goos == "windows" {
		// MSVC-built; linked through its plain C exports (vpkc.go).
		return []string{"velopack_libc.dll"}
	}
	// Linux and macOS link velopack_libc's static library into the
	// executable, on macOS each slice its architecture's: Velopack's
	// universal velopack_libc_osx.dylib has its build machine's path as its
	// install name, so shipping it would need rewriting and signing another
	// file.
	return nil
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

// findGpuikit is the download's lib folder, which holds the static library.
func findGpuikit(root string) (string, error) {
	dir := filepath.Join(gpuikitDir(root), "lib")
	if info, err := os.Stat(filepath.Join(dir, gpuikitLibrary)); err == nil && info.Mode().IsRegular() {
		return dir, nil
	}
	return "", task.Errorf("%s is not in %s; fetch the gpuikit download for this platform with desktop/scripts/fetch-gpuikit.sh (desktop/README.md)",
		gpuikitLibrary, dir)
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
	vpkVersion, err := licenses.VPKVersion(root)
	if err != nil {
		return "", err
	}
	base := filepath.Join(root, "desktop", "third_party", "velopack")
	version, err := os.ReadFile(filepath.Join(base, "VERSION"))
	if err != nil {
		return "", task.Errorf("%s: %s; run desktop/scripts/fetch-velopack.sh %s",
			filepath.Join(base, "VERSION"), task.IOError(err), fetch)
	}
	if got := strings.TrimSpace(string(version)); got != vpkVersion {
		return "", task.Errorf("desktop/third_party/velopack is velopack_libc %s, not %s; run desktop/scripts/fetch-velopack.sh %s",
			got, vpkVersion, fetch)
	}
	dir := filepath.Join(base, goos+"-"+goarch)
	if _, err := os.Stat(dir); err != nil {
		return "", task.Errorf("velopack_libc has no %s-%s build in %s; run desktop/scripts/fetch-velopack.sh %s",
			goos, goarch, base, fetch)
	}
	return dir, nil
}

// velopackLinkFlags links velopack_libc from dir. They go in CGO_LDFLAGS,
// not in vpkc's #cgo LDFLAGS: Velopack's static libraries carry LLVM bitcode
// (.llvmbc, __LLVM,__bitcode), and since Go 1.27.4 cgo refuses a library
// with an LTO section named by a #cgo directive; flags from the environment
// are the builder's own and are not checked. vpkc's directives keep the
// system libraries velopack_libc needs, which follow it on the link line.
//
// On Linux velopack_libc and gpuikit are both Rust static libraries, and
// each carries Rust's unwinding routine under the one unmangled name
// rust_eh_personality. GNU ld refuses the second copy unless told to keep
// the first; either copy serves both libraries.
func velopackLinkFlags(goos, dir string) string {
	flags := "-L" + dir + " -lvelopack_libc"
	if goos == "linux" {
		flags += " -Wl,--allow-multiple-definition"
	}
	return flags
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
// the notices in documentPath compiled in, and on Windows copies
// velopack_libc.dll beside it. On macOS goarch is "universal": one build per
// architecture, joined by lipo.
func BuildApp(root string, target *string, goos, goarch, documentPath string, embedded bool) error {
	desktop := filepath.Join(root, "desktop")
	gpuikit, err := findGpuikit(root)
	if err != nil {
		return err
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
		// The payload the `embedded` tag compiles in, checked by
		// stage-payload; RC2FX_RELEASE_BUILD=1 applies the release build
		// line.
		if err := task.Run(host("run", "./cmd/stage-payload", "-root", root)); err != nil {
			return err
		}
	}
	// Build with a Go -overlay that replaces desktop/app/licenses/licenses.json
	// with the validated notices file in a temp directory, so the tracked
	// placeholder file in the checkout is never mutated on disk.
	licensesText, err := applicenses.Resolve(true, documentPath)
	if err != nil {
		return task.Errorf("cannot embed licenses: %s", err)
	}
	overlayDir, err := os.MkdirTemp("", "rc2fx-licenses-overlay-*")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer func() { _ = os.RemoveAll(overlayDir) }()
	overlayLicenses := filepath.Join(overlayDir, "licenses.json")
	if err := os.WriteFile(overlayLicenses, []byte(licensesText), 0o644); err != nil {
		return task.Errorf("%s: %s", overlayLicenses, task.IOError(err))
	}
	overlayPath := filepath.Join(overlayDir, "overlay.json")
	overlayBytes, err := json.Marshal(struct {
		Replace map[string]string `json:"Replace"`
	}{
		Replace: map[string]string{
			filepath.Join(desktop, "app", "licenses", "licenses.json"): overlayLicenses,
		},
	})
	if err != nil {
		return task.Error(err.Error())
	}
	if err := os.WriteFile(overlayPath, overlayBytes, 0o644); err != nil {
		return task.Errorf("%s: %s", overlayPath, task.IOError(err))
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
	// No symbol table or debug information: gpuikit's symbol table alone is
	// over 20 MB, and Go's stack traces work without either.
	ldflags, cgoLDFlags := appLinkFlags(goos, gpuikit, "-s -w")
	var built []string
	for _, arch := range arches {
		out := exe
		if goos == "darwin" {
			out = exe + "-" + arch
			defer func() { _ = os.Remove(out) }()
		}
		vpk, err := velopackDir(root, goos, arch, fetchTarget(goos, goarch))
		if err != nil {
			return err
		}
		build := host("build", "-trimpath", "-overlay", overlayPath, "-tags", tags, "-ldflags", ldflags, "-o", out, "./cmd/rc2fx")
		build.Env = buildEnv(os.Environ(), goos, arch, cgoLDFlags+" "+velopackLinkFlags(goos, vpk))
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

// CheckUILink verifies that ./cmd/rc2fx links with the release Velopack and
// gpuikit linker flags (appLinkFlags + velopackLinkFlags + buildEnv) for the
// host platform (both arm64 and amd64 slices on macOS), without duplicating
// those flags in CI workflows.
func CheckUILink(outDir string) error {
	root := task.Root()
	desktop := filepath.Join(root, "desktop")
	gpuikit, err := findGpuikit(root)
	if err != nil {
		return err
	}
	goos, goarch := runtime.GOOS, runtime.GOARCH
	if goos == "darwin" {
		goarch = "universal"
	}
	arches := goArches(goos, goarch)
	if outDir == "" {
		dir, err := os.MkdirTemp("", "rc2fx-ui-check-*")
		if err != nil {
			return task.Error(task.IOError(err))
		}
		defer func() { _ = os.RemoveAll(dir) }()
		outDir = dir
	} else if err := os.MkdirAll(outDir, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	ldflags, cgoLDFlags := appLinkFlags(goos, gpuikit, "-s -w")
	for _, arch := range arches {
		vpk, err := velopackDir(root, goos, arch, fetchTarget(goos, goarch))
		if err != nil {
			return err
		}
		name := "rc2fx"
		if goos == "darwin" {
			name += "-" + arch
		} else if goos == "windows" {
			name += ".exe"
		}
		build := exec.Command("go", "build", "-tags", "velopack", "-ldflags", ldflags, "-o", filepath.Join(outDir, name), "./cmd/rc2fx")
		build.Dir = desktop
		build.Env = buildEnv(os.Environ(), goos, arch, cgoLDFlags+" "+velopackLinkFlags(goos, vpk))
		if err := task.Run(build); err != nil {
			return err
		}
	}
	return nil
}

// NonUIPackages prints every Go package under desktop/ that does not depend
// on gpuikit.dev/gk, one per line. Both `just test` and CI call this so the
// package filter is defined in one place.
func NonUIPackages() error {
	desktop := filepath.Join(task.Root(), "desktop")
	cmd := exec.Command("go", "list", "-e", "-f",
		`{{$ui := false}}{{range .Deps}}{{if eq . "gpuikit.dev/gk"}}{{$ui = true}}{{end}}{{end}}{{if not $ui}}{{.ImportPath}}{{end}}`,
		"./...")
	cmd.Dir = desktop
	out, err := cmd.Output()
	if err != nil {
		return task.Errorf("go list non-ui packages: %s", err)
	}
	for _, line := range strings.Split(string(out), "\n") {
		if line = strings.TrimSpace(line); line != "" {
			fmt.Println(line)
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

// windowsResources gives rc2fx.exe its icon and version details: Explorer,
// the Start menu, Installed apps and the taskbar read the details, and the
// window loads icon 1. The application manifest
// (common controls 6, without which the program does not start, and
// per-monitor DPI awareness) is not among them: package gk links its own
// (gpuikit_windows_amd64.syso), and the linker keeps that one over MinGW's
// default (appLinkFlags' -force:multipleres). It compiles them with windres
// (RC2FX_WINDRES, or x86_64-w64-mingw32-windres or windres on PATH) into
// desktop/cmd/rc2fx/rsrc_windows_amd64.syso, which the caller removes after
// the build. Development machines without a resource compiler still build;
// releases must carry the icon.
func windowsResources(root string) (string, error) {
	icon := filepath.Join(root, filepath.FromSlash(task.AssetsDir), "rc2fx.ico")
	script := resourceScript(icon, installer.AppVersion)
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
			return "", task.Errorf("\n\ncannot embed the Windows icon and version: %s\n\n", reason)
		}
		fmt.Printf("warning: cannot embed the Windows icon and version: %s\n", reason)
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

// resourceScript is the rc2fx.rc resource script for version, without a
// manifest (package gk links its own).
func resourceScript(icon, version string) string {
	parts := strings.FieldsFunc(version, func(r rune) bool { return r == '.' || r == '-' || r == '+' })
	numbers := make([]string, 4)
	for i := range numbers {
		numbers[i] = "0"
		if i < 3 && i < len(parts) {
			if n, ok := common.ParseUint(parts[i], 16); ok {
				numbers[i] = strconv.FormatUint(n, 10)
			}
		}
	}
	numeric := strings.Join(numbers, ",")
	quote := func(text string) string {
		return `"` + strings.ReplaceAll(strings.ReplaceAll(text, `\`, `\\`), `"`, `""`) + `"`
	}
	return fmt.Sprintf(`#pragma code_page(65001)
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
`, quote(icon), numeric, numeric, quote(version), quote(version))
}
