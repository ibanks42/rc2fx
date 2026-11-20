//go:build windows

package platform

// Windows: SetupAPI/HID for the control interface, Win32 volumes and
// storage IOCTLs for safe ejection.

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"os"
	"runtime"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

var (
	hid      = windows.NewLazySystemDLL("hid.dll")
	setupapi = windows.NewLazySystemDLL("setupapi.dll")
	kernel32 = windows.NewLazySystemDLL("kernel32.dll")

	procHidDGetHidGuid             = hid.NewProc("HidD_GetHidGuid")
	procHidDGetAttributes          = hid.NewProc("HidD_GetAttributes")
	procHidDGetPreparsedData       = hid.NewProc("HidD_GetPreparsedData")
	procHidDFreePreparsedData      = hid.NewProc("HidD_FreePreparsedData")
	procHidDGetSerialNumberString  = hid.NewProc("HidD_GetSerialNumberString")
	procHidDSetNumInputBuffers     = hid.NewProc("HidD_SetNumInputBuffers")
	procHidPGetCaps                = hid.NewProc("HidP_GetCaps")
	procHidPGetValueCaps           = hid.NewProc("HidP_GetValueCaps")
	procSetupDiGetClassDevsW       = setupapi.NewProc("SetupDiGetClassDevsW")
	procSetupDiEnumInterfaces      = setupapi.NewProc("SetupDiEnumDeviceInterfaces")
	procSetupDiGetInterfaceDetailW = setupapi.NewProc("SetupDiGetDeviceInterfaceDetailW")
	procSetupDiDestroyInfoList     = setupapi.NewProc("SetupDiDestroyDeviceInfoList")
	procSetThreadErrorMode         = kernel32.NewProc("SetThreadErrorMode")
)

const (
	digcfPresent         = 0x2
	digcfDeviceInterface = 0x10
	hidpInput            = 0
	hidpOutput           = 1
	hidpStatusSuccess    = 0x00110000
	// sizeofValueCaps is sizeof(HIDP_VALUE_CAPS); ReportID is at offset 2.
	sizeofValueCaps = 72

	ioctlStorageQueryProperty = 0x002d1400
	fsctlLockVolume           = 0x00090018
	fsctlDismountVolume       = 0x00090020
	ioctlStorageMediaRemoval  = 0x002d4804
	ioctlStorageEjectMedia    = 0x002d4808
)

type hiddAttributes struct {
	Size          uint32
	VendorID      uint16
	ProductID     uint16
	VersionNumber uint16
}

type hidpCaps struct {
	Usage                     uint16
	UsagePage                 uint16
	InputReportByteLength     uint16
	OutputReportByteLength    uint16
	FeatureReportByteLength   uint16
	Reserved                  [17]uint16
	NumberLinkCollectionNodes uint16
	NumberInputButtonCaps     uint16
	NumberInputValueCaps      uint16
	NumberInputDataIndices    uint16
	NumberOutputButtonCaps    uint16
	NumberOutputValueCaps     uint16
	NumberOutputDataIndices   uint16
	NumberFeatureButtonCaps   uint16
	NumberFeatureValueCaps    uint16
	NumberFeatureDataIndices  uint16
}

type spDeviceInterfaceData struct {
	CbSize             uint32
	InterfaceClassGUID windows.GUID
	Flags              uint32
	Reserved           uintptr
}

type host struct{}

// Host is this system's USB access.
func Host() Platform { return host{} }

func wide(s string) *uint16 {
	p, _ := windows.UTF16PtrFromString(s)
	return p
}

func openPath(name string, access, flags uint32) (windows.Handle, error) {
	return windows.CreateFile(wide(name), access, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, nil, windows.OPEN_EXISTING, flags, 0)
}

func lastError(err error) error {
	if errno, ok := err.(syscall.Errno); ok && errno == 0 {
		return errors.New("the call failed")
	}
	return err
}

func inspectHID(device windows.Handle, name string) (*HID, error) {
	a := hiddAttributes{Size: uint32(unsafe.Sizeof(hiddAttributes{}))}
	if r, _, err := procHidDGetAttributes.Call(uintptr(device), uintptr(unsafe.Pointer(&a))); r == 0 {
		return nil, lastError(err)
	}
	if a.VendorID != model.VendorID {
		return nil, nil
	}
	var preparsed uintptr
	if r, _, err := procHidDGetPreparsedData.Call(uintptr(device), uintptr(unsafe.Pointer(&preparsed))); r == 0 {
		return nil, lastError(err)
	}
	var caps hidpCaps
	status, _, _ := procHidPGetCaps.Call(preparsed, uintptr(unsafe.Pointer(&caps)))
	// The application's reports sit in RØDE's collection; their presence is
	// its layout.
	p, probed := controlProbe()
	control := probed && uint32(status) == hidpStatusSuccess &&
		declares(preparsed, hidpOutput, caps.NumberOutputValueCaps, p.OutputID) &&
		declares(preparsed, hidpInput, caps.NumberInputValueCaps, p.InputID)
	procHidDFreePreparsedData.Call(preparsed)
	if uint32(status) != hidpStatusSuccess {
		return nil, platformError("cannot read HID report capabilities")
	}
	serial := make([]uint16, 256)
	ok, _, _ := procHidDGetSerialNumberString.Call(uintptr(device), uintptr(unsafe.Pointer(&serial[0])), uintptr(len(serial)*2))
	if ok == 0 && caps.UsagePage == 0xff00 && a.ProductID != 0x26 {
		return nil, platformError("RØDE USB control interface has no readable serial")
	}
	return &HID{
		Path:    name,
		Serial:  windows.UTF16ToString(serial),
		PID:     a.ProductID,
		Release: a.VersionNumber,
		Usage:   caps.UsagePage,
		Input:   int(caps.InputReportByteLength),
		Output:  int(caps.OutputReportByteLength),
		Control: control,
	}, nil
}

// declares is whether the collection declares report id of kind.
func declares(preparsed uintptr, kind uintptr, count uint16, id byte) bool {
	if count == 0 {
		return false
	}
	caps := make([]byte, int(count)*sizeofValueCaps)
	n := count
	status, _, _ := procHidPGetValueCaps.Call(kind, uintptr(unsafe.Pointer(&caps[0])), uintptr(unsafe.Pointer(&n)), preparsed)
	if uint32(status) != hidpStatusSuccess {
		return false
	}
	for i := range int(min(n, count)) {
		if caps[i*sizeofValueCaps+2] == id {
			return true
		}
	}
	return false
}

type winReports struct{ handle windows.Handle }

func (r *winReports) transfer(buf []byte, write bool, timeout time.Duration) (int, error) {
	event, err := windows.CreateEvent(nil, 1, 0, nil)
	if err != nil {
		return 0, err
	}
	defer windows.CloseHandle(event)
	over := new(windows.Overlapped)
	over.HEvent = event
	var pin runtime.Pinner
	pin.Pin(over)
	if len(buf) > 0 {
		pin.Pin(&buf[0])
	}
	defer pin.Unpin()
	var count uint32
	if write {
		err = windows.WriteFile(r.handle, buf, &count, over)
	} else {
		err = windows.ReadFile(r.handle, buf, &count, over)
	}
	if err != nil {
		if err != windows.ERROR_IO_PENDING {
			return 0, err
		}
		ms := uint32(min(max(timeout.Milliseconds(), 1), 0xffffffff))
		status, _ := windows.WaitForSingleObject(event, ms)
		if status != windows.WAIT_OBJECT_0 {
			// Cancel and wait so the kernel no longer uses buf.
			windows.CancelIoEx(r.handle, over)
			windows.GetOverlappedResult(r.handle, over, &count, true)
			return 0, platformError("USB transfer timed out")
		}
		if err := windows.GetOverlappedResult(r.handle, over, &count, false); err != nil {
			return 0, err
		}
	}
	return int(count), nil
}

func (r *winReports) Write(report []byte, timeout time.Duration) (int, error) {
	return r.transfer(append([]byte{}, report...), true, timeout)
}

func (r *winReports) Read(buf []byte, timeout time.Duration) (int, error) {
	return r.transfer(buf, false, timeout)
}

func (r *winReports) Close() error {
	if r.handle == windows.InvalidHandle {
		return nil
	}
	err := windows.CloseHandle(r.handle)
	r.handle = windows.InvalidHandle
	return err
}

// DriveRoot normalizes a drive root such as `e:` to `E:\`.
func DriveRoot(p string) (string, error) {
	if len(p) == 2 {
		p += `\`
	}
	if len(p) != 3 || p[1] != ':' || (p[2] != '\\' && p[2] != '/') || !(p[0] >= 'a' && p[0] <= 'z' || p[0] >= 'A' && p[0] <= 'Z') {
		return "", platformError("expected a Windows drive root")
	}
	return strings.ToUpper(p[:1]) + `:\`, nil
}

// volumeInfo is the filesystem name and capacity of a mounted volume. It
// never shows an Insert Disk dialog for an empty slot.
func volumeInfo(root string) (string, uint64, uint64, error) {
	var previous uint32
	if r, _, err := procSetThreadErrorMode.Call(windows.SEM_FAILCRITICALERRORS|windows.SEM_NOOPENFILEERRORBOX, uintptr(unsafe.Pointer(&previous))); r == 0 {
		return "", 0, 0, lastError(err)
	}
	defer procSetThreadErrorMode.Call(uintptr(previous), 0)
	fs := make([]uint16, 32)
	if err := windows.GetVolumeInformation(wide(root), nil, 0, nil, nil, nil, &fs[0], uint32(len(fs))); err != nil {
		return "", 0, 0, err
	}
	var free, total uint64
	if err := windows.GetDiskFreeSpaceEx(wide(root), &free, &total, nil); err != nil {
		return "", 0, 0, err
	}
	return windows.UTF16ToString(fs), total, free, nil
}

// VolumePath is the device path of the volume at a drive root, such as
// `\\.\E:`.
func VolumePath(root string) string { return `\\.\` + root[:2] }

// storageIdentity is the USB storage serial and whether the volume is on a
// removable USB bus.
func storageIdentity(root string) (string, bool, error) {
	device, err := openPath(VolumePath(root), 0, 0)
	if err != nil {
		return "", false, err
	}
	defer windows.CloseHandle(device)
	query := make([]byte, 12) // StorageDeviceProperty, PropertyStandardQuery
	buf := make([]byte, 4096)
	var n uint32
	if err := windows.DeviceIoControl(device, ioctlStorageQueryProperty, &query[0], uint32(len(query)), &buf[0], uint32(len(buf)), &n, nil); err != nil {
		return "", false, err
	}
	if n < 36 {
		return "", false, platformError("short storage descriptor")
	}
	offset, bus := int(binary.LittleEndian.Uint32(buf[24:])), binary.LittleEndian.Uint32(buf[28:])
	if offset < 36 || offset >= int(n) {
		return "", false, platformError("USB storage serial is missing")
	}
	serial := buf[offset:n]
	for i, b := range serial {
		if b == 0 {
			serial = serial[:i]
			break
		}
	}
	// BusTypeUsb is 7; byte 10 is RemovableMedia.
	return strings.TrimSpace(strings.ToValidUTF8(string(serial), "\ufffd")), bus == 7 && buf[10] != 0, nil
}

func noMedia(err error) bool {
	errno, ok := err.(syscall.Errno)
	if !ok {
		return false
	}
	switch errno {
	case 2, 3, 15, 21, 55, 1112, 1167:
		return true
	}
	return false
}

func (host) Inventory() ([]HID, error) {
	var guid windows.GUID
	procHidDGetHidGuid.Call(uintptr(unsafe.Pointer(&guid)))
	set, _, err := procSetupDiGetClassDevsW.Call(uintptr(unsafe.Pointer(&guid)), 0, 0, digcfPresent|digcfDeviceInterface)
	if set == uintptr(windows.InvalidHandle) {
		return nil, lastError(err)
	}
	defer procSetupDiDestroyInfoList.Call(set)
	var all []HID
	for i := uint32(0); ; i++ {
		data := spDeviceInterfaceData{CbSize: uint32(unsafe.Sizeof(spDeviceInterfaceData{}))}
		if r, _, err := procSetupDiEnumInterfaces.Call(set, 0, uintptr(unsafe.Pointer(&guid)), uintptr(i), uintptr(unsafe.Pointer(&data))); r == 0 {
			if err == windows.ERROR_NO_MORE_ITEMS {
				break
			}
			return nil, lastError(err)
		}
		var needed uint32
		procSetupDiGetInterfaceDetailW.Call(set, uintptr(unsafe.Pointer(&data)), 0, 0, uintptr(unsafe.Pointer(&needed)), 0)
		if needed < 6 || needed > 65536 {
			return nil, platformError("invalid HID interface path size")
		}
		// u32-aligned storage for SP_DEVICE_INTERFACE_DETAIL_DATA_W.
		buf := make([]uint32, (needed+3)/4)
		if unsafe.Sizeof(uintptr(0)) == 8 {
			buf[0] = 8
		} else {
			buf[0] = 6
		}
		if r, _, err := procSetupDiGetInterfaceDetailW.Call(set, uintptr(unsafe.Pointer(&data)), uintptr(unsafe.Pointer(&buf[0])), uintptr(needed), 0, 0); r == 0 {
			return nil, lastError(err)
		}
		// DevicePath is a NUL-terminated UTF-16 string at offset 4.
		path := unsafe.Slice((*uint16)(unsafe.Add(unsafe.Pointer(&buf[0]), 4)), (int(needed)-4)/2)
		name := windows.UTF16ToString(path)
		if !strings.Contains(strings.ToLower(name), "vid_19f7") {
			continue
		}
		device, err := openPath(name, 0, 0)
		if err != nil {
			return nil, platformError(fmt.Sprintf("inspect RØDE USB interface: %v", err))
		}
		info, err := inspectHID(device, name)
		windows.CloseHandle(device)
		if err != nil {
			return nil, err
		}
		if info != nil && info.PID != 0 {
			all = append(all, *info)
		}
	}
	return all, nil
}

func (host) Open(h *HID) (Reports, error) {
	handle, err := openPath(h.Path, windows.GENERIC_READ|windows.GENERIC_WRITE, windows.FILE_FLAG_OVERLAPPED)
	if err != nil {
		return nil, err
	}
	actual, _ := inspectHID(handle, h.Path)
	if actual == nil || actual.Serial != h.Serial || actual.PID != h.PID || actual.Usage != h.Usage || actual.Output != h.Output {
		windows.CloseHandle(handle)
		return nil, platformError("HID identity or capabilities changed before opening")
	}
	// RØDE's own input reports share this collection; a deep queue keeps a
	// burst of them from displacing RC2FX's replies (512 is the most).
	if r, _, err := procHidDSetNumInputBuffers.Call(uintptr(handle), 512); r == 0 {
		windows.CloseHandle(handle)
		return nil, lastError(err)
	}
	return &winReports{handle: handle}, nil
}

func (h host) OpenControl(hid *HID) (Reports, error) { return h.Open(hid) }

func (host) Volumes() ([]Volume, error) { return drives(UpdateSize) }

// ReportDisks: the removable USB drives that hold a medium smaller than any
// update drive. Flash code matches them to the mixer by serial.
func (host) ReportDisks() ([]Volume, error) {
	return drives(func(size uint64) bool { return size > 0 && size < minDiskBytes })
}

// drives lists the removable USB drives with a medium whose size fits.
func drives(fits func(uint64) bool) ([]Volume, error) {
	roots := make([]uint16, 512)
	n, err := windows.GetLogicalDriveStrings(uint32(len(roots)), &roots[0])
	if n == 0 {
		return nil, lastError(err)
	}
	if int(n) >= len(roots) {
		return nil, platformError("drive inventory too large")
	}
	var result []Volume
	var failure error
	start := 0
	for i := 0; i <= int(n); i++ {
		if i < int(n) && roots[i] != 0 {
			continue
		}
		root := windows.UTF16ToString(roots[start:i])
		start = i + 1
		if root == "" {
			continue
		}
		if windows.GetDriveType(wide(root)) != windows.DRIVE_REMOVABLE {
			continue
		}
		fs, total, free, err := volumeInfo(root)
		if err != nil {
			if noMedia(err) {
				continue
			}
			if failure == nil {
				failure = platformError(fmt.Sprintf("cannot read drive %s: %v", root, err))
			}
			continue
		}
		if !fits(total) {
			continue
		}
		serial, usb, err := storageIdentity(root)
		if err != nil {
			if failure == nil {
				failure = platformError(fmt.Sprintf("cannot identify drive %s: %v", root, err))
			}
			continue
		}
		if usb {
			result = append(result, Volume{Path: root, Device: root, Serial: serial, FS: fs, Total: total, Free: free})
		}
	}
	// A locked or unformatted drive elsewhere must not hide the mixer's.
	if failure != nil && len(result) == 0 {
		return nil, failure
	}
	return result, nil
}

func (host) Mount(v Volume) (Volume, error) {
	fs, total, free, err := volumeInfo(v.Path)
	if err != nil {
		return Volume{}, err
	}
	v.FS, v.Total, v.Free = fs, total, free
	return v, nil
}

func (host) MediaPresent(v *Volume) (bool, error) {
	root, err := DriveRoot(v.Path)
	if err != nil {
		return false, err
	}
	if _, _, _, err := volumeInfo(root); err != nil {
		if noMedia(err) {
			return false, nil
		}
		return false, platformError(fmt.Sprintf("cannot inspect update media: %v", err))
	}
	return true, nil
}

// Eject ejects through the volume itself, as Microsoft documents for
// removable media: lock (retrying while something briefly holds a file
// open, such as a virus scan of the new firmware), dismount, then eject the
// media. Unlike the Shell's eject verb this never shows a dialog, and it
// never removes the USB parent device.
func (host) Eject(v *Volume) error {
	root, err := DriveRoot(v.Path)
	if err != nil {
		return err
	}
	serial, usb, err := storageIdentity(root)
	if err != nil {
		return err
	}
	if !usb || serial != v.Serial {
		return platformError("volume identity changed before ejection")
	}
	device, err := openPath(VolumePath(root), windows.GENERIC_READ|windows.GENERIC_WRITE, 0)
	if err != nil {
		return platformError(fmt.Sprintf("cannot open the update drive to eject it: %v", err))
	}
	defer windows.CloseHandle(device)
	control := func(code uint32, input []byte) error {
		var returned uint32
		var in *byte
		if len(input) > 0 {
			in = &input[0]
		}
		return windows.DeviceIoControl(device, code, in, uint32(len(input)), nil, 0, &returned, nil)
	}
	deadline := time.Now().Add(20 * time.Second)
	for {
		err := control(fsctlLockVolume, nil)
		if err == nil {
			break
		}
		if time.Now().After(deadline) {
			return platformError(fmt.Sprintf("something on this computer is still using the update drive, such as an Explorer window or a virus scan. Close it and try again; nothing was flashed (%v)", err))
		}
		time.Sleep(500 * time.Millisecond)
	}
	windows.FlushFileBuffers(device)
	if err := control(fsctlDismountVolume, nil); err != nil {
		return platformError(fmt.Sprintf("could not dismount the update drive: %v", err))
	}
	// PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }; not every device
	// supports it, and ejection decides.
	control(ioctlStorageMediaRemoval, []byte{0})
	if err := control(ioctlStorageEjectMedia, nil); err != nil {
		return platformError(fmt.Sprintf("could not eject the update drive: %v", err))
	}
	return nil
}

// ReadReport reads name from a report disk through its drive letter.
func (host) ReadReport(v Volume, name string, limit int64) ([]byte, error) {
	path, err := reportPath(v, name)
	if err != nil {
		return nil, err
	}
	if info, err := os.Lstat(path); err != nil {
		return nil, err
	} else if !info.Mode().IsRegular() {
		return nil, platformError("not a regular file")
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	return io.ReadAll(io.LimitReader(file, limit))
}

// WriteReport writes name to a report disk through its drive letter and
// flushes it. Eject, which follows, locks and dismounts the volume, which
// writes back the rest.
func (host) WriteReport(v Volume, name string, data []byte) error {
	path, err := reportPath(v, name)
	if err != nil {
		return err
	}
	if info, err := os.Lstat(path); err == nil && !info.Mode().IsRegular() {
		return platformError("not a regular file")
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o644)
	if err != nil {
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
	return file.Close()
}

// reportPath is name on report disk v, once v is checked to still be the
// same removable USB FAT drive.
func reportPath(v Volume, name string) (string, error) {
	if name == "" || name == "." || name == ".." || strings.ContainsAny(name, `\/:`) {
		return "", platformError("invalid file name")
	}
	root, err := DriveRoot(v.Path)
	if err != nil {
		return "", err
	}
	serial, usb, err := storageIdentity(root)
	if err != nil {
		return "", err
	}
	if !usb || serial != v.Serial {
		return "", platformError("USB disk identity changed")
	}
	fs, total, _, err := volumeInfo(root)
	if err != nil {
		return "", err
	}
	if !strings.HasPrefix(strings.ToUpper(fs), "FAT") || total == 0 || total >= minDiskBytes {
		return "", platformError("the mixer's USB disk is not a small FAT drive")
	}
	return root + name, nil
}
