package disk

import (
	"fmt"
	"os"
)

type imageDisk struct {
	f       *os.File
	sectors uint64
}

// OpenImage opens a regular disk-image file as a Disk (for tests and dry runs).
func OpenImage(path string, write bool) (Disk, error) {
	// Lstat first so we never open a device node, FIFO, or symlink.
	before, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !before.Mode().IsRegular() {
		return nil, fmt.Errorf("%s is not a regular file", path)
	}
	flag := os.O_RDONLY
	if write {
		flag = os.O_RDWR
	}
	f, err := os.OpenFile(path, flag, 0)
	if err != nil {
		return nil, err
	}
	st, err := f.Stat()
	if err != nil {
		f.Close()
		return nil, err
	}
	if !st.Mode().IsRegular() || !os.SameFile(before, st) {
		f.Close()
		return nil, fmt.Errorf("%s changed while it was being opened", path)
	}
	return &imageDisk{f: f, sectors: uint64(st.Size()) / SectorSize}, nil
}

func (d *imageDisk) ReadAt(p []byte, off int64) (int, error)  { return d.f.ReadAt(p, off) }
func (d *imageDisk) WriteAt(p []byte, off int64) (int, error) { return d.f.WriteAt(p, off) }
func (d *imageDisk) Sync() error                              { return d.f.Sync() }
func (d *imageDisk) Sectors() uint64                          { return d.sectors }
func (d *imageDisk) Close() error                             { return d.f.Close() }
