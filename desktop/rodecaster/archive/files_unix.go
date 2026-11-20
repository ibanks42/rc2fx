//go:build unix

package archive

import (
	"os"
	"path/filepath"
	"syscall"
)

// NoFollow avoids following a trailing symlink and prevents open from blocking
// on a FIFO.
const NoFollow = syscall.O_NOFOLLOW | syscall.O_NONBLOCK

// ReplaceLocalFile renames temp over path and syncs the parent directory.
func ReplaceLocalFile(temp, path string) error {
	if err := os.Rename(temp, path); err != nil {
		return err
	}
	// Make sure the directory entry hits disk before any subsequent USB command.
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
