package autofix

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/rode"
)

// download puts RØDE's archive for the mixer's version and its archive.md5
// in dir, checked against RØDE's MD5. It returns that MD5.
func (r *run) download(dir string) (string, error) {
	if err := workDir(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be made: %v. Nothing was changed", err)
	}
	dest := filepath.Join(dir, flash.UpdateFiles[0])
	var md string
	if r.o.LocalArchive != "" {
		r.emit(StageDownload, "Checking RØDE's firmware, downloaded earlier")
		data, err := readSmall(r.o.LocalMD5, 4096)
		if err == nil {
			md, err = parseMD5(data)
		}
		if err != nil {
			return "", fmt.Errorf("the checksum of RØDE's firmware, downloaded earlier, could not be used (%v). Nothing was changed", err)
		}
		if err := copyChecked(r.o.LocalArchive, dest, md, r.progress(StageDownload, "Copying RØDE's firmware")); err != nil {
			return "", fmt.Errorf("RØDE's firmware, downloaded earlier, could not be used (%v). Nothing was changed", err)
		}
	} else {
		base := fmt.Sprintf("https://update.rode.com/%s/%s/", r.model.FirmwareDirectory(), r.version)
		archiveURL, err1 := rode.Location(r.model, base+"os.tar.gz", "os.tar.gz", r.version)
		digestURL, err2 := rode.Location(r.model, base+"os.md5", "os.md5", r.version)
		if err := errors.Join(err1, err2); err != nil {
			return "", fmt.Errorf("%v. Nothing was changed", err)
		}
		r.emit(StageDownload, fmt.Sprintf("Asking RØDE's server for firmware %s", r.version))
		digest, err := rode.Get(r.o.Client(), digestURL, 128)
		if err == nil {
			md, err = parseMD5(digest)
		}
		if err != nil {
			return "", fmt.Errorf("RØDE's checksum for firmware %s could not be fetched (%v). Check the internet connection and start again. Nothing was changed", r.version, err)
		}
		text := fmt.Sprintf("Downloading RØDE's firmware %s", r.version)
		bytes := r.progress(StageDownload, text)
		check := r.progress(StageDownload, "Checking the download")
		err = rode.DownloadFile(rode.Download{
			Client: r.o.Client,
			URL:    archiveURL,
			Dest:   dest,
			MD5:    md,
			Limit:  maxDownload,
			Progress: func(phase string, completed, total uint64, _ string) {
				if phase == "download" {
					bytes(completed, total)
				} else {
					check(completed, total)
				}
			},
			Cancel: r.canceled,
		})
		if errors.Is(err, ErrCanceled) {
			return "", fmt.Errorf("%w. Nothing was changed", err)
		}
		if err != nil {
			return "", fmt.Errorf("RØDE's firmware %s could not be downloaded (%v). Check the internet connection and start again. Nothing was changed", r.version, err)
		}
	}
	if err := archive.AtomicFile(filepath.Join(dir, flash.UpdateFiles[1]), []byte(md)); err != nil {
		return "", fmt.Errorf("the work folder could not be written: %v. Nothing was changed", err)
	}
	return md, nil
}

func readSmall(path string, limit int64) ([]byte, error) {
	f, err := archive.OpenRegular(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return io.ReadAll(io.LimitReader(f, limit))
}

// copyChecked copies src to dst through a private temporary file, keeping
// it only when what was written has MD5 md.
func copyChecked(src, dst, md string, progress func(uint64, uint64)) error {
	in, err := archive.OpenRegular(src)
	if err != nil {
		return err
	}
	defer in.Close()
	info, err := in.Stat()
	if err != nil {
		return err
	}
	if info.Size() > maxDownload {
		return errors.New("the file is larger than any RØDE firmware")
	}
	t, err := archive.NewTemp(filepath.Dir(dst), ".local-")
	if err != nil {
		return err
	}
	defer t.Cleanup()
	hashed := archive.HashingWriter(t)
	total := uint64(info.Size())
	buf := make([]byte, 1<<20)
	var done uint64
	progress(0, total)
	for {
		n, err := in.Read(buf)
		if n > 0 {
			if _, err := hashed.Write(buf[:n]); err != nil {
				return err
			}
			done += uint64(n)
			progress(done, total)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	if _, got := hashed.Finish(); got != md {
		return errors.New("the file does not match its MD5")
	}
	return t.Persist(dst)
}

// inspect refuses an archive the carrier script cannot run on, reading its
// member list once.
func (r *run) inspect(path string) error {
	r.emit(StageInspect, "Checking that the fix can run on this firmware")
	names := map[string]bool{}
	err := archive.Walk(path, 4<<30, false, func(m *archive.Member) error {
		names[archive.Normalized(m.Path)] = true
		return nil
	})
	if err != nil {
		return fmt.Errorf("RØDE's firmware file could not be read (%v). Nothing was changed", err)
	}
	var missing []string
	for _, alternatives := range requiredMembers {
		found := false
		for _, name := range alternatives {
			found = found || names[name]
		}
		if !found {
			missing = append(missing, "/"+strings.Join(alternatives, " or /"))
		}
	}
	var present []string
	for _, path := range carrier.Paths() {
		if names[path] {
			present = append(present, "/"+path)
		}
	}
	if len(missing) == 0 && len(present) == 0 {
		return nil
	}
	why := ""
	if len(missing) > 0 {
		why = "it has no " + strings.Join(missing, ", ")
	}
	if len(present) > 0 {
		if why != "" {
			why += "; "
		}
		why += "it already contains " + strings.Join(present, ", ")
	}
	return &Refusal{fmt.Errorf("firmware %s is not supported by this fix yet (%s). Nothing was changed", r.version, why)}
}

// build writes RØDE's archive plus the carrier files and o.ExtraEntries to
// dir, with its archive.md5. It returns the new archive's MD5.
func (r *run) build(src, dir string) (string, error) {
	r.emit(StageBuild, "Adding the fix to RØDE's firmware")
	if err := os.RemoveAll(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be cleared: %v. Nothing was changed", err)
	}
	if err := workDir(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be made: %v. Nothing was changed", err)
	}
	var extra []archive.Entry
	for _, f := range carrier.FilesFor(carrier.Options{USBReport: r.report, DebugLog: r.o.DebugLog}) {
		e := archive.Entry{Kind: archive.EntryFile, Path: f.Path, Mode: uint32(f.Mode.Perm()), Data: f.Data}
		if f.Symlink {
			e = archive.Entry{Kind: archive.EntryLink, Path: f.Path, Mode: uint32(f.Mode.Perm()), Target: f.Target}
		}
		extra = append(extra, e)
	}
	extra = append(extra, r.o.ExtraEntries...)
	patched, err := archive.Patch(src, filepath.Join(dir, flash.UpdateFiles[0]), archive.PatchOptions{Model: r.model, Extra: extra})
	if err != nil {
		return "", fmt.Errorf("the fix could not be added to RØDE's firmware (%v). Nothing was changed", err)
	}
	if err := archive.AtomicFile(filepath.Join(dir, flash.UpdateFiles[1]), []byte(patched.ArchiveMD5)); err != nil {
		return "", fmt.Errorf("the work folder could not be written: %v. Nothing was changed", err)
	}
	return patched.ArchiveMD5, nil
}

func (r *run) flashOptions() flash.Options {
	return flash.Options{
		Allowed: r.o.Allowed,
		Record:  r.o.Record,
		Step: func(phase, detail string) {
			switch phase {
			case "usb-mode":
				r.emit(StageUpdateMode, "Switching the mixer to USB update mode")
			case "eject":
				r.emit(StageCopy, "Safely ejecting the mixer's update drive")
			case "flash":
				r.flashSent = true
				r.emit(StageFlash, "Starting the update")
			default:
				r.emit(StageCopy, detail)
			}
		},
		Progress: func(phase, detail string) func(uint64, uint64) {
			if phase == "verify" {
				return r.progress(StageCopy, "Reading the firmware back from the mixer")
			}
			return r.progress(StageCopy, "Copying "+detail+" to the mixer")
		},
	}
}

// verify checks the staged files on the drive against md.
func verify(md string) flash.Verify {
	return func(drive string, progress func(uint64, uint64)) error {
		_, got, err := archive.HashBoth(filepath.Join(drive, flash.UpdateFiles[0]), progress)
		if err != nil {
			return err
		}
		if got != md {
			return errors.New("the firmware read back from the mixer's update drive does not match what was copied")
		}
		data, err := flash.ReadFile(drive, flash.UpdateFiles[1], 128)
		if err != nil {
			return err
		}
		if string(archive.TrimASCII(data)) != md {
			return errors.New("archive.md5 read back from the mixer's update drive does not match what was copied")
		}
		return nil
	}
}

// inUpdateMode is the error of a step that left the mixer in update mode
// with nothing flashed.
func inUpdateMode(err error) error {
	return fmt.Errorf("%v.\n\nThe mixer was left in USB update mode and nothing was flashed. %s", strings.TrimSuffix(err.Error(), "."), powerCycle)
}

// flash does one USB update with the files in dir. inUpdateMode: the
// mixer is already in update mode.
func (r *run) flash(dir, md string, carried, already bool) error {
	r.current = &Flash{Number: r.flashes + 1, Carrier: carried}
	r.flashSent = false
	r.safe = false
	opts := r.flashOptions()
	if !already {
		if err := flash.EnterUpdateMode(r.p, r.model, r.serial, opts); err != nil {
			return fmt.Errorf("%v.\n\nIf the mixer shows \"Updating via USB\", %s; nothing was flashed", strings.TrimSuffix(err.Error(), "."), powerCycleLower)
		}
	}
	if err := r.waitUpdateDrive(StageUpdateMode); err != nil {
		return err
	}
	if carried {
		if err := r.removeOldMarker(); err != nil {
			return inUpdateMode(err)
		}
	}
	err := flash.Flash(r.p, r.model, r.serial, dir, verify(md), opts)
	if r.flashSent {
		r.updateMode = false
	}
	if err != nil {
		if r.flashSent {
			return fmt.Errorf("%v.\n\nThe mixer may have started the update anyway. Leave it switched on and connected for 15 minutes. If it then shows its normal screen, start again; if it still shows \"Updating via USB\", %s", strings.TrimSuffix(err.Error(), "."), powerCycleLower)
		}
		return inUpdateMode(err)
	}
	r.flashes++
	r.safe = true
	return nil
}

// waitUpdateDrive waits for the mixer in update mode with its update drive.
// It cannot be canceled: the mixer is already in update mode.
func (r *run) waitUpdateDrive(stage string) error {
	r.emit(stage, "Waiting for the mixer's update drive")
	deadline := r.o.now().Add(r.o.UpdateModeLimit)
	for {
		inventory, err := r.p.Inventory()
		if err == nil {
			if _, err := flash.SelectHID(inventory, r.model, true, r.serial, r.o.Allowed); err == nil {
				volumes, err := r.p.Volumes()
				if err == nil {
					for _, v := range volumes {
						if v.Serial == r.serial && flash.UpdateSize(v.Total) {
							return nil
						}
					}
				}
			}
		}
		if !r.o.now().Before(deadline) {
			return fmt.Errorf("the mixer's update drive did not appear within %s. If the mixer shows \"Updating via USB\", %s; nothing was flashed", minutes(r.o.UpdateModeLimit), powerCycleLower)
		}
		r.o.sleep(r.o.Poll)
	}
}

// removeOldMarker deletes a marker an earlier run may have left on the
// update drive, so the next one read is this run's.
func (r *run) removeOldMarker() error {
	v, err := flash.UpdateDrive(r.p, r.serial)
	if err != nil {
		return err
	}
	path := filepath.Join(v.Path, carrier.MarkerName)
	info, err := os.Lstat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil
	case err != nil:
		return err
	case !info.Mode().IsRegular():
		return fmt.Errorf("%s on the mixer's update drive is not a file", carrier.MarkerName)
	}
	return os.Remove(path)
}

// errTimeout is a wait for the mixer that ran out of time.
var errTimeout = errors.New("timeout")
