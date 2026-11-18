// Package platform is operating-system USB access: HID control interfaces
// and update volumes.
package platform

import (
	"errors"
	"io"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// HID is one HID top-level collection of a RØDE device.
type HID struct {
	Path   string `json:"path"`
	Serial string `json:"serial"`
	PID    uint16 `json:"pid"`
	// Usage is the HID usage page of the top-level collection.
	Usage  uint16 `json:"usage"`
	Input  int    `json:"input"`
	Output int    `json:"output"`
	// Control: the descriptor declares the application's control reports
	// (see SetControlProbe), so its USB layout is live on the mixer.
	Control bool `json:"control"`
	// Release is the USB device release number (`bcdDevice`). RØDE sets it
	// from the installed firmware version: 1.7.7 is `0x0177`.
	Release uint16 `json:"release"`
}

// ControlProbe recognizes an application's own HID control reports, which
// fill HID.Control. Descriptor reads a HID report descriptor (Linux and
// macOS); OutputID and InputID are the report IDs Windows looks for.
type ControlProbe struct {
	Descriptor func(descriptor []byte) bool
	OutputID   byte
	InputID    byte
}

var probe struct {
	sync.Mutex
	set   bool
	value ControlProbe
}

// SetControlProbe registers the application's control-report probe. Without
// one, HID.Control is always false.
func SetControlProbe(p ControlProbe) {
	probe.Lock()
	defer probe.Unlock()
	probe.set, probe.value = true, p
}

func controlProbe() (ControlProbe, bool) {
	probe.Lock()
	defer probe.Unlock()
	return probe.value, probe.set
}

// declaresControl is whether descriptor declares the registered control
// reports.
func declaresControl(descriptor []byte) bool {
	p, ok := controlProbe()
	return ok && p.Descriptor != nil && p.Descriptor(descriptor)
}

// Volume is a mass-storage volume.
type Volume struct {
	Path   string `json:"path"`
	Device string `json:"device"`
	Serial string `json:"serial"`
	FS     string `json:"fs"`
	Total  uint64 `json:"total"`
	Free   uint64 `json:"free"`
}

// Reports is an open HID report channel. Close it when done.
type Reports interface {
	Write(report []byte, timeout time.Duration) (int, error)
	Read(buf []byte, timeout time.Duration) (int, error)
	Close() error
}

// Platform is the operating system's USB access.
type Platform interface {
	Inventory() ([]HID, error)
	Open(hid *HID) (Reports, error)
	// OpenControl: ordinary HID control does not require update-volume
	// administration. Platforms without that distinction return Open.
	OpenControl(hid *HID) (Reports, error)
	Volumes() ([]Volume, error)
	Mount(volume Volume) (Volume, error)
	MediaPresent(volume *Volume) (bool, error)
	Eject(volume *Volume) error
}

// ReportDisks is implemented by platforms that can see a mixer's USB disk
// in normal mode, where the storage fix's script offers its result
// (rodecaster/carrier). Eject and MediaPresent work on these disks too.
// Platforms without it read that result from the update drive only.
type ReportDisks interface {
	// ReportDisks lists the normal-mode USB disks of RØDE mixers that hold
	// a medium smaller than any update drive.
	ReportDisks() ([]Volume, error)
	// ReadReport reads at most limit bytes of the regular file name in the
	// root of report disk v, mounting it read-only for the read when
	// nothing has it mounted.
	ReadReport(v Volume, name string, limit int64) ([]byte, error)
	// WriteReport writes data as the regular file name in the root of
	// report disk v and flushes it to the disk: where the disk is already
	// mounted, or through a private read-write mount that is removed again.
	// It refuses a disk that is no longer the mixer's disk v names.
	WriteReport(v Volume, name string, data []byte) error
}

// USBPermission is the start of the error the app gets when Linux refuses
// it the mixer's USB 1 control interface (no udev rule). The app offers to
// install the helper's USB rule when it sees exactly this.
const USBPermission = "USB permission required"

const (
	// minDiskBytes: below this the mass-storage function has no real update
	// drive behind it.
	minDiskBytes = 64 << 20
	diskBytes    = 560_000 * 512
)

// UpdateSize is whether size could be an update drive.
func UpdateSize(size uint64) bool { return size >= minDiskBytes && size <= diskBytes }

// IsUpdatePID is whether pid is any model's USB update mode.
func IsUpdatePID(pid uint16) bool {
	for _, m := range model.All {
		if m.UpdatePID() == pid {
			return true
		}
	}
	return false
}

// VendorUsage is whether a HID report descriptor declares a vendor-page
// (0xff00) application collection. Parsing never sends data to the device.
func VendorUsage(descriptor []byte) bool {
	var page uint32
	var stack []uint32
	i := 0
	for i < len(descriptor) {
		prefix := descriptor[i]
		i++
		if prefix == 0xfe {
			if i+2 > len(descriptor) {
				return false
			}
			i += 2 + int(descriptor[i])
			if i > len(descriptor) {
				return false
			}
			continue
		}
		n := int(prefix & 3)
		if n == 3 {
			n = 4
		}
		if i+n > len(descriptor) {
			return false
		}
		var v uint32
		for j, b := range descriptor[i : i+n] {
			v |= uint32(b) << (8 * j)
		}
		i += n
		switch prefix & 0xfc {
		case 0x04:
			page = v
		case 0xa4:
			stack = append(stack, page)
		case 0xb4:
			if len(stack) == 0 {
				return false
			}
			page = stack[len(stack)-1]
			stack = stack[:len(stack)-1]
		case 0xa0:
			if page == 0xff00 {
				return true
			}
		}
	}
	return false
}

// Bounded counts bytes written to an inner writer, rejecting anything past
// a limit.
type Bounded struct {
	Inner     io.Writer
	Remaining uint64
}

func (b *Bounded) Write(buf []byte) (int, error) {
	if uint64(len(buf)) > b.Remaining {
		return 0, errors.New("response exceeds size limit")
	}
	n, err := b.Inner.Write(buf)
	b.Remaining -= uint64(n)
	return n, err
}

// errorf is an error with the user-facing text.
type platformError string

func (e platformError) Error() string { return string(e) }
