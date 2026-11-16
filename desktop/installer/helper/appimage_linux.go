//go:build linux

package helper

// Stage an AppImage helper on ordinary storage before asking polkit to run
// it. Root cannot enter the desktop user's FUSE mount. Copy the complete
// AppDir, including its loader libraries, rather than extracting the outer
// image as root into the AppImage runtime's predictable shared temporary
// directory.

import (
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

func prepareLaunch(exe string) (string, *stagedAppDir, error) {
	hint, ok := os.LookupEnv("APPDIR")
	if !ok {
		return prepare(exe, nil)
	}
	return prepare(exe, &hint)
}

type budget struct {
	entries int
	bytes   uint64
}

const (
	maxEntries = 16_384
	maxBytes   = 1024 * 1024 * 1024
	maxDepth   = 64
)

func canonicalize(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(abs)
}

// within is Rust's component-wise Path::starts_with.
func within(path, root string) bool {
	return path == root || strings.HasPrefix(path, strings.TrimSuffix(root, "/")+"/")
}

// prepare: packaging hints are not authority. Only use an AppDir containing
// this exact running executable and the expected launcher; unrelated
// inherited hints leave standalone launches unchanged.
func prepare(exe string, appdir *string) (string, *stagedAppDir, error) {
	if appdir == nil {
		return exe, nil, nil
	}
	hint := *appdir
	root, err := canonicalize(hint)
	if err != nil {
		return exe, nil, nil
	}
	exePath, err := canonicalize(exe)
	if err != nil {
		return "", nil, err
	}
	if !within(exePath, root) {
		return exe, nil, nil
	}
	if !filepath.IsAbs(hint) || root == "/" {
		return "", nil, installer.Error("invalid AppImage directory")
	}
	launched, err := canonicalize(filepath.Join(root, "usr/bin/rc2fx"))
	if err != nil {
		return "", nil, err
	}
	if launched != exePath {
		return "", nil, installer.Error("unrecognized AppImage executable layout")
	}
	info, err := os.Lstat(filepath.Join(root, "AppRun"))
	if err != nil {
		return "", nil, err
	}
	if !info.Mode().IsRegular() || info.Mode().Perm()&0o111 == 0 {
		return "", nil, installer.Error("the AppImage needs a regular executable AppRun")
	}
	// Do not inherit TMPDIR: it can name the FUSE mount (or another unusual
	// filesystem). /tmp may be noexec, in which case launching fails normally
	// rather than falling back to unsafe root extraction.
	dir, err := os.MkdirTemp("/tmp", "rc2fx-helper-")
	if err != nil {
		return "", nil, err
	}
	guard := &stagedAppDir{path: dir}
	if err := os.Chmod(dir, 0o700); err != nil {
		guard.Remove()
		return "", nil, err
	}
	destination := filepath.Join(dir, "AppDir")
	b := &budget{entries: maxEntries, bytes: maxBytes}
	if err := copyEntry(root, destination, root, destination, 0, b); err != nil {
		guard.Remove()
		return "", nil, err
	}
	return filepath.Join(destination, "AppRun"), guard, nil
}

func copyEntry(source, destination, sourceRoot, destinationRoot string, depth int, b *budget) error {
	if depth > maxDepth || b.entries == 0 {
		return installer.Error("AppImage helper staging exceeds its entry/depth limit")
	}
	b.entries--
	info, err := os.Lstat(source)
	if err != nil {
		return err
	}
	mode := info.Mode()
	switch {
	case mode&os.ModeSymlink != 0:
		target, err := canonicalize(source)
		if err != nil {
			return err
		}
		if !within(target, sourceRoot) {
			return installer.Error("AppImage symlink escapes its AppDir")
		}
		relative, err := filepath.Rel(sourceRoot, target)
		if err != nil {
			return installer.Error("AppImage symlink escapes its AppDir")
		}
		link, err := os.Readlink(source)
		if err != nil {
			return err
		}
		// Relative links retain their exact spelling; contained absolute links
		// must point into the copy, not back into the inaccessible FUSE mount.
		if filepath.IsAbs(link) {
			link = filepath.Join(destinationRoot, relative)
		}
		return os.Symlink(link, destination)
	case mode.IsDir():
		if err := os.Mkdir(destination, 0o777); err != nil {
			return err
		}
		entries, err := os.ReadDir(source)
		if err != nil {
			return err
		}
		for _, entry := range entries {
			if err := copyEntry(filepath.Join(source, entry.Name()), filepath.Join(destination, entry.Name()), sourceRoot, destinationRoot, depth+1, b); err != nil {
				return err
			}
		}
		return os.Chmod(destination, mode.Perm())
	case mode.IsRegular():
		size := uint64(info.Size())
		if size > b.bytes {
			return installer.Error("AppImage helper staging exceeds its byte limit")
		}
		b.bytes -= size
		// Limit reads as well as the initial stat: a changing source cannot
		// make this copy unbounded. Source AppImages are read-only mounts.
		input, err := os.Open(source)
		if err != nil {
			return err
		}
		defer input.Close()
		output, err := os.OpenFile(destination, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o666)
		if err != nil {
			return err
		}
		defer output.Close()
		n, err := io.Copy(output, io.LimitReader(input, int64(size)+1))
		if err != nil {
			return err
		}
		if uint64(n) != size {
			return installer.Error("AppImage file changed while staging its helper")
		}
		return os.Chmod(destination, mode.Perm())
	}
	return installer.Error("unsupported file type in AppImage helper staging")
}
