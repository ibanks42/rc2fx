//go:build unix && !linux

package installer

import (
	"os"
	"path/filepath"
)

func homeDir() (string, bool) {
	home := os.Getenv("HOME")
	return home, home != "" && filepath.IsAbs(home)
}
