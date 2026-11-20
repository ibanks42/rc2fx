//go:build linux

package platform

import (
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
)

func (host) RecoveryDisk() (*uint64, error) {
	disks, err := disk.Find()
	if err != nil {
		return nil, err
	}
	switch len(disks) {
	case 0:
		return nil, nil
	case 1:
		size := disks[0].Sectors * disk.SectorSize
		return &size, nil
	}
	return nil, platformError("more than one mixer is in USB storage mode; connect only one")
}

func (host) RecoveryDevice() (*RecoveryDevice, error) {
	disks, err := disk.Find()
	if err != nil {
		return nil, err
	}
	switch len(disks) {
	case 0:
		return nil, platformError("the mixer's storage is not visible; hold its power button for 15 seconds with USB 1 connected")
	case 1:
	default:
		return nil, platformError("more than one mixer is in USB storage mode; connect only one")
	}
	claim, err := disk.Open(disks[0], false)
	if err != nil {
		return nil, platformError(fmt.Sprintf("cannot open the mixer's storage exclusively: %v", err))
	}
	return &RecoveryDevice{
		Path:  disks[0].Path,
		Len:   disks[0].Sectors * disk.SectorSize,
		Claim: claim,
	}, nil
}
