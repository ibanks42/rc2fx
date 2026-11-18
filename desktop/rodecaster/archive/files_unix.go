//go:build unix

package archive

import (
	"os"
	"path/filepath"
	"syscall"
)

// NoFollow does not follow a final symlink, and does not wait for a writer
// on a FIFO.
const NoFollow = syscall.O_NOFOLLOW | syscall.O_NONBLOCK

// ReplaceLocalFile renames temp over path and syncs the directory.
func ReplaceLocalFile(temp, path string) error {
	if err := os.Rename(temp, path); err != nil {
		return err
	}
	// Persist the directory entry as well as file data before a USB command.
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
