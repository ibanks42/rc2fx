package disk

import (
	"encoding/binary"
	"reflect"
	"testing"
	"unicode/utf16"
)

func TestWinIoctlCodes(t *testing.T) {
	const (
		buffered   = 0
		anyAccess  = 0
		readAccess = 1
	)
	for _, c := range []struct {
		name string
		got  uint32
		want uint32
	}{
		{"IOCTL_STORAGE_GET_DEVICE_NUMBER", winIoctlStorageGetDeviceNumber, winCtlCode(0x2d, 0x420, buffered, anyAccess)},
		{"IOCTL_DISK_GET_DRIVE_GEOMETRY_EX", winIoctlDiskGetDriveGeometryEx, winCtlCode(7, 0x28, buffered, anyAccess)},
		{"IOCTL_DISK_GET_LENGTH_INFO", winIoctlDiskGetLengthInfo, winCtlCode(7, 0x17, buffered, readAccess)},
		{"IOCTL_DISK_UPDATE_PROPERTIES", winIoctlDiskUpdateProperties, winCtlCode(7, 0x50, buffered, anyAccess)},
		{"IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS", winIoctlVolumeGetVolumeDiskExtents, winCtlCode('V', 0, buffered, anyAccess)},
		{"FSCTL_LOCK_VOLUME", winFsctlLockVolume, winCtlCode(9, 6, buffered, anyAccess)},
		{"FSCTL_UNLOCK_VOLUME", winFsctlUnlockVolume, winCtlCode(9, 7, buffered, anyAccess)},
		{"FSCTL_DISMOUNT_VOLUME", winFsctlDismountVolume, winCtlCode(9, 8, buffered, anyAccess)},
	} {
		if c.got != c.want {
			t.Errorf("%s = %#x, want %#x", c.name, c.got, c.want)
		}
	}
}

func TestWinMixerInstanceIDs(t *testing.T) {
	for id, want := range map[string]bool{
		`USB\VID_1B8E&PID_FADA\0123456789`:       true,
		`usb\vid_1b8e&pid_fada\5&2A1B3C&0&1`:     true,
		`USB\VID_1B8E&PID_FADA&MI_00\6&1&0&0000`: true,
		`USB\VID_1B8E&PID_FADB\0123`:             false,
		`USB\VID_1B8E&PID_FADA0\0123`:            false,
		`USB\VID_19F7&PID_0030\GV0044100`:        false,
		`USBSTOR\DISK&VEN_LINUX&PROD_UMS\1&0`:    false,
		`USB\VID_1B8E&PID_FADA`:                  false,
	} {
		if got := winIsMixerID(id); got != want {
			t.Errorf("winIsMixerID(%q) = %v", id, got)
		}
	}
	for id, want := range map[string]bool{
		`USB\VID_0951&PID_1666\1`:             true,
		`USBSTOR\DISK&VEN_LINUX&PROD_UMS\1&0`: false,
		`USB\ROOT_HUB30\4&1&0`:                false,
		`SCSI\DISK&VEN_X\1`:                   false,
	} {
		if got := winIsUSBDeviceID(id); got != want {
			t.Errorf("winIsUSBDeviceID(%q) = %v", id, got)
		}
	}
}

func TestWinDecodeDeviceNumber(t *testing.T) {
	b := make([]byte, 12)
	binary.LittleEndian.PutUint32(b[0:], 7)
	binary.LittleEndian.PutUint32(b[4:], 3)
	binary.LittleEndian.PutUint32(b[8:], 0)
	if n, err := winDecodeDeviceNumber(b); err != nil || n != 3 {
		t.Fatalf("%d %v", n, err)
	}
	if _, err := winDecodeDeviceNumber(b[:11]); err == nil {
		t.Fatal("short answer accepted")
	}
	binary.LittleEndian.PutUint32(b[0:], 2) // FILE_DEVICE_CD_ROM
	if _, err := winDecodeDeviceNumber(b); err == nil {
		t.Fatal("non-disk accepted")
	}
}

func TestWinDecodeGeometryAndLength(t *testing.T) {
	b := make([]byte, 40)
	binary.LittleEndian.PutUint32(b[20:], 512)
	binary.LittleEndian.PutUint64(b[24:], 7818182656)
	sector, size, err := winDecodeGeometryEx(b[:33])
	if err != nil || sector != 512 || size != 7818182656 {
		t.Fatalf("%d %d %v", sector, size, err)
	}
	if _, _, err := winDecodeGeometryEx(b[:31]); err == nil {
		t.Fatal("short geometry accepted")
	}
	if n, err := winDecodeLength(b[24:32]); err != nil || n != 7818182656 {
		t.Fatalf("%d %v", n, err)
	}
	if _, err := winDecodeLength(make([]byte, 8)); err == nil {
		t.Fatal("zero length accepted")
	}
}

func TestWinDecodeExtents(t *testing.T) {
	b := make([]byte, 8+2*24)
	binary.LittleEndian.PutUint32(b[0:], 2)
	binary.LittleEndian.PutUint32(b[8:], 3)
	binary.LittleEndian.PutUint64(b[16:], 2530213888)
	binary.LittleEndian.PutUint32(b[32:], 5)
	disks, err := winDecodeExtentDisks(b)
	if err != nil || !reflect.DeepEqual(disks, []uint32{3, 5}) {
		t.Fatalf("%v %v", disks, err)
	}
	if _, err := winDecodeExtentDisks(b[:8+24]); err == nil {
		t.Fatal("truncated extents accepted")
	}
	if disks, err := winDecodeExtentDisks(make([]byte, 8)); err != nil || len(disks) != 0 {
		t.Fatalf("%v %v", disks, err)
	}
}

func TestWinPathsAndNames(t *testing.T) {
	if got := winVolumeDevice(`\\?\Volume{1b2c3d4e-0000-0000-0000-100000000000}\`); got != `\\?\Volume{1b2c3d4e-0000-0000-0000-100000000000}` {
		t.Fatal(got)
	}
	list := utf16.Encode([]rune("E:\\\x00F:\\mnt\\\x00\x00"))
	if got := winFirstPath(list); got != `E:\` {
		t.Fatal(got)
	}
	if got := winFirstPath([]uint16{0, 0}); got != "" {
		t.Fatal(got)
	}
	for path, want := range map[string]int64{
		`\\.\PhysicalDrive3`:  3,
		`\\.\PHYSICALDRIVE12`: 12,
		`\\.\PhysicalDrive`:   -1,
		`\\.\PhysicalDrive3a`: -1,
		`\\.\C:`:              -1,
		`/dev/sdc`:            -1,
	} {
		n, ok := winDriveNumber(path)
		if (want < 0) == ok || ok && int64(n) != want {
			t.Errorf("winDriveNumber(%q) = %d, %v", path, n, ok)
		}
	}
	if winDrivePath(3) != `\\.\PhysicalDrive3` {
		t.Fatal(winDrivePath(3))
	}
	if got := winDescribe(3, 7818182656); got != "Disk 3, 7.8 GB" {
		t.Fatal(got)
	}
}
