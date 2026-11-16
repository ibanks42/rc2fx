//go:build linux

package platform

// Linux: hidraw for the control interface; sysfs, mountinfo, mount(2) and
// SCSI commands for the update disk. Requires root (the privileged helper).

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unsafe"

	"golang.org/x/sys/unix"

	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

type host struct{}

// Host is this system's USB access.
func Host() Platform { return host{} }

func readTrim(path string) string {
	data, _ := os.ReadFile(path)
	return strings.TrimSpace(string(data))
}

func isRoot() bool { return os.Geteuid() == 0 }

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

// usbAncestor is the serial and product ID of the RØDE USB device above a
// sysfs node.
func usbAncestor(start string) (string, uint16, error) {
	current, err := filepath.EvalSymlinks(start)
	if err != nil {
		return "", 0, err
	}
	for {
		if readTrim(filepath.Join(current, "idVendor")) == "19f7" {
			pid, err := strconv.ParseUint(readTrim(filepath.Join(current, "idProduct")), 16, 16)
			if err != nil {
				return "", 0, err
			}
			return readTrim(filepath.Join(current, "serial")), uint16(pid), nil
		}
		parent := filepath.Dir(current)
		if parent == current || parent == "/" {
			return "", 0, platformError("device is not attached to a RØDE USB parent")
		}
		current = parent
	}
}

// usbRelease is the release number (`bcdDevice`) of the RØDE USB device
// above a sysfs node; 0 when it can't be read.
func usbRelease(start string) uint16 {
	current, err := filepath.EvalSymlinks(start)
	if err != nil {
		return 0
	}
	for {
		if readTrim(filepath.Join(current, "idVendor")) == "19f7" {
			v, err := strconv.ParseUint(readTrim(filepath.Join(current, "bcdDevice")), 16, 16)
			if err != nil {
				return 0
			}
			return uint16(v)
		}
		parent := filepath.Dir(current)
		if parent == current || parent == "/" {
			return 0
		}
		current = parent
	}
}

func openFD(path string, flags int) (int, error) {
	for {
		fd, err := unix.Open(path, flags|unix.O_CLOEXEC, 0)
		if err == unix.EINTR {
			continue
		}
		return fd, err
	}
}

func waitFD(fd int, event int16, deadline time.Time) error {
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return platformError("USB transfer timed out")
		}
		ms := int(min(max(remaining.Milliseconds(), 1), 100))
		poll := []unix.PollFd{{Fd: int32(fd), Events: event}}
		n, err := unix.Poll(poll, ms)
		if err == unix.EINTR {
			continue
		}
		if err != nil {
			return err
		}
		if n > 0 {
			if poll[0].Revents&(unix.POLLERR|unix.POLLHUP|unix.POLLNVAL) != 0 {
				return platformError("USB disconnected or reported an I/O error")
			}
			if poll[0].Revents&event != 0 {
				return nil
			}
		}
	}
}

type linuxReports struct{ fd int }

func (r *linuxReports) Write(report []byte, timeout time.Duration) (int, error) {
	if err := waitFD(r.fd, unix.POLLOUT, time.Now().Add(timeout)); err != nil {
		return 0, err
	}
	return unix.Write(r.fd, report)
}

func (r *linuxReports) Read(buf []byte, timeout time.Duration) (int, error) {
	deadline := time.Now().Add(timeout)
	for {
		if err := waitFD(r.fd, unix.POLLIN, deadline); err != nil {
			return 0, err
		}
		n, err := unix.Read(r.fd, buf)
		if err == nil {
			return n, nil
		}
		if err != unix.EAGAIN && err != unix.EINTR {
			return 0, err
		}
	}
}

func (r *linuxReports) Close() error {
	if r.fd < 0 {
		return nil
	}
	err := unix.Close(r.fd)
	r.fd = -1
	return err
}

// Mount is one line of mountinfo.
type Mount struct {
	Device string
	Path   string
	FS     string
}

func decodeMount(s string) string {
	return strings.NewReplacer(`\040`, " ", `\011`, "\t", `\012`, "\n", `\134`, `\`).Replace(s)
}

// ParseMounts parses /proc/self/mountinfo.
func ParseMounts(data string) ([]Mount, error) {
	var result []Mount
	for _, line := range strings.Split(data, "\n") {
		line = strings.TrimSuffix(line, "\r")
		if strings.TrimSpace(line) == "" {
			continue
		}
		left, right, ok := strings.Cut(line, " - ")
		if !ok {
			return nil, platformError("invalid mount inventory")
		}
		l, r := strings.Fields(left), strings.Fields(right)
		if len(l) < 6 || len(r) < 3 {
			return nil, platformError("invalid mount inventory")
		}
		result = append(result, Mount{Device: decodeMount(r[1]), Path: decodeMount(l[4]), FS: r[0]})
	}
	return result, nil
}

func mounts() ([]Mount, error) {
	data, err := os.ReadFile("/proc/self/mountinfo")
	if err != nil {
		return nil, err
	}
	return ParseMounts(string(data))
}

// blockIdentity is the USB serial and whole-disk node of an update block
// device.
func blockIdentity(device string) (string, string, error) {
	name := filepath.Base(device)
	if filepath.Dir(device) != "/dev" || strings.Contains(name, "..") || name == "" || name == "/" || name == "." {
		return "", "", platformError("invalid block device")
	}
	sys := filepath.Join("/sys/class/block", name)
	serial, pid, err := usbAncestor(sys)
	if err != nil || !IsUpdatePID(pid) || serial == "" {
		return "", "", platformError("block device is not a RØDECaster update disk")
	}
	whole := device
	if exists(filepath.Join(sys, "partition")) {
		resolved, err := filepath.EvalSymlinks(sys)
		if err != nil {
			return "", "", err
		}
		whole = "/dev/" + filepath.Base(filepath.Dir(resolved))
	}
	return serial, whole, nil
}

func capacity(v Volume) (Volume, error) {
	var stat unix.Statfs_t
	if err := unix.Statfs(v.Path, &stat); err != nil {
		return Volume{}, err
	}
	if int64(stat.Type) != 0x4d44 {
		return Volume{}, platformError("update filesystem is not FAT")
	}
	v.Total = stat.Blocks * uint64(stat.Bsize)
	v.Free = stat.Bavail * uint64(stat.Bsize)
	v.FS = "vfat"
	return v, nil
}

// SgHeader is Linux sg_io_hdr_t from <scsi/sg.h>. No data transfer is
// requested.
type SgHeader struct {
	Interface    int32
	Direction    int32
	CmdLen       uint8
	SenseLen     uint8
	IovecCount   uint16
	TransferLen  uint32
	Transfer     uintptr
	Command      uintptr
	Sense        uintptr
	Timeout      uint32
	Flags        uint32
	PackID       int32
	User         uintptr
	Status       uint8
	MaskedStatus uint8
	Message      uint8
	SenseWritten uint8
	HostStatus   uint16
	DriverStatus uint16
	Residual     int32
	Duration     uint32
	Info         uint32
}

// ScsiKind is a SCSI command's outcome.
type ScsiKind int

const (
	ScsiOK ScsiKind = iota
	ScsiNoMedium
	ScsiUnitAttention
	ScsiFailed
)

// Scsi is a SCSI command's outcome; Error says why it failed.
type Scsi struct {
	Kind  ScsiKind
	Error string
}

// ScsiStatus classifies a finished SG_IO command.
func ScsiStatus(h *SgHeader, sense []byte) Scsi {
	if h.Info&1 == 0 && h.Status == 0 && h.HostStatus == 0 && h.DriverStatus == 0 {
		return Scsi{Kind: ScsiOK}
	}
	if h.Status == 2 && h.HostStatus == 0 && (h.DriverStatus == 0 || h.DriverStatus == 8) {
		var key, code byte
		parsed := false
		if len(sense) >= 14 && (sense[0]&0x7f == 0x70 || sense[0]&0x7f == 0x71) && sense[7] >= 6 {
			key, code, parsed = sense[2]&0x0f, sense[12], true
		} else if len(sense) >= 8 && (sense[0]&0x7f == 0x72 || sense[0]&0x7f == 0x73) {
			key, code, parsed = sense[1]&0x0f, sense[2], true
		}
		if parsed && key == 2 && code == 0x3a {
			return Scsi{Kind: ScsiNoMedium}
		}
		if parsed && key == 6 {
			return Scsi{Kind: ScsiUnitAttention}
		}
	}
	return Scsi{Kind: ScsiFailed, Error: fmt.Sprintf("SCSI media operation failed (status %02x, sense %s)", h.Status, hex.EncodeToString(sense))}
}

func scsiNoData(fd int, command []byte) (Scsi, error) {
	sense := make([]byte, 64)
	cmd := append([]byte{}, command...)
	var pin runtime.Pinner
	pin.Pin(&sense[0])
	pin.Pin(&cmd[0])
	header := &SgHeader{
		Interface: 'S',
		Direction: -1, // SG_DXFER_NONE
		CmdLen:    uint8(len(cmd)),
		SenseLen:  uint8(len(sense)),
		Command:   uintptr(unsafe.Pointer(&cmd[0])),
		Sense:     uintptr(unsafe.Pointer(&sense[0])),
		Timeout:   10_000,
	}
	pin.Pin(header)
	defer pin.Unpin()
	_, _, errno := unix.Syscall(unix.SYS_IOCTL, uintptr(fd), 0x2285, uintptr(unsafe.Pointer(header)))
	if errno != 0 {
		if errno == unix.ENOMEDIUM {
			return Scsi{Kind: ScsiNoMedium}, nil
		}
		return Scsi{}, errno
	}
	written := min(int(header.SenseWritten), len(sense))
	return ScsiStatus(header, sense[:written]), nil
}

func (host) Inventory() ([]HID, error) {
	var result []HID
	entries, err := os.ReadDir("/sys/class/hidraw")
	if err != nil {
		return result, nil
	}
	for _, entry := range entries {
		p := filepath.Join("/sys/class/hidraw", entry.Name())
		serial, pid, err := usbAncestor(filepath.Join(p, "device"))
		if err != nil {
			continue
		}
		descriptor, err := os.ReadFile(filepath.Join(p, "device", "report_descriptor"))
		if err != nil {
			return nil, platformError(fmt.Sprintf("read USB report descriptor: %v", err))
		}
		if VendorUsage(descriptor) {
			result = append(result, HID{
				Path:    "/dev/" + entry.Name(),
				Serial:  serial,
				PID:     pid,
				Usage:   0xff00,
				Input:   4096,
				Output:  64,
				Control: usbproto.DeclaresControl(descriptor),
				Release: usbRelease(filepath.Join(p, "device")),
			})
		}
	}
	return result, nil
}

func (h host) Open(hid *HID) (Reports, error) {
	// Fail before M even if a udev rule grants hidraw access: staging and
	// safe media ejection still require the privileged helper.
	if !isRoot() {
		return nil, platformError("USB installation requires administrator permission")
	}
	return h.OpenControl(hid)
}

func (host) OpenControl(hid *HID) (Reports, error) {
	name := filepath.Base(hid.Path)
	serial, pid, err := usbAncestor(filepath.Join("/sys/class/hidraw", name, "device"))
	if err != nil || serial != hid.Serial || pid != hid.PID {
		return nil, platformError("USB identity changed")
	}
	fd, err := openFD(hid.Path, unix.O_RDWR|unix.O_NONBLOCK)
	if err != nil {
		// Only a refusal is a permission problem; the app offers to fix that
		// one (the helper's USB rule).
		if errors.Is(err, unix.EACCES) || errors.Is(err, unix.EPERM) {
			return nil, platformError(fmt.Sprintf("%s: %v", USBPermission, err))
		}
		return nil, platformError(fmt.Sprintf("Couldn't open the mixer's USB control: %v", err))
	}
	return &linuxReports{fd: fd}, nil
}

func (host) Volumes() ([]Volume, error) {
	mounted, err := mounts()
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir("/sys/class/block")
	if err != nil {
		return nil, err
	}
	var result []Volume
	for _, entry := range entries {
		name := entry.Name()
		device := "/dev/" + name
		serial, _, err := blockIdentity(device)
		if err != nil {
			continue
		}
		sys := filepath.Join("/sys/class/block", name)
		sectors, err := strconv.ParseUint(readTrim(filepath.Join(sys, "size")), 10, 64)
		if err != nil {
			continue
		}
		if !UpdateSize(sectors * 512) {
			continue
		}
		if !exists(filepath.Join(sys, "partition")) {
			// A whole disk with partitions is represented by its partition.
			children, err := os.ReadDir(sys)
			if err != nil {
				return nil, err
			}
			hasChildren := false
			for _, child := range children {
				if strings.HasPrefix(child.Name(), name) && exists(filepath.Join(sys, child.Name(), "partition")) {
					hasChildren = true
				}
			}
			if hasChildren {
				continue
			}
		}
		v := Volume{Device: device, Serial: serial, Total: sectors * 512}
		for _, m := range mounted {
			if m.Device != device {
				continue
			}
			if v.Path != "" {
				return nil, platformError("update disk has multiple mounts; close extra mounts before continuing")
			}
			v.Path, v.FS = m.Path, m.FS
		}
		if v.Path != "" {
			if v, err = capacity(v); err != nil {
				return nil, err
			}
		}
		result = append(result, v)
	}
	return result, nil
}

func (host) Mount(v Volume) (Volume, error) {
	serial, _, err := blockIdentity(v.Device)
	if err != nil || serial != v.Serial {
		return Volume{}, platformError("USB disk identity changed")
	}
	all, err := mounts()
	if err != nil {
		return Volume{}, err
	}
	var paths []string
	for _, m := range all {
		if m.Device == v.Device {
			paths = append(paths, m.Path)
		}
	}
	if len(paths) > 1 {
		return Volume{}, platformError("update volume has multiple mounts")
	}
	if len(paths) == 1 {
		v.Path = paths[0]
		return capacity(v)
	}
	if !isRoot() {
		return Volume{}, platformError("mounting requires administrator permission")
	}
	nonce := make([]byte, 6)
	if _, err := rand.Read(nonce); err != nil {
		return Volume{}, err
	}
	dir := "/run/rc2fx-media-" + hex.EncodeToString(nonce)
	if err := os.Mkdir(dir, 0o700); err != nil {
		return Volume{}, err
	}
	if err := unix.Mount(v.Device, dir, "vfat", unix.MS_NODEV|unix.MS_NOSUID|unix.MS_NOEXEC, "umask=077"); err != nil {
		os.Remove(dir)
		return Volume{}, platformError(fmt.Sprintf("mount update volume: %v", err))
	}
	v.Path = dir
	return capacity(v)
}

func (host) MediaPresent(v *Volume) (bool, error) {
	if v.Device == "" {
		return false, platformError("Linux progress lacks the update block device")
	}
	serial, _, err := blockIdentity(v.Device)
	if err != nil {
		return false, err
	}
	if serial != v.Serial {
		return false, platformError("update disk identity changed")
	}
	fd, err := openFD(v.Device, unix.O_RDONLY|unix.O_NONBLOCK)
	if errors.Is(err, unix.ENOMEDIUM) || errors.Is(err, unix.ENXIO) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	defer unix.Close(fd)
	// BLKGETSIZE64 can retain cached capacity after media removal. Ask the
	// identified LUN directly; only an explicit no-medium response counts as
	// absence. Unit attention is not proof of ejection.
	status, err := scsiNoData(fd, []byte{0, 0, 0, 0, 0, 0})
	if err != nil {
		return false, err
	}
	switch status.Kind {
	case ScsiOK, ScsiUnitAttention:
		return true, nil
	case ScsiNoMedium:
		return false, nil
	}
	return false, platformError(status.Error)
}

func (host) Eject(v *Volume) error {
	serial, whole, err := blockIdentity(v.Device)
	if err != nil || serial != v.Serial {
		return platformError("update volume identity changed before ejection")
	}
	all, err := mounts()
	if err != nil {
		return err
	}
	for _, m := range all {
		if m.Device != v.Device {
			continue
		}
		if err := unix.Unmount(m.Path, 0); err != nil {
			return platformError(fmt.Sprintf("update files are still in use; close them and retry: %v", err))
		}
		if filepath.Dir(m.Path) == "/run" && strings.HasPrefix(filepath.Base(m.Path), "rc2fx-media-") {
			os.Remove(m.Path)
		}
	}
	// O_EXCL refuses a disk with another mounted partition. No forced
	// unmount, power-off, USB unbind or removal of the HID parent.
	fd, err := openFD(whole, unix.O_RDONLY|unix.O_NONBLOCK|unix.O_EXCL)
	if err != nil {
		return platformError(fmt.Sprintf("cannot exclusively access update media: %v", err))
	}
	defer unix.Close(fd)
	for _, command := range [][]byte{{0x1e, 0, 0, 0, 0, 0}, {0x1b, 0, 0, 0, 2, 0}} {
		status, err := scsiNoData(fd, command)
		if err != nil {
			return err
		}
		switch status.Kind {
		case ScsiNoMedium:
			return platformError("update media disappeared during ejection")
		case ScsiUnitAttention:
			return platformError("SCSI unit attention during ejection; retry")
		case ScsiFailed:
			return platformError(status.Error)
		}
	}
	return nil
}
