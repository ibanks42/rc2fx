//go:build linux

package platform

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"golang.org/x/sys/unix"
)

type recoveryDisk struct {
	device string
	size   uint64
}

// recoveryDisks is the whole-disk block devices of a mixer whose bootloader
// shows its eMMC over USB (`ums mmc 0`: Amlogic's gadget, `1b8e:fada`), with
// their size.
func recoveryDisks() ([]recoveryDisk, error) {
	entries, err := os.ReadDir("/sys/class/block")
	if err != nil {
		return nil, err
	}
	var found []recoveryDisk
	for _, entry := range entries {
		name := entry.Name()
		sys := filepath.Join("/sys/class/block", name)
		if exists(filepath.Join(sys, "partition")) || !strings.HasPrefix(name, "sd") {
			continue
		}
		current, err := filepath.EvalSymlinks(sys)
		if err != nil {
			continue
		}
		usb := ""
		for {
			if exists(filepath.Join(current, "idVendor")) {
				usb = current
				break
			}
			parent := filepath.Dir(current)
			if parent == current || parent == "/" {
				break
			}
			current = parent
		}
		if usb == "" {
			continue
		}
		if readTrim(filepath.Join(usb, "idVendor")) != recoveryVendor ||
			readTrim(filepath.Join(usb, "idProduct")) != recoveryProduct {
			continue
		}
		sectors, _ := strconv.ParseUint(readTrim(filepath.Join(sys, "size")), 10, 64)
		found = append(found, recoveryDisk{"/dev/" + name, sectors * 512})
	}
	return found, nil
}

func (host) RecoveryDisk() (*uint64, error) {
	disks, err := recoveryDisks()
	if err != nil {
		return nil, err
	}
	switch len(disks) {
	case 0:
		return nil, nil
	case 1:
		size := disks[0].size
		return &size, nil
	}
	return nil, platformError("more than one mixer is in USB storage mode; connect only one")
}

// partitionOf is whether a mounted device is a partition of the whole disk:
// the disk's name followed by digits only.
func partitionOf(mounted, disk string) bool {
	n, ok := strings.CutPrefix(mounted, disk)
	if !ok || n == "" {
		return false
	}
	for _, b := range []byte(n) {
		if b < '0' || b > '9' {
			return false
		}
	}
	return true
}

func (host) RecoveryDevice() (*RecoveryDevice, error) {
	disks, err := recoveryDisks()
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
	device, size := disks[0].device, disks[0].size
	// A desktop may have mounted the FAT partitions on its own. Unmount
	// them; never anything else.
	all, err := mounts()
	if err != nil {
		return nil, err
	}
	for _, m := range all {
		if !partitionOf(m.Device, device) {
			continue
		}
		if err := unix.Unmount(m.Path, 0); err != nil {
			return nil, platformError(fmt.Sprintf("%s from the mixer is in use; close it and retry: %v", m.Path, err))
		}
	}
	// The kernel refuses O_EXCL while a partition is mounted, and holding it
	// keeps anything from mounting one during the repair.
	fd, err := openFD(device, unix.O_RDONLY|unix.O_EXCL)
	if err != nil {
		return nil, platformError(fmt.Sprintf("cannot open the mixer's storage exclusively: %v", err))
	}
	return &RecoveryDevice{Path: device, Len: size, Claim: os.NewFile(uintptr(fd), device)}, nil
}
