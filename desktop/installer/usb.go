package installer

// The USB update: HID mode commands, copying firmware to the update drive,
// safe ejection and the flash command.

import (
	"bytes"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"

	"github.com/ibanks42/rc2fx/desktop/firmware"
)

// allowedModels is the session's model policy for the flashing library:
// the Duo only with the experimental opt-in.
func allowedModels(allow bool) flash.Allowed {
	return func(m model.Model) error {
		if err := firmware.FromHardware(m).RequireRuntimeWith(allow); err != nil {
			return Error(err.Error())
		}
		return nil
	}
}

// rejectExperimental: inventory is already restricted to RØDE by the
// platform. Recognition must not broaden the list of devices to which we
// can send commands.
func rejectExperimental(items []HID, serial string, allow bool) error {
	return flash.Refuse(items, serial, allowedModels(allow))
}

// mixers is the control interfaces, one per mixer.
func mixers(items []HID) []USBMixer {
	out := []USBMixer{}
	for _, m := range flash.Mixers(items) {
		out = append(out, USBMixer{Model: firmware.FromHardware(m.Model), Serial: m.Serial, UpdateMode: m.UpdateMode, Firmware: m.Firmware})
	}
	return out
}

// FirmwareVersion is a RØDE firmware version from a USB release number
// (see flash.FirmwareVersion).
func FirmwareVersion(release uint16) (string, bool) { return flash.FirmwareVersion(release) }

// archiveMatches checks `archive.tar.gz` and `archive.md5` in dir against
// the prepared firmware, reading the archive once. progress receives bytes
// read.
func archiveMatches(dir string, p *Prepared, progress func(uint64, uint64)) error {
	archivePath := filepath.Join(dir, "archive.tar.gz")
	sha, md5sum, err := archive.HashBoth(archivePath, progress)
	if err != nil {
		return err
	}
	if sha != p.ArchiveSHA || md5sum != p.ArchiveMD5 {
		return errorf("%s does not match the prepared firmware", archivePath)
	}
	file, err := archive.OpenRegular(filepath.Join(dir, "archive.md5"))
	if err != nil {
		return err
	}
	defer file.Close()
	md, err := readLimited(file, 128)
	if err != nil {
		return err
	}
	if !bytes.Equal(archive.TrimASCII(md), []byte(p.ArchiveMD5)) {
		return Error("archive.md5 does not match the prepared firmware")
	}
	return nil
}

// updateSize is whether size could be an update drive (see
// flash.UpdateSize).
func updateSize(size uint64) bool { return flash.UpdateSize(size) }

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

// commandLog records each command in the saved state before it is sent,
// and its result after.
func (e *Engine) commandLog(name, serial string) (func(error) error, error) {
	index, err := e.recordCommand(name, serial)
	if err != nil {
		return nil, err
	}
	return func(result error) error { return e.finishCommand(index, result) }, nil
}

// usbOptions is how this engine takes part in a USB update.
func (e *Engine) usbOptions() flash.Options {
	return flash.Options{
		Allowed:  allowedModels(e.allowExperimentalDuo),
		Record:   e.commandLog,
		Step:     e.step,
		Progress: e.reporter,
	}
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
	return flash.EnterUpdateMode(e.Platform, prepared.ModelOr(firmware.ModelProIi).Hardware(), serial, e.usbOptions())
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
	verify := func(drive string, progress func(uint64, uint64)) error {
		return archiveMatches(drive, prepared, progress)
	}
	return flash.Flash(e.Platform, prepared.ModelOr(firmware.ModelProIi).Hardware(), serial, e.Work, verify, e.usbOptions())
}
