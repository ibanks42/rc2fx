package packaging

// Linux release payload: a minimal AppDir assembled here, shared libraries
// deployed by the externally supplied `linuxdeploy`, and the AppImage, its
// update package and feed made from that AppDir by Velopack's vpk (see
// velopack.go). No repository documentation, source, configuration or mixer
// sidecars enter the image.

import (
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// The AppDir's launcher, desktop entry and icon, in the repository.
const (
	appRunPath  = task.AssetsDir + "/rc2fx.AppRun"
	desktopPath = task.AssetsDir + "/rc2fx.desktop"
	svgPath     = task.AssetsDir + "/rc2fx.svg"
)

// notices for the AppImage runtime, written by prepare-appimage-notices.py.
const notices = "target/appimage-legal/notices.json"

// tools is the pinned AppImage tools.
type tools struct {
	linuxdeploy string
	patchelf    string
}

// toolsFromEnv: the release workflow installs and verifies these binaries,
// then passes their absolute paths. Keeping versions out of xtask leaves one
// pinned dependency definition in CI.
func toolsFromEnv() (tools, error) {
	linuxdeploy, err := tool("RC2FX_LINUXDEPLOY")
	if err != nil {
		return tools{}, err
	}
	patchelf, err := tool("RC2FX_PATCHELF")
	if err != nil {
		return tools{}, err
	}
	return tools{linuxdeploy, patchelf}, nil
}

func tool(variable string) (string, error) {
	path, ok := os.LookupEnv(variable)
	if !ok {
		return "", task.Errorf("%s must point to the pinned AppImage tool", variable)
	}
	if !filepath.IsAbs(path) {
		return "", task.Errorf("%s must be an absolute path: %s", variable, path)
	}
	if info, err := os.Stat(path); err != nil || !info.Mode().IsRegular() {
		return "", task.Errorf("%s is not a file: %s", variable, path)
	}
	return path, nil
}

func appimageArch(arch string) (string, error) {
	switch arch {
	case "x64":
		return "x86_64", nil
	case "arm64":
		return "aarch64", nil
	}
	return "", task.Errorf("AppImage has no architecture mapping for %s", arch)
}

func writeMember(path string, data []byte, mode os.FileMode) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return task.Errorf("%s: %s", filepath.Dir(path), task.IOError(err))
	}
	if err := os.WriteFile(path, data, 0o666); err != nil {
		return task.Errorf("%s: %s", path, task.IOError(err))
	}
	// Windows does not have Unix file permission bits.
	if runtime.GOOS != "windows" {
		if err := os.Chmod(path, mode); err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
	}
	return nil
}

func licenseName(name string) (string, error) {
	if name == "" || name == "." || name == ".." || strings.ContainsAny(name, `/\`) || filepath.Base(name) != name {
		return "", task.Errorf("invalid licence file name %s", task.Debug(name))
	}
	return name, nil
}

func asset(path string) ([]byte, error) {
	data, err := os.ReadFile(filepath.Join(task.Root(), filepath.FromSlash(path)))
	if err != nil {
		return nil, task.Errorf("%s: %s", path, task.IOError(err))
	}
	return data, nil
}

// stage creates the application payload before dependency deployment. The
// only repository files here are the icon, desktop entry and licence
// reports.
func stage(appdir string, executable []byte, licenceFiles []licenseFile) error {
	if _, err := os.Lstat(appdir); err == nil {
		if err := os.RemoveAll(appdir); err != nil {
			return task.Errorf("%s: %s", appdir, task.IOError(err))
		}
	}
	appRun, err := asset(appRunPath)
	if err != nil {
		return err
	}
	desktopEntry, err := asset(desktopPath)
	if err != nil {
		return err
	}
	icon, err := asset(svgPath)
	if err != nil {
		return err
	}
	if err := writeMember(filepath.Join(appdir, "usr/bin/rc2fx"), executable, 0o755); err != nil {
		return err
	}
	if err := writeMember(filepath.Join(appdir, "usr/share/applications/rc2fx.desktop"), desktopEntry, 0o644); err != nil {
		return err
	}
	if err := writeMember(filepath.Join(appdir, "usr/share/icons/hicolor/scalable/apps/rc2fx.svg"), icon, 0o644); err != nil {
		return err
	}
	for _, l := range licenceFiles {
		name, err := licenseName(l.name)
		if err != nil {
			return err
		}
		if err := writeMember(filepath.Join(appdir, "usr/share/licenses/rc2fx", name), l.data, 0o644); err != nil {
			return err
		}
	}
	return writeMember(filepath.Join(appdir, "AppRun"), appRun, 0o755)
}

// stageLibraries puts the libraries the app loads in the AppDir's usr/lib,
// beside where linuxdeploy deploys the system libraries: usr/bin/rc2fx
// finds them through its $ORIGIN/../lib run path, and AppRun puts usr/lib
// on LD_LIBRARY_PATH.
func stageLibraries(appdir string, libraries []library) error {
	for _, l := range libraries {
		if err := writeMember(filepath.Join(appdir, "usr/lib", l.name), l.data, 0o755); err != nil {
			return err
		}
	}
	return nil
}

// ensureRootEntries: AppDir root entries the AppImage format requires.
// linuxdeploy normally makes these, but keep the fallback deterministic when
// it does not.
func ensureRootEntries(appdir string) error {
	for _, pair := range [][2]string{
		{"usr/share/applications/rc2fx.desktop", "rc2fx.desktop"},
		{"usr/share/icons/hicolor/scalable/apps/rc2fx.svg", "rc2fx.svg"},
	} {
		target := filepath.Join(appdir, pair[1])
		if _, err := os.Stat(target); err != nil {
			data, err := os.ReadFile(filepath.Join(appdir, pair[0]))
			if err == nil {
				err = os.WriteFile(target, data, 0o644)
			}
			if err != nil {
				return task.Errorf("%s: %s", target, task.IOError(err))
			}
		}
	}
	return nil
}

// ensureRegularAppRun: the user-private helper requires a regular,
// executable AppRun at the AppDir root. Recheck the launcher after
// linuxdeploy, keeping the contract explicit if a tool ever replaces it with
// a link.
func ensureRegularAppRun(appdir string) error {
	path := filepath.Join(appdir, "AppRun")
	info, err := os.Lstat(path)
	if err != nil {
		return task.Errorf("%s: %s", path, task.IOError(err))
	}
	switch {
	case info.Mode()&fs.ModeSymlink != 0:
		data, err := os.ReadFile(path)
		if err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
		if err := os.Remove(path); err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
		return writeMember(path, data, 0o755)
	case !info.Mode().IsRegular():
		return task.Errorf("%s is not a regular file", path)
	case runtime.GOOS != "windows":
		if err := os.Chmod(path, 0o755); err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
	}
	return nil
}

// licenseSections is notices for the libraries linuxdeploy is allowed to
// copy. Take the exact build host's copyright files; never substitute
// generic license templates. Paths are Debian's and then RHEL's (the release
// container is AlmaLinux 8). RHEL's libxcb ships no license file, so that
// container extracts libxcb's COPYING from the matching source package into
// /usr/share/licenses/libxcb. libXdmcp, libbsd and libmd come in only where
// libxcb links them (Debian), so they need notices only when the build host
// has them.
func licenseSections(root string) ([]licenses.Section, error) {
	var sections []licenses.Section
	for _, entry := range []struct {
		name      string
		locations []string
		onlyWith  string
	}{
		{"libxkbcommon / libxkbcommon-x11", []string{
			"/usr/share/doc/libxkbcommon0/copyright",
			"/usr/share/licenses/libxkbcommon/LICENSE",
		}, ""},
		{"libXau", []string{
			"/usr/share/doc/libxau6/copyright",
			"/usr/share/doc/libXau/COPYING",
			"/usr/share/licenses/libXau/COPYING",
		}, ""},
		{"libXdmcp", []string{
			"/usr/share/doc/libxdmcp6/copyright",
			"/usr/share/doc/libXdmcp/COPYING",
			"/usr/share/licenses/libXdmcp/COPYING",
		}, "libXdmcp.so.6"},
		{"libxcb-xkb", []string{
			"/usr/share/doc/libxcb-xkb1/copyright",
			"/usr/share/licenses/libxcb/COPYING",
		}, ""},
		{"libbsd", []string{
			"/usr/share/doc/libbsd0/copyright",
			"/usr/share/licenses/libbsd/COPYING",
		}, "libbsd.so.0"},
		{"libmd", []string{
			"/usr/share/doc/libmd0/copyright",
			"/usr/share/licenses/libmd/COPYING",
		}, "libmd.so.0"},
	} {
		if entry.onlyWith != "" && !hostHasLibrary(entry.onlyWith) {
			continue
		}
		var path string
		for _, location := range entry.locations {
			if info, err := os.Stat(location); err == nil && info.Mode().IsRegular() {
				path = location
				break
			}
		}
		if path == "" {
			return nil, task.Errorf("missing build-host copyright file for bundled %s", entry.name)
		}
		text, err := os.ReadFile(path)
		if err != nil {
			return nil, task.Errorf("%s: %s", path, task.IOError(err))
		}
		if strings.TrimSpace(string(text)) == "" {
			return nil, task.Errorf("empty copyright file for %s", entry.name)
		}
		sections = append(sections, licenses.Section{Title: "AppImage library — " + entry.name, Text: string(text)})
	}
	document, err := readNotices(root)
	if err != nil {
		return nil, err
	}
	runtimeValue, _ := document.Get("sections")
	runtimeSections, ok := runtimeValue.AsArray()
	if !ok || len(runtimeSections) == 0 {
		return nil, task.Error("missing AppImage runtime notices")
	}
	for _, section := range runtimeSections {
		titleValue, _ := section.Get("title")
		title, ok := titleValue.AsString()
		if !ok || strings.TrimSpace(title) == "" {
			return nil, task.Error("missing runtime notice title")
		}
		textValue, _ := section.Get("text")
		text, ok := textValue.AsString()
		if !ok || strings.TrimSpace(text) == "" {
			return nil, task.Error("missing runtime license text")
		}
		sections = append(sections, licenses.Section{Title: title, Text: text})
	}
	return sections, nil
}

// readNotices is the runtime notices, which name the exact runtime they
// cover.
func readNotices(root string) (core.Value, error) {
	path := filepath.Join(root, filepath.FromSlash(notices))
	data, err := os.ReadFile(path)
	if err != nil {
		return core.Value{}, task.Errorf("%s: %s; run python3 .github/scripts/prepare-appimage-notices.py", path, task.IOError(err))
	}
	document, err := core.ParseJSONBytes(data, 64<<20, 4_000_000)
	if err != nil {
		return core.Value{}, task.Error(err.Error())
	}
	schema, _ := document.Get("schemaVersion")
	if n, ok := schema.AsU64(); !ok || n != 2 {
		return core.Value{}, task.Errorf("unsupported AppImage notices schema in %s; rerun prepare-appimage-notices.py", path)
	}
	return document, nil
}

// checkRuntime: vpk puts its own runtime in front of the image. It must be
// the one the notices and the published runtime sources cover.
func checkRuntime(appimage []byte, notices core.Value) error {
	sizeValue, _ := notices.Get("runtimeSize")
	size, ok := sizeValue.AsU64()
	if !ok || size > uint64(^uint(0)>>1) {
		return task.Error("AppImage notices do not give the runtime's size")
	}
	shaValue, _ := notices.Get("runtimeSha256")
	expected, ok := shaValue.AsString()
	if !ok {
		return task.Error("AppImage notices do not identify the runtime")
	}
	if uint64(len(appimage)) >= size && task.Sha256(appimage[:size]) == expected {
		return nil
	}
	return task.Error("the AppImage runtime differs from the one covered by its notices and source bundle")
}

// hostHasLibrary reports whether the build host has library in a system
// library directory, where linuxdeploy would find it.
func hostHasLibrary(library string) bool {
	for _, dir := range []string{
		"/usr/lib/x86_64-linux-gnu",
		"/lib/x86_64-linux-gnu",
		"/usr/lib/aarch64-linux-gnu",
		"/lib/aarch64-linux-gnu",
		"/usr/lib64",
		"/lib64",
		"/usr/lib",
	} {
		if _, err := os.Stat(filepath.Join(dir, library)); err == nil {
			return true
		}
	}
	return false
}

// coveredLibrary: a library the AppImage may carry because its notice is in
// the licence document. libgpuikit's notices are its own THIRD_PARTY.md and
// licence texts (licenses.Gpuikit).
func coveredLibrary(name string) bool {
	for _, prefix := range []string{
		"libxkbcommon.so",
		"libxkbcommon-x11.so",
		"libXau.so",
		"libXdmcp.so",
		"libxcb-xkb.so",
		"libbsd.so",
		"libmd.so",
		"libgpuikit.so",
	} {
		if name == prefix || strings.HasPrefix(name, prefix+".") {
			return true
		}
	}
	return false
}

func verifyLibraryNotices(directory string) error {
	if _, err := os.Stat(directory); err != nil {
		return nil
	}
	entries, err := os.ReadDir(directory)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	for _, entry := range entries {
		if entry.IsDir() {
			if err := verifyLibraryNotices(filepath.Join(directory, entry.Name())); err != nil {
				return err
			}
		} else if !coveredLibrary(entry.Name()) {
			return task.Errorf("linuxdeploy bundled %s without a reviewed license notice; add its build-host copyright to AppImage license_sections", entry.Name())
		}
	}
	return nil
}

// deploy deploys shared libraries with linuxdeploy and checks the AppDir it
// leaves. linuxdeploy is never asked to produce the image itself: vpk does.
func deploy(appdir, arch string, t tools) error {
	machine, err := appimageArch(arch)
	if err != nil {
		return err
	}
	command := exec.Command(t.linuxdeploy,
		"--appdir", appdir,
		"--executable", filepath.Join(appdir, "usr/bin/rc2fx"),
		"--desktop-file", filepath.Join(appdir, "usr/share/applications/rc2fx.desktop"),
		"--icon-file", filepath.Join(appdir, "usr/share/icons/hicolor/scalable/apps/rc2fx.svg"),
	)
	command.Dir = filepath.Dir(appdir)
	command.Env = append(os.Environ(), "ARCH="+machine, "APPIMAGE_EXTRACT_AND_RUN=1", "NO_STRIP=1", "PATCHELF="+t.patchelf)
	if err := task.Run(command); err != nil {
		return err
	}
	if err := verifyLibraryNotices(filepath.Join(appdir, "usr/lib")); err != nil {
		return err
	}
	if err := ensureRegularAppRun(appdir); err != nil {
		return err
	}
	return ensureRootEntries(appdir)
}

// packageAppImage stages and deploys the AppDir, then has vpk package it:
// the AppImage, its update package and feed, returned as published. The
// AppDir is removed afterwards; out keeps only published files.
func packageAppImage(root, out, name, version string, executable []byte, licenceFiles []licenseFile,
	libraries []library, arch string, t tools) ([]string, error) {
	runtimeNotices, err := readNotices(root)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(out, 0o777); err != nil {
		return nil, task.Errorf("%s: %s", out, task.IOError(err))
	}
	// Older layouts (a tar.gz, a folder, a versioned AppImage) must not
	// survive into this release's output.
	_ = os.Remove(filepath.Join(out, name+".tar.gz"))
	_ = os.Remove(filepath.Join(out, name+".AppImage"))
	_ = os.RemoveAll(filepath.Join(out, name))
	_ = os.RemoveAll(filepath.Join(out, "."+name+".AppDir"))
	_ = os.Remove(filepath.Join(out, "SHA256SUMS"))
	// Keep the working AppDir outside target/package. vpk packages a folder
	// as a finished AppDir only when its name ends in .AppDir.
	work := filepath.Join(filepath.Dir(out), name+"-appimage-work")
	_ = os.RemoveAll(work)
	if err := os.MkdirAll(work, 0o777); err != nil {
		return nil, task.Errorf("%s: %s", work, task.IOError(err))
	}
	defer os.RemoveAll(work)
	appdir := filepath.Join(work, "rc2fx.AppDir")
	if err := stage(appdir, executable, licenceFiles); err != nil {
		return nil, err
	}
	if err := stageLibraries(appdir, libraries); err != nil {
		return nil, err
	}
	if err := deploy(appdir, arch, t); err != nil {
		return nil, err
	}
	if expected, err := embed.Find(executable); err == nil {
		// linuxdeploy may adjust ELF search paths. The embedded installer
		// payload must still be the one validated before staging.
		deployed, err := os.ReadFile(filepath.Join(appdir, "usr/bin/rc2fx"))
		if err != nil {
			return nil, task.Errorf("deployed executable: %s", task.IOError(err))
		}
		found, err := embed.Find(deployed)
		if err != nil {
			return nil, task.Error(err.Error())
		}
		if err := found.RequireMatches(expected); err != nil {
			return nil, task.Error(err.Error())
		}
	}
	published, err := pack(root, "linux", arch, version, name, appdir, out)
	if err != nil {
		return nil, err
	}
	var appimage string
	for _, path := range published {
		if extension(path) == "AppImage" {
			appimage = path
			break
		}
	}
	if appimage == "" {
		return nil, task.Error("vpk published no AppImage")
	}
	image, err := os.ReadFile(appimage)
	if err != nil {
		return nil, task.Error(task.IOError(err))
	}
	if err := checkRuntime(image, runtimeNotices); err != nil {
		return nil, err
	}
	for _, path := range published {
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, task.Error(task.IOError(err))
		}
		fmt.Printf("Package ready:\n  %s\n  sha256 %s\n", path, task.Sha256(data))
	}
	return published, nil
}

// extension is Path::extension of a path's file name.
func extension(path string) string {
	name := filepath.Base(path)
	at := strings.LastIndexByte(name, '.')
	if at <= 0 {
		return ""
	}
	return name[at+1:]
}
