//go:build darwin && cgo

package platform

// macOS: IOKit HID for the control interface; `ioreg`/`diskutil` for the
// update disk.

/*
#cgo LDFLAGS: -framework CoreFoundation -framework IOKit
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/hid/IOHIDManager.h>
#include <IOKit/hid/IOHIDDevice.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
	uint64_t id;
	char serial[1024];
	int64_t pid;
	int64_t release;
	uint8_t descriptor[8192];
	long descriptor_len;
} rc2fx_hid;

static CFTypeRef rc2fx_property(IOHIDDeviceRef d, const char *name) {
	CFStringRef key = CFStringCreateWithCString(NULL, name, kCFStringEncodingUTF8);
	CFTypeRef p = IOHIDDeviceGetProperty(d, key);
	CFRelease(key);
	return p;
}

static int64_t rc2fx_integer(IOHIDDeviceRef d, const char *name) {
	CFTypeRef p = rc2fx_property(d, name);
	int64_t n = 0;
	if (p != NULL && CFGetTypeID(p) == CFNumberGetTypeID()) {
		CFNumberGetValue((CFNumberRef)p, kCFNumberSInt64Type, &n);
	}
	return n;
}

static void rc2fx_text(IOHIDDeviceRef d, const char *name, char *out, long size) {
	out[0] = 0;
	CFTypeRef p = rc2fx_property(d, name);
	if (p == NULL || CFGetTypeID(p) != CFStringGetTypeID()) {
		return;
	}
	if (!CFStringGetCString((CFStringRef)p, out, size, kCFStringEncodingUTF8)) {
		out[0] = 0;
	}
}

static long rc2fx_bytes(IOHIDDeviceRef d, const char *name, uint8_t *out, long size) {
	CFTypeRef p = rc2fx_property(d, name);
	if (p == NULL || CFGetTypeID(p) != CFDataGetTypeID()) {
		return 0;
	}
	long length = CFDataGetLength((CFDataRef)p);
	const uint8_t *start = CFDataGetBytePtr((CFDataRef)p);
	if (start == NULL || length <= 0) {
		return 0;
	}
	if (length > size) {
		length = size;
	}
	memcpy(out, start, length);
	return length;
}

// rc2fx_devices is the manager and the copied set of RØDE devices.
static int rc2fx_devices(IOHIDManagerRef *manager, CFSetRef *set) {
	*manager = IOHIDManagerCreate(NULL, 0);
	*set = NULL;
	if (*manager == NULL) {
		return -1;
	}
	CFStringRef key = CFStringCreateWithCString(NULL, "VendorID", kCFStringEncodingUTF8);
	int64_t vendor = 0x19f7;
	CFNumberRef number = CFNumberCreate(NULL, kCFNumberSInt64Type, &vendor);
	const void *keys[1] = {key};
	const void *values[1] = {number};
	CFDictionaryRef matching = CFDictionaryCreate(NULL, keys, values, 1,
		&kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
	CFRelease(key);
	CFRelease(number);
	if (matching == NULL) {
		return -2;
	}
	IOHIDManagerSetDeviceMatching(*manager, matching);
	CFRelease(matching);
	*set = IOHIDManagerCopyDevices(*manager);
	return 0;
}

static void rc2fx_release(IOHIDManagerRef manager, CFSetRef set) {
	if (set != NULL) {
		CFRelease(set);
	}
	if (manager != NULL) {
		CFRelease(manager);
	}
}

// rc2fx_list fills out with the RØDE vendor-page HID devices. It returns
// the count, or -1 (no manager), -2 (no matching dictionary), -3 (too many
// devices) or -4 (no registry entry ID).
static int rc2fx_list(rc2fx_hid *out, int max) {
	IOHIDManagerRef manager;
	CFSetRef set;
	int status = rc2fx_devices(&manager, &set);
	if (status != 0) {
		rc2fx_release(manager, set);
		return status;
	}
	int found = 0;
	if (set != NULL) {
		CFIndex count = CFSetGetCount(set);
		if (count < 0 || count > 64 || count > max) {
			rc2fx_release(manager, set);
			return -3;
		}
		const void **devices = calloc(count > 0 ? count : 1, sizeof(void *));
		if (count > 0) {
			CFSetGetValues(set, devices);
		}
		for (CFIndex i = 0; i < count; i++) {
			IOHIDDeviceRef d = (IOHIDDeviceRef)devices[i];
			if (rc2fx_integer(d, "VendorID") != 0x19f7 || rc2fx_integer(d, "PrimaryUsagePage") != 0xff00) {
				continue;
			}
			uint64_t id = 0;
			if (IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(d), &id) != 0) {
				free(devices);
				rc2fx_release(manager, set);
				return -4;
			}
			rc2fx_hid *h = &out[found++];
			h->id = id;
			rc2fx_text(d, "SerialNumber", h->serial, sizeof(h->serial));
			h->pid = rc2fx_integer(d, "ProductID");
			h->release = rc2fx_integer(d, "VersionNumber");
			h->descriptor_len = rc2fx_bytes(d, "ReportDescriptor", h->descriptor, sizeof(h->descriptor));
		}
		free(devices);
	}
	rc2fx_release(manager, set);
	return found;
}

typedef struct {
	IOHIDDeviceRef device;
	CFRunLoopRef run_loop;
	CFStringRef mode;
	uint8_t buffer[4096];
	uint8_t queue[8][4096];
	long lengths[8];
	int head, count;
	IOReturn error;
} rc2fx_reports;

static void rc2fx_input(void *context, IOReturn result, void *sender, IOHIDReportType kind,
		uint32_t id, uint8_t *report, CFIndex length) {
	rc2fx_reports *r = context;
	if (result != 0) {
		r->error = result;
		return;
	}
	if (length < 0 || length > 4096 || report == NULL) {
		return;
	}
	if (r->count < 8) {
		int at = (r->head + r->count) % 8;
		memcpy(r->queue[at], report, length);
		r->lengths[at] = length;
		r->count++;
	}
}

// rc2fx_open opens the device with this registry ID, serial and product
// on the current thread's run loop, in a private mode. status is -1 when
// the identity changed, otherwise the IOReturn of a failed open.
static rc2fx_reports *rc2fx_open(uint64_t want, const char *serial, int64_t pid, int32_t *status) {
	IOHIDManagerRef manager;
	CFSetRef set;
	*status = rc2fx_devices(&manager, &set);
	if (*status != 0) {
		rc2fx_release(manager, set);
		return NULL;
	}
	IOHIDDeviceRef device = NULL;
	if (set != NULL) {
		CFIndex count = CFSetGetCount(set);
		if (count > 0 && count <= 64) {
			const void **devices = calloc(count, sizeof(void *));
			CFSetGetValues(set, devices);
			for (CFIndex i = 0; i < count && device == NULL; i++) {
				IOHIDDeviceRef d = (IOHIDDeviceRef)devices[i];
				if (rc2fx_integer(d, "VendorID") != 0x19f7 || rc2fx_integer(d, "PrimaryUsagePage") != 0xff00) {
					continue;
				}
				uint64_t id = 0;
				char text[1024];
				rc2fx_text(d, "SerialNumber", text, sizeof(text));
				if (IORegistryEntryGetRegistryEntryID(IOHIDDeviceGetService(d), &id) == 0 &&
						id == want && strcmp(text, serial) == 0 && rc2fx_integer(d, "ProductID") == pid) {
					device = (IOHIDDeviceRef)CFRetain(d);
				}
			}
			free(devices);
		}
	}
	rc2fx_release(manager, set);
	if (device == NULL) {
		*status = -1;
		return NULL;
	}
	IOReturn opened = IOHIDDeviceOpen(device, 0);
	if (opened != 0) {
		CFRelease(device);
		*status = opened;
		return NULL;
	}
	rc2fx_reports *r = calloc(1, sizeof(rc2fx_reports));
	r->device = device;
	r->run_loop = CFRunLoopGetCurrent();
	r->mode = CFStringCreateWithCString(NULL, "RC2FXUSB", kCFStringEncodingUTF8);
	IOHIDDeviceRegisterInputReportCallback(device, r->buffer, sizeof(r->buffer), rc2fx_input, r);
	IOHIDDeviceScheduleWithRunLoop(device, r->run_loop, r->mode);
	*status = 0;
	return r;
}

static IOReturn rc2fx_write(rc2fx_reports *r, const uint8_t *report, long length) {
	return IOHIDDeviceSetReport(r->device, kIOHIDReportTypeOutput, report[0], report, length);
}

// rc2fx_read returns a report's length, -1 at the deadline, or -2 with
// status set when IOKit reported an input error.
static long rc2fx_read(rc2fx_reports *r, uint8_t *out, long size, double timeout, int32_t *status) {
	CFAbsoluteTime deadline = CFAbsoluteTimeGetCurrent() + timeout;
	for (;;) {
		if (r->error != 0) {
			*status = r->error;
			return -2;
		}
		if (r->count > 0) {
			long n = r->lengths[r->head];
			if (n > size) {
				n = size;
			}
			memcpy(out, r->queue[r->head], n);
			r->head = (r->head + 1) % 8;
			r->count--;
			return n;
		}
		if (CFAbsoluteTimeGetCurrent() > deadline) {
			return -1;
		}
		CFRunLoopRunInMode(r->mode, 0.02, true);
	}
}

static void rc2fx_close(rc2fx_reports *r) {
	IOHIDDeviceRegisterInputReportCallback(r->device, r->buffer, sizeof(r->buffer), NULL, NULL);
	IOHIDDeviceUnscheduleFromRunLoop(r->device, r->run_loop, r->mode);
	IOHIDDeviceClose(r->device, 0);
	CFRelease(r->device);
	CFRelease(r->mode);
	free(r);
}
*/
import "C"

import (
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"
	"unsafe"

	"golang.org/x/sys/unix"
)

type host struct{}

// Host is this system's USB access.
func Host() Platform { return host{} }

func enumerate() ([]HID, error) {
	const max = 64
	out := (*[max]C.rc2fx_hid)(C.calloc(max, C.size_t(unsafe.Sizeof(C.rc2fx_hid{}))))
	defer C.free(unsafe.Pointer(out))
	n := int(C.rc2fx_list(&out[0], max))
	switch n {
	case -1:
		return nil, platformError("cannot initialize macOS USB discovery")
	case -2:
		return nil, platformError("cannot create USB matching dictionary")
	case -3:
		return nil, platformError("unexpected USB device count")
	case -4:
		return nil, platformError("cannot identify HID registry entry")
	}
	items := make([]HID, 0, n)
	for i := range n {
		h := &out[i]
		descriptor := C.GoBytes(unsafe.Pointer(&h.descriptor[0]), C.int(h.descriptor_len))
		items = append(items, HID{
			Path:    strconv.FormatUint(uint64(h.id), 10),
			Serial:  C.GoString(&h.serial[0]),
			PID:     uint16(h.pid),
			Usage:   0xff00,
			Input:   4096,
			Output:  64,
			Control: declaresControl(descriptor),
			Release: uint16(h.release),
		})
	}
	return items, nil
}

// macReports is an open device scheduled on one locked OS thread's run
// loop in a private mode. Every call, including Close, runs on that thread.
type macReports struct {
	calls chan func()
	r     *C.rc2fx_reports
}

func (m *macReports) do(f func()) {
	done := make(chan struct{})
	m.calls <- func() {
		f()
		close(done)
	}
	<-done
}

func (m *macReports) Write(report []byte, _ time.Duration) (int, error) {
	if m.r == nil {
		return 0, platformError("USB is not connected")
	}
	if len(report) == 0 {
		return 0, platformError("empty HID report")
	}
	var status C.IOReturn
	m.do(func() {
		data := C.CBytes(report)
		defer C.free(data)
		status = C.rc2fx_write(m.r, (*C.uint8_t)(data), C.long(len(report)))
	})
	if status != 0 {
		return 0, platformError(fmt.Sprintf("macOS HID write failed: 0x%08x", uint32(status)))
	}
	return len(report), nil
}

func (m *macReports) Read(buf []byte, timeout time.Duration) (int, error) {
	if m.r == nil {
		return 0, platformError("USB is not connected")
	}
	var n C.long
	var status C.int32_t
	m.do(func() {
		out := C.malloc(C.size_t(max(len(buf), 1)))
		defer C.free(out)
		n = C.rc2fx_read(m.r, (*C.uint8_t)(out), C.long(len(buf)), C.double(timeout.Seconds()), &status)
		if n > 0 {
			copy(buf, C.GoBytes(out, C.int(n)))
		}
	})
	switch {
	case n == -2:
		return 0, platformError(fmt.Sprintf("macOS HID input failed: 0x%08x", uint32(status)))
	case n < 0:
		return 0, platformError("USB transfer timed out")
	}
	return int(n), nil
}

func (m *macReports) Close() error {
	if m.r == nil {
		return nil
	}
	m.do(func() { C.rc2fx_close(m.r) })
	m.r = nil
	close(m.calls)
	return nil
}

func (host) Inventory() ([]HID, error) { return enumerate() }

func (host) Open(h *HID) (Reports, error) {
	id, err := strconv.ParseUint(h.Path, 10, 64)
	if err != nil {
		return nil, platformError("macOS USB identity changed")
	}
	m := &macReports{calls: make(chan func())}
	opened := make(chan error, 1)
	go func() {
		runtime.LockOSThread()
		defer runtime.UnlockOSThread()
		serial := C.CString(h.Serial)
		var status C.int32_t
		m.r = C.rc2fx_open(C.uint64_t(id), serial, C.int64_t(h.PID), &status)
		C.free(unsafe.Pointer(serial))
		switch {
		case m.r != nil:
			opened <- nil
		case status == -1:
			opened <- platformError("macOS USB identity changed")
			return
		case status == -2:
			opened <- platformError("cannot create USB matching dictionary")
			return
		default:
			opened <- platformError(fmt.Sprintf("macOS USB access failed: 0x%08x", uint32(status)))
			return
		}
		for f := range m.calls {
			f()
		}
	}()
	if err := <-opened; err != nil {
		return nil, err
	}
	return m, nil
}

func (h host) OpenControl(hid *HID) (Reports, error) { return h.Open(hid) }

// run runs program, bounded to 30 s and 8 MiB of output.
func run(program string, args ...string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, program, args...)
	out := &Bounded{Inner: &bytes.Buffer{}, Remaining: 8 << 20}
	var stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = out, &stderr
	err := cmd.Run()
	if ctx.Err() == context.DeadlineExceeded {
		return nil, platformError(program + " timed out")
	}
	if err != nil {
		if exit, ok := err.(*exec.ExitError); ok {
			return nil, platformError(fmt.Sprintf("%s: %s (%v)", filepath.Base(program), strings.TrimSpace(stderr.String()), exit.ProcessState))
		}
		return nil, platformError(fmt.Sprintf("%s: %v", program, err))
	}
	return out.Inner.(*bytes.Buffer).Bytes(), nil
}

func plistOf(program string, args ...string) (any, error) {
	data, err := run(program, args...)
	if err != nil {
		return nil, err
	}
	v, err := ParsePlist(data)
	if err != nil {
		return nil, platformError(fmt.Sprintf("%s: %v", program, err))
	}
	return v, nil
}

// ioregArgs: `-l` is required: alone, `-c` prints properties only on the
// matched USB device, leaving the disks below it without their `BSD Name`.
var ioregArgs = []string{"-a", "-l", "-r", "-c", "IOUSBHostDevice"}

// diskMap is the update drives' disks and their mixers' serials.
func diskMap() (map[string]string, error) {
	tree, err := plistOf("/usr/sbin/ioreg", ioregArgs...)
	if err != nil {
		return nil, err
	}
	return USBDisks(tree), nil
}

// mixerDiskMap is every disk of a mixer, in update or normal mode.
func mixerDiskMap() (map[string]MixerDisk, error) {
	tree, err := plistOf("/usr/sbin/ioreg", ioregArgs...)
	if err != nil {
		return nil, err
	}
	return MixerDisks(tree), nil
}

func statfs(path string) (bool, uint64, uint64, error) {
	var stat unix.Statfs_t
	if err := unix.Statfs(path, &stat); err != nil {
		return false, 0, 0, err
	}
	name := unix.ByteSliceToString(stat.Fstypename[:])
	return name == "msdos", stat.Blocks * uint64(stat.Bsize), stat.Bavail * uint64(stat.Bsize), nil
}

func volume(device, serial string) (Volume, error) {
	if !ValidDiskName(filepath.Base(device)) || filepath.Dir(device) != "/dev" {
		return Volume{}, platformError("invalid macOS disk identifier")
	}
	tree, err := plistOf("/usr/sbin/diskutil", "info", "-plist", device)
	if err != nil {
		return Volume{}, err
	}
	info, ok := ParseDiskInfo(tree)
	if !ok {
		return Volume{}, platformError("invalid disk information")
	}
	v := Volume{Device: device, Serial: serial, Path: info.MountPoint, Total: info.Total, Free: info.Free}
	if info.FAT {
		v.FS = "FAT"
	}
	if v.Path != "" {
		fat, total, free, err := statfs(v.Path)
		if err != nil {
			return Volume{}, err
		}
		if !fat {
			return Volume{}, platformError("update filesystem is not FAT")
		}
		v.FS, v.Total, v.Free = "FAT", total, free
	}
	return v, nil
}

func (host) Volumes() ([]Volume, error) {
	devices, err := diskMap()
	if err != nil {
		return nil, err
	}
	names := make([]string, 0, len(devices))
	for name := range devices {
		names = append(names, name)
	}
	sort.Strings(names)
	var result []Volume
	for _, device := range names {
		v, err := volume(device, devices[device])
		if err != nil {
			return nil, err
		}
		if !UpdateSize(v.Total) {
			continue
		}
		partitioned := false
		for _, other := range names {
			if other != device && strings.HasPrefix(other, device+"s") {
				partitioned = true
			}
		}
		if !partitioned {
			result = append(result, v)
		}
	}
	return result, nil
}

func (host) Mount(v Volume) (Volume, error) {
	devices, err := diskMap()
	if err != nil {
		return Volume{}, err
	}
	if serial, ok := devices[v.Device]; !ok || serial != v.Serial {
		return Volume{}, platformError("macOS disk ownership changed")
	}
	current, err := volume(v.Device, v.Serial)
	if err != nil {
		return Volume{}, err
	}
	if current.Path == "" {
		if _, err := run("/usr/sbin/diskutil", "mount", v.Device); err != nil {
			return Volume{}, err
		}
		return volume(v.Device, v.Serial)
	}
	return current, nil
}

// MediaPresent works on an update drive or a report disk (ReportDisks).
func (host) MediaPresent(v *Volume) (bool, error) {
	devices, err := mixerDiskMap()
	if err != nil {
		return false, err
	}
	d, ok := devices[v.Device]
	if !ok {
		return false, nil
	}
	if d.Serial != v.Serial {
		return false, platformError("macOS disk identity changed")
	}
	current, err := volume(v.Device, d.Serial)
	if err != nil {
		return false, err
	}
	return current.Total > 0, nil
}

// Eject works on an update drive or a report disk (ReportDisks).
func (host) Eject(v *Volume) error {
	devices, err := mixerDiskMap()
	if err != nil {
		return err
	}
	if d, ok := devices[v.Device]; !ok || d.Serial != v.Serial {
		return platformError("macOS disk ownership changed before eject")
	}
	_, err = run("/usr/sbin/diskutil", "eject", v.Device)
	return err
}

// ReportDisks: the mixer's disks in normal mode that hold a medium smaller
// than any update drive. macOS usually mounts them by itself.
func (host) ReportDisks() ([]Volume, error) {
	devices, err := mixerDiskMap()
	if err != nil {
		return nil, err
	}
	names := make([]string, 0, len(devices))
	for name, d := range devices {
		if !d.Update {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	var result []Volume
	for _, device := range names {
		partitioned := false
		for _, other := range names {
			if other != device && strings.HasPrefix(other, device+"s") {
				partitioned = true
			}
		}
		if partitioned {
			continue
		}
		// A disk that cannot be looked at now (just appearing or going
		// away) is looked for again at the next poll.
		v, err := volume(device, devices[device].Serial)
		if err != nil || v.Total == 0 || v.Total >= minDiskBytes {
			continue
		}
		result = append(result, v)
	}
	return result, nil
}

// mountReportDisk checks that v is still the mixer's report disk and returns
// its mount path, mounting the disk (read-only unless write) when nothing has
// it mounted and returning an unmount callback for temporary mounts.
func mountReportDisk(v Volume, write bool) (string, func() error, error) {
	devices, err := mixerDiskMap()
	if err != nil {
		return "", nil, err
	}
	if d, ok := devices[v.Device]; !ok || d.Serial != v.Serial || d.Update {
		return "", nil, platformError("USB disk identity changed")
	}
	current, err := volume(v.Device, v.Serial)
	if err != nil {
		return "", nil, err
	}
	var unmount func() error
	if current.Path == "" {
		args := []string{"mount"}
		if !write {
			args = append(args, "readOnly")
		}
		if _, err := run("/usr/sbin/diskutil", append(args, v.Device)...); err != nil {
			return "", nil, platformError(fmt.Sprintf("mount the mixer's USB disk: %v", err))
		}
		unmount = func() error {
			_, err := run("/usr/sbin/diskutil", "unmount", v.Device)
			return err
		}
		if current, err = volume(v.Device, v.Serial); err != nil || current.Path == "" {
			unmount()
			if err == nil {
				err = platformError("the mixer's USB disk did not mount")
			}
			return "", nil, err
		}
	}
	if current.FS != "FAT" {
		if unmount != nil {
			unmount()
		}
		return "", nil, platformError("the mixer's USB disk is not FAT")
	}
	return current.Path, unmount, nil
}
