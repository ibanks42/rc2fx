package installer

// The USB update: HID mode commands, copying firmware to the update drive,
// safe ejection and the flash command.

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

// VendorID is RØDE's USB vendor.
const VendorID = firmware.VendorID

// rejectExperimental: inventory is already restricted to RØDE by the
// platform. Recognition must not broaden the list of devices to which we
// can send commands.
func rejectExperimental(items []HID, serial string, allow bool) error {
	for _, h := range items {
		if h.Usage != 0xff00 || (serial != "" && h.Serial != serial) {
			continue
		}
		if model, ok := firmware.ModelFromUSB(firmware.VendorID, h.PID); ok {
			if err := model.RequireRuntimeWith(allow); err != nil {
				return Error(err.Error())
			}
		}
	}
	return nil
}

var updateFiles = [2]string{"archive.tar.gz", "archive.md5"}

// minDiskBytes: below this the mass-storage function has no real update
// drive behind it.
const minDiskBytes = 64 << 20

func control(h *HID) bool {
	_, ok := firmware.ModelFromUSB(firmware.VendorID, h.PID)
	return h.Usage == 0xff00 && h.Serial != "" && ok
}

// mixers is the control interfaces, one per mixer.
func mixers(items []HID) []USBMixer {
	out := []USBMixer{}
	for i := range items {
		h := &items[i]
		if !control(h) {
			continue
		}
		model, _ := firmware.ModelFromUSB(firmware.VendorID, h.PID)
		update := platform.IsUpdatePID(h.PID)
		var version *string
		if !update {
			if v, ok := FirmwareVersion(h.Release); ok {
				version = &v
			}
		}
		out = append(out, USBMixer{Model: model, Serial: h.Serial, UpdateMode: update, Firmware: version})
	}
	return out
}

// VendorUsage is whether a HID report descriptor declares a vendor-page
// (0xff00) application collection. Parsing never sends data to the device.
func VendorUsage(descriptor []byte) bool { return platform.VendorUsage(descriptor) }

// FirmwareVersion is a RØDE firmware version from a USB release number.
// RØDE's gadget script writes the version with its dots removed as hex
// (`1.7.7` → `0x0177`), so only versions with single-digit parts can be
// read back; anything else is unknown rather than guessed.
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

func selectHID(items []HID, model firmware.Model, update bool, serial string, allow bool) (HID, error) {
	if err := rejectExperimental(items, "", allow); err != nil {
		return HID{}, err
	}
	if err := model.RequireRuntimeWith(allow); err != nil {
		return HID{}, Error(err.Error())
	}
	devices := mixers(items)
	if len(devices) > 1 && (serial == "" || slices.ContainsFunc(devices, func(m USBMixer) bool { return m.Model != model })) {
		return HID{}, Error("more than one RØDECaster is connected; connect only one model")
	}
	var pids []uint16
	if update {
		pids = []uint16{model.UpdatePID()}
	} else {
		for _, pid := range model.NormalPIDs() {
			pids = append(pids, pid)
		}
	}
	var found []HID
	for i := range items {
		h := &items[i]
		if control(h) && slices.Contains(pids, h.PID) && (serial == "" || serial == h.Serial) {
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
		return HID{}, errorf("no matching %s %sis connected (device model and serial must match the prepared firmware)", model.Label(), mode)
	}
	return HID{}, Error("more than one RØDECaster is connected; connect only one")
}

// archiveMatches checks `archive.tar.gz` and `archive.md5` in dir against
// the prepared firmware, reading the archive once. progress receives bytes
// read.
func archiveMatches(dir string, p *Prepared, progress func(uint64, uint64)) error {
	archive := filepath.Join(dir, "archive.tar.gz")
	sha, md5sum, err := hashBoth(archive, progress)
	if err != nil {
		return err
	}
	if sha != p.ArchiveSHA || md5sum != p.ArchiveMD5 {
		return errorf("%s does not match the prepared firmware", archive)
	}
	file, err := openRegular(filepath.Join(dir, "archive.md5"))
	if err != nil {
		return err
	}
	defer file.Close()
	md, err := readLimited(file, 128)
	if err != nil {
		return err
	}
	if !bytes.Equal(trimASCII(md), []byte(p.ArchiveMD5)) {
		return Error("archive.md5 does not match the prepared firmware")
	}
	return nil
}

func verifyArchive(dir string, p *Prepared) error {
	return archiveMatches(dir, p, func(uint64, uint64) {})
}

// updateSize is whether size could be an update drive. RØDE's engine builds
// the drive with `dd count=560000` on its staging partition, so diskBytes
// is a ceiling and some mixers have a smaller drive (224 MB has been seen).
// The USB serial identifies the drive; a drive too small for the firmware is
// refused by flash, with a reason, rather than waited on here.
func updateSize(size uint64) bool {
	return size >= minDiskBytes && size <= diskBytes
}

func validVolume(v *Volume) error {
	if v.Path == "" || !slices.Contains([]string{"FAT", "FAT32", "vfat"}, v.FS) || !updateSize(v.Total) {
		return Error("the drive is not the mixer's FAT update drive")
	}
	return nil
}

// placeholderSerial: RØDE's USB setup reports its serial from a file on the
// settings partition and falls back to a placeholder (`RD0000000`, or the
// model letters and zeros) when the file is missing: the partition did not
// mount. That partition also holds the update drive, so a flash cannot help.
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

func requireRealSerial(serial string) error {
	if placeholderSerial(serial) {
		return errorf("the mixer reports the placeholder serial number %s instead of its own. That happens when its settings partition does not mount, and the update drive lives on that partition too, so flashing cannot fix it; nothing was changed. See \"The mixer forgets its setup at every restart\" in Tips and troubleshooting", serial)
	}
	return nil
}

// fits refuses firmware larger than the update drive before writing any of
// it.
func fits(needed, total uint64) error {
	if needed > total {
		return errorf("the firmware needs %d MB but the mixer's update drive holds %d MB. Disconnect the mixer's power for 30 seconds, turn it on and start again; nothing was flashed. If it stays this small, the mixer's settings partition is probably not mounting: see \"The mixer forgets its setup at every restart\" in Tips and troubleshooting",
			(needed+(1<<20)-1)>>20, total>>20)
	}
	return nil
}

// stageFile copies src over dst through a temporary file on the same drive.
// Only our two file names are ever written; the caller reads the copy back.
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
			return errorf("%s on the update drive is not a file", path)
		case !errors.Is(err, fs.ErrNotExist):
			return err
		}
	}
	if err := copyWithProgress(src, tempPath, progress); err != nil {
		return err
	}
	return os.Rename(tempPath, dst)
}

func (e *Engine) recordCommand(command, serial string) (int, error) {
	saved, err := e.load()
	if err != nil {
		return 0, err
	}
	saved.Commands = append(saved.Commands, CommandRecord{
		Command: command,
		Serial:  serial,
		At:      utc(),
		AtUnix:  nowUnix(),
	})
	if excess := len(saved.Commands) - 50; excess > 0 {
		saved.Commands = saved.Commands[excess:]
	}
	if err := e.save(saved); err != nil {
		return 0, err
	}
	return len(saved.Commands) - 1, nil
}

func (e *Engine) finishCommand(index int, result error) error {
	saved, err := e.load()
	if err != nil {
		return err
	}
	if index >= 0 && index < len(saved.Commands) {
		if result == nil {
			saved.Commands[index].Acknowledged = true
		} else {
			saved.Commands[index].Error = result.Error()
		}
	}
	return e.save(saved)
}

// command sends one HID command letter, logging it first. A missing ACK
// leaves the result unknown; nothing is retried.
func (e *Engine) command(h *HID, letter byte, name string) error {
	d, err := e.Platform.Open(h)
	if err != nil {
		return err
	}
	defer d.Close()
	length := h.Output
	if length < 64 || length > 4096 {
		return Error("unexpected HID output report size")
	}
	input := max(h.Input, 256)
	if input > 4096 {
		return Error("unexpected HID input report size")
	}
	report := make([]byte, length)
	report[0] = 1
	report[1] = letter
	index, err := e.recordCommand(name, h.Serial)
	if err != nil {
		return err
	}
	result := func() error {
		n, err := d.Write(report, 5*second)
		if err != nil {
			return err
		}
		// Windows may pad an output report and return the padded byte count.
		// Only a count shorter than the report is a short write.
		if n < len(report) {
			return errorf("HID short write: %d/%d", n, len(report))
		}
		buf := make([]byte, input)
		n, err = d.Read(buf, 5*second)
		if err != nil {
			return errorf("the mixer did not acknowledge the command, which it may still have received: %v", err)
		}
		if n < 2 || buf[0] != 2 || buf[1] != 'A' {
			return Error("the mixer's reply was not the expected acknowledgement")
		}
		return nil
	}()
	if err := e.finishCommand(index, result); err != nil {
		return err
	}
	return result
}

// enterUSBMode switches a normal-mode mixer to USB update mode (HID `M`).
// Its audio stops and it shows "Updating via USB"; nothing is written yet.
func (e *Engine) enterUSBMode(serial string) error {
	saved, err := e.load()
	if err != nil {
		return err
	}
	if saved.Prepared == nil {
		return Error("prepare firmware before switching the mixer to update mode")
	}
	prepared := saved.Prepared
	if err := e.verifyPrepared(prepared); err != nil {
		return err
	}
	all, err := e.Platform.Inventory()
	if err != nil {
		return err
	}
	if err := rejectExperimental(all, "", e.allowExperimentalDuo); err != nil {
		return err
	}
	if slices.ContainsFunc(mixers(all), func(m USBMixer) bool { return m.UpdateMode }) {
		return Error("a mixer is already in USB update mode")
	}
	h, err := selectHID(all, prepared.ModelOr(firmware.ModelProIi), false, serial, e.allowExperimentalDuo)
	if err != nil {
		return err
	}
	if err := requireRealSerial(h.Serial); err != nil {
		return err
	}
	e.step("usb-mode", "Switching the mixer to USB update mode")
	return e.command(&h, 'M', "usb-mode")
}

// flash copies the prepared firmware to the mixer's update drive, reads it
// back, ejects the drive safely and sends the flash command (HID `U`).
func (e *Engine) flash(serial string) error {
	saved, err := e.load()
	if err != nil {
		return err
	}
	if saved.Prepared == nil {
		return Error("prepare firmware before flashing")
	}
	prepared := saved.Prepared
	if err := e.verifyPrepared(prepared); err != nil {
		return err
	}
	model := prepared.ModelOr(firmware.ModelProIi)
	inventory, err := e.Platform.Inventory()
	if err != nil {
		return err
	}
	h, err := selectHID(inventory, model, true, serial, e.allowExperimentalDuo)
	if err != nil {
		return err
	}
	if err := requireRealSerial(h.Serial); err != nil {
		return err
	}
	volumes, err := e.Platform.Volumes()
	if err != nil {
		return err
	}
	var owned []Volume
	for _, v := range volumes {
		if v.Serial == h.Serial && updateSize(v.Total) {
			owned = append(owned, v)
		}
	}
	switch len(owned) {
	case 0:
		return Error("the mixer's update drive is not visible. If it was ejected, disconnect the mixer's power for 30 seconds, turn it on and start again; nothing was flashed")
	case 1:
	default:
		return Error("found several update drives for this mixer; connect only one mixer")
	}
	v, err := e.Platform.Mount(owned[0])
	if err != nil {
		return err
	}
	if err := validVolume(&v); err != nil {
		return err
	}
	var needed uint64
	for _, name := range updateFiles {
		file, err := openWorkFile(filepath.Join(e.Work, name))
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
	if err := fits(needed, v.Total); err != nil {
		return err
	}
	drive := v.Path
	for _, name := range updateFiles {
		if err := stageFile(filepath.Join(e.Work, name), filepath.Join(drive, name), e.reporter("stage", name)); err != nil {
			return err
		}
	}
	if err := archiveMatches(drive, prepared, e.reporter("verify", "Reading the firmware back from the mixer")); err != nil {
		return err
	}
	for _, name := range updateFiles {
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
	e.step("eject", "Safely ejecting the update drive")
	if err := e.Platform.Eject(&v); err != nil {
		return err
	}
	deadline := time.Now().Add(20 * time.Second)
	for {
		present, err := e.Platform.MediaPresent(&v)
		if err != nil {
			return err
		}
		if !present {
			break
		}
		if time.Now().After(deadline) {
			return Error("the computer has not released the update drive; close anything using it and try again. Nothing was flashed")
		}
		time.Sleep(250 * time.Millisecond)
	}
	inventory, err = e.Platform.Inventory()
	if err != nil {
		return err
	}
	h, err = selectHID(inventory, model, true, h.Serial, e.allowExperimentalDuo)
	if err != nil {
		return err
	}
	e.step("flash", "Starting the firmware update")
	return e.command(&h, 'U', "flash")
}
