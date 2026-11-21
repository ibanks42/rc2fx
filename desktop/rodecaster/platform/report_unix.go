//go:build linux || (darwin && cgo)

package platform

// Reading and writing the storage fix's report file on the mixer's USB disk
// (ReportDisks), shared by Linux and macOS.

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

// ReadReport reads name from a report disk: where it is already mounted, or
// through a read-only mount that is removed again.
func (host) ReadReport(v Volume, name string, limit int64) ([]byte, error) {
	var data []byte
	err := onReportDisk(v, name, false, func(path string) error {
		var err error
		data, err = readSmall(path, limit)
		return err
	})
	return data, err
}

// WriteReport writes name to a report disk: where it is already mounted, or
// through a mount that is removed again. The file (and its folder on Linux) is
// flushed before it returns, and a mount of its own is unmounted, so the data
// has reached the mixer.
func (host) WriteReport(v Volume, name string, data []byte) error {
	return onReportDisk(v, name, true, func(path string) error {
		return writeSynced(path, data)
	})
}

// onReportDisk validates name, ensures v is mounted via mountReportDisk
// (read-only unless write), invokes do with the target file path, and unmounts
// any temporary mount created for this call.
func onReportDisk(v Volume, name string, write bool, do func(path string) error) error {
	if name == "" || name == "." || name == ".." || filepath.Base(name) != name {
		return platformError("invalid file name")
	}
	dir, unmount, err := mountReportDisk(v, write)
	if err != nil {
		return err
	}
	err = do(filepath.Join(dir, name))
	if unmount != nil {
		// Unmounting writes back whatever the filesystem still holds; a write
		// counts only once that has worked.
		if uerr := unmount(); uerr != nil && write && err == nil {
			err = platformError(fmt.Sprintf("unmount the mixer's USB disk: %v", uerr))
		}
	}
	return err
}

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
