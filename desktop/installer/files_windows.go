//go:build windows

package installer

import (
	"os"
	"path/filepath"

	"golang.org/x/sys/windows"
)

// lockAction takes an exclusive, non-blocking lock on file; the returned
// function releases it.
func lockAction(file *os.File) (func(), error) {
	handle := windows.Handle(file.Fd())
	overlapped := new(windows.Overlapped)
	flags := uint32(windows.LOCKFILE_EXCLUSIVE_LOCK | windows.LOCKFILE_FAIL_IMMEDIATELY)
	if err := windows.LockFileEx(handle, flags, 0, ^uint32(0), ^uint32(0), overlapped); err != nil {
		return nil, err
	}
	return func() { windows.UnlockFileEx(handle, 0, ^uint32(0), ^uint32(0), overlapped) }, nil
}

func homeDir() (string, bool) {
	home := os.Getenv("USERPROFILE")
	return home, home != "" && filepath.IsAbs(home)
}
