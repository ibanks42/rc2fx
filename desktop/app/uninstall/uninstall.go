// Package uninstall handles optional user-data cleanup during Windows
// uninstalls (downloaded firmware, settings backups, preferences, installer
// state, and the developer SSH key pair).
//
// Velopack invokes OfferCleanup right before removing the app, with a 60s
// timeout enforced by Update.exe. The prompt defaults to "No" so unattended
// uninstalls leave user data intact. Custom RC2FX_INSTALLER_WORK directories
// are never deleted.
package uninstall

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

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
