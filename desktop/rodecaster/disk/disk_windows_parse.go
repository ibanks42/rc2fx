package disk

// Pure-Go decoding of Windows device IDs and IOCTL buffers so unit tests run
// on all platforms.

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

// winIsUSBDeviceID matches USB device/interface instance IDs (USB\VID_...),
// excluding USBSTOR children and root hubs.
func winIsUSBDeviceID(id string) bool {
	return strings.HasPrefix(strings.ToUpper(id), `USB\VID_`)
}

// winIsMixerID matches the storage gadget's device or composite interface ID.
func winIsMixerID(id string) bool {
	rest, ok := strings.CutPrefix(strings.ToUpper(id), fmt.Sprintf(`USB\VID_%04X&PID_%04X`, USBVendor, USBProduct))
	return ok && (strings.HasPrefix(rest, `\`) || strings.HasPrefix(rest, `&`))
}

// winDecodeDeviceNumber parses STORAGE_DEVICE_NUMBER (DeviceType, DeviceNumber, PartitionNumber).
func winDecodeDeviceNumber(b []byte) (uint32, error) {
	if len(b) < 12 {
		return 0, fmt.Errorf("short device number answer (%d bytes)", len(b))
	}
	if t := binary.LittleEndian.Uint32(b[0:]); t != winFileDeviceDisk {
		return 0, fmt.Errorf("device type %#x is not a disk", t)
	}
	return binary.LittleEndian.Uint32(b[4:]), nil
}

// winDecodeGeometryEx parses BytesPerSector (offset 20) and DiskSize (offset 24)
// from DISK_GEOMETRY_EX.
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

// winDecodeLength parses GET_LENGTH_INFORMATION.
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

// VOLUME_DISK_EXTENTS layout: 4-byte count padded to 8 bytes, then 24-byte DISK_EXTENT entries.
const (
	winExtentsHeader = 8
	winExtentSize    = 24
)

// winExtentsCount reads NumberOfDiskExtents (populated even on ERROR_MORE_DATA).
func winExtentsCount(b []byte) (uint32, bool) {
	if len(b) < 4 {
		return 0, false
	}
	return binary.LittleEndian.Uint32(b), true
}

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

// Strip the trailing backslash from \\?\Volume{...}\ so CreateFile opens the
// volume device itself rather than its root directory.
func winVolumeDevice(name string) string {
	return strings.TrimSuffix(name, `\`)
}

// winFirstPath returns the first NUL-terminated string from a REG_MULTI_SZ UTF-16 buffer.
func winFirstPath(list []uint16) string {
	for i, c := range list {
		if c == 0 {
			return string(utf16.Decode(list[:i]))
		}
	}
	return ""
}

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

func winDrivePath(n uint32) string {
	return fmt.Sprintf(`\\.\PhysicalDrive%d`, n)
}

func winDescribe(n uint32, size uint64) string {
	return fmt.Sprintf("Disk %d, %.1f GB", n, float64(size)/1e9)
}
