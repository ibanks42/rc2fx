package disk

// macOS: the I/O registry (through Apple's ioreg) ties a whole disk to the
// mixer's USB device; diskutil unmounts its volumes; the raw device node
// /dev/rdiskN carries the I/O.

import (
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"

	"golang.org/x/sys/unix"
)

const (
	macIoreg    = "/usr/sbin/ioreg"
	macDiskutil = "/usr/sbin/diskutil"
)

func find() ([]Info, error) {
	out, err := exec.Command(macIoreg, "-r", "-c", "IOUSBHostDevice", "-l", "-a").Output()
	if err != nil {
		return nil, fmt.Errorf("could not list this Mac's USB devices: %w", err)
	}
	if len(strings.TrimSpace(string(out))) == 0 {
		return nil, nil
	}
	disks, err := macParseIoreg(out)
	if err != nil {
		return nil, fmt.Errorf("could not read this Mac's list of USB devices: %w", err)
	}
	infos := make([]Info, 0, len(disks))
	for _, d := range disks {
		infos = append(infos, Info{
			Path:    "/dev/r" + d.BSDName,
			Sectors: d.Size / SectorSize,
			Name:    macDescribe(d),
		})
	}
	return infos, nil
}

func open(info Info, write bool) (Disk, error) {
	name, ok := strings.CutPrefix(info.Path, "/dev/r")
	if !ok || !macWholeName(name) {
		return nil, fmt.Errorf("%q is not a whole disk on this Mac", info.Path)
	}
	// Disk numbers are reused: make sure this one is still the mixer.
	if err := macStillMixer(info); err != nil {
		return nil, err
	}
	flags := unix.O_RDONLY
	if write {
		flags = unix.O_RDWR
	}
	flags |= unix.O_CLOEXEC

	// Check the rights before unmounting anything, so a run without them
	// leaves the user's volumes alone. The kernel checks permission before
	// the disk driver sees the open, so EBUSY here (volumes still mounted)
	// means the rights are there.
	if fd, err := unix.Open(info.Path, flags, 0); err == nil {
		unix.Close(fd)
	} else if !errors.Is(err, unix.EBUSY) {
		return nil, macOpenError(info.Path, err)
	}

	// unmountDisk unmounts only this disk's volumes. With nothing mounted it
	// succeeds as well.
	if out, err := exec.Command(macDiskutil, "unmountDisk", "/dev/"+name).CombinedOutput(); err != nil {
		msg := strings.TrimSpace(string(out))
		lower := strings.ToLower(msg)
		if !strings.Contains(lower, "already unmounted") && !strings.Contains(lower, "not mounted") {
			if msg == "" {
				msg = err.Error()
			}
			return nil, fmt.Errorf("could not unmount the mixer's volumes: %s. Close any Finder window or program that is using them and try again", msg)
		}
	}

	// No O_EXLOCK: devfs implements no advisory locks, so open(2) with it
	// fails (ENOTSUP) on a device node. Exclusion comes from IOKit instead:
	// a writable open of a whole disk is refused (EBUSY) while any of its
	// volumes is mounted or another writer has it open.
	fd, err := unix.Open(info.Path, flags, 0)
	if err != nil {
		return nil, macOpenError(info.Path, err)
	}
	d := &macRawDisk{fd: fd, write: write, sectors: info.Sectors, path: info.Path}
	if err := d.checkGeometry(); err != nil {
		unix.Close(fd)
		return nil, err
	}
	return d, nil
}

// macStillMixer asks the registry again whether info.Path is a mixer of
// the same size.
func macStillMixer(info Info) error {
	found, err := find()
	if err != nil {
		return err
	}
	for _, f := range found {
		if f.Path == info.Path && f.Sectors == info.Sectors {
			return nil
		}
	}
	return fmt.Errorf("%s is no longer the mixer's storage. Check that the mixer is still connected and in storage mode, then search again", info.Path)
}

func macOpenError(path string, err error) error {
	switch {
	case errors.Is(err, unix.EACCES), errors.Is(err, unix.EPERM):
		return fmt.Errorf("%w (opening %s: %v)", ErrPermission, path, err)
	case errors.Is(err, unix.EBUSY):
		return fmt.Errorf("another program is using the mixer's storage (%s). Close it and try again", path)
	case errors.Is(err, unix.ENOENT), errors.Is(err, unix.ENXIO):
		return fmt.Errorf("the mixer's storage (%s) is no longer connected. Check the USB cable and that the mixer is still in storage mode", path)
	}
	return fmt.Errorf("could not open the mixer's storage (%s): %w", path, err)
}

type macRawDisk struct {
	fd      int
	write   bool
	sectors uint64
	path    string
}

// checkGeometry confirms that the opened device has 512-byte blocks and
// the size the registry reported.
func (d *macRawDisk) checkGeometry() error {
	// The kernel writes a uint32 into the int; macOS is little-endian on
	// every CPU it runs on, so the value lands in the low bytes.
	block, err := unix.IoctlGetInt(d.fd, macDKIOCGetBlockSize)
	if err != nil {
		return fmt.Errorf("could not read the block size of %s: %w", d.path, err)
	}
	count, err := unix.IoctlGetInt(d.fd, macDKIOCGetBlockCount)
	if err != nil {
		return fmt.Errorf("could not read the size of %s: %w", d.path, err)
	}
	if uint32(block) != SectorSize {
		return fmt.Errorf("%s has %d-byte blocks; the mixer's storage has %d-byte blocks", d.path, uint32(block), SectorSize)
	}
	if uint64(count) != d.sectors {
		return fmt.Errorf("%s changed size since it was found (%d blocks, expected %d). Search again", d.path, uint64(count), d.sectors)
	}
	return nil
}

func (d *macRawDisk) Sectors() uint64 { return d.sectors }

// span checks that I/O is whole sectors at a sector offset and returns how
// much of it lies on the disk.
func (d *macRawDisk) span(n int, off int64) (int, error) {
	if d.fd < 0 {
		return 0, os.ErrClosed
	}
	if off < 0 || off%SectorSize != 0 || n%SectorSize != 0 {
		return 0, fmt.Errorf("disk I/O must be whole %d-byte sectors (offset %d, length %d)", SectorSize, off, n)
	}
	size := int64(d.sectors) * SectorSize
	if off >= size {
		return 0, nil
	}
	return int(min(int64(n), size-off)), nil
}

func (d *macRawDisk) ReadAt(p []byte, off int64) (int, error) {
	want, err := d.span(len(p), off)
	if err != nil {
		return 0, err
	}
	done := 0
	for done < want {
		n, err := unix.Pread(d.fd, p[done:want], off+int64(done))
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if n > 0 {
			done += n
		}
		if err != nil {
			return done, fmt.Errorf("reading %s: %w", d.path, err)
		}
		if n <= 0 {
			return done, io.ErrUnexpectedEOF
		}
	}
	if want < len(p) {
		return done, io.EOF
	}
	return done, nil
}

func (d *macRawDisk) WriteAt(p []byte, off int64) (int, error) {
	if !d.write {
		return 0, errors.New("the mixer's storage was opened for reading only")
	}
	want, err := d.span(len(p), off)
	if err != nil {
		return 0, err
	}
	if want != len(p) {
		return 0, fmt.Errorf("write at offset %d, length %d goes past the end of %s", off, len(p), d.path)
	}
	done := 0
	for done < want {
		n, err := unix.Pwrite(d.fd, p[done:], off+int64(done))
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if n > 0 {
			done += n
		}
		if err != nil {
			return done, fmt.Errorf("writing %s: %w", d.path, err)
		}
		if n <= 0 {
			return done, io.ErrShortWrite
		}
	}
	return done, nil
}

// Sync: the raw device keeps no cache on this side, so a write has reached
// the device's driver when Pwrite returns; fsync covers the vnode, and
// DKIOCSYNCHRONIZECACHE asks the device to commit its own write cache.
func (d *macRawDisk) Sync() error {
	if d.fd < 0 {
		return os.ErrClosed
	}
	if !d.write {
		return nil
	}
	if err := unix.Fsync(d.fd); err != nil {
		return fmt.Errorf("flushing %s: %w", d.path, err)
	}
	// Devices without a cache command answer "not supported": nothing is
	// held back in that case.
	if err := unix.IoctlSetInt(d.fd, macDKIOCSynchronizeCache, 0); err != nil &&
		!errors.Is(err, unix.ENOTSUP) && !errors.Is(err, unix.EOPNOTSUPP) && !errors.Is(err, unix.ENOTTY) {
		return fmt.Errorf("flushing the device cache of %s: %w", d.path, err)
	}
	return nil
}

// Close does not remount: the volumes come back when the mixer restarts or
// is plugged in again.
func (d *macRawDisk) Close() error {
	if d.fd < 0 {
		return nil
	}
	err := unix.Close(d.fd)
	d.fd = -1
	return err
}
