package platform

import "os"

// The USB identity of the bootloader's mass-storage gadget (`ums mmc 0`,
// Amlogic's gadget), which shows a RØDECaster's whole eMMC as a USB disk.
const (
	recoveryVendor  = "1b8e"
	recoveryProduct = "fada"
)

// Recovery is the part of Platform that finds a mixer's whole eMMC while its
// bootloader shows it as a USB disk (see the installer's repair). It is
// separate from Platform, as Rust's trait gives these methods defaults:
// a Platform without them has no such disk, and use RecoveryDiskOf and
// RecoveryDeviceOf.
type Recovery interface {
	// RecoveryDisk is the size in bytes of that disk, or nil.
	RecoveryDisk() (*uint64, error)
	// RecoveryDevice is that disk, ready for the repair: anything this
	// computer mounted from it is unmounted, and it stays claimed until the
	// returned value is closed so nothing mounts it meanwhile.
	RecoveryDevice() (*RecoveryDevice, error)
}

// RecoveryDevice is the mixer's whole eMMC, held for a repair.
type RecoveryDevice struct {
	// Path is what libext2fs and the partition reads open (`/dev/sdX`).
	Path string
	Len  uint64
	// Claim keeps the disk claimed (on Linux, an O_EXCL descriptor).
	Claim *os.File
}

// Close releases the claim.
func (d *RecoveryDevice) Close() error {
	if d == nil || d.Claim == nil {
		return nil
	}
	err := d.Claim.Close()
	d.Claim = nil
	return err
}

// errRecoveryUnavailable is the default RecoveryDevice's answer.
const errRecoveryUnavailable = platformError("Repairing the mixer from this computer isn't available on this system yet")

// RecoveryDiskOf is p's RecoveryDisk, or nil for a platform without one.
func RecoveryDiskOf(p Platform) (*uint64, error) {
	if r, ok := p.(Recovery); ok {
		return r.RecoveryDisk()
	}
	return nil, nil
}

// RecoveryDeviceOf is p's RecoveryDevice, or an error for a platform
// without one.
func RecoveryDeviceOf(p Platform) (*RecoveryDevice, error) {
	if r, ok := p.(Recovery); ok {
		return r.RecoveryDevice()
	}
	return nil, errRecoveryUnavailable
}
