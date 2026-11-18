// Package packaging is `package`: the self-contained desktop app and legal
// notices. Release executables download the mixer payload; packaging checks
// the executable's payload mode, including for --skip-build. Every platform
// is packaged by Velopack (see velopack.go): an AppImage on Linux, an
// installer and a portable zip on Windows and macOS, and the update package
// and feed the app updates itself from.
//
// The app is the Go one (desktop/cmd/rc2fx, cgo against the gpuikit
// static library, which is linked into it), shipped on Windows with
// Velopack's velopack_libc.dll beside it. The macOS app is universal (arm64 and x86_64 in one
// executable), so its packages are named rc2fx-macos-universal.
package packaging

import (
	"bytes"
	"debug/buildinfo"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"sort"
	"strings"

	applicenses "github.com/ibanks42/rc2fx/desktop/app/licenses"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const top = "rc2fx"

// Member is one file of a package.
type Member struct {
	Data []byte
	Mode os.FileMode
}

// Files is a package by path.
type Files map[string]Member

// sortedNames is the paths in order (a BTreeMap's iteration).
func (f Files) sortedNames() []string {
	names := make([]string, 0, len(f))
	for name := range f {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

func file(data []byte) Member { return Member{Data: data, Mode: 0o644} }

func read(path string) ([]byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, task.Errorf("%s: %s", path, task.IOError(err))
	}
	return data, nil
}

// run runs a packaging tool; on failure, it reports what the tool printed.
func run(program string, args []string) error {
	command := exec.Command(program, args...)
	var stdout, stderr bytes.Buffer
	command.Stdout = &stdout
	command.Stderr = &stderr
	if err := command.Start(); err != nil {
		return task.Errorf("could not run %s: %s", program, task.IOError(err))
	}
	_ = command.Wait()
	if !command.ProcessState.Success() {
		first := ""
		if len(args) > 0 {
			first = args[0]
		}
		return task.Errorf("%s %s failed:\n%s%s", program, first,
			strings.ToValidUTF8(stdout.String(), "�"), strings.ToValidUTF8(stderr.String(), "�"))
	}
	return nil
}

// setting is a packaging setting from the environment; empty counts as
// unset.
func setting(name string) (string, bool) {
	value, ok := os.LookupEnv(name)
	if !ok || value == "" {
		return "", false
	}
	return value, true
}

// platform is the operating system and architecture names for a target
// triple. Every macOS package is universal, whichever Apple triple names it
// (aarch64-, x86_64- or universal-apple-darwin).
func platform(target string) (string, string, error) {
	var system, arch string
	switch {
	case strings.Contains(target, "windows"):
		system = "windows"
	case strings.Contains(target, "apple"):
		system = "macos"
	case strings.Contains(target, "linux"):
		system = "linux"
	default:
		return "", "", task.Errorf("unsupported package target %s", target)
	}
	switch {
	case system == "macos" && (strings.HasPrefix(target, "x86_64") || strings.HasPrefix(target, "aarch64") ||
		strings.HasPrefix(target, "universal")):
		arch = "universal"
	case strings.HasPrefix(target, "x86_64"):
		arch = "x64"
	case strings.HasPrefix(target, "aarch64"):
		arch = "arm64"
	default:
		return "", "", task.Errorf("unsupported package architecture %s", target)
	}
	return system, arch, nil
}

// goPlatform is GOOS and GOARCH for a package's names; GOARCH is
// "universal" for the macOS app (goArches gives its builds).
func goPlatform(system, arch string) (string, string) {
	goos := system
	if system == "macos" {
		goos = "darwin"
	}
	goarch := "amd64"
	switch arch {
	case "arm64", "universal":
		goarch = arch
	}
	return goos, goarch
}

// mergeSections is the sections of every list, each once, in first-seen
// order.
func mergeSections(lists ...[]licenses.Section) []licenses.Section {
	var out []licenses.Section
	seen := map[licenses.Section]bool{}
	for _, list := range lists {
		for _, section := range list {
			if !seen[section] {
				seen[section] = true
				out = append(out, section)
			}
		}
	}
	return out
}

// image is one architecture's executable: the macOS app's slices, or the
// whole executable elsewhere.
type image struct {
	arch string // lipo's name on macOS, empty elsewhere
	data []byte
}

// executableImages is what the checks on the executable's contents (payload
// mode, notices, build tags) run on: each slice of the universal macOS
// executable, which must hold exactly arm64 and x86_64, or the whole
// executable. A universal file holds each slice's payload, notices and build
// information once.
func executableImages(system string, executable []byte) ([]image, error) {
	if system != "macos" {
		return []image{{"", executable}}, nil
	}
	found, err := checkUniversal("the executable", executable, macOSMinimum)
	if err != nil {
		return nil, task.Errorf("%s\nRebuild with ./xtask package, which builds both slices.", err)
	}
	var out []image
	for _, s := range found {
		out = append(out, image{s.arch, s.data})
	}
	return out, nil
}

// hostTarget is this machine's target triple, in rustc's naming, from the
// Go toolchain's GOOS and GOARCH.
func hostTarget() (string, error) {
	arch := map[string]string{"amd64": "x86_64", "arm64": "aarch64"}[runtime.GOARCH]
	vendor := map[string]string{
		"linux":   "unknown-linux-gnu",
		"darwin":  "apple-darwin",
		"windows": "pc-windows-msvc",
	}[runtime.GOOS]
	if arch == "" || vendor == "" {
		return "", task.Errorf("unsupported host %s/%s", runtime.GOOS, runtime.GOARCH)
	}
	return arch + "-" + vendor, nil
}

// releaseBuild: official release builds embed the quiet-diagnostics
// launcher policy.
func releaseBuild() bool { return os.Getenv("RC2FX_RELEASE_BUILD") == "1" }

const (
	downloadMode = "RC2FX-PAYLOAD-MODE:download:1"
	embeddedMode = "RC2FX-PAYLOAD-MODE:embedded:1"
)

func validateExecutable(root string, executable []byte, release bool) error {
	if bytes.Contains(executable, []byte(downloadMode)) {
		if _, err := embed.Find(executable); err == nil {
			return task.Error("download-only executable unexpectedly contains a mixer payload")
		}
		return nil
	}
	embedded, err := embed.Find(executable)
	if err != nil {
		return task.Error(err.Error())
	}
	expected, err := embed.Expected(root, release)
	if err != nil {
		return task.Error(err.Error())
	}
	if err := embedded.RequireMatches(expected); err != nil {
		return task.Error(err.Error())
	}
	return nil
}

// checkGoBuild refuses an executable a release package must not ship: one
// built with the mock mixer or a development tag, without Velopack (it
// could not update itself and would open on Velopack's install hooks), or
// with the placeholder licenses.
func checkGoBuild(executable []byte) error {
	info, err := buildinfo.Read(bytes.NewReader(executable))
	if err != nil {
		return task.Errorf("the executable is not a Go build of the app (%s); rebuild with ./xtask package", err)
	}
	var tags []string
	for _, s := range info.Settings {
		if s.Key == "-tags" {
			tags = strings.Split(s.Value, ",")
		}
	}
	for _, refused := range []string{"mockmixer", "rc2fxdev", "rc2fxdebug"} {
		if slices.Contains(tags, refused) {
			return task.Errorf("the executable was built with -tags %s; packages never ship it: rebuild with ./xtask package", refused)
		}
	}
	if !slices.Contains(tags, "velopack") {
		return task.Error("the executable was built without -tags velopack, so it cannot update itself and ignores Velopack's install hooks; rebuild with ./xtask package")
	}
	if bytes.Contains(executable, []byte(applicenses.Placeholder)) {
		return task.Error("the executable embeds the development placeholder instead of the license notices; rebuild with ./xtask package")
	}
	return nil
}

func collectWith(root, system string, executable []byte, licenseFiles []licenseFile, release bool) (Files, error) {
	files := Files{}
	if err := validateExecutable(root, executable, release); err != nil {
		return nil, err
	}
	for _, l := range licenseFiles {
		files["licenses/"+l.name] = file(l.data)
	}
	name := "rc2fx"
	if system == "windows" {
		name = "rc2fx.exe"
	}
	files[name] = Member{Data: executable, Mode: 0o755}
	return files, nil
}

// licenseFile is a license file for the package's licenses folder.
type licenseFile struct {
	name string
	data []byte
}

// writeFolder writes the executable and legal notices as an unpacked
// package.
func writeFolder(dir string, files Files) error {
	_ = os.RemoveAll(dir)
	for _, name := range files.sortedNames() {
		m := files[name]
		path := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
			return task.Error(task.IOError(err))
		}
		if err := os.WriteFile(path, m.Data, 0o666); err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
		if runtime.GOOS != "windows" {
			if err := os.Chmod(path, m.Mode); err != nil {
				return task.Error(task.IOError(err))
			}
		}
	}
	return nil
}

// writeChecksums writes SHA256SUMS for every published Linux file, beside
// them.
func writeChecksums(out string, published []string) error {
	var sums strings.Builder
	for _, path := range published {
		data, err := read(path)
		if err != nil {
			return err
		}
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), filepath.Base(path))
	}
	path := filepath.Join(out, "SHA256SUMS")
	if err := os.WriteFile(path, []byte(sums.String()), 0o666); err != nil {
		return task.Errorf("%s: %s", path, task.IOError(err))
	}
	return nil
}

func usage() error {
	return task.Error("usage: ./xtask package [--target TRIPLE] [--build-only | --skip-build] [--embedded]\n" +
		"--build-only stops after the build; --skip-build then packages that build.\n" +
		"Linux requires RC2FX_LINUXDEPLOY and RC2FX_PATCHELF to name the pinned tools,\n" +
		"vpk (RC2FX_VPK) and mksquashfs.")
}

// Run is `package`.
func Run(args []string) error {
	var target *string
	build, packIt, embedded := true, true, false
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--target":
			if i+1 >= len(args) {
				return usage()
			}
			i++
			t := args[i]
			target = &t
		case "--skip-build":
			build = false
		case "--build-only":
			packIt = false
		case "--embedded":
			embedded = true
		default:
			return usage()
		}
	}
	root := task.Root()
	host, err := hostTarget()
	if err != nil {
		return err
	}
	triple := host
	if target != nil {
		triple = *target
	}
	system, arch, err := platform(triple)
	if err != nil {
		return err
	}
	goos, goarch := goPlatform(system, arch)
	// The notices cover every slice's Go dependencies.
	var perArch [][]licenses.Section
	for _, a := range goArches(goos, goarch) {
		sections, err := licenses.GoApp(root, goos, a, []string{"velopack"})
		if err != nil {
			return err
		}
		perArch = append(perArch, sections)
	}
	extra := mergeSections(perArch...)
	if system == "linux" {
		sections, err := licenseSections(root)
		if err != nil {
			return err
		}
		extra = append(extra, sections...)
	}
	document, err := licenses.Generate(root, extra)
	if err != nil {
		return err
	}
	documentPath := filepath.Join(root, "target", "package-licenses.json")
	if err := os.MkdirAll(filepath.Join(root, "target"), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := os.WriteFile(documentPath, document.JSON, 0o666); err != nil {
		return task.Error(task.IOError(err))
	}
	if !build && !packIt {
		return usage()
	}
	if build {
		if err := BuildApp(root, target, goos, goarch, documentPath, embedded); err != nil {
			return err
		}
	}
	if !packIt {
		return nil
	}
	release := releaseDir(root, target)
	exeName := "rc2fx"
	if system == "windows" {
		exeName = "rc2fx.exe"
	}
	executable, err := read(filepath.Join(release, exeName))
	if err != nil {
		return task.Errorf("%s\nBuild the app or drop --skip-build.", err)
	}
	mode := downloadMode
	if embedded {
		mode = embeddedMode
	}
	images, err := executableImages(system, executable)
	if err != nil {
		return err
	}
	releaseMode := releaseBuild()
	for _, im := range images {
		fail := func(err error) error {
			if im.arch == "" {
				return err
			}
			return task.Errorf("the %s slice: %s", im.arch, err)
		}
		if !bytes.Contains(im.data, []byte(mode)) {
			return fail(task.Error("executable payload mode differs from packaging options; rebuild with the requested mode"))
		}
		if err := licenses.VerifyEmbedded(im.data, document.JSON); err != nil {
			return fail(err)
		}
		if err := checkGoBuild(im.data); err != nil {
			return fail(err)
		}
		if err := validateExecutable(root, im.data, releaseMode); err != nil {
			return fail(err)
		}
	}
	libraries, err := shippedLibraries(root, goos, goarch, release)
	if err != nil {
		return err
	}
	out := filepath.Join(root, "target", "package")
	version := installer.AppVersion
	// No version in published names: releases/latest/download/NAME then
	// always links the newest release.
	name := fmt.Sprintf("rc2fx-%s-%s", system, arch)
	if system == "linux" {
		tools, err := toolsFromEnv()
		if err != nil {
			return err
		}
		published, err := packageAppImage(root, out, name, version, executable,
			[]licenseFile{{"LICENSES.html", document.HTML}}, libraries, arch, tools)
		if err != nil {
			return err
		}
		// prepare-appimage-notices.py wrote the runtime's corresponding source.
		published = append(published, filepath.Join(out, "rc2fx-"+version+"-appimage-runtime-sources.tar.gz"))
		return writeChecksums(out, published)
	}
	// These apps carry the notices in their Settings dialog. collectWith
	// checks one image's payload (all were checked above, and a universal
	// executable holds one payload per slice); the package ships the whole
	// executable.
	files, err := collectWith(root, system, images[0].data, nil, releaseMode)
	if err != nil {
		return err
	}
	files[exeName] = Member{Data: executable, Mode: 0o755}
	for _, library := range libraries {
		files[library.name] = Member{Data: library.data, Mode: 0o755}
	}
	folder := filepath.Join(out, name, top)
	packDir := folder
	if system == "macos" {
		bundled, err := bundle(files, version)
		if err != nil {
			return err
		}
		if err := checkMacBundle(bundled); err != nil {
			return err
		}
		if err := writeFolder(folder, bundled); err != nil {
			return err
		}
		packDir = filepath.Join(folder, app)
	} else if err := writeFolder(folder, files); err != nil {
		return err
	}
	// collectWith() checked the executable's payload mode; the package
	// deliberately has no payload sidecars.
	packages, err := pack(root, system, arch, version, name, packDir, out)
	if err != nil {
		return err
	}
	fmt.Println("Packages ready:")
	for _, p := range packages {
		data, err := read(p)
		if err != nil {
			return err
		}
		fmt.Printf("  %s\n  sha256 %s\n", p, task.Sha256(data))
	}
	fmt.Printf("  unpacked app in %s\n", packDir)
	return nil
}

// releaseDir is where the build puts the app: target/release, or
// target/TRIPLE/release for --target, as cargo did.
func releaseDir(root string, target *string) string {
	dir := filepath.Join(root, "target")
	if target != nil {
		dir = filepath.Join(dir, *target)
	}
	return filepath.Join(dir, "release")
}
