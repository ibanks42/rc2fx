package selfupdate

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

// VelopackLocator is Velopack's paths for this AppImage, with downloaded
// packages kept in a folder only this user can open instead of the shared
// /var/tmp, where another account could swap a package between its check
// and its use. ok is false when Velopack did not package this copy. main
// (cmd/rc2fx) gives the same paths to Velopack's startup hooks, so both see
// the same packages.
//
// It is velopack 1.3.39's auto_locate_app_manifest(FromCurrentExe) on Linux,
// which the C library does not offer.
func VelopackLocator() (LocatorConfig, bool) {
	exe, err := os.Executable()
	if err != nil {
		return LocatorConfig{}, false
	}
	idx := strings.Index(exe, "/usr/bin/")
	if idx < 0 {
		return LocatorConfig{}, false
	}
	contents := filepath.Join(exe[:idx], "usr", "bin")
	update := filepath.Join(contents, "UpdateNix")
	metadata := filepath.Join(contents, "sq.version")
	if !exists(update) {
		return LocatorConfig{}, false
	}
	appimage := os.Getenv("APPIMAGE")
	if appimage == "" || !exists(appimage) {
		return LocatorConfig{}, false
	}
	if _, err := readManifest(metadata); err != nil {
		return LocatorConfig{}, false
	}
	packages, ok := PrivatePackagesDir()
	if !ok {
		return LocatorConfig{}, false
	}
	return LocatorConfig{
		RootAppDir:       appimage,
		UpdateExePath:    update,
		PackagesDir:      packages,
		ManifestPath:     metadata,
		CurrentBinaryDir: contents,
		IsPortable:       true,
	}, true
}

// PackagesDir is only used where Velopack finds its own paths.
func PackagesDir() (string, bool) {
	config, ok := VelopackLocator()
	return config.PackagesDir, ok
}

// PrivatePackagesDir is $XDG_CACHE_HOME/rc2fx/velopack-packages, or under
// ~/.cache.
func PrivatePackagesDir() (string, bool) {
	absolute := func(name string) (string, bool) {
		value, ok := os.LookupEnv(name)
		return value, ok && filepath.IsAbs(value)
	}
	cache, ok := absolute("XDG_CACHE_HOME")
	if !ok {
		home, ok := absolute("HOME")
		if !ok {
			return "", false
		}
		cache = filepath.Join(home, ".cache")
	}
	return filepath.Join(cache, "rc2fx", "velopack-packages"), true
}

// EnsurePrivate creates dir with mode 0700, or checks an existing one is a
// real folder this user owns, and closes it to others.
func EnsurePrivate(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return err
	}
	// `/proc/self` belongs to this process's user.
	owner := func(info os.FileInfo) (uint32, bool) {
		st, ok := info.Sys().(*syscall.Stat_t)
		if !ok {
			return 0, false
		}
		return st.Uid, true
	}
	var me *uint32
	if self, err := os.Stat("/proc/self"); err == nil {
		if uid, ok := owner(self); ok {
			me = &uid
		}
	}
	uid, _ := owner(info)
	if !info.IsDir() || (me != nil && *me != uid) {
		return fmt.Errorf("%s is not a folder of this user", dir)
	}
	if info.Mode().Perm()&0o077 != 0 {
		return os.Chmod(dir, 0o700)
	}
	return nil
}
