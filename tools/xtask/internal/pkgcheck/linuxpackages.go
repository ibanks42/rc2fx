package pkgcheck

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// defaultImages are the distributions the AppImage is checked on. AlmaLinux
// 8 has the oldest supported glibc (2.28); the rest are common current ones.
const defaultImages = "docker.io/library/almalinux:8 docker.io/library/ubuntu:22.04 docker.io/library/debian:12-slim " +
	"docker.io/library/ubuntu:24.04 docker.io/library/debian:13-slim docker.io/library/fedora:44"

const linuxPackagesUsage = `usage: go -C tools/xtask run . test-linux-packages [DIR] [--artifacts DIR] [--version VERSION]
                                  [--runtime auto|podman|docker]
Checks the current version's AppImage, update package, and feed, then
inspects the AppImage in throwaway containers (RC2FX_APPIMAGE_IMAGES lists
them). Defaults: target/package, the version in config/app-release.json, and
CONTAINER_ENGINE or else podman when installed, otherwise docker.`

var releaseVersion = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$`)

func failf(format string, args ...any) error {
	return task.Errorf("package check failed: "+format, args...)
}

// TestLinuxPackages is `test-linux-packages`. It checks the Linux release
// artifacts on the host, then streams a copy of the AppImage and a static
// xtask into each container, so no artifact is modified, nothing on the
// host is mounted, and no device, display, or FUSE access is needed. The
// containers need network access to install inspection tools.
func TestLinuxPackages(args []string) error {
	root := task.Root()
	artifacts, version, runtime := "", "", os.Getenv("CONTAINER_ENGINE")
	if runtime == "" {
		runtime = os.Getenv("RC2FX_CONTAINER_RUNTIME")
	}
	for i := 0; i < len(args); i++ {
		switch arg := args[i]; {
		case arg == "--artifacts" || arg == "--version" || arg == "--runtime":
			if i+1 >= len(args) {
				return task.Error(linuxPackagesUsage)
			}
			i++
			switch arg {
			case "--artifacts":
				artifacts = args[i]
			case "--version":
				version = args[i]
			default:
				runtime = args[i]
			}
		case arg == "-h" || arg == "--help":
			fmt.Println(linuxPackagesUsage)
			return nil
		case strings.HasPrefix(arg, "-"):
			return task.Errorf("unknown argument: %s\n%s", arg, linuxPackagesUsage)
		default:
			if artifacts != "" {
				return task.Errorf("artifact directory given twice\n%s", linuxPackagesUsage)
			}
			artifacts = arg
		}
	}
	if artifacts == "" {
		artifacts = filepath.Join(root, "target", "package")
	}
	artifacts, err := filepath.Abs(artifacts)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	if info, err := os.Stat(artifacts); err != nil || !info.IsDir() {
		return failf("artifact directory does not exist: %s", artifacts)
	}
	if version == "" {
		var config struct {
			Version string `json:"version"`
		}
		data, err := os.ReadFile(filepath.Join(root, "config", "app-release.json"))
		if err == nil {
			err = json.Unmarshal(data, &config)
		}
		if err != nil {
			return task.Errorf("config/app-release.json: %s", err)
		}
		version = config.Version
	}
	if !releaseVersion.MatchString(version) {
		return failf("invalid version: %s", version)
	}

	// Published names leave out the version; the update package and feed
	// carry it.
	appimage := filepath.Join(artifacts, "rc2fx-linux-x64.AppImage")
	nupkg := filepath.Join(artifacts, "io.github.ibanks42.rc2fx-"+version+"-linux-full.nupkg")
	feed := filepath.Join(artifacts, "releases.linux.json")
	sources := filepath.Join(artifacts, "rc2fx-"+version+"-appimage-runtime-sources.tar.gz")
	for _, file := range []string{appimage, nupkg, feed} {
		if err := checkArtifact(file); err != nil {
			return err
		}
	}
	// Refuse other versions' artifacts so nothing can pick them up by mistake.
	for _, pattern := range []string{"rc2fx*.AppImage", "io.github.ibanks42.rc2fx-*-linux-full.nupkg"} {
		matches, _ := filepath.Glob(filepath.Join(artifacts, pattern))
		for _, match := range matches {
			if match != appimage && match != nupkg {
				return failf("stale artifact in %s: %s", artifacts, filepath.Base(match))
			}
		}
	}
	if err := checkAppImageHeader(appimage); err != nil {
		return failf("%s", err)
	}
	if err := checkFeed(feed, nupkg, appimage, version); err != nil {
		return failf("the update feed or package does not match the AppImage: %s", err)
	}
	if err := checkArtifact(sources); err != nil {
		return err
	}
	if err := checkSources(sources); err != nil {
		return failf("%s", err)
	}

	runtime, err = engine(runtime)
	if err != nil {
		return failf("%s", err)
	}
	if err := exec.Command(runtime, "info").Run(); err != nil {
		return failf("%s is not usable by this user", runtime)
	}

	if err := os.MkdirAll(filepath.Join(root, "target"), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := os.MkdirTemp(filepath.Join(root, "target"), "linux-package-check.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(stage)
	// A static xtask runs the checks in the container.
	build := exec.Command("go", "build", "-o", filepath.Join(stage, "xtask"), ".")
	build.Dir = filepath.Join(root, "tools", "xtask")
	build.Env = append(os.Environ(), "CGO_ENABLED=0", "GOOS=linux", "GOARCH=amd64")
	if err := task.Run(build); err != nil {
		return err
	}
	if err := copyFile(appimage, filepath.Join(stage, filepath.Base(appimage))); err != nil {
		return err
	}

	images := os.Getenv("RC2FX_APPIMAGE_IMAGES")
	if images == "" {
		images = defaultImages
	}
	fmt.Printf("Checking %s artifacts in %s with %s\n", version, artifacts, runtime)
	for _, image := range strings.Fields(images) {
		fmt.Printf("\n=== %s: %s ===\n", image, filepath.Base(appimage))
		if err := runInContainer(runtime, image, stage, filepath.Base(appimage), version); err != nil {
			return failf("%s failed for %s: %s", image, filepath.Base(appimage), err)
		}
	}
	fmt.Printf("\nAll Linux package checks passed for %s.\n", version)
	return nil
}

func checkArtifact(file string) error {
	info, err := os.Lstat(file)
	switch {
	case err != nil:
		return failf("missing exact artifact: %s", file)
	case info.Mode()&os.ModeSymlink != 0:
		return failf("artifact is a symlink: %s", file)
	case !info.Mode().IsRegular():
		return failf("missing exact artifact: %s", file)
	case info.Size() == 0:
		return failf("artifact is empty: %s", file)
	}
	f, err := os.Open(file)
	if err != nil {
		return failf("artifact is not readable: %s", file)
	}
	return f.Close()
}

// checkAppImageHeader checks for an ELF file marked as a type 2 AppImage.
func checkAppImageHeader(file string) error {
	f, err := os.Open(file)
	if err != nil {
		return err
	}
	defer f.Close()
	header := make([]byte, 11)
	if _, err := io.ReadFull(f, header); err != nil {
		return fmt.Errorf("AppImage is too short")
	}
	if !bytes.Equal(header[:4], []byte("\x7fELF")) {
		return fmt.Errorf("AppImage does not start with an ELF header")
	}
	if !bytes.Equal(header[8:11], []byte("AI\x02")) {
		return fmt.Errorf("AppImage is not type 2")
	}
	return nil
}

func fileSHA256(file string) (string, error) {
	f, err := os.Open(file)
	if err != nil {
		return "", err
	}
	defer f.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

// checkFeed checks that the feed lists exactly this update package and
// that the package holds exactly this AppImage.
func checkFeed(feed, nupkg, appimage, version string) error {
	data, err := os.ReadFile(feed)
	if err != nil {
		return err
	}
	var document struct {
		Assets []struct {
			PackageID string `json:"PackageId"`
			Version   string
			Type      string
			FileName  string
			SHA256    string
		}
	}
	if err := json.Unmarshal(data, &document); err != nil {
		return fmt.Errorf("%s: %s", filepath.Base(feed), err)
	}
	digest, err := fileSHA256(nupkg)
	if err != nil {
		return err
	}
	assets := document.Assets
	if len(assets) != 1 || assets[0].PackageID != "io.github.ibanks42.rc2fx" || assets[0].Version != version ||
		assets[0].Type != "Full" || assets[0].FileName != filepath.Base(nupkg) || strings.ToLower(assets[0].SHA256) != digest {
		return fmt.Errorf("unexpected feed assets %+v", assets)
	}
	reader, err := zip.OpenReader(nupkg)
	if err != nil {
		return err
	}
	defer reader.Close()
	member, err := reader.Open("lib/app/io.github.ibanks42.rc2fx.AppImage")
	if err != nil {
		return fmt.Errorf("%s: %s", filepath.Base(nupkg), err)
	}
	defer member.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, member); err != nil {
		return err
	}
	want, err := fileSHA256(appimage)
	if err != nil {
		return err
	}
	if hex.EncodeToString(hash.Sum(nil)) != want {
		return fmt.Errorf("%s holds a different AppImage", filepath.Base(nupkg))
	}
	return nil
}

// checkSources checks that the AppImage runtime sources list themselves in
// SOURCES.json.
func checkSources(file string) error {
	f, err := os.Open(file)
	if err != nil {
		return err
	}
	defer f.Close()
	decoder, err := gzip.NewReader(f)
	if err != nil {
		return fmt.Errorf("%s: %s", filepath.Base(file), err)
	}
	reader := tar.NewReader(decoder)
	for {
		header, err := reader.Next()
		if err == io.EOF {
			return fmt.Errorf("AppImage runtime sources have no SOURCES.json")
		}
		if err != nil {
			return fmt.Errorf("%s: %s", filepath.Base(file), err)
		}
		if path.Base(header.Name) == "SOURCES.json" {
			return nil
		}
	}
}

func copyFile(from, to string) error {
	data, err := os.ReadFile(from)
	if err != nil {
		return task.Errorf("%s: %s", from, task.IOError(err))
	}
	if err := os.WriteFile(to, data, 0o644); err != nil {
		return task.Errorf("%s: %s", to, task.IOError(err))
	}
	return nil
}

// runInContainer streams stage into a throwaway container from image and
// runs `xtask check-appimage` there.
func runInContainer(runtime, image, stage, appimage, version string) error {
	command := exec.Command(runtime, "run", "--rm", "-i",
		"-e", "CHECK_ARTIFACT="+appimage, "-e", "CHECK_VERSION="+version,
		image, "sh", "-ec", `work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xf - -C "$work"
"$work/xtask" check-appimage "$work/$CHECK_ARTIFACT" "$CHECK_VERSION"`)
	reader, writer := io.Pipe()
	command.Stdin = reader
	go func() { writer.CloseWithError(writeFolder(writer, stage)) }()
	err := task.Run(command)
	reader.Close()
	return err
}

// writeFolder writes the files in dir as a tar stream.
func writeFolder(w io.Writer, dir string) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	var names []string
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return writeSnapshot(w, dir, names)
}
