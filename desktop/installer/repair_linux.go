//go:build linux

package installer

// Host-side eMMC repair for mixers booted into bootloader USB mass-storage mode
// (holding power ~15 s at boot exposes eMMC via `ums mmc 0`, Amlogic 1b8e:fada).
//
// The eMMC has four partitions: two alternating ext4 root slots, a FAT settings
// partition, and a FAT data partition. If a flash leaves a file zero-filled or
// corrupted on a root slot, repair compares files against the image's manifest
// (or the locally cached archive if it matches), backs up the settings partition,
// and overwrites damaged regular files in place with identical-length copies from
// the other slot or the cached archive via libext2fs.

import (
	"bytes"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/ext2fs"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/fix"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

// RepairSupported controls whether the UI exposes host-side eMMC repair.
const RepairSupported = true

const repairSector = disk.SectorSize

var mixerHostnames = [2]string{"rodecaster-pro-ii", "rodecaster-pro-ii-mini"}

const sumsMember = "opt/rc2fx/SHA256SUMS"

const maxRepairFile = 256 << 20

type repairPart struct {
	start, len uint64
}

func partitions(sector0 *[512]byte, diskLen uint64) ([4]repairPart, error) {
	var parts [4]repairPart
	parsed, err := fix.ParseMBR(sector0[:], diskLen/repairSector)
	if err != nil {
		return parts, Error(err.Error())
	}
	for i, p := range parsed {
		parts[i] = repairPart{
			start: uint64(p.Start) * repairSector,
			len:   uint64(p.Sectors) * repairSector,
		}
	}
	return parts, nil
}

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

// parseManifest parses "sha256  path" lines from the image manifest. Returns
// ok=false if empty or any line is malformed/zeroed, so a truncated manifest
// won't silently hide missing entries.
func parseManifest(text string) ([][2]string, bool) {
	var out [][2]string
	for _, l := range common.Lines(text) {
		if l == "" {
			continue
		}
		sha, path, ok := strings.Cut(l, "  ")
		if !ok || len(sha) != 64 || !common.IsHex(sha) || path == "" || strings.Contains(path, "\x00") {
			return nil, false
		}
		out = append(out, [2]string{sha, path})
	}
	return out, len(out) > 0
}

type savedImage struct {
	path     string
	sums     []byte
	manifest [][2]string
}

func readSavedImage(path string) (*savedImage, error) {
	saved := &savedImage{path: path}
	err := archive.Walk(path, 4<<30, false, func(entry *archive.Member) error {
		if !entry.Header.Regular() {
			return nil
		}
		name := archive.Normalized(entry.Path)
		if name == sumsMember {
			data, err := archive.ReadAll(entry)
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

func savedMembers(path string, wanted map[string]bool) (map[string][]byte, error) {
	found := map[string][]byte{}
	if len(wanted) == 0 {
		return found, nil
	}
	err := archive.Walk(path, 4<<30, false, func(entry *archive.Member) error {
		name := archive.Normalized(entry.Path)
		if wanted[name] && entry.Size <= maxRepairFile {
			data, err := archive.ReadAll(entry)
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

type repairFix struct {
	path string
	want string
	data []byte
}

type suspect struct {
	path string
	want string
	size uint64
}

// manifestOf resolves a root's expected file hashes from its own manifest or,
// failing that, from a matching local archive. damaged is non-nil when the
// root's manifest exists but is unreadable.
func manifestOf(fs *ext2fs.Filesystem, saved func() *savedImage) (reference string, manifest [][2]string, damaged *string, err error) {
	list, data, err := readRootFile(fs, manifestMember)
	if err != nil {
		return "", nil, nil, err
	}
	if len(data) > 0 {
		if own, ok := parseManifest(strings.ToValidUTF8(string(data), "�")); ok {
			return "the image's file list", own, nil, nil
		}
	}
	entry, sums, err := readRootFile(fs, sumsMember)
	if err != nil {
		return "", nil, nil, err
	}
	// Present but empty/corrupt means damaged; absent means an older image.
	why := func(text string) *string {
		if list == nil {
			return nil
		}
		return &text
	}
	if image := saved(); image != nil && entry != nil && image.sums != nil && bytes.Equal(image.sums, sums) {
		return "the firmware image saved on this computer", slices.Clone(image.manifest),
			why("this system's files were compared with the firmware image saved on this computer instead"), nil
	}
	if list != nil {
		return "nothing: this system's file list is damaged", nil,
			why("this system's files can't be checked without it; flashing the mixer again replaces it"), nil
	}
	return "nothing: no file list for this system", nil, nil, nil
}

// repairSources locates verified, same-size replacement bytes for suspects
// from the other root slot first, then the local archive.
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
		if entry != nil && common.SHA256Hex(data) == s.want {
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
		case ok && common.SHA256Hex(data) != s.want:
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

func (e *Engine) compare(fs *ext2fs.Filesystem, index int, manifest [][2]string) ([]DamagedFile, []suspect, error) {
	detail := fmt.Sprintf("Checking system partition %d", index)
	var broken []DamagedFile
	var suspects []suspect
	total := uint64(len(manifest))
	for done, m := range manifest {
		want, path := m[0], m[1]
		if e.canceled() {
			return nil, nil, Error(Canceled)
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
		case common.SHA256Hex(data) != want:
			suspects = append(suspects, suspect{path: path, want: want, size: entry.Size})
		}
	}
	return broken, suspects, nil
}

func (e *Engine) savedImage() *savedImage {
	archive := filepath.Join(e.Work, "archive.tar.gz")
	if !common.PathExists(archive) {
		return nil
	}
	e.step("repair-check", "Reading the firmware image saved on this computer")
	saved, err := readSavedImage(archive)
	if err != nil {
		return nil
	}
	return saved
}

func openOther(device string, start uint64) *ext2fs.Filesystem {
	fs, err := ext2fs.Open(device, start, false)
	if err != nil {
		return nil
	}
	return fs
}

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
	reference, manifest, listDamaged, err := manifestOf(fs, func() *savedImage { return saved })
	if err != nil {
		return nil, err
	}
	damaged, suspects, err := e.compare(fs, index+1, manifest)
	if err != nil {
		return nil, err
	}
	if listDamaged != nil {
		damaged = append(damaged, DamagedFile{Path: manifestMember, Why: listDamaged})
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

// repair re-verifies layout, expected hashes, and replacement sources for the
// requested targets, backs up the settings partition, and restores only those
// files in place.
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

func (e *Engine) planRoot(layout *diskLayout, index int, wanted []string, saved **savedImage, result *RepairResult, notRepaired func(string, string)) ([]repairFix, error) {
	fs, err := ext2fs.Open(layout.device, layout.parts[index].start, false)
	if err != nil {
		return nil, ext2(err)
	}
	defer fs.Close()
	other := openOther(layout.device, layout.parts[1-index].start)
	defer other.Close()
	_, manifest, _, err := manifestOf(fs, func() *savedImage {
		if *saved == nil {
			*saved = e.savedImage()
		}
		return *saved
	})
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

// writeRoot overwrites damaged files in place and reads each back to verify.
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
		if entry != nil && common.SHA256Hex(data) == fix.want {
			result.Repaired = append(result.Repaired, fix.path)
		} else {
			notRepaired(fix.path, "the restored copy didn't read back identical")
		}
	}
	return nil
}

// backupSettings copies the FAT settings partition into work/repair, owned by
// the invoking desktop user even when run via the root helper.
func (e *Engine) backupSettings(layout *diskLayout) (string, error) {
	part := layout.parts[2]
	dir := filepath.Join(e.Work, "repair")
	if err := privateDirs(dir); err != nil {
		return "", err
	}
	name := fmt.Sprintf("settings-partition-%d.img", common.NowUnix())
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
		if e.canceled() {
			return "", Error(Canceled)
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
