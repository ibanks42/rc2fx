package disk

// Windows: SetupAPI lists disk interfaces, the configuration manager ties
// each to its USB device, storage IOCTLs give its number and size, and
// \\.\PhysicalDriveN carries the I/O.

import (
	"errors"
	"fmt"
	"io"
	"os"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	winSetupapi = windows.NewLazySystemDLL("setupapi.dll")
	winCfgmgr   = windows.NewLazySystemDLL("cfgmgr32.dll")

	winProcEnumDeviceInterfaces      = winSetupapi.NewProc("SetupDiEnumDeviceInterfaces")
	winProcGetDeviceInterfaceDetailW = winSetupapi.NewProc("SetupDiGetDeviceInterfaceDetailW")
	winProcCMGetParent               = winCfgmgr.NewProc("CM_Get_Parent")
	winProcCMGetDeviceIDW            = winCfgmgr.NewProc("CM_Get_Device_IDW")
)

// GUID_DEVINTERFACE_DISK {53F56307-B6BF-11D0-94F2-00A0C91EFB8B}.
var winDiskInterface = windows.GUID{
	Data1: 0x53f56307, Data2: 0xb6bf, Data3: 0x11d0,
	Data4: [8]byte{0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b},
}

// SP_DEVICE_INTERFACE_DATA: DWORD cbSize, GUID InterfaceClassGuid, DWORD
// Flags, ULONG_PTR Reserved (32 bytes on 64-bit Windows, 28 on 32-bit).
type winInterfaceData struct {
	CbSize   uint32
	Class    windows.GUID
	Flags    uint32
	Reserved uintptr
}

// SP_DEVINFO_DATA: DWORD cbSize, GUID ClassGuid, DWORD DevInst, ULONG_PTR
// Reserved. windows.DevInfoData keeps cbSize unexported, so it cannot be
// passed to a hand-made call.
type winDevInfoData struct {
	CbSize   uint32
	Class    windows.GUID
	DevInst  uint32
	Reserved uintptr
}

// How long to keep trying to lock a volume that something (Explorer, a
// virus scanner) has open for a moment.
const winLockTimeout = 10 * time.Second

func find() ([]Info, error) {
	set, err := windows.SetupDiGetClassDevsEx(&winDiskInterface, "", 0, windows.DIGCF_PRESENT|windows.DIGCF_DEVICEINTERFACE, 0, "")
	if err != nil {
		return nil, fmt.Errorf("could not list this computer's disks: %w", err)
	}
	defer set.Close()

	var infos []Info
	for i := uint32(0); ; i++ {
		iface := winInterfaceData{CbSize: uint32(unsafe.Sizeof(winInterfaceData{}))}
		r, _, e := winProcEnumDeviceInterfaces.Call(uintptr(set), 0, uintptr(unsafe.Pointer(&winDiskInterface)), uintptr(i), uintptr(unsafe.Pointer(&iface)))
		if r == 0 {
			if e == windows.ERROR_NO_MORE_ITEMS {
				break
			}
			return nil, fmt.Errorf("could not list this computer's disks: %w", winCallError(e))
		}
		path, devInst, err := winInterfaceDetail(set, &iface)
		if err != nil {
			// A disk that cannot be identified is not one we may use.
			continue
		}
		if !winBelongsToMixer(devInst) {
			continue
		}
		info, err := winDescribeDisk(path)
		if err != nil {
			return nil, fmt.Errorf("found the mixer's storage but could not read its details: %w", err)
		}
		infos = append(infos, info)
	}
	return infos, nil
}

// winInterfaceDetail returns the interface's device path and the device
// instance (DEVINST) of the disk behind it.
func winInterfaceDetail(set windows.DevInfo, iface *winInterfaceData) (string, uint32, error) {
	var needed uint32
	// The sizing call fails with ERROR_INSUFFICIENT_BUFFER by design.
	winProcGetDeviceInterfaceDetailW.Call(uintptr(set), uintptr(unsafe.Pointer(iface)), 0, 0, uintptr(unsafe.Pointer(&needed)), 0)
	if needed < 6 || needed > 64<<10 {
		return "", 0, errors.New("invalid disk interface path size")
	}
	// SP_DEVICE_INTERFACE_DETAIL_DATA_W: DWORD cbSize, then the NUL-terminated
	// UTF-16 DevicePath at offset 4. cbSize is the size of the struct with a
	// one-character path: 8 on 64-bit Windows, 6 on 32-bit (packed).
	buf := make([]uint64, (needed+7)/8)
	cb := uint32(6)
	if unsafe.Sizeof(uintptr(0)) == 8 {
		cb = 8
	}
	*(*uint32)(unsafe.Pointer(&buf[0])) = cb
	dev := winDevInfoData{CbSize: uint32(unsafe.Sizeof(winDevInfoData{}))}
	r, _, e := winProcGetDeviceInterfaceDetailW.Call(uintptr(set), uintptr(unsafe.Pointer(iface)),
		uintptr(unsafe.Pointer(&buf[0])), uintptr(needed), 0, uintptr(unsafe.Pointer(&dev)))
	if r == 0 {
		return "", 0, winCallError(e)
	}
	chars := unsafe.Slice((*uint16)(unsafe.Add(unsafe.Pointer(&buf[0]), 4)), (needed-4)/2)
	n := 0
	for n < len(chars) && chars[n] != 0 {
		n++
	}
	if n == 0 || n == len(chars) {
		return "", 0, errors.New("disk interface path is not terminated")
	}
	return windows.UTF16ToString(chars[:n]), dev.DevInst, nil
}

// winBelongsToMixer walks up the device tree from a disk to its nearest
// USB device (or interface) and asks whether that is the mixer's gadget.
// A disk with no USB device above it, or whose IDs cannot be read, is not.
func winBelongsToMixer(inst uint32) bool {
	for depth := 0; depth < 32; depth++ {
		id, err := winDeviceID(inst)
		if err != nil {
			return false
		}
		if winIsUSBDeviceID(id) {
			return winIsMixerID(id)
		}
		var parent uint32
		r, _, _ := winProcCMGetParent.Call(uintptr(unsafe.Pointer(&parent)), uintptr(inst), 0)
		if windows.CONFIGRET(r) != windows.CR_SUCCESS {
			// CR_NO_SUCH_DEVNODE at the root of the tree.
			return false
		}
		inst = parent
	}
	return false
}

// winDeviceID is CM_Get_Device_IDW: the device instance ID of inst.
func winDeviceID(inst uint32) (string, error) {
	var buf [windows.MAX_DEVICE_ID_LEN + 1]uint16
	r, _, _ := winProcCMGetDeviceIDW.Call(uintptr(inst), uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)), 0)
	if ret := windows.CONFIGRET(r); ret != windows.CR_SUCCESS {
		return "", ret
	}
	for i, c := range buf {
		if c == 0 {
			return windows.UTF16ToString(buf[:i]), nil
		}
	}
	return "", errors.New("device instance ID is not terminated")
}

// winDescribeDisk reads a disk interface's number and size. Both IOCTLs are
// FILE_ANY_ACCESS, so a handle with no access rights, which needs no
// elevation, is enough. IOCTL_DISK_GET_LENGTH_INFO would need read access
// (administrator rights on a physical disk), so the size comes from
// DISK_GEOMETRY_EX.DiskSize; open checks it against the length.
func winDescribeDisk(path string) (Info, error) {
	h, err := winCreateFile(path, 0, 0)
	if err != nil {
		return Info{}, err
	}
	defer windows.CloseHandle(h)
	out, err := winIoctl(h, winIoctlStorageGetDeviceNumber, 12)
	if err != nil {
		return Info{}, fmt.Errorf("disk number: %w", err)
	}
	number, err := winDecodeDeviceNumber(out)
	if err != nil {
		return Info{}, err
	}
	out, err = winIoctl(h, winIoctlDiskGetDriveGeometryEx, 256)
	if err != nil {
		return Info{}, fmt.Errorf("disk size: %w", err)
	}
	_, size, err := winDecodeGeometryEx(out)
	if err != nil {
		return Info{}, err
	}
	return Info{Path: winDrivePath(number), Sectors: size / SectorSize, Name: winDescribe(number, size)}, nil
}

// winIoctl issues a METHOD_BUFFERED IOCTL with no input and returns the
// bytes it answered. On ERROR_MORE_DATA it returns the whole (zeroed, then
// partly filled) buffer, whose header says how much is needed.
func winIoctl(h windows.Handle, code uint32, outSize int) ([]byte, error) {
	out := make([]byte, outSize)
	var n uint32
	if err := windows.DeviceIoControl(h, code, nil, 0, &out[0], uint32(len(out)), &n, nil); err != nil {
		if errors.Is(err, windows.ERROR_MORE_DATA) {
			return out, err
		}
		return nil, err
	}
	if int(n) > len(out) {
		return nil, errors.New("IOCTL answered more than its buffer")
	}
	return out[:n], nil
}

func winCreateFile(path string, access, flags uint32) (windows.Handle, error) {
	p, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return windows.InvalidHandle, err
	}
	return windows.CreateFile(p, access, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, nil, windows.OPEN_EXISTING, flags, 0)
}

// winCallError turns the zero errno a failed LazyProc.Call can carry into
// a real error.
func winCallError(err error) error {
	if errno, ok := err.(syscall.Errno); ok && errno == 0 {
		return errors.New("the call failed")
	}
	return err
}

func open(info Info, write bool) (Disk, error) {
	number, ok := winDriveNumber(info.Path)
	if !ok {
		return nil, fmt.Errorf("%q is not a physical disk on this computer", info.Path)
	}
	// Disk numbers are reused: make sure this one is still the mixer.
	if err := winStillMixer(info); err != nil {
		return nil, err
	}
	var volumes []winVolume
	if write {
		var err error
		if volumes, err = winLockVolumes(number); err != nil {
			return nil, err
		}
	}
	access := uint32(windows.GENERIC_READ)
	if write {
		access |= windows.GENERIC_WRITE
	}
	h, err := winCreateFile(info.Path, access, windows.FILE_FLAG_NO_BUFFERING|windows.FILE_FLAG_WRITE_THROUGH)
	if err != nil {
		winReleaseVolumes(volumes)
		return nil, winOpenError(info.Path, err)
	}
	d := &winDisk{h: h, write: write, sectors: info.Sectors, path: info.Path, volumes: volumes}
	if err := d.checkSize(); err != nil {
		windows.CloseHandle(h)
		winReleaseVolumes(volumes)
		return nil, err
	}
	return d, nil
}

func winStillMixer(info Info) error {
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

func winOpenError(path string, err error) error {
	switch {
	case errors.Is(err, windows.ERROR_ACCESS_DENIED):
		return fmt.Errorf("%w (opening %s: %v)", ErrPermission, path, err)
	case errors.Is(err, windows.ERROR_SHARING_VIOLATION):
		return fmt.Errorf("another program is using the mixer's storage (%s). Close it and try again", path)
	case errors.Is(err, windows.ERROR_FILE_NOT_FOUND), errors.Is(err, windows.ERROR_PATH_NOT_FOUND):
		return fmt.Errorf("the mixer's storage (%s) is no longer connected. Check the USB cable and that the mixer is still in storage mode", path)
	}
	return fmt.Errorf("could not open the mixer's storage (%s): %w", path, err)
}

// winVolume is a locked, dismounted volume of the mixer's disk.
type winVolume struct {
	h    windows.Handle
	name string
}

// winLockVolumes locks and dismounts every volume with an extent on disk
// number, and returns the handles: while they stay open and locked,
// Windows neither remounts the volumes nor refuses raw writes to them.
// Volumes on other disks are only asked for their extents.
func winLockVolumes(number uint32) ([]winVolume, error) {
	names, err := winVolumeNames()
	if err != nil {
		return nil, err
	}
	var held []winVolume
	for _, name := range names {
		if !winVolumeOnDisk(name, number) {
			continue
		}
		v, err := winLockVolume(name)
		if err != nil {
			winReleaseVolumes(held)
			return nil, err
		}
		held = append(held, v)
	}
	return held, nil
}

// winVolumeNames lists `\\?\Volume{...}\` for every volume on the system.
func winVolumeNames() ([]string, error) {
	buf := make([]uint16, 1024)
	find, err := windows.FindFirstVolume(&buf[0], uint32(len(buf)))
	if err != nil {
		return nil, fmt.Errorf("could not list this computer's volumes: %w", err)
	}
	defer windows.FindVolumeClose(find)
	var names []string
	for {
		names = append(names, windows.UTF16ToString(buf))
		if err := windows.FindNextVolume(find, &buf[0], uint32(len(buf))); err != nil {
			if errors.Is(err, windows.ERROR_NO_MORE_FILES) {
				return names, nil
			}
			return nil, fmt.Errorf("could not list this computer's volumes: %w", err)
		}
	}
}

// winVolumeOnDisk is whether the volume has an extent on disk number. A
// volume that cannot answer (no media, not a disk volume) is not on it.
func winVolumeOnDisk(name string, number uint32) bool {
	h, err := winCreateFile(winVolumeDevice(name), 0, 0)
	if err != nil {
		return false
	}
	defer windows.CloseHandle(h)
	size := winExtentsHeader + 4*winExtentSize
	for range 3 {
		out, err := winIoctl(h, winIoctlVolumeGetVolumeDiskExtents, size)
		if errors.Is(err, windows.ERROR_MORE_DATA) {
			count, ok := winExtentsCount(out)
			if !ok || count > 1024 {
				return false
			}
			size = winExtentsHeader + int(count)*winExtentSize
			continue
		}
		if err != nil {
			return false
		}
		disks, err := winDecodeExtentDisks(out)
		if err != nil {
			return false
		}
		for _, d := range disks {
			if d == number {
				return true
			}
		}
		return false
	}
	return false
}

// winLockVolume opens a volume for read and write, locks it (retrying for
// a while if something has a file open on it) and dismounts it.
func winLockVolume(name string) (winVolume, error) {
	label := winVolumeLabel(name)
	h, err := winCreateFile(winVolumeDevice(name), windows.GENERIC_READ|windows.GENERIC_WRITE, 0)
	if err != nil {
		if errors.Is(err, windows.ERROR_ACCESS_DENIED) {
			return winVolume{}, fmt.Errorf("%w (opening %s: %v)", ErrPermission, label, err)
		}
		return winVolume{}, fmt.Errorf("could not open %s on the mixer's storage: %w", label, err)
	}
	var n uint32
	deadline := time.Now().Add(winLockTimeout)
	for {
		err = windows.DeviceIoControl(h, winFsctlLockVolume, nil, 0, nil, 0, &n, nil)
		if err == nil {
			break
		}
		if time.Now().After(deadline) {
			windows.CloseHandle(h)
			return winVolume{}, fmt.Errorf("%s on the mixer's storage is in use by another program, such as an Explorer window or a virus scan. Close it and try again; nothing was changed (%v)", label, err)
		}
		time.Sleep(500 * time.Millisecond)
	}
	if err := windows.DeviceIoControl(h, winFsctlDismountVolume, nil, 0, nil, 0, &n, nil); err != nil {
		windows.DeviceIoControl(h, winFsctlUnlockVolume, nil, 0, nil, 0, &n, nil)
		windows.CloseHandle(h)
		return winVolume{}, fmt.Errorf("could not dismount %s on the mixer's storage; nothing was changed (%v)", label, err)
	}
	return winVolume{h: h, name: label}, nil
}

// winVolumeLabel names a volume for people: its drive letter or folder if
// it has one ("drive E:\"), else its GUID path.
func winVolumeLabel(name string) string {
	p, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return name
	}
	buf := make([]uint16, 1024)
	var n uint32
	if err := windows.GetVolumePathNamesForVolumeName(p, &buf[0], uint32(len(buf)), &n); err == nil {
		if first := winFirstPath(buf); first != "" {
			return "drive " + first
		}
	}
	return "volume " + name
}

func winReleaseVolumes(volumes []winVolume) {
	for _, v := range volumes {
		var n uint32
		windows.DeviceIoControl(v.h, winFsctlUnlockVolume, nil, 0, nil, 0, &n, nil)
		windows.CloseHandle(v.h)
	}
}

type winDisk struct {
	h       windows.Handle
	write   bool
	sectors uint64
	path    string
	volumes []winVolume
	closed  bool
}

// checkSize confirms 512-byte sectors and the size find reported, on the
// handle that will do the I/O.
func (d *winDisk) checkSize() error {
	out, err := winIoctl(d.h, winIoctlDiskGetLengthInfo, 8)
	if err != nil {
		return fmt.Errorf("could not read the size of %s: %w", d.path, err)
	}
	length, err := winDecodeLength(out)
	if err != nil {
		return err
	}
	out, err = winIoctl(d.h, winIoctlDiskGetDriveGeometryEx, 256)
	if err != nil {
		return fmt.Errorf("could not read the sector size of %s: %w", d.path, err)
	}
	sector, _, err := winDecodeGeometryEx(out)
	if err != nil {
		return err
	}
	if sector != SectorSize {
		return fmt.Errorf("%s has %d-byte sectors; the mixer's storage has %d-byte sectors", d.path, sector, SectorSize)
	}
	if length != d.sectors*SectorSize {
		return fmt.Errorf("%s changed size since it was found (%d bytes, expected %d). Search again", d.path, length, d.sectors*SectorSize)
	}
	return nil
}

func (d *winDisk) Sectors() uint64 { return d.sectors }

// winChunk is the most one ReadFile or WriteFile moves.
const winChunk = 1 << 20

// winAligned returns n bytes at a 4096-byte boundary in memory, which
// covers the sector alignment FILE_FLAG_NO_BUFFERING requires of buffers.
// Go's heap does not move objects, so the address stays put.
func winAligned(n int) []byte {
	const align = 4096
	b := make([]byte, n+align)
	off := (align - int(uintptr(unsafe.Pointer(&b[0]))&(align-1))) & (align - 1)
	return b[off : off+n : off+n]
}

// span checks that I/O is whole sectors at a sector offset and returns how
// much of it lies on the disk.
func (d *winDisk) span(n int, off int64) (int, error) {
	if d.closed {
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

// transfer reads or writes b at off. The handle is synchronous, so with an
// OVERLAPPED the call uses its offset and returns when the I/O is done.
func (d *winDisk) transfer(b []byte, off int64, write bool) (int, error) {
	var ov windows.Overlapped
	ov.Offset = uint32(off)
	ov.OffsetHigh = uint32(off >> 32)
	var n uint32
	var err error
	if write {
		err = windows.WriteFile(d.h, b, &n, &ov)
	} else {
		err = windows.ReadFile(d.h, b, &n, &ov)
	}
	return int(n), err
}

func (d *winDisk) ReadAt(p []byte, off int64) (int, error) {
	want, err := d.span(len(p), off)
	if err != nil {
		return 0, err
	}
	if want == 0 {
		if len(p) == 0 {
			return 0, nil
		}
		return 0, io.EOF
	}
	buf := winAligned(min(want, winChunk))
	done := 0
	for done < want {
		chunk := buf[:min(len(buf), want-done)]
		n, err := d.transfer(chunk, off+int64(done), false)
		n = max(0, min(n, len(chunk)))
		copy(p[done:], chunk[:n])
		done += n
		if err != nil {
			return done, fmt.Errorf("reading %s: %w", d.path, err)
		}
		if n != len(chunk) {
			return done, io.ErrUnexpectedEOF
		}
	}
	if want < len(p) {
		return done, io.EOF
	}
	return done, nil
}

func (d *winDisk) WriteAt(p []byte, off int64) (int, error) {
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
	buf := winAligned(min(want, winChunk))
	done := 0
	for done < want {
		chunk := buf[:min(len(buf), want-done)]
		copy(chunk, p[done:])
		n, err := d.transfer(chunk, off+int64(done), true)
		n = max(0, min(n, len(chunk)))
		done += n
		if err != nil {
			return done, fmt.Errorf("writing %s: %w", d.path, err)
		}
		if n != len(chunk) {
			return done, io.ErrShortWrite
		}
	}
	return done, nil
}

// Sync: with FILE_FLAG_NO_BUFFERING there is no system cache of the disk,
// and FlushFileBuffers asks the device to commit its own write cache.
func (d *winDisk) Sync() error {
	if d.closed {
		return os.ErrClosed
	}
	if !d.write {
		return nil
	}
	if err := windows.FlushFileBuffers(d.h); err != nil {
		return fmt.Errorf("flushing %s: %w", d.path, err)
	}
	return nil
}

func (d *winDisk) Close() error {
	if d.closed {
		return nil
	}
	d.closed = true
	if d.write {
		// Re-read the partition table; a failure here changes nothing.
		var n uint32
		windows.DeviceIoControl(d.h, winIoctlDiskUpdateProperties, nil, 0, nil, 0, &n, nil)
	}
	winReleaseVolumes(d.volumes)
	d.volumes = nil
	return windows.CloseHandle(d.h)
}
