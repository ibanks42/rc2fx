//go:build linux

package disk

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

const sysBlock = "/sys/class/block"

func readTrim(path string) string {
	b, _ := os.ReadFile(path)
	return strings.TrimSpace(string(b))
}

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

// usbDevice is the sysfs directory of the USB device a block device hangs
// off, or "" when it is not on USB.
func usbDevice(sys string) string {
	current, err := filepath.EvalSymlinks(sys)
	if err != nil {
		return ""
	}
	for current != "/" && current != "/sys" && current != "." {
		if exists(filepath.Join(current, "idVendor")) {
			return current
		}
		current = filepath.Dir(current)
	}
	return ""
}

func find() ([]Info, error) {
	entries, err := os.ReadDir(sysBlock)
	if err != nil {
		return nil, err
	}
	vendor, product := fmt.Sprintf("%04x", USBVendor), fmt.Sprintf("%04x", USBProduct)
	var found []Info
	for _, e := range entries {
		name := e.Name()
		sys := filepath.Join(sysBlock, name)
		if !strings.HasPrefix(name, "sd") || exists(filepath.Join(sys, "partition")) {
			continue
		}
		usb := usbDevice(sys)
		if usb == "" ||
			!strings.EqualFold(readTrim(filepath.Join(usb, "idVendor")), vendor) ||
			!strings.EqualFold(readTrim(filepath.Join(usb, "idProduct")), product) {
			continue
		}
		// sysfs "size" is in 512-byte units whatever the device's sector size.
		sectors, err := strconv.ParseUint(readTrim(filepath.Join(sys, "size")), 10, 64)
		if err != nil || sectors == 0 {
			continue
		}
		found = append(found, Info{
			Path:    "/dev/" + name,
			Sectors: sectors,
			Name:    fmt.Sprintf("/dev/%s, %.1f GB", name, float64(sectors)*SectorSize/1e9),
		})
	}
	return found, nil
}

// isPartitionName is whether name is the disk's name followed by digits only.
func isPartitionName(name, diskName string) bool {
	n, ok := strings.CutPrefix(name, diskName)
	if !ok || n == "" {
		return false
	}
	for _, c := range []byte(n) {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

// partitionDevs is the "major:minor" of each of the disk's partitions.
func partitionDevs(diskName string) map[string]bool {
	devs := map[string]bool{}
	entries, _ := os.ReadDir(filepath.Join(sysBlock, diskName))
	for _, e := range entries {
		if isPartitionName(e.Name(), diskName) {
			if dev := readTrim(filepath.Join(sysBlock, diskName, e.Name(), "dev")); dev != "" {
				devs[dev] = true
			}
		}
	}
	return devs
}

// unescapeMount undoes mountinfo's octal escapes (\040 for a space, …).
func unescapeMount(s string) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		if s[i] == '\\' && i+4 <= len(s) {
			if v, err := strconv.ParseUint(s[i+1:i+4], 8, 8); err == nil {
				b.WriteByte(byte(v))
				i += 3
				continue
			}
		}
		b.WriteByte(s[i])
	}
	return b.String()
}

// mountsOf is the mount points of the disk's partitions, in the order they
// were mounted. A mount counts when its source is /dev/<disk><digits> or its
// device number is one of the disk's partitions; nothing else.
func mountsOf(diskName string) ([]string, error) {
	data, err := os.ReadFile("/proc/self/mountinfo")
	if err != nil {
		return nil, err
	}
	devs := partitionDevs(diskName)
	var points []string
	for _, line := range strings.Split(string(data), "\n") {
		left, right, ok := strings.Cut(line, " - ")
		if !ok {
			continue
		}
		l, r := strings.Fields(left), strings.Fields(right)
		if len(l) < 5 || len(r) < 2 {
			continue
		}
		source := unescapeMount(r[1])
		if devs[l[2]] || (strings.HasPrefix(source, "/dev/") && isPartitionName(strings.TrimPrefix(source, "/dev/"), diskName)) {
			points = append(points, unescapeMount(l[4]))
		}
	}
	return points, nil
}

func unmountPartitions(diskName string) error {
	points, err := mountsOf(diskName)
	if err != nil {
		return fmt.Errorf("could not read the list of mounted disks: %w", err)
	}
	// Last mounted first, so a mount inside another goes before it.
	for i := len(points) - 1; i >= 0; i-- {
		for {
			err = unix.Unmount(points[i], 0)
			if err != unix.EINTR {
				break
			}
		}
		switch err {
		case nil, unix.EINVAL, unix.ENOENT:
			// EINVAL/ENOENT: already gone.
		case unix.EPERM, unix.EACCES:
			return ErrPermission
		case unix.EBUSY:
			return fmt.Errorf("%s, on the mixer, is in use; close any window or program using it and try again", points[i])
		default:
			return fmt.Errorf("could not unmount %s from the mixer: %w", points[i], err)
		}
	}
	return nil
}

func open(info Info, write bool) (Disk, error) {
	// Device names are reused: make sure this path is still the same mixer.
	current, err := find()
	if err != nil {
		return nil, err
	}
	still := false
	for _, c := range current {
		if c.Path == info.Path && c.Sectors == info.Sectors {
			still = true
		}
	}
	if !still {
		return nil, fmt.Errorf("the mixer's storage is no longer at %s; check the connection and try again", info.Path)
	}
	diskName := filepath.Base(info.Path)

	flags := unix.O_RDONLY
	if write {
		flags = unix.O_RDWR
	}
	// O_EXCL on a whole disk fails while one of its partitions is mounted,
	// and once held it keeps the desktop from mounting one again.
	flags |= unix.O_EXCL | unix.O_CLOEXEC
	var fd int
	for attempt := 0; ; attempt++ {
		if err := unmountPartitions(diskName); err != nil {
			return nil, err
		}
		fd, err = unix.Open(info.Path, flags, 0)
		if err == unix.EINTR {
			continue
		}
		// A desktop can mount a partition again between the unmount and
		// the open.
		if err == unix.EBUSY && attempt < 5 {
			time.Sleep(500 * time.Millisecond)
			continue
		}
		break
	}
	switch {
	case err == nil:
	case errors.Is(err, unix.EACCES), errors.Is(err, unix.EPERM):
		return nil, ErrPermission
	case errors.Is(err, unix.EBUSY):
		return nil, fmt.Errorf("the mixer's storage at %s is in use by another program; close it and try again", info.Path)
	default:
		return nil, fmt.Errorf("could not open the mixer's storage at %s: %w", info.Path, err)
	}
	return &linuxDisk{f: os.NewFile(uintptr(fd), info.Path), sectors: info.Sectors}, nil
}

type linuxDisk struct {
	f       *os.File
	sectors uint64
	wrote   bool
}

func (d *linuxDisk) ReadAt(p []byte, off int64) (int, error) { return d.f.ReadAt(p, off) }

func (d *linuxDisk) WriteAt(p []byte, off int64) (int, error) {
	n, err := d.f.WriteAt(p, off)
	if n > 0 {
		d.wrote = true
	}
	return n, err
}

func (d *linuxDisk) ioctl(req uint) error {
	rc, err := d.f.SyscallConn()
	if err != nil {
		return err
	}
	var ioErr error
	if err := rc.Control(func(fd uintptr) {
		if _, _, e := unix.Syscall(unix.SYS_IOCTL, fd, uintptr(req), 0); e != 0 {
			ioErr = e
		}
	}); err != nil {
		return err
	}
	return ioErr
}

// Sync flushes to the device, then drops the kernel's cached copy of the
// disk so the next read comes from the device.
func (d *linuxDisk) Sync() error {
	if err := d.f.Sync(); err != nil {
		return err
	}
	return d.ioctl(unix.BLKFLSBUF)
}

func (d *linuxDisk) Sectors() uint64 { return d.sectors }

func (d *linuxDisk) Close() error {
	if d.wrote {
		_ = d.ioctl(unix.BLKRRPART)
	}
	return d.f.Close()
}
