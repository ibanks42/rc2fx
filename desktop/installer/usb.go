package installer

// USB update helpers: HID mode switching, staging to the update drive,
// ejection, and flashing.

import (
	"bytes"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"

	"github.com/ibanks42/rc2fx/desktop/firmware"
)

func allowedModels(allow bool) flash.Allowed {
	return func(m model.Model) error {
		if err := firmware.FromHardware(m).RequireRuntimeWith(allow); err != nil {
			return Error(err.Error())
		}
		return nil
	}
}

func rejectExperimental(items []HID, serial string, allow bool) error {
	return flash.Refuse(items, serial, allowedModels(allow))
}

func mixers(items []HID) []USBMixer {
	out := []USBMixer{}
	for _, m := range flash.Mixers(items) {
		out = append(out, USBMixer{Model: firmware.FromHardware(m.Model), Serial: m.Serial, UpdateMode: m.UpdateMode, Firmware: m.Firmware})
	}
	return out
}

func FirmwareVersion(release uint16) (string, bool) { return flash.FirmwareVersion(release) }

// archiveMatches verifies archive.tar.gz and archive.md5 in dir against p.
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

// commandLog persists each USB command before sending and records its result after.
func (e *Engine) commandLog(name, serial string) (func(error) error, error) {
	index, err := e.recordCommand(name, serial)
	if err != nil {
		return nil, err
	}
	return func(result error) error { return e.finishCommand(index, result) }, nil
}

func (e *Engine) usbOptions() flash.Options {
	return flash.Options{
		Allowed:  allowedModels(e.allowExperimentalDuo),
		Record:   e.commandLog,
		Step:     e.step,
		Progress: e.reporter,
	}
}

// enterUSBMode switches a normal-mode mixer into USB update mode (HID 'M').
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

// flash copies the prepared firmware to the update drive, verifies it, ejects
// the drive, and sends the flash command (HID 'U').
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
