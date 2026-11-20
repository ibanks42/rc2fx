//go:build linux

package installer

import (
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

func homeDir() (string, bool) {
	home := os.Getenv("HOME")
	homeOK := home != "" && filepath.IsAbs(home)
	// sudo/pkexec can replace HOME with /root. Keys must still belong to the
	// user who launched the installer, just like its cache files.
	elevated := os.Geteuid() == 0
	var invoker *[2]uint32
	if elevated {
		found, err := archive.InvokingUser()
		if err != nil {
			return "", false
		}
		invoker = found
	}
	var uid *uint32
	if invoker != nil {
		uid = &invoker[0]
	}
	var current *string
	if homeOK {
		current = &home
	}
	out := homeForIdentity(current, elevated, uid)
	if out == nil {
		return "", false
	}
	return *out, true
}

func homeForIdentity(home *string, elevated bool, invoker *uint32) *string {
	if elevated && invoker != nil {
		_, found, ok := archive.Account(*invoker)
		if !ok {
			return nil
		}
		return &found
	}
	return home
}
