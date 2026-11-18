// Package platform is RC2FX's view of the flashing library's operating-system
// USB access. It registers RC2FX's control reports so HID.Control reports
// whether RC2FX's USB layout is live on the mixer.
package platform

import (
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"

	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

func init() {
	platform.SetControlProbe(platform.ControlProbe{
		Descriptor: usbproto.DeclaresControl,
		OutputID:   usbproto.OutputID,
		InputID:    usbproto.InputID,
	})
}

type (
	HID            = platform.HID
	Volume         = platform.Volume
	Reports        = platform.Reports
	Platform       = platform.Platform
	RecoveryDevice = platform.RecoveryDevice
)

const USBPermission = platform.USBPermission

// Host is this system's USB access.
func Host() Platform { return platform.Host() }

func IsUpdatePID(pid uint16) bool                { return platform.IsUpdatePID(pid) }
func RecoveryDiskOf(p Platform) (*uint64, error) { return platform.RecoveryDiskOf(p) }
func RecoveryDeviceOf(p Platform) (*RecoveryDevice, error) {
	return platform.RecoveryDeviceOf(p)
}
