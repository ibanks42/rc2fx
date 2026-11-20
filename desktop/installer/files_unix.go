//go:build unix

package installer

import (
	"os"
	"syscall"
)

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
