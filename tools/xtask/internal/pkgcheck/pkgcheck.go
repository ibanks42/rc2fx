// Package pkgcheck builds the Linux AppImage in the AlmaLinux 8 build
// container and checks it across distribution containers, for releases and
// `just package-check`.
package pkgcheck

import (
	"archive/tar"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// containerfile builds the image the AppImage is built in.
const containerfile = ".github/scripts/package-check.Containerfile"

// ImageTag names the build image after the Containerfile and the Rust
// toolchain, so both package-check and the build-image workflow rebuild
// whenever Go, Rust, or the base packages change.
func ImageTag(root string) (string, error) {
	hash := sha256.New()
	for _, name := range []string{containerfile, "rust-toolchain.toml"} {
		data, err := os.ReadFile(filepath.Join(root, filepath.FromSlash(name)))
		if err != nil {
			return "", task.Errorf("%s: %s", name, task.IOError(err))
		}
		hash.Write(data)
	}
	return hex.EncodeToString(hash.Sum(nil))[:12], nil
}

// PrintImageTag is `build-image-tag`.
func PrintImageTag() error {
	tag, err := ImageTag(task.Root())
	if err != nil {
		return err
	}
	fmt.Println(tag)
	return nil
}

var rustChannel = regexp.MustCompile(`(?m)^channel = "(.*)"$`)

// RustVersion is the toolchain rust-toolchain.toml pins.
func RustVersion(root string) (string, error) {
	data, err := os.ReadFile(filepath.Join(root, "rust-toolchain.toml"))
	if err != nil {
		return "", task.Errorf("rust-toolchain.toml: %s", task.IOError(err))
	}
	match := rustChannel.FindSubmatch(data)
	if match == nil {
		return "", task.Error("rust-toolchain.toml has no channel")
	}
	return string(match[1]), nil
}

// engine resolves choice to "podman" or "docker". When choice is empty or
// "auto", it picks whichever is installed, preferring podman.
func engine(choice string) (string, error) {
	if choice == "" || choice == "auto" {
		for _, candidate := range []string{"podman", "docker"} {
			if _, err := exec.LookPath(candidate); err == nil {
				return candidate, nil
			}
		}
		return "", task.Error("podman or docker is required")
	}
	if choice != "podman" && choice != "docker" {
		return "", task.Errorf("unsupported container engine %s", choice)
	}
	if _, err := exec.LookPath(choice); err != nil {
		return "", task.Errorf("%s is not installed", choice)
	}
	return choice, nil
}

// Check is `package-check`: it builds the AppImage from the working tree
// in the build container and tests it in distribution containers. Output
// goes under target/package-check.
func Check() error {
	if runtime.GOOS != "linux" || runtime.GOARCH != "amd64" {
		return task.Error("package-check needs x86-64 Linux")
	}
	root := task.Root()
	engine, err := engine(os.Getenv("CONTAINER_ENGINE"))
	if err != nil {
		return task.Errorf("package-check: %s", err)
	}
	// Keep mounted build outputs owned by the host user.
	var user []string
	switch engine {
	case "podman":
		user = []string{"--userns=keep-id"}
	case "docker":
		info, err := exec.Command("docker", "info", "--format", "{{.SecurityOptions}}").Output()
		if err != nil {
			return task.Errorf("docker info: %s", err)
		}
		if !strings.Contains(string(info), "rootless") {
			user = []string{"--user", fmt.Sprintf("%d:%d", os.Getuid(), os.Getgid())}
		}
	}

	tag, err := ImageTag(root)
	if err != nil {
		return err
	}
	// CI passes the prebuilt image from build-image.yml; local runs build it.
	image := os.Getenv("RC2FX_BUILD_IMAGE")
	if image != "" {
		if !strings.HasSuffix(image, ":"+tag) {
			return task.Errorf("RC2FX_BUILD_IMAGE %s is not the build image for tag %s", image, tag)
		}
		if err := task.Run(exec.Command(engine, "pull", image)); err != nil {
			return err
		}
	} else {
		rust, err := RustVersion(root)
		if err != nil {
			return err
		}
		image = "localhost/rc2fx-package-check:" + tag
		build := exec.Command(engine, "build", "--build-arg", "RUST_VERSION="+rust,
			"--build-arg", "PACKAGE_REFRESH="+strconv.FormatInt(time.Now().Unix(), 10),
			"-t", image, "-f", filepath.Join(root, filepath.FromSlash(containerfile)),
			filepath.Join(root, ".github", "scripts"))
		if err := task.Run(build); err != nil {
			return err
		}
	}

	check := filepath.Join(root, "target", "package-check")
	for _, dir := range []string{"target", "cache"} {
		if err := os.MkdirAll(filepath.Join(check, dir), 0o777); err != nil {
			return task.Error(task.IOError(err))
		}
	}
	// Under `docker --user`, .NET (vpk) cannot create its named mutexes for
	// a UID the image does not know, so give it passwd and group entries.
	if engine == "docker" && len(user) > 0 {
		mounts, err := dockerIdentity(image, filepath.Join(check, "identity"))
		if err != nil {
			return err
		}
		user = append(user, mounts...)
	}

	files, err := sourceFiles(root)
	if err != nil {
		return err
	}
	digest, err := sourceDigest(root, files)
	if err != nil {
		return err
	}
	head, _ := exec.Command("git", "-C", root, "rev-parse", "--short", "HEAD").Output()
	changes, _ := exec.Command("git", "-C", root, "status", "--short").Output()
	headText := strings.TrimSpace(string(head))
	changeText := strings.TrimRight(string(changes), "\n")
	fmt.Printf("Packaging %s, source digest %s, with these local changes:\n", headText, digest)
	if changeText == "" {
		fmt.Println("(none)")
	} else {
		fmt.Println(changeText)
	}

	// -modcacherw lets the host user delete the mounted Go module cache.
	args := append([]string{"run", "--rm", "-i"}, user...)
	args = append(args, "--security-opt", "label=disable",
		"-v", filepath.Join(check, "target")+":/work/src/target",
		"-v", filepath.Join(check, "cache")+":/work/cache",
		"-e", "CARGO_HOME=/work/cache/cargo", "-e", "HOME=/work/cache/home",
		"-e", "DOTNET_CLI_HOME=/work/cache/home",
		"-e", "MISE_DATA_DIR=/work/cache/mise", "-e", "MISE_CACHE_DIR=/work/cache/mise-cache",
		"-e", "MISE_TRUSTED_CONFIG_PATHS=/work/src", "-e", "MISE_YES=1",
		"-e", "GOCACHE=/work/cache/go-build", "-e", "GOMODCACHE=/work/cache/gomod",
		"-e", "GOFLAGS=-modcacherw", "-e", "GOTOOLCHAIN=local",
		"-w", "/work/src", image, "sh", "-ec",
		`mkdir -p "$HOME"
tar --extract --no-same-owner --directory=/work/src
exec go -C tools/xtask run . package-check-build`)
	run := exec.Command(engine, args...)
	reader, writer := io.Pipe()
	run.Stdin = reader
	go func() { writer.CloseWithError(writeSnapshot(writer, root, files)) }()
	err = task.Run(run)
	reader.Close()
	if err != nil {
		return err
	}

	if err := TestLinuxPackages([]string{"--artifacts", filepath.Join(check, "target", "package"), "--runtime", engine}); err != nil {
		return err
	}
	// The package tests read the live tree, so a change during the run
	// voids the result.
	files, err = sourceFiles(root)
	if err != nil {
		return err
	}
	after, err := sourceDigest(root, files)
	if err != nil {
		return err
	}
	if after != digest {
		return task.Errorf("Package check is void: the source changed during the run (was digest %s)", digest)
	}
	suffix := ""
	if changeText != "" {
		suffix = ", with local changes"
	}
	fmt.Printf("Package check passed for %s, source digest %s%s\n", headText, digest, suffix)
	fmt.Printf("Packages: %s\n", filepath.Join(check, "target", "package"))
	return nil
}

// dockerIdentity writes the image's passwd and group files with entries
// added for this user, and returns the mounts that put them in place.
func dockerIdentity(image, dir string) ([]string, error) {
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return nil, task.Error(task.IOError(err))
	}
	uid, gid := strconv.Itoa(os.Getuid()), strconv.Itoa(os.Getgid())
	entries := []struct{ name, id, line string }{
		{"passwd", uid, "rc2fx:x:" + uid + ":" + gid + "::/work/cache/home:/bin/sh"},
		{"group", gid, "rc2fx:x:" + gid + ":"},
	}
	var mounts []string
	for _, entry := range entries {
		data, err := exec.Command("docker", "run", "--rm", image, "cat", "/etc/"+entry.name).Output()
		if err != nil {
			return nil, task.Errorf("could not read /etc/%s from %s: %s", entry.name, image, err)
		}
		found := false
		for _, line := range strings.Split(string(data), "\n") {
			if fields := strings.Split(line, ":"); len(fields) > 2 && fields[2] == entry.id {
				found = true
				break
			}
		}
		if !found {
			if len(data) > 0 && !bytes.HasSuffix(data, []byte("\n")) {
				data = append(data, '\n')
			}
			data = append(data, entry.line+"\n"...)
		}
		path := filepath.Join(dir, entry.name)
		if err := os.WriteFile(path, data, 0o644); err != nil {
			return nil, task.Error(task.IOError(err))
		}
		if err := os.Chmod(path, 0o644); err != nil {
			return nil, task.Error(task.IOError(err))
		}
		mounts = append(mounts, "-v", path+":/etc/"+entry.name+":ro")
	}
	return mounts, nil
}

// sourceFiles lists the tracked and untracked, not ignored, files that
// exist, sorted.
func sourceFiles(root string) ([]string, error) {
	output, err := exec.Command("git", "-C", root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").Output()
	if err != nil {
		return nil, task.Errorf("git ls-files: %s", err)
	}
	var files []string
	for _, name := range strings.Split(string(output), "\x00") {
		if name == "" {
			continue
		}
		if _, err := os.Lstat(filepath.Join(root, filepath.FromSlash(name))); err == nil {
			files = append(files, name)
		}
	}
	sort.Strings(files)
	return files, nil
}

// writeSnapshot writes files as a tar stream, keeping modification times
// so builds in the container stay incremental.
func writeSnapshot(w io.Writer, root string, files []string) error {
	archive := tar.NewWriter(w)
	for _, name := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		info, err := os.Lstat(path)
		if err != nil {
			return err
		}
		link := ""
		if info.Mode()&os.ModeSymlink != 0 {
			if link, err = os.Readlink(path); err != nil {
				return err
			}
		}
		header, err := tar.FileInfoHeader(info, link)
		if err != nil {
			return err
		}
		header.Name = name
		header.Uid, header.Gid, header.Uname, header.Gname = 0, 0, "", ""
		if err := archive.WriteHeader(header); err != nil {
			return err
		}
		if info.Mode().IsRegular() {
			file, err := os.Open(path)
			if err != nil {
				return err
			}
			_, err = io.Copy(archive, file)
			file.Close()
			if err != nil {
				return err
			}
		}
	}
	return archive.Close()
}

// sourceDigest hashes the names, modes, and contents (or link targets) of
// files, ignoring times and ownership.
func sourceDigest(root string, files []string) (string, error) {
	hash := sha256.New()
	for _, name := range files {
		path := filepath.Join(root, filepath.FromSlash(name))
		info, err := os.Lstat(path)
		if err != nil {
			return "", task.Errorf("%s: %s", name, task.IOError(err))
		}
		fmt.Fprintf(hash, "%s\x00%o\x00", name, uint32(info.Mode()))
		switch {
		case info.Mode()&os.ModeSymlink != 0:
			link, err := os.Readlink(path)
			if err != nil {
				return "", task.Errorf("%s: %s", name, task.IOError(err))
			}
			fmt.Fprintf(hash, "%s\x00", link)
		case info.Mode().IsRegular():
			file, err := os.Open(path)
			if err != nil {
				return "", task.Errorf("%s: %s", name, task.IOError(err))
			}
			fmt.Fprintf(hash, "%d\x00", info.Size())
			_, err = io.Copy(hash, file)
			file.Close()
			if err != nil {
				return "", task.Errorf("%s: %s", name, task.IOError(err))
			}
		}
	}
	return hex.EncodeToString(hash.Sum(nil))[:16], nil
}
