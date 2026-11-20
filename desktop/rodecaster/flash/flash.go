// Package flash implements RØDECaster USB firmware updates: HID discovery,
// update-mode switching, staging and verifying update files, safe ejection,
// and triggering the flash command.
package flash

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

// UpdateFiles are the files staged onto the update drive, in copy order.
var UpdateFiles = [2]string{"archive.tar.gz", "archive.md5"}

// Mixer describes a detected RØDECaster control interface.
type Mixer struct {
	Model      model.Model
	Serial     string
	UpdateMode bool
	// Parsed from USB bcdDevice in normal mode; nil in update mode where the
	// release number doesn't necessarily reflect the installed OS.
	Firmware *string
}

// Allowed filters which mixer models may be commanded (nil allows all).
type Allowed func(model.Model) error

func (a Allowed) check(m model.Model) error {
	if a == nil {
		return nil
	}
	return a(m)
}

// Refuse returns the first error from allowed for any recognized mixer in items
// (filtered to serial if non-empty).
func Refuse(items []platform.HID, serial string, allowed Allowed) error {
	for _, h := range items {
		if h.Usage != 0xff00 || (serial != "" && h.Serial != serial) {
			continue
		}
		if m, ok := model.FromUSB(model.VendorID, h.PID); ok {
			if err := allowed.check(m); err != nil {
				return err
			}
		}
	}
	return nil
}

// Control reports whether h is a RØDECaster HID control interface.
func Control(h *platform.HID) bool {
	_, ok := model.FromUSB(model.VendorID, h.PID)
	return h.Usage == 0xff00 && h.Serial != "" && ok
}

// Mixers extracts one Mixer entry per control interface in items.
func Mixers(items []platform.HID) []Mixer {
	out := []Mixer{}
	for i := range items {
		h := &items[i]
		if !Control(h) {
			continue
		}
		m, _ := model.FromUSB(model.VendorID, h.PID)
		update := platform.IsUpdatePID(h.PID)
		var version *string
		if !update {
			if v, ok := FirmwareVersion(h.Release); ok {
				version = &v
			}
		}
		out = append(out, Mixer{Model: m, Serial: h.Serial, UpdateMode: update, Firmware: version})
	}
	return out
}

// FirmwareVersion decodes RØDE's USB bcdDevice format, which strips dots as
// hex digits (e.g. "1.7.7" -> 0x0177). Only 3-digit BCD values are decoded.
func FirmwareVersion(release uint16) (string, bool) {
	digits := fmt.Sprintf("%x", release)
	if len(digits) != 3 {
		return "", false
	}
	for _, c := range digits {
		if c < '0' || c > '9' {
			return "", false
		}
	}
	return string(digits[0]) + "." + string(digits[1]) + "." + string(digits[2]), true
}

// SelectHID finds the unique control interface matching m, mode, and optional serial.
func SelectHID(items []platform.HID, m model.Model, update bool, serial string, allowed Allowed) (platform.HID, error) {
	if err := Refuse(items, "", allowed); err != nil {
		return platform.HID{}, err
	}
	if err := allowed.check(m); err != nil {
		return platform.HID{}, err
	}
	devices := Mixers(items)
	if len(devices) > 1 && (serial == "" || slices.ContainsFunc(devices, func(d Mixer) bool { return d.Model != m })) {
		return platform.HID{}, errors.New("more than one RØDECaster is connected; connect only one model")
	}
	var pids []uint16
	if update {
		pids = []uint16{m.UpdatePID()}
	} else {
		for _, pid := range m.NormalPIDs() {
			pids = append(pids, pid)
		}
	}
	var found []platform.HID
	for i := range items {
		h := &items[i]
		if Control(h) && slices.Contains(pids, h.PID) && (serial == "" || serial == h.Serial) {
			found = append(found, *h)
		}
	}
	switch len(found) {
	case 1:
		return found[0], nil
	case 0:
		mode := ""
		if update {
			mode = "in USB update mode "
		}
		return platform.HID{}, fmt.Errorf("no matching %s %sis connected (device model and serial must match the prepared firmware)", m.Label(), mode)
	}
	return platform.HID{}, errors.New("more than one RØDECaster is connected; connect only one")
}

// RØDE's USB gadget reads the serial from the settings partition and falls back
// to a placeholder like "RD0000000" if that partition failed to mount. Since
// the update drive lives on the same partition, flashing won't work either.
func placeholderSerial(serial string) bool {
	if len(serial) < 3 {
		return false
	}
	for _, b := range []byte(serial[:2]) {
		if b < 'A' || b > 'Z' {
			return false
		}
	}
	for _, b := range []byte(serial[2:]) {
		if b != '0' {
			return false
		}
	}
	return true
}

// RequireRealSerial refuses a mixer that reports a placeholder serial.
func RequireRealSerial(serial string) error {
	if placeholderSerial(serial) {
		return fmt.Errorf("the mixer reports the placeholder serial number %s instead of its own. That happens when its settings partition does not mount, and the update drive lives on that partition too, so flashing cannot fix it; nothing was changed. See \"The mixer forgets its setup at every restart\" in Tips and troubleshooting", serial)
	}
	return nil
}

// Record is called before sending a HID command and returns a hook to record its outcome.
type Record func(name, serial string) (finish func(result error) error, err error)

// Command sends a single HID control letter and waits for the 'A' ack.
// Never retries on missing ack since the mixer may have already acted on it.
func Command(p platform.Platform, h *platform.HID, letter byte, name string, record Record) error {
	d, err := p.Open(h)
	if err != nil {
		return err
	}
	defer d.Close()
	length := h.Output
	if length < 64 || length > 4096 {
		return errors.New("unexpected HID output report size")
	}
	input := max(h.Input, 256)
	if input > 4096 {
		return errors.New("unexpected HID input report size")
	}
	report := make([]byte, length)
	report[0] = 1
	report[1] = letter
	finish := func(error) error { return nil }
	if record != nil {
		if finish, err = record(name, h.Serial); err != nil {
			return err
		}
	}
	result := func() error {
		n, err := d.Write(report, 5*time.Second)
		if err != nil {
			return err
		}
		// Windows may pad an output report and return the padded byte count.
		// Only a count shorter than the report is a short write.
		if n < len(report) {
			return fmt.Errorf("HID short write: %d/%d", n, len(report))
		}
		buf := make([]byte, input)
		n, err = d.Read(buf, 5*time.Second)
		if err != nil {
			return fmt.Errorf("the mixer did not acknowledge the command, which it may still have received: %v", err)
		}
		if n < 2 || buf[0] != 2 || buf[1] != 'A' {
			return errors.New("the mixer's reply was not the expected acknowledgement")
		}
		return nil
	}()
	if err := finish(result); err != nil {
		return err
	}
	return result
}

type Options struct {
	Allowed  Allowed
	Record   Record
	Step     func(phase, detail string)
	Progress func(phase, detail string) func(completed, total uint64)
	Cancel   func() error // checked before staging each file and before eject
}

func (o *Options) step(phase, detail string) {
	if o.Step != nil {
		o.Step(phase, detail)
	}
}

func (o *Options) progress(phase, detail string) func(completed, total uint64) {
	if o.Progress == nil {
		return func(uint64, uint64) {}
	}
	return o.Progress(phase, detail)
}

func (o *Options) canceled() error {
	if o.Cancel == nil {
		return nil
	}
	return o.Cancel()
}

// EnterUpdateMode switches a normal-mode m mixer to USB update mode (HID
// `M`). Its audio stops and it shows "Updating via USB"; nothing is written
// yet.
func EnterUpdateMode(p platform.Platform, m model.Model, serial string, o Options) error {
	all, err := p.Inventory()
	if err != nil {
		return err
	}
	if err := Refuse(all, "", o.Allowed); err != nil {
		return err
	}
	if slices.ContainsFunc(Mixers(all), func(d Mixer) bool { return d.UpdateMode }) {
		return errors.New("a mixer is already in USB update mode")
	}
	h, err := SelectHID(all, m, false, serial, o.Allowed)
	if err != nil {
		return err
	}
	if err := RequireRealSerial(h.Serial); err != nil {
		return err
	}
	o.step("usb-mode", "Switching the mixer to USB update mode")
	return Command(p, &h, 'M', "usb-mode", o.Record)
}

// UpdateSize reports whether size is within the expected bounds of an update
// drive. RØDE creates the image with `dd count=560000`, though some mixers
// expose smaller drives (down to ~224 MB); Flash checks exact space via Fits.
func UpdateSize(size uint64) bool { return platform.UpdateSize(size) }

func validVolume(v *platform.Volume) error {
	if v.Path == "" || !slices.Contains([]string{"FAT", "FAT32", "vfat"}, v.FS) || !UpdateSize(v.Total) {
		return errors.New("the drive is not the mixer's FAT update drive")
	}
	return nil
}

// Fits refuses firmware larger than the update drive before writing any of
// it.
func Fits(needed, total uint64) error {
	if needed > total {
		return fmt.Errorf("the firmware needs %d MB but the mixer's update drive holds %d MB. Disconnect the mixer's power for 30 seconds, turn it on and start again; nothing was flashed. If it stays this small, the mixer's settings partition is probably not mounting: see \"The mixer forgets its setup at every restart\" in Tips and troubleshooting",
			(needed+(1<<20)-1)>>20, total>>20)
	}
	return nil
}

// UpdateDrive finds and mounts the update drive of the mixer with serial,
// in update mode.
func UpdateDrive(p platform.Platform, serial string) (platform.Volume, error) {
	volumes, err := p.Volumes()
	if err != nil {
		return platform.Volume{}, err
	}
	var owned []platform.Volume
	for _, v := range volumes {
		if v.Serial == serial && UpdateSize(v.Total) {
			owned = append(owned, v)
		}
	}
	switch len(owned) {
	case 0:
		return platform.Volume{}, errors.New("the mixer's update drive is not visible. If it was ejected, disconnect the mixer's power for 30 seconds, turn it on and start again; nothing was flashed")
	case 1:
	default:
		return platform.Volume{}, errors.New("found several update drives for this mixer; connect only one mixer")
	}
	v, err := p.Mount(owned[0])
	if err != nil {
		return platform.Volume{}, err
	}
	if err := validVolume(&v); err != nil {
		return platform.Volume{}, err
	}
	return v, nil
}

// CanReport reports whether p supports reading normal-mode USB report disks.
func CanReport(p platform.Platform) bool {
	_, ok := p.(platform.ReportDisks)
	return ok
}

// ReportDisk returns the normal-mode USB report disk for serial, if mounted
// with media present.
func ReportDisk(p platform.Platform, serial string) (platform.Volume, bool, error) {
	rd, ok := p.(platform.ReportDisks)
	if !ok || serial == "" {
		return platform.Volume{}, false, nil
	}
	volumes, err := rd.ReportDisks()
	if err != nil {
		return platform.Volume{}, false, err
	}
	var found []platform.Volume
	for _, v := range volumes {
		if v.Serial == serial && v.Total > 0 {
			found = append(found, v)
		}
	}
	switch len(found) {
	case 0:
		return platform.Volume{}, false, nil
	case 1:
		return found[0], true, nil
	}
	return platform.Volume{}, false, errors.New("found several USB disks for this mixer")
}

// ReadReport reads at most limit bytes of the regular file name in the root
// of report disk v (ReportDisk).
func ReadReport(p platform.Platform, v platform.Volume, name string, limit int64) ([]byte, error) {
	rd, ok := p.(platform.ReportDisks)
	if !ok {
		return nil, errors.New("this computer cannot read the mixer's USB disk")
	}
	if name == "" || name == "." || name == ".." || filepath.Base(name) != name || filepath.VolumeName(name) != "" {
		return nil, fmt.Errorf("%s is not a file name on the mixer's USB disk", name)
	}
	return rd.ReadReport(v, name, limit)
}

// WriteReport writes data as the regular file name in the root of report
// disk v (ReportDisk) and flushes it to the disk.
func WriteReport(p platform.Platform, v platform.Volume, name string, data []byte) error {
	rd, ok := p.(platform.ReportDisks)
	if !ok {
		return errors.New("this computer cannot write to the mixer's USB disk")
	}
	if name == "" || name == "." || name == ".." || filepath.Base(name) != name || filepath.VolumeName(name) != "" {
		return fmt.Errorf("%s is not a file name on the mixer's USB disk", name)
	}
	return rd.WriteReport(v, name, data)
}

// ReadFile reads at most limit bytes of the regular file name directly in
// the drive's root. Links, folders and other paths are refused.
func ReadFile(drive, name string, limit int64) ([]byte, error) {
	if name == "" || name == "." || name == ".." || filepath.Base(name) != name || filepath.VolumeName(name) != "" {
		return nil, fmt.Errorf("%s is not a file name on the update drive", name)
	}
	file, err := archive.OpenRegular(filepath.Join(drive, name))
	if err != nil {
		return nil, err
	}
	defer file.Close()
	return io.ReadAll(io.LimitReader(file, limit))
}

// stageFile copies src over dst through a temporary file on the same drive.
// Only the update file names are ever written; the caller reads the copy
// back.
func stageFile(src, dst string, progress func(uint64, uint64)) error {
	tempPath := filepath.Join(filepath.Dir(dst), ".rc2fx-"+filepath.Base(dst)+".part")
	for _, path := range []string{dst, tempPath} {
		info, err := os.Lstat(path)
		switch {
		case err == nil && info.Mode().IsRegular():
			if err := os.Remove(path); err != nil {
				return err
			}
		case err == nil:
			return fmt.Errorf("%s on the update drive is not a file", path)
		case !errors.Is(err, fs.ErrNotExist):
			return err
		}
	}
	if err := archive.CopyWithProgress(src, tempPath, progress); err != nil {
		return err
	}
	return os.Rename(tempPath, dst)
}

// Verify checks the staged files on the drive; progress receives bytes
// read.
type Verify func(drive string, progress func(uint64, uint64)) error

// Flash copies UpdateFiles from work to the update drive of the m mixer in
// update mode (with serial, when given), checks them there with verify,
// ejects the drive safely and sends the flash command (HID `U`).
func Flash(p platform.Platform, m model.Model, serial, work string, verify Verify, o Options) error {
	if verify == nil {
		return errors.New("flashing needs a check of the staged firmware")
	}
	inventory, err := p.Inventory()
	if err != nil {
		return err
	}
	h, err := SelectHID(inventory, m, true, serial, o.Allowed)
	if err != nil {
		return err
	}
	if err := RequireRealSerial(h.Serial); err != nil {
		return err
	}
	v, err := UpdateDrive(p, h.Serial)
	if err != nil {
		return err
	}
	var needed uint64
	for _, name := range UpdateFiles {
		file, err := archive.OpenWorkFile(filepath.Join(work, name))
		if err != nil {
			return err
		}
		info, err := file.Stat()
		file.Close()
		if err != nil {
			return err
		}
		needed += uint64(info.Size())
	}
	if err := Fits(needed, v.Total); err != nil {
		return err
	}
	drive := v.Path
	for _, name := range UpdateFiles {
		if err := o.canceled(); err != nil {
			return err
		}
		if err := stageFile(filepath.Join(work, name), filepath.Join(drive, name), o.progress("stage", name)); err != nil {
			return err
		}
	}
	if err := verify(drive, o.progress("verify", "Reading the firmware back from the mixer")); err != nil {
		return err
	}
	for _, name := range UpdateFiles {
		file, err := os.OpenFile(filepath.Join(drive, name), os.O_RDWR, 0)
		if err != nil {
			return err
		}
		err = file.Sync()
		file.Close()
		if err != nil {
			return err
		}
	}
	if err := o.canceled(); err != nil {
		return err
	}
	o.step("eject", "Safely ejecting the update drive")
	if err := p.Eject(&v); err != nil {
		return err
	}
	deadline := time.Now().Add(20 * time.Second)
	for {
		present, err := p.MediaPresent(&v)
		if err != nil {
			return err
		}
		if !present {
			break
		}
		if time.Now().After(deadline) {
			return errors.New("the computer has not released the update drive; close anything using it and try again. Nothing was flashed")
		}
		time.Sleep(250 * time.Millisecond)
	}
	inventory, err = p.Inventory()
	if err != nil {
		return err
	}
	h, err = SelectHID(inventory, m, true, h.Serial, o.Allowed)
	if err != nil {
		return err
	}
	o.step("flash", "Starting the firmware update")
	return Command(p, &h, 'U', "flash", o.Record)
}
