// Package platform abstracts OS-level USB HID and mass-storage access for
// RØDECaster mixers.
package platform

import (
	"errors"
	"io"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// HID describes a RØDE HID top-level collection.
type HID struct {
	Path    string `json:"path"`
	Serial  string `json:"serial"`
	PID     uint16 `json:"pid"`
	Usage   uint16 `json:"usage"` // HID usage page
	Input   int    `json:"input"`
	Output  int    `json:"output"`
	Control bool   `json:"control"` // true when SetControlProbe matched custom control reports
	Release uint16 `json:"release"` // USB bcdDevice (e.g. 0x0177 for firmware 1.7.7)
}

// ControlProbe detects application-specific HID control reports for HID.Control.
// Linux/macOS inspect the raw report descriptor; Windows checks OutputID/InputID.
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

// SetControlProbe registers the probe used to populate HID.Control.
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

func declaresControl(descriptor []byte) bool {
	p, ok := controlProbe()
	return ok && p.Descriptor != nil && p.Descriptor(descriptor)
}

type Volume struct {
	Path   string `json:"path"`
	Device string `json:"device"`
	Serial string `json:"serial"`
	FS     string `json:"fs"`
	Total  uint64 `json:"total"`
	Free   uint64 `json:"free"`
}

// Reports is an open HID report channel.
type Reports interface {
	Write(report []byte, timeout time.Duration) (int, error)
	Read(buf []byte, timeout time.Duration) (int, error)
	Close() error
}

// Platform provides OS-specific USB HID and update-volume operations.
type Platform interface {
	Inventory() ([]HID, error)
	Open(hid *HID) (Reports, error)
	// OpenControl opens a HID channel for normal control traffic without
	// requiring update-volume privileges (aliases Open on platforms without
	// that split).
	OpenControl(hid *HID) (Reports, error)
	Volumes() ([]Volume, error)
	Mount(volume Volume) (Volume, error)
	MediaPresent(volume *Volume) (bool, error)
	Eject(volume *Volume) error
}

// ReportDisks is an optional Platform extension for OSes that can inspect the
// mixer's normal-mode USB mass-storage LUN (used by the carrier script to
// report results without entering update mode). Eject and MediaPresent also
// accept volumes returned by ReportDisks.
type ReportDisks interface {
	ReportDisks() ([]Volume, error)
	ReadReport(v Volume, name string, limit int64) ([]byte, error)
	WriteReport(v Volume, name string, data []byte) error
}

// USBPermission prefixes Linux hidraw permission errors so the desktop UI knows
// when to offer installing its udev rule.
const USBPermission = "USB permission required"

const (
	minDiskBytes = 64 << 20 // below this there is no real update drive behind the LUN
	diskBytes    = 560_000 * 512
)

func UpdateSize(size uint64) bool { return size >= minDiskBytes && size <= diskBytes }

func IsUpdatePID(pid uint16) bool {
	for _, m := range model.All {
		if m.UpdatePID() == pid {
			return true
		}
	}
	return false
}

// VendorUsage parses a HID report descriptor and reports whether it declares a
// vendor-defined (0xff00) collection.
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

// Bounded wraps an io.Writer and fails once Remaining bytes are exceeded.
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

type platformError string

func (e platformError) Error() string { return string(e) }
