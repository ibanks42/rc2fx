package disk

// Windows identifiers and IOCTL buffers, decoded in plain Go. This file has
// no build constraint (a "_parse" suffix is not a GOOS), so its tests run on
// every system.

import (
	"encoding/binary"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"unicode/utf16"
)

// winCtlCode is CTL_CODE from <winioctl.h>.
func winCtlCode(deviceType, function, method, access uint32) uint32 {
	return deviceType<<16 | access<<14 | function<<2 | method
}

// IOCTL and FSCTL codes, written out; a test checks them against winCtlCode.
const (
	winIoctlStorageGetDeviceNumber     = 0x002d1080 // CTL_CODE(IOCTL_STORAGE_BASE, 0x420, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winIoctlDiskGetDriveGeometryEx     = 0x000700a0 // CTL_CODE(IOCTL_DISK_BASE, 0x28, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winIoctlDiskGetLengthInfo          = 0x0007405c // CTL_CODE(IOCTL_DISK_BASE, 0x17, METHOD_BUFFERED, FILE_READ_ACCESS)
	winIoctlDiskUpdateProperties       = 0x00070140 // CTL_CODE(IOCTL_DISK_BASE, 0x50, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winIoctlVolumeGetVolumeDiskExtents = 0x00560000 // CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winFsctlLockVolume                 = 0x00090018 // CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 6, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winFsctlUnlockVolume               = 0x0009001c // CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 7, METHOD_BUFFERED, FILE_ANY_ACCESS)
	winFsctlDismountVolume             = 0x00090020 // CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 8, METHOD_BUFFERED, FILE_ANY_ACCESS)
)

// FILE_DEVICE_DISK, a STORAGE_DEVICE_NUMBER's DeviceType for a disk.
const winFileDeviceDisk = 7

// winIsUSBDeviceID is a device instance ID of a USB device or interface
// (`USB\VID_xxxx&PID_xxxx\...`), as opposed to a storage child such as
// `USBSTOR\...` or a root hub's `USB\ROOT_HUB30\...`.
func winIsUSBDeviceID(id string) bool {
	return strings.HasPrefix(strings.ToUpper(id), `USB\VID_`)
}

// winIsMixerID is the instance ID of the mixer's storage gadget, or one of
// its interfaces on a composite device (`...&MI_00`).
func winIsMixerID(id string) bool {
	rest, ok := strings.CutPrefix(strings.ToUpper(id), fmt.Sprintf(`USB\VID_%04X&PID_%04X`, USBVendor, USBProduct))
	return ok && (strings.HasPrefix(rest, `\`) || strings.HasPrefix(rest, `&`))
}

// winDecodeDeviceNumber reads STORAGE_DEVICE_NUMBER: DEVICE_TYPE DeviceType
// (4 bytes), ULONG DeviceNumber, ULONG PartitionNumber.
func winDecodeDeviceNumber(b []byte) (uint32, error) {
	if len(b) < 12 {
		return 0, fmt.Errorf("short device number answer (%d bytes)", len(b))
	}
	if t := binary.LittleEndian.Uint32(b[0:]); t != winFileDeviceDisk {
		return 0, fmt.Errorf("device type %#x is not a disk", t)
	}
	return binary.LittleEndian.Uint32(b[4:]), nil
}

// winDecodeGeometryEx reads DISK_GEOMETRY_EX: DISK_GEOMETRY (LARGE_INTEGER
// Cylinders at 0, MEDIA_TYPE at 8, DWORD TracksPerCylinder at 12, DWORD
// SectorsPerTrack at 16, DWORD BytesPerSector at 20), then LARGE_INTEGER
// DiskSize at 24.
func winDecodeGeometryEx(b []byte) (bytesPerSector uint32, size uint64, err error) {
	if len(b) < 32 {
		return 0, 0, fmt.Errorf("short disk geometry answer (%d bytes)", len(b))
	}
	s := int64(binary.LittleEndian.Uint64(b[24:]))
	if s <= 0 {
		return 0, 0, errors.New("the disk reports no size")
	}
	return binary.LittleEndian.Uint32(b[20:]), uint64(s), nil
}

// winDecodeLength reads GET_LENGTH_INFORMATION: LARGE_INTEGER Length.
func winDecodeLength(b []byte) (uint64, error) {
	if len(b) < 8 {
		return 0, fmt.Errorf("short disk length answer (%d bytes)", len(b))
	}
	s := int64(binary.LittleEndian.Uint64(b))
	if s <= 0 {
		return 0, errors.New("the disk reports no size")
	}
	return uint64(s), nil
}

// winExtentsHeader is the size of VOLUME_DISK_EXTENTS before its array: a
// DWORD count padded to the 8-byte alignment of DISK_EXTENT.
const winExtentsHeader = 8

// winExtentSize is sizeof(DISK_EXTENT): DWORD DiskNumber, 4 bytes padding,
// LARGE_INTEGER StartingOffset, LARGE_INTEGER ExtentLength.
const winExtentSize = 24

// winExtentsCount is NumberOfDiskExtents, which the volume fills in even
// when it answers ERROR_MORE_DATA.
func winExtentsCount(b []byte) (uint32, bool) {
	if len(b) < 4 {
		return 0, false
	}
	return binary.LittleEndian.Uint32(b), true
}

// winDecodeExtentDisks lists the disk number of every extent in a
// VOLUME_DISK_EXTENTS answer of length len(b).
func winDecodeExtentDisks(b []byte) ([]uint32, error) {
	count, ok := winExtentsCount(b)
	if !ok {
		return nil, fmt.Errorf("short volume extents answer (%d bytes)", len(b))
	}
	if count > 1024 || winExtentsHeader+int(count)*winExtentSize > len(b) {
		return nil, fmt.Errorf("volume extents answer of %d bytes cannot hold %d extents", len(b), count)
	}
	disks := make([]uint32, count)
	for i := range disks {
		disks[i] = binary.LittleEndian.Uint32(b[winExtentsHeader+i*winExtentSize:])
	}
	return disks, nil
}

// winVolumeDevice is the device path of a volume GUID path: FindFirstVolume
// gives `\\?\Volume{...}\`, and the trailing backslash would open the root
// directory instead of the volume.
func winVolumeDevice(name string) string {
	return strings.TrimSuffix(name, `\`)
}

// winFirstPath is the first string of a REG_MULTI_SZ-style list of UTF-16
// strings (GetVolumePathNamesForVolumeName's answer), or "".
func winFirstPath(list []uint16) string {
	for i, c := range list {
		if c == 0 {
			return string(utf16.Decode(list[:i]))
		}
	}
	return ""
}

// winDriveNumber is N in `\\.\PhysicalDriveN`.
func winDriveNumber(path string) (uint32, bool) {
	const prefix = `\\.\PHYSICALDRIVE`
	if len(path) <= len(prefix) || len(path) > len(prefix)+10 || !strings.EqualFold(path[:len(prefix)], prefix) {
		return 0, false
	}
	digits := path[len(prefix):]
	for _, c := range []byte(digits) {
		if c < '0' || c > '9' {
			return 0, false
		}
	}
	n, err := strconv.ParseUint(digits, 10, 32)
	return uint32(n), err == nil
}

// winDrivePath is the path Open uses for disk number n.
func winDrivePath(n uint32) string {
	return fmt.Sprintf(`\\.\PhysicalDrive%d`, n)
}

// winDescribe is Info's Name: "Disk 3, 7.3 GB".
func winDescribe(n uint32, size uint64) string {
	return fmt.Sprintf("Disk %d, %.1f GB", n, float64(size)/1e9)
}
