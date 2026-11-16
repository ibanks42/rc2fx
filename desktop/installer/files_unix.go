//go:build unix

package installer

import (
	"os"
	"path/filepath"
	"syscall"
)

// noFollow does not follow a final symlink, and does not wait for a writer
// on a FIFO.
const noFollow = syscall.O_NOFOLLOW | syscall.O_NONBLOCK

func replaceLocalFile(temp, path string) error {
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

// lockAction takes an exclusive, non-blocking advisory lock on file; the
// returned function releases it.
func lockAction(file *os.File) (func(), error) {
	fd := int(file.Fd())
	for {
		err := syscall.Flock(fd, syscall.LOCK_EX|syscall.LOCK_NB)
		if err == syscall.EINTR {
			continue
		}
		if err != nil {
			return nil, err
		}
		return func() { syscall.Flock(fd, syscall.LOCK_UN) }, nil
	}
}
