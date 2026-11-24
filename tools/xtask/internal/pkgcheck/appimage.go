package pkgcheck

import (
	"bytes"
	"context"
	"crypto/sha256"
	"debug/elf"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// hostLibraries are the libraries the AppImage may take from the host
// instead of bundling: core system libraries (not xkb, which it bundles),
// libresolv from Go's cgo net package, libutil from velopack_libc, and
// libfreetype.so.6, which goui-kit links and every desktop Linux system has.
var hostLibraries = map[string]bool{}

func init() {
	for _, name := range strings.Fields("libc.so.6 libm.so.6 libgcc_s.so.1 libxcb.so.1 libfontconfig.so.1 " +
		"libfreetype.so.6 libwayland-client.so.0 libvulkan.so.1 libdl.so.2 libpthread.so.0 librt.so.1 " +
		"libresolv.so.2 libutil.so.1 ld-linux-x86-64.so.2") {
		hostLibraries[name] = true
	}
}

func say(title string) { fmt.Printf("\n== %s ==\n", title) }

// run runs a command with its output passed through.
func run(name string, args ...string) error {
	return task.Run(exec.Command(name, args...))
}

// output runs a command and returns its standard output.
func output(name string, args ...string) (string, error) {
	command := exec.Command(name, args...)
	command.Stderr = os.Stderr
	data, err := command.Output()
	if err != nil {
		return "", task.Errorf("%s: %s", name, err)
	}
	return string(data), nil
}

// CheckAppImage is `check-appimage`, which test-linux-packages runs in each
// throwaway container: it unpacks the AppImage (no FUSE mount, GUI launch,
// or device access) and inspects what it holds.
func CheckAppImage(appimage, version string) error {
	if !inContainer() {
		return task.Error("check-appimage installs packages, so it only runs inside a throwaway container; run `go -C tools/xtask run . test-linux-packages`")
	}
	if filepath.Base(appimage) != "rc2fx-linux-x64.AppImage" {
		return task.Errorf("unexpected AppImage name: %s", appimage)
	}
	if version == "" {
		return task.Error("check-appimage needs the expected version")
	}

	say("installing inspection tools")
	if _, err := exec.LookPath("apt-get"); err == nil {
		if err := os.Setenv("DEBIAN_FRONTEND", "noninteractive"); err != nil {
			return task.Error(task.IOError(err))
		}
		if err := run("apt-get", "update", "-qq"); err != nil {
			return err
		}
		if err := run("apt-get", "install", "-y", "--no-install-recommends", "file", "desktop-file-utils",
			"libxcb1", "libfontconfig1", "libfreetype6", "libwayland-client0", "libvulkan1"); err != nil {
			return err
		}
	} else if _, err := exec.LookPath("dnf"); err == nil {
		if err := run("dnf", "install", "-y", "file", "desktop-file-utils",
			"libxcb", "fontconfig", "freetype", "libwayland-client", "vulkan-loader"); err != nil {
			return err
		}
	} else {
		return task.Error("no apt-get or dnf in the AppImage check container")
	}

	work, err := os.MkdirTemp("", "rc2fx-appimage.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(work)
	app := filepath.Join(work, "rc2fx.AppImage")
	if err := copyFile(appimage, app); err != nil {
		return err
	}
	if err := os.Chmod(app, 0o755); err != nil {
		return task.Error(task.IOError(err))
	}

	say("AppImage header and extraction")
	if err := checkAppImageHeader(app); err != nil {
		return task.Error(err.Error())
	}
	extract := exec.Command(app, "--appimage-extract")
	extract.Dir = work
	if log, err := extract.CombinedOutput(); err != nil {
		return task.Errorf("the AppImage did not unpack: %s\n%s", err, log)
	}
	root := filepath.Join(work, "squashfs-root")
	if info, err := os.Stat(root); err != nil || !info.IsDir() {
		return task.Error("the AppImage unpacked no squashfs-root")
	}

	say("AppRun and application payload")
	appRun := filepath.Join(root, "AppRun")
	if info, err := os.Lstat(appRun); err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return task.Error("AppRun is missing, a link, or not executable")
	}
	if err := run("sh", "-n", appRun); err != nil {
		return err
	}
	if err := contains(appRun, `exec "$APPDIR/usr/bin/rc2fx"`); err != nil {
		return err
	}
	executable := filepath.Join(root, "usr", "bin", "rc2fx")
	if err := checkELF64(executable); err != nil {
		return err
	}
	desktopFile := filepath.Join(root, "usr", "share", "applications", "rc2fx.desktop")
	for _, file := range []string{desktopFile, filepath.Join(root, "usr", "share", "icons", "hicolor", "scalable", "apps", "rc2fx.svg")} {
		if info, err := os.Stat(file); err != nil || !info.Mode().IsRegular() {
			return task.Errorf("missing %s", strings.TrimPrefix(file, root+"/"))
		}
	}
	if err := run("desktop-file-validate", desktopFile); err != nil {
		return err
	}

	say("Velopack updater and version")
	// The AppImage name has no version, so check vpk's sq.version manifest.
	manifest := filepath.Join(root, "usr", "bin", "sq.version")
	for _, want := range []string{"<id>io.github.ibanks42.rc2fx</id>", "<version>" + version + "</version>", "<channel>linux</channel>"} {
		if err := contains(manifest, want); err != nil {
			return err
		}
	}
	if err := checkELF64(filepath.Join(root, "usr", "bin", "UpdateNix")); err != nil {
		return err
	}

	say("licenses and bundled libraries")
	licenses, err := os.ReadFile(filepath.Join(root, "usr", "share", "licenses", "rc2fx", "LICENSES.html"))
	if err != nil || len(licenses) == 0 || !bytes.Contains(bytes.ToLower(licenses), []byte("mit")) {
		return task.Error("LICENSES.html is missing, empty, or has no MIT notices")
	}
	libDir := filepath.Join(root, "usr", "lib")
	entries, err := os.ReadDir(libDir)
	if err != nil {
		return task.Error("the AppImage has no usr/lib")
	}
	bundled := false
	for _, entry := range entries {
		if strings.Contains(entry.Name(), ".so") && (entry.Type().IsRegular() || entry.Type()&fs.ModeSymlink != 0) {
			bundled = true
			break
		}
	}
	if !bundled {
		return task.Error("usr/lib bundles no shared libraries")
	}
	binary, err := elf.Open(executable)
	if err != nil {
		return task.Errorf("%s: %s", executable, err)
	}
	defer binary.Close()
	// Every RUNPATH and RPATH entry must be relative to $ORIGIN, never a
	// build machine's path.
	for _, tag := range []elf.DynTag{elf.DT_RUNPATH, elf.DT_RPATH} {
		values, err := binary.DynString(tag)
		if err != nil {
			return task.Errorf("%s: %s", executable, err)
		}
		for _, value := range values {
			for _, entry := range strings.Split(value, ":") {
				if !strings.HasPrefix(entry, "$ORIGIN") {
					return task.Errorf("AppImage executable has a non-relative RPATH/RUNPATH entry: %s", entry)
				}
			}
		}
	}

	say("dynamic libraries and the embedded mixer envelope")
	needed, err := binary.ImportedLibraries()
	if err != nil {
		return task.Errorf("%s: %s", executable, err)
	}
	for _, library := range needed {
		if _, err := os.Lstat(filepath.Join(libDir, library)); err == nil || hostLibraries[library] {
			continue
		}
		return task.Errorf("AppImage needs %s, but does not bundle it and it is not in the host baseline", library)
	}
	// Test AppRun's own library setup before setting LD_LIBRARY_PATH.
	if err := os.Unsetenv("LD_LIBRARY_PATH"); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := expectInstallerHelperExit2(app); err != nil {
		return err
	}
	ldd := exec.Command("ldd", "-r", executable)
	ldd.Env = append(os.Environ(), "LD_LIBRARY_PATH="+libDir)
	lddOutput, err := ldd.CombinedOutput()
	os.Stdout.Write(lddOutput)
	if err != nil {
		return task.Errorf("ldd -r failed: %s", err)
	}
	if unresolved := regexp.MustCompile(`not found|undefined symbol|version .* not found`).FindAll(lddOutput, -1); len(unresolved) > 0 {
		return task.Error("unresolved library or symbol")
	}
	if err := ExpectEmbeddedEnvelope(executable); err != nil {
		return err
	}
	// Mixer binaries belong in the embedded payload, never loose in the AppImage.
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		switch entry.Name() {
		case "librc2fx_native.so", "rc2fxd", "mixer-release.json":
			return task.Errorf("AppImage contains mixer payload file %s", strings.TrimPrefix(path, root+"/"))
		}
		return nil
	})
	if err != nil {
		return err
	}
	fmt.Println("AppImage extraction, AppRun, libraries, notices, and envelope passed.")
	return nil
}

func contains(file, text string) error {
	data, err := os.ReadFile(file)
	if err != nil {
		return task.Errorf("%s: %s", file, task.IOError(err))
	}
	if !bytes.Contains(data, []byte(text)) {
		return task.Errorf("%s does not contain %q", filepath.Base(file), text)
	}
	return nil
}

// checkELF64 checks that file is an executable 64-bit ELF file, as `file`
// reports it.
func checkELF64(file string) error {
	info, err := os.Stat(file)
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return task.Errorf("%s is missing or not executable", file)
	}
	description, err := output("file", file)
	if err != nil {
		return err
	}
	if !strings.Contains(description, "ELF 64-bit") {
		return task.Errorf("%s is not a 64-bit ELF file: %s", file, strings.TrimSpace(description))
	}
	return nil
}

// expectInstallerHelperExit2 runs the AppImage with --installer-helper and
// no arguments, which exits 2 at once without output and without touching
// devices, files, or the GUI. A shell error in AppRun also exits 2, but
// writes something.
func expectInstallerHelperExit2(app string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	command := exec.CommandContext(ctx, app, "--installer-helper")
	command.Env = append(os.Environ(), "APPIMAGE_EXTRACT_AND_RUN=1")
	var stdout, stderr bytes.Buffer
	command.Stdout, command.Stderr = &stdout, &stderr
	err := command.Run()
	var exit *exec.ExitError
	if !errors.As(err, &exit) || exit.ExitCode() != 2 {
		return task.Errorf("expected --installer-helper to exit 2, got %v", err)
	}
	if stdout.Len() > 0 || stderr.Len() > 0 {
		return task.Errorf("expected --installer-helper to be silent, but it wrote:\n%s%s", stdout.String(), stderr.String())
	}
	return nil
}

// payloadMagic starts the mixer payload envelope embedded in the app.
const payloadMagic = "RC2FX-EMBEDDED-PAYLOAD 1"

// ExpectEmbeddedEnvelope checks that executable is either a download-mode
// build or holds exactly one embedded mixer payload whose length and
// SHA-256 match its header.
func ExpectEmbeddedEnvelope(executable string) error {
	data, err := os.ReadFile(executable)
	if err != nil {
		return task.Errorf("missing executable: %s", executable)
	}
	if bytes.Contains(data, []byte("RC2FX-PAYLOAD-MODE:download:1")) {
		if bytes.Contains(data, []byte("RC2FX-PAYLOAD-MODE:embedded:1")) {
			return task.Error("executable contains conflicting mixer payload modes")
		}
		return nil
	}
	if count := bytes.Count(data, []byte(payloadMagic)); count != 1 {
		return task.Errorf("expected exactly one embedded payload in %s, found %d", executable, count)
	}
	// The magic line is followed by "length N\nsha256 HEX\n" and the body.
	rest := data[bytes.Index(data, []byte(payloadMagic))+len(payloadMagic)+1:]
	lengthLine, rest, _ := bytes.Cut(rest, []byte("\n"))
	digestLine, body, _ := bytes.Cut(rest, []byte("\n"))
	lengthText, ok := strings.CutPrefix(string(lengthLine), "length ")
	length, err := strconv.ParseUint(lengthText, 10, 64)
	if !ok || err != nil || strings.TrimLeft(lengthText, "0123456789") != "" {
		return task.Error("embedded payload length is missing or not a number")
	}
	digest, ok := strings.CutPrefix(string(digestLine), "sha256 ")
	if !ok || len(digest) != 64 || strings.Trim(digest, "0123456789abcdef") != "" {
		return task.Error("embedded payload digest is missing or malformed")
	}
	if length > 256<<20 {
		return task.Errorf("embedded payload is implausibly large: %d", length)
	}
	if uint64(len(body)) < length {
		return task.Error("embedded payload is cut short")
	}
	actual := sha256.Sum256(body[:length])
	if hex.EncodeToString(actual[:]) != digest {
		return task.Error("embedded payload does not match its SHA-256 header")
	}
	return nil
}
