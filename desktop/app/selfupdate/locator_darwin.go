package selfupdate

import (
	"os"
	"os/user"
	"path/filepath"
	"strings"
)

// VelopackLocator is only given on Linux; elsewhere Velopack's own choice is
// already private to the user.
func VelopackLocator() (LocatorConfig, bool) { return LocatorConfig{}, false }

// PackagesDir is where Velopack keeps this copy's packages:
// auto_locate_app_manifest(FromCurrentExe).get_packages_dir() of velopack
// 1.3.39 on macOS, which the C library does not offer.
// ~/Library/Caches/velopack/<id>/packages. ok is false when Velopack did not
// install this copy.
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
	// std::env::home_dir: $HOME, else the user database.
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

// EnsurePrivate is a Linux check; elsewhere the folder is the user's own.
func EnsurePrivate(dir string) error { return nil }
