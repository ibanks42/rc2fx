package selfupdate

import (
	"os"
	"os/user"
	"path/filepath"
	"strings"
)

// VelopackLocator is unused on macOS because Velopack's default cache path is
// already per-user.
func VelopackLocator() (LocatorConfig, bool) { return LocatorConfig{}, false }

// PackagesDir returns ~/Library/Caches/velopack/<id>/packages, matching
// Velopack 1.3.39's macOS locator (not exposed by velopack_libc).
func PackagesDir() (string, bool) {
	exe, err := os.Executable()
	if err != nil {
		return "", false
	}
	idx := strings.Index(exe, ".app/")
	if idx < 0 {
		return "", false
	}
	root := exe[:idx+4]
	contents := filepath.Join(root, "Contents", "MacOS")
	if !exists(filepath.Join(contents, "UpdateMac")) {
		return "", false
	}
	m, err := readManifest(filepath.Join(contents, "sq.version"))
	if err != nil {
		m, err = readManifest(filepath.Join(root, "Contents", "Resources", "sq.version"))
		if err != nil {
			return "", false
		}
	}
	home := os.Getenv("HOME")
	if home == "" {
		u, err := user.Current()
		if err != nil {
			return "", false
		}
		home = u.HomeDir
	}
	return filepath.Join(home, "Library", "Caches", "velopack", m.ID, "packages"), true
}

func EnsurePrivate(dir string) error { return nil }
