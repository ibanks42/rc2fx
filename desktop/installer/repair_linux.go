//go:build linux

package installer

// Repairing a mixer from a computer, for when it can't repair itself.
//
// Holding a RØDECaster's power button for about 15 seconds at power-on makes
// its bootloader show the whole eMMC to the computer as a USB disk (`ums mmc
// 0`, Amlogic's gadget `1b8e:fada`). Four partitions are on it: two ext4
// system roots (RØDE's updater alternates between them), the FAT settings
// partition and the FAT data partition.
//
// A flash once left one of RØDE's start-up scripts zero-filled on the new
// root: the right size, none of the contents. That script mounts the
// settings partition, so the mixer lost its setup at every start. The repair
// compares each root's files with the list the installer put in the image
// (image_check.go), or with the image saved on this computer when it is the
// one that was flashed, and replaces a damaged file's contents with a
// byte-identical copy: from the other root, or from that image. It only ever
// rewrites a regular file in place with the same number of bytes, and all
// ext4 work is libext2fs's (package ext2fs). Before writing, it copies the
// settings partition to this computer.

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"

	"github.com/ibanks42/rc2fx/desktop/ext2fs"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

// RepairSupported: this system can check and repair a mixer in storage
// mode. The app offers Repair mixer only where it is.
const RepairSupported = true

const repairSector = 512

var mixerHostnames = [2]string{"rodecaster-pro-ii", "rodecaster-pro-ii-mini"}

const sumsMember = "opt/rc2fx/SHA256SUMS"

// maxRepairFile is the largest file read whole; RØDE's largest is its mixer
// executable.
const maxRepairFile = 256 << 20

// repairPart is one primary partition, in bytes.
type repairPart struct {
	start, len uint64
}

// partitions is the four primary partitions of an MBR, refusing anything
// that isn't four ordered, non-overlapping partitions inside the disk.
func partitions(sector0 *[512]byte, diskLen uint64) ([4]repairPart, error) {
	var parts [4]repairPart
	if sector0[510] != 0x55 || sector0[511] != 0xAA {
		return parts, Error("the disk has no partition table")
	}
	end := uint64(repairSector)
	for i := range parts {
		entry := sector0[446+16*i : 462+16*i]
		kind := entry[4]
		lba := uint64(binary.LittleEndian.Uint32(entry[8:12]))
		sectors := uint64(binary.LittleEndian.Uint32(entry[12:16]))
		start, length := lba*repairSector, sectors*repairSector
		if kind == 0 || sectors == 0 || start < end || start+length > diskLen {
			return parts, Error("the disk's partitions are not a RØDECaster's")
		}
		end = start + length
		parts[i] = repairPart{start, length}
	}
	return parts, nil
}

// diskLayout is RØDE's layout on this disk: four partitions, ext4 roots in
// the first two with a RØDECaster's hostname, and whether the settings
// partition's FAT boot sector looks sound.
type diskLayout struct {
	device     string
	parts      [4]repairPart
	hostname   string
	settingsOK bool
}

func layoutOf(device string, diskLen uint64) (*diskLayout, error) {
	disk, err := os.Open(device)
	if err != nil {
		return nil, err
	}
	defer disk.Close()
	var sector0 [512]byte
	if _, err := io.ReadFull(disk, sector0[:]); err != nil {
		return nil, err
	}
	parts, err := partitions(&sector0, diskLen)
	if err != nil {
		return nil, err
	}
	hostname := ""
	for _, part := range parts[:2] {
		fs, err := ext2fs.Open(device, part.start, false)
		if err != nil {
			return nil, errorf("a system partition is not ext4 (%v); this is not a RØDECaster's storage", err)
		}
		entry, data, err := readRootFile(fs, "etc/hostname")
		fs.Close()
		if err != nil {
			return nil, err
		}
		if entry != nil {
			name := strings.TrimSpace(strings.ToValidUTF8(string(data), "�"))
			if slices.Contains(mixerHostnames[:], name) {
				hostname = name
			}
		}
	}
	if hostname == "" {
		return nil, Error("neither system partition belongs to a RØDECaster; nothing was changed")
	}
	var boot [512]byte
	if _, err := disk.Seek(int64(parts[2].start), io.SeekStart); err != nil {
		return nil, err
	}
	if _, err := io.ReadFull(disk, boot[:]); err != nil {
		return nil, err
	}
	settingsOK := boot[510] == 0x55 && boot[511] == 0xAA &&
		(string(boot[82:87]) == "FAT32" || string(boot[54:58]) == "FAT1")
	return &diskLayout{device: device, parts: parts, hostname: hostname, settingsOK: settingsOK}, nil
}

// readRootFile is a regular file's entry and contents; a nil entry when it
// doesn't exist, and no contents when it isn't regular or is too large.
func readRootFile(fs *ext2fs.Filesystem, path string) (*ext2fs.Entry, []byte, error) {
	entry, err := fs.Lookup(path)
	if err != nil {
		return nil, nil, ext2(err)
	}
	if entry == nil {
		return nil, nil, nil
	}
	if !entry.Regular || entry.Size > maxRepairFile {
		return entry, []byte{}, nil
	}
	data, err := fs.Read(*entry)
	if err != nil {
		return nil, nil, ext2(err)
	}
	return entry, data, nil
}

func ext2(err error) error {
	return errorf("libext2fs: %v", err)
}

// strLines is Rust's str::lines: split at \n, with a \r just before a \n
// going with it, and no empty last line for a final \n.
func strLines(text string) []string {
	parts := strings.Split(text, "\n")
	last := len(parts) - 1
	for i := range parts[:last] {
		parts[i] = strings.TrimSuffix(parts[i], "\r")
	}
	if parts[last] == "" {
		parts = parts[:last]
	}
	return parts
}

func isHex(s string) bool {
	for _, b := range []byte(s) {
		if !('0' <= b && b <= '9' || 'a' <= b && b <= 'f' || 'A' <= b && b <= 'F') {
			return false
		}
	}
	return true
}

// parseManifest reads `sha256  path` lines, as image_check.go writes them,
// as (sha, path) pairs.
func parseManifest(text string) [][2]string {
	var out [][2]string
	for _, l := range strLines(text) {
		sha, path, ok := strings.Cut(l, "  ")
		if ok && len(sha) == 64 && isHex(sha) && path != "" {
			out = append(out, [2]string{sha, path})
		}
	}
	return out
}

// savedImage is the firmware image saved on this computer: its RC2FX
// checksums, to tell whether it is what a root was flashed with, and its
// file list.
type savedImage struct {
	path string
	// sums is nil when the image has none.
	sums     []byte
	manifest [][2]string
}

func readSavedImage(path string) (*savedImage, error) {
	saved := &savedImage{path: path}
	err := walkArchive(path, 4<<30, false, func(entry *tarEntry) error {
		if !entry.Header.Regular() {
			return nil
		}
		name := normalized(entry.Path)
		if name == sumsMember {
			data, err := readAll(entry)
			if err != nil {
				return err
			}
			saved.sums = data
		} else if inManifest(name) {
			hashing := archive.HashingReader(entry)
			if _, err := io.Copy(io.Discard, hashing); err != nil {
				return err
			}
			sha, _ := hashing.Finish()
			saved.manifest = append(saved.manifest, [2]string{sha, name})
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return saved, nil
}

// savedMembers is the members of the saved image named in wanted.
func savedMembers(path string, wanted map[string]bool) (map[string][]byte, error) {
	found := map[string][]byte{}
	if len(wanted) == 0 {
		return found, nil
	}
	err := walkArchive(path, 4<<30, false, func(entry *tarEntry) error {
		name := normalized(entry.Path)
		if wanted[name] && entry.Size <= maxRepairFile {
			data, err := readAll(entry)
			if err != nil {
				return err
			}
			found[name] = data
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return found, nil
}

// repairFix is a damaged file and the identical bytes that restore it.
type repairFix struct {
	path string
	want string
	data []byte
}

// suspect is a file whose contents differ: path, expected hash, size.
type suspect struct {
	path string
	want string
	size uint64
}

// manifestOf is where a root's expected hashes come from.
func manifestOf(fs *ext2fs.Filesystem, saved *savedImage) (string, [][2]string, error) {
	// The image's own list, or the saved image when it is the one this root
	// was flashed with.
	_, data, err := readRootFile(fs, manifestMember)
	if err != nil {
		return "", nil, err
	}
	if len(data) > 0 {
		return "the image's file list", parseManifest(strings.ToValidUTF8(string(data), "�")), nil
	}
	entry, sums, err := readRootFile(fs, sumsMember)
	if err != nil {
		return "", nil, err
	}
	if saved != nil && entry != nil && saved.sums != nil && bytes.Equal(saved.sums, sums) {
		return "the firmware image saved on this computer", slices.Clone(saved.manifest), nil
	}
	return "nothing: no file list for this system", nil, nil
}

// repairSources is, for files whose contents don't match, the identical
// bytes that restore each, or why there are none. Sources are the other
// root's copy, then the saved image; a copy counts only if its SHA-256 is
// the expected one and its size is the damaged file's.
func repairSources(other *ext2fs.Filesystem, saved *savedImage, unclean bool, suspects []suspect) ([]repairFix, []DamagedFile, error) {
	found := map[string][]byte{}
	missing := map[string]bool{}
	for _, s := range suspects {
		var entry *ext2fs.Entry
		var data []byte
		if other != nil {
			var err error
			entry, data, err = readRootFile(other, s.path)
			if err != nil {
				return nil, nil, err
			}
		}
		if entry != nil && sum(data) == s.want {
			found[s.path] = data
		} else {
			missing[s.path] = true
		}
	}
	if saved != nil {
		more, err := savedMembers(saved.path, missing)
		if err != nil {
			return nil, nil, err
		}
		for path, data := range more {
			found[path] = data
		}
	}
	var fixes []repairFix
	var damaged []DamagedFile
	why := func(text string) *string { return &text }
	for _, s := range suspects {
		data, ok := found[s.path]
		delete(found, s.path)
		file := DamagedFile{Path: s.path}
		switch {
		case unclean:
			file.Why = why("the mixer wasn't shut down cleanly, so nothing is written to this system; start the mixer normally once, then check again")
		case ok && sum(data) != s.want:
			file.Why = why("no identical copy to restore it from")
		case ok && uint64(len(data)) != s.size:
			file.Why = why("its size changed; a repair only replaces contents of the same size")
		case ok:
			fixes = append(fixes, repairFix{path: s.path, want: s.want, data: data})
		default:
			file.Why = why("no identical copy to restore it from")
		}
		damaged = append(damaged, file)
	}
	return fixes, damaged, nil
}

// compare is one root's manifest compared with its files: the missing or
// irregular ones, and the ones whose contents differ.
func (e *Engine) compare(fs *ext2fs.Filesystem, index int, manifest [][2]string) ([]DamagedFile, []suspect, error) {
	detail := fmt.Sprintf("Checking system partition %d", index)
	var broken []DamagedFile
	var suspects []suspect
	total := uint64(len(manifest))
	for done, m := range manifest {
		want, path := m[0], m[1]
		if e.cancelled() {
			return nil, nil, Error(Cancelled)
		}
		if done%64 == 0 {
			e.reporter("repair-check", detail)(uint64(done), total)
		}
		entry, data, err := readRootFile(fs, path)
		if err != nil {
			return nil, nil, err
		}
		switch {
		case entry == nil:
			why := "it is missing; a repair only restores files that are there"
			broken = append(broken, DamagedFile{Path: path, Why: &why})
		case !entry.Regular:
			why := "it is no longer a regular file"
			broken = append(broken, DamagedFile{Path: path, Why: &why})
		case sum(data) != want:
			suspects = append(suspects, suspect{path: path, want: want, size: entry.Size})
		}
	}
	return broken, suspects, nil
}

// savedImage is the saved image, read only when a root needs it.
func (e *Engine) savedImage() *savedImage {
	archive := filepath.Join(e.Work, "archive.tar.gz")
	if !exists(archive) {
		return nil
	}
	e.step("repair-check", "Reading the firmware image saved on this computer")
	saved, err := readSavedImage(archive)
	if err != nil {
		return nil
	}
	return saved
}

// openOther is the other root, or nil when it doesn't open.
func openOther(device string, start uint64) *ext2fs.Filesystem {
	fs, err := ext2fs.Open(device, start, false)
	if err != nil {
		return nil
	}
	return fs
}

// repairInspect looks the mixer's storage over. Reads only.
func (e *Engine) repairInspect() (*RepairFindings, error) {
	device, err := platform.RecoveryDeviceOf(e.Platform)
	if err != nil {
		return nil, err
	}
	defer device.Close()
	e.step("repair-check", "Reading the mixer's partitions")
	layout, err := layoutOf(device.Path, device.Len)
	if err != nil {
		return nil, err
	}
	saved := e.savedImage()
	var roots []RootFindings
	for index := range 2 {
		root, err := e.inspectRoot(layout, index, saved)
		if err != nil {
			return nil, err
		}
		roots = append(roots, *root)
	}
	return &RepairFindings{
		Hostname:   layout.hostname,
		SettingsOK: layout.settingsOK,
		Roots:      roots,
	}, nil
}

func (e *Engine) inspectRoot(layout *diskLayout, index int, saved *savedImage) (*RootFindings, error) {
	fs, err := ext2fs.Open(layout.device, layout.parts[index].start, false)
	if err != nil {
		return nil, ext2(err)
	}
	defer fs.Close()
	other := openOther(layout.device, layout.parts[1-index].start)
	defer other.Close()
	unclean := fs.NeedsRecovery()
	reference, manifest, err := manifestOf(fs, saved)
	if err != nil {
		return nil, err
	}
	damaged, suspects, err := e.compare(fs, index+1, manifest)
	if err != nil {
		return nil, err
	}
	_, more, err := repairSources(other, saved, unclean, suspects)
	if err != nil {
		return nil, err
	}
	damaged = append(damaged, more...)
	slices.SortStableFunc(damaged, func(a, b DamagedFile) int { return strings.Compare(a.Path, b.Path) })
	return &RootFindings{
		Index:     uint8(index + 1),
		Reference: reference,
		Unclean:   unclean,
		Damaged:   damaged,
	}, nil
}

type repairPlan struct {
	index int
	fixes []repairFix
}

// repair backs up the settings partition, then restores the files the user
// approved from a check. Nothing from the app is trusted: the layout, each
// file's expected hash, its damage and its source are all checked again
// here, for those files only, and nothing else is written.
func (e *Engine) repair(targets []RepairTarget) (*RepairResult, error) {
	device, err := platform.RecoveryDeviceOf(e.Platform)
	if err != nil {
		return nil, err
	}
	defer device.Close()
	e.step("repair-check", "Reading the mixer's partitions")
	layout, err := layoutOf(device.Path, device.Len)
	if err != nil {
		return nil, err
	}
	var saved *savedImage
	var plans []repairPlan
	result := &RepairResult{}
	notRepaired := func(path, why string) {
		result.NotRepaired = append(result.NotRepaired, [2]string{path, why})
	}
	for index := range 2 {
		var wanted []string
		for _, t := range targets {
			if int(t.Root) == index+1 {
				wanted = append(wanted, t.Path)
			}
		}
		if len(wanted) == 0 {
			continue
		}
		fixes, err := e.planRoot(layout, index, wanted, &saved, result, notRepaired)
		if err != nil {
			return nil, err
		}
		plans = append(plans, repairPlan{index, fixes})
	}
	if !slices.ContainsFunc(plans, func(p repairPlan) bool { return len(p.fixes) > 0 }) {
		return result, nil
	}
	backup, err := e.backupSettings(layout)
	if err != nil {
		return nil, err
	}
	result.Backup = backup
	for _, plan := range plans {
		if len(plan.fixes) == 0 {
			continue
		}
		if err := e.writeRoot(layout, plan, result, notRepaired); err != nil {
			return nil, err
		}
	}
	return result, nil
}

// planRoot checks the files asked for on one root again and finds their
// sources, recording what is refused or already intact.
func (e *Engine) planRoot(layout *diskLayout, index int, wanted []string, saved **savedImage, result *RepairResult, notRepaired func(string, string)) ([]repairFix, error) {
	fs, err := ext2fs.Open(layout.device, layout.parts[index].start, false)
	if err != nil {
		return nil, ext2(err)
	}
	defer fs.Close()
	other := openOther(layout.device, layout.parts[1-index].start)
	defer other.Close()
	entry, _, err := readRootFile(fs, manifestMember)
	if err != nil {
		return nil, err
	}
	if entry == nil && *saved == nil {
		*saved = e.savedImage()
	}
	_, manifest, err := manifestOf(fs, *saved)
	if err != nil {
		return nil, err
	}
	expected := map[string]string{}
	for _, m := range manifest {
		expected[m[1]] = m[0]
	}
	var listed [][2]string
	for _, path := range wanted {
		if want, ok := expected[path]; ok {
			listed = append(listed, [2]string{want, path})
		} else {
			notRepaired(path, "it is not in this system's file list")
		}
	}
	broken, suspects, err := e.compare(fs, index+1, listed)
	if err != nil {
		return nil, err
	}
	for _, file := range broken {
		why := ""
		if file.Why != nil {
			why = *file.Why
		}
		notRepaired(file.Path, why)
	}
	// Asked for, but intact now: nothing to do.
	for _, l := range listed {
		path := l[1]
		if !slices.ContainsFunc(suspects, func(s suspect) bool { return s.path == path }) &&
			!slices.ContainsFunc(result.NotRepaired, func(n [2]string) bool { return n[0] == path }) {
			result.Repaired = append(result.Repaired, path)
		}
	}
	fixes, damaged, err := repairSources(other, *saved, fs.NeedsRecovery(), suspects)
	if err != nil {
		return nil, err
	}
	for _, file := range damaged {
		if file.Why != nil {
			notRepaired(file.Path, *file.Why)
		}
	}
	return fixes, nil
}

// writeRoot restores one root's files, then reads each back from the disk.
func (e *Engine) writeRoot(layout *diskLayout, plan repairPlan, result *RepairResult, notRepaired func(string, string)) error {
	start := layout.parts[plan.index].start
	e.step("repair-write", fmt.Sprintf("Restoring files on system partition %d", plan.index+1))
	fs, err := ext2fs.Open(layout.device, start, true)
	if err != nil {
		return ext2(err)
	}
	if fs.NeedsRecovery() {
		fs.Close()
		return Error("the mixer's system was not shut down cleanly; nothing was written")
	}
	for _, fix := range plan.fixes {
		entry, err := fs.Lookup(fix.path)
		if err != nil {
			fs.Close()
			return ext2(err)
		}
		if entry == nil {
			continue
		}
		if err := fs.Overwrite(*entry, fix.data); err != nil {
			fs.Close()
			return ext2(err)
		}
	}
	if err := fs.Close(); err != nil {
		return ext2(err)
	}
	// Read everything back from the disk.
	fs, err = ext2fs.Open(layout.device, start, false)
	if err != nil {
		return ext2(err)
	}
	defer fs.Close()
	for _, fix := range plan.fixes {
		entry, data, err := readRootFile(fs, fix.path)
		if err != nil {
			return err
		}
		if entry != nil && sum(data) == fix.want {
			result.Repaired = append(result.Repaired, fix.path)
		} else {
			notRepaired(fix.path, "the restored copy didn't read back identical")
		}
	}
	return nil
}

// backupSettings is a copy of the settings partition (the mixer's setup,
// shows, Wi-Fi and CallMe pairing, serial) in the work folder: private to
// the user who started the installer, even when the helper writes it as
// root.
func (e *Engine) backupSettings(layout *diskLayout) (string, error) {
	part := layout.parts[2]
	dir := filepath.Join(e.Work, "repair")
	if err := privateDirs(dir); err != nil {
		return "", err
	}
	name := fmt.Sprintf("settings-partition-%d.img", nowUnix())
	path := filepath.Join(dir, name)
	disk, err := os.Open(layout.device)
	if err != nil {
		return "", err
	}
	defer disk.Close()
	if _, err := disk.Seek(int64(part.start), io.SeekStart); err != nil {
		return "", err
	}
	t, err := archive.NewTemp(dir, ".settings-")
	if err != nil {
		return "", err
	}
	defer t.Cleanup()
	out := archive.HashingWriter(t)
	buf := make([]byte, 1<<20)
	var done uint64
	for done < part.len {
		if e.cancelled() {
			return "", Error(Cancelled)
		}
		n := uint64(len(buf))
		if part.len-done < n {
			n = part.len - done
		}
		if _, err := io.ReadFull(disk, buf[:n]); err != nil {
			return "", err
		}
		if _, err := out.Write(buf[:n]); err != nil {
			return "", err
		}
		done += n
		e.reporter("repair-backup", "Copying the settings partition to this computer")(done, part.len)
	}
	sha, _ := out.Finish()
	if err := t.Persist(path); err != nil {
		return "", err
	}
	sumFile, err := archive.NewTemp(dir, ".settings-")
	if err != nil {
		return "", err
	}
	defer sumFile.Cleanup()
	if _, err := sumFile.Write([]byte(sha + "  " + name + "\n")); err != nil {
		return "", err
	}
	if err := sumFile.Persist(path + ".sha256"); err != nil {
		return "", err
	}
	return path, nil
}
