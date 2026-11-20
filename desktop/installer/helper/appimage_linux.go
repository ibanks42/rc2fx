//go:build linux

package helper

// Stage an AppImage's AppDir onto regular storage before invoking pkexec,
// since root can't traverse the desktop user's FUSE mount. We copy the whole
// AppDir (including bundled shared libs) instead of letting the AppImage
// runtime extract as root into a predictable /tmp path.

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

func within(path, root string) bool {
	return path == root || strings.HasPrefix(path, strings.TrimSuffix(root, "/")+"/")
}

// prepare verifies APPDIR actually contains this running binary and a valid
// AppRun before staging a copy; unrelated APPDIR values leave standalone
// launches alone.
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
	// Use /tmp explicitly instead of TMPDIR, which might point inside the FUSE
	// mount. If /tmp is noexec, launch fails cleanly instead of falling back.
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
		// Keep relative symlinks as-is, but rewrite absolute in-AppDir links so
		// they point into the staged copy instead of the FUSE mount.
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
		// Cap reads to the stat size in case a non-readonly source changes mid-copy.
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
