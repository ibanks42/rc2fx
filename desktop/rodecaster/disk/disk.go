// Package disk finds a RØDECaster whose bootloader is showing its whole eMMC
// to this computer as a USB disk ("storage mode": hold the power button for
// about 15 seconds at power-on; U-Boot runs `ums mmc 0`, USB 1b8e:fada), and
// opens that disk for raw sector access.
//
// Each operating system has its own file (disk_linux.go, disk_darwin.go,
// disk_windows.go); disk_other.go answers ErrUnsupported. Nothing here knows
// what a partition table is: that is package fix.
package disk

import (
	"errors"
	"io"
)

// The USB identity of the bootloader's storage gadget.
const (
	USBVendor  = 0x1b8e
	USBProduct = 0xfada
)

// SectorSize is the only sector size a mixer's eMMC has.
const SectorSize = 512

// ErrUnsupported is every call's answer on a system without an implementation.
var ErrUnsupported = errors.New("this operating system is not supported")

// ErrPermission means the disk is there but this process may not open it
// raw: it is not root (Linux, macOS) or not elevated (Windows).
var ErrPermission = errors.New("administrator rights are needed to open the mixer's storage")

// Info is one mixer in storage mode.
type Info struct {
	// Path is what Open opens: /dev/sdX, /dev/rdiskN, \\.\PhysicalDriveN.
	Path string
	// Sectors is the disk's size in 512-byte sectors.
	Sectors uint64
	// Name is a short description for people ("/dev/sdc, 7.3 GB").
	Name string
}

// Find lists the mixers in storage mode: whole disks whose USB device is
// 1b8e:fada, and nothing else, ever. It needs no special rights.
func Find() ([]Info, error) { return find() }

// Disk is a mixer's whole eMMC, held exclusively.
type Disk interface {
	io.ReaderAt
	// WriteAt fails on a disk opened without write.
	io.WriterAt
	// Sync makes what was written reach the device and drops this
	// computer's cached copy of it, so a ReadAt after Sync reads the device.
	Sync() error
	Sectors() uint64
	// Close releases the disk. After a write it also asks the system to
	// re-read the partition table where that is possible; failing to is not
	// an error.
	Close() error
}

// Open opens the disk for raw access. Anything this computer mounted from
// it (the two FAT partitions, typically) is unmounted first and kept from
// being mounted until Close; nothing else is ever unmounted. Offsets and
// lengths passed to the Disk are multiples of SectorSize.
func Open(info Info, write bool) (Disk, error) { return open(info, write) }
