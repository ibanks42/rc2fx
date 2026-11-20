package selfupdate

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
)

// VelopackLocator builds Velopack's AppImage paths with a per-user packages
// directory instead of shared /var/tmp (which would allow another local user
// to swap a package between verification and install). Mirrors Velopack
// 1.3.39's Linux auto-locator, which velopack_libc doesn't expose.
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

func PackagesDir() (string, bool) {
	config, ok := VelopackLocator()
	return config.PackagesDir, ok
}

// PrivatePackagesDir returns $XDG_CACHE_HOME/rc2fx/velopack-packages (falling
// back to ~/.cache).
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

// EnsurePrivate creates dir with 0700 permissions, or verifies an existing
// directory is a real directory owned by the current user and restricts its
// mode to 0700.
func EnsurePrivate(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return err
	}
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
