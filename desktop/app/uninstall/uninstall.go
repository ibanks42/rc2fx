// Package uninstall is what uninstalling RC2FX on Windows offers to remove
// (the Rust app's uninstall.rs before): the data RC2FX keeps for this user
// (downloaded firmware, backups of the mixer's RC2FX settings, the
// installer's journal and known_hosts, preferences) and the SSH key pair it
// made for the mixer's developer option.
//
// Velopack runs OfferCleanup just before it removes the app, and stops it
// after 60 seconds (Update.exe's limit for this hook). The question defaults
// to keeping everything, so a silent or unattended uninstall keeps it too. A
// folder set with RC2FX_INSTALLER_WORK is the user's own and never removed.
package uninstall

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// targets is everything to remove, if present.
func targets() []string {
	var targets []string
	if work, ok := installer.DefaultWork(); ok {
		if data := filepath.Dir(work); data != work {
			targets = append(targets, data)
		}
	}
	if key := installer.DefaultKey(); filepath.IsAbs(key) {
		targets = append(targets, key+".pub", key)
	}
	return targets
}

// remove deletes path, a folder with everything in it or a file; one that
// is already gone counts as removed.
func remove(path string) error {
	var err error
	if info, statErr := os.Stat(path); statErr == nil && info.IsDir() {
		err = os.RemoveAll(path)
	} else {
		err = os.Remove(path)
	}
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return err
}

func existing() []string {
	var found []string
	for _, path := range targets() {
		if _, err := os.Stat(path); err == nil {
			found = append(found, path)
		}
	}
	return found
}
