//go:build linux || (darwin && cgo)

package platform

// Reading and writing the storage fix's report file on the mixer's USB disk
// (ReportDisks), shared by Linux and macOS.

import (
	"errors"
	"io"
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

// writeSynced replaces path with data, not following a link, and flushes
// the file and its folder.
func writeSynced(path string, data []byte) error {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0o644)
	if err != nil {
		return err
	}
	if info, err := file.Stat(); err != nil || !info.Mode().IsRegular() {
		file.Close()
		if err == nil {
			err = platformError("not a regular file")
		}
		return err
	}
	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	dir, err := os.Open(filepath.Dir(path))
	if err != nil {
		return err
	}
	defer dir.Close()
	// macOS's FAT driver may refuse a flush of a folder; the file itself is
	// flushed, and the unmount or eject that follows writes the rest.
	if err := dir.Sync(); err != nil && !errors.Is(err, unix.EINVAL) && !errors.Is(err, unix.ENOTSUP) {
		return err
	}
	return nil
}

// readSmall reads at most limit bytes of a regular file, not following a
// link.
func readSmall(path string, limit int64) ([]byte, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, platformError("not a regular file")
	}
	return io.ReadAll(io.LimitReader(file, limit))
}
