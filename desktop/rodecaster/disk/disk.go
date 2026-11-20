// Package disk discovers RØDECaster mixers in bootloader storage mode
// (hold power ~15s at boot; U-Boot exports the eMMC via `ums mmc 0` as USB
// 1b8e:fada) and opens the whole disk for raw sector I/O.
//
// Partition table parsing and repair live in package fix.
package disk

import (
	"errors"
	"io"
)

// USB IDs of the U-Boot mass-storage gadget.
const (
	USBVendor  = 0x1b8e
	USBProduct = 0xfada
)

const SectorSize = 512

var ErrUnsupported = errors.New("this operating system is not supported")

var ErrPermission = errors.New("administrator rights are needed to open the mixer's storage")

// Info identifies a mixer exposed in storage mode.
type Info struct {
	Path    string // OS raw device path (/dev/sdX, /dev/rdiskN, \\.\PhysicalDriveN)
	Sectors uint64 // size in 512-byte sectors
	Name    string // human-readable label (e.g. "/dev/sdc, 7.3 GB")
}

// Find returns all whole disks attached to USB 1b8e:fada. Does not require
// elevated privileges.
func Find() ([]Info, error) { return find() }

// Disk provides exclusive raw sector access to a mixer's eMMC.
type Disk interface {
	io.ReaderAt
	io.WriterAt // fails if opened read-only
	// Sync flushes writes to the device and invalidates host read caches so
	// subsequent ReadAt calls read back from hardware.
	Sync() error
	Sectors() uint64
	// Close releases the device and best-effort asks the OS to re-read the
	// partition table if any writes occurred.
	Close() error
}

// Open unmounts any mounted volumes on the mixer's disk and opens it for raw
// sector-aligned I/O.
func Open(info Info, write bool) (Disk, error) { return open(info, write) }
