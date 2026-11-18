//go:build linux && cgo

package installer

// A small fake mixer disk built from real parts: an MBR with RØDE's four
// partitions, two ext4 roots made by mkfs.ext4 with a Pro II's features, and
// a FAT settings partition made by mkfs.vfat. Skipped where those tools are
// missing; CI's Ubuntu has them.

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/ext2fs"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

var repairScript = []byte("#!/bin/sh\nmount -t vfat /dev/mmcblk1p3 /Application/update-staging\n")

const mib = 1 << 20

// repairParts is the partition starts and sizes in MiB.
var repairParts = [4][2]uint64{{1, 8}, {9, 8}, {17, 34}, {51, 1}}

func systemTool(name string) string {
	for _, dir := range []string{"/usr/sbin", "/sbin", "/usr/bin", "/bin"} {
		path := filepath.Join(dir, name)
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

type fakeDiskImage struct {
	t    *testing.T
	dir  string
	path string
}

type tree struct {
	hostname string
	manifest bool
}

var proII = tree{hostname: "rodecaster-pro-ii", manifest: true}

func bigFile() []byte {
	big := make([]byte, 200_000)
	for n := range big {
		big[n] = byte(uint32(n) * 13 % 251)
	}
	return big
}

func rootManifest(hostname string) string {
	return line(sum([]byte(hostname+"\n")), "etc/hostname") +
		line(sum(repairScript), "usr/bin/rode-init-update-env.sh") +
		line(sum(bigFile()), "usr/bin/big")
}

func writeAt(t *testing.T, path string, offset uint64, data []byte) {
	t.Helper()
	file, err := os.OpenFile(path, os.O_WRONLY, 0)
	must(t, err)
	defer file.Close()
	_, err = file.WriteAt(data, int64(offset))
	must(t, err)
}

func newDisk(t *testing.T, trees [2]tree) *fakeDiskImage {
	t.Helper()
	mkfs, vfat := systemTool("mkfs.ext4"), systemTool("mkfs.vfat")
	if mkfs == "" || vfat == "" {
		t.Skip("skipped: mkfs.ext4 or mkfs.vfat is not installed")
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "disk.img")
	disk, err := os.Create(path)
	must(t, err)
	must(t, disk.Truncate(52*mib))
	must(t, disk.Close())
	var sector0 [512]byte
	for i, p := range repairParts {
		entry := sector0[446+16*i : 462+16*i]
		entry[4] = 0x0c
		if i < 2 {
			entry[4] = 0x83
		}
		binary.LittleEndian.PutUint32(entry[8:12], uint32(p[0]*mib/repairSector))
		binary.LittleEndian.PutUint32(entry[12:16], uint32(p[1]*mib/repairSector))
	}
	sector0[510], sector0[511] = 0x55, 0xAA
	writeAt(t, path, 0, sector0[:])
	for i, tr := range trees {
		root := filepath.Join(dir, fmt.Sprintf("tree%d", i))
		for _, sub := range []string{"usr/bin", "etc", "opt/rc2fx"} {
			must(t, os.MkdirAll(filepath.Join(root, sub), 0o755))
		}
		put(t, filepath.Join(root, "etc/hostname"), []byte(tr.hostname+"\n"))
		put(t, filepath.Join(root, "usr/bin/rode-init-update-env.sh"), repairScript)
		put(t, filepath.Join(root, "usr/bin/big"), bigFile())
		put(t, filepath.Join(root, "opt/rc2fx/SHA256SUMS"), []byte("aa  rc2fxd\n"))
		if tr.manifest {
			put(t, filepath.Join(root, manifestMember), []byte(rootManifest(tr.hostname)))
		}
		image := filepath.Join(dir, fmt.Sprintf("root%d.img", i))
		block := "4096"
		if i == 0 {
			block = "1024"
		}
		out, err := exec.Command(mkfs, "-F", "-q", "-t", "ext4", "-b", block, "-I", "256", "-O",
			"metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
			"-d", root, image, fmt.Sprintf("%dM", repairParts[i][1])).CombinedOutput()
		if err != nil {
			t.Fatalf("mkfs.ext4: %v\n%s", err, out)
		}
		data, err := os.ReadFile(image)
		must(t, err)
		writeAt(t, path, repairParts[i][0]*mib, data)
	}
	fat := filepath.Join(dir, "fat.img")
	if out, err := exec.Command(vfat, "-F", "32", "-C", fat, fmt.Sprint(repairParts[2][1]*1024)).CombinedOutput(); err != nil {
		t.Fatalf("mkfs.vfat: %v\n%s", err, out)
	}
	data, err := os.ReadFile(fat)
	must(t, err)
	writeAt(t, path, repairParts[2][0]*mib, data)
	return &fakeDiskImage{t: t, dir: dir, path: path}
}

func (d *fakeDiskImage) engine() *Engine {
	work := filepath.Join(d.dir, "work")
	must(d.t, os.MkdirAll(work, 0o755))
	return WithPlatform(d.dir, work, fakeDisk{d.path})
}

func (d *fakeDiskImage) root(index int, writable bool) *ext2fs.Filesystem {
	fs, err := ext2fs.Open(d.path, repairParts[index][0]*mib, writable)
	must(d.t, err)
	return fs
}

// zero is what the bad flash did: the right size, no contents.
func (d *fakeDiskImage) zero(index int, path string) {
	fs := d.root(index, true)
	entry, err := fs.Lookup(path)
	must(d.t, err)
	must(d.t, fs.Overwrite(*entry, make([]byte, entry.Size)))
	must(d.t, fs.Close())
}

// zeroFrom zeroes a file from byte keep on, keeping its size.
func (d *fakeDiskImage) zeroFrom(index int, path string, keep int) {
	data := d.contents(index, path)
	clear(data[keep:])
	fs := d.root(index, true)
	entry, err := fs.Lookup(path)
	must(d.t, err)
	must(d.t, fs.Overwrite(*entry, data))
	must(d.t, fs.Close())
}

// saveFlashedImage saves the image that was flashed on this computer: the
// same RC2FX checksums and an intact script.
func (d *fakeDiskImage) saveFlashedImage() {
	t := d.t
	archive := filepath.Join(d.dir, "work/archive.tar.gz")
	must(t, os.MkdirAll(filepath.Dir(archive), 0o755))
	var items []tarItem
	for _, m := range []struct {
		name string
		data []byte
	}{
		{"./etc/hostname", []byte("rodecaster-pro-ii\n")},
		{"./usr/bin/rode-init-update-env.sh", repairScript},
		{"./opt/rc2fx/SHA256SUMS", []byte("aa  rc2fxd\n")},
	} {
		items = append(items, tarItem{testHeader(t, m.name, typeRegular, len(m.data)), m.data})
	}
	testTar(t, archive, items)
}

func (d *fakeDiskImage) contents(index int, path string) []byte {
	fs := d.root(index, false)
	defer fs.Close()
	entry, err := fs.Lookup(path)
	must(d.t, err)
	data, err := fs.Read(*entry)
	must(d.t, err)
	return data
}

// clean runs e2fsck -fn on one root, copied out of the disk.
func (d *fakeDiskImage) clean(index int) {
	e2fsck := systemTool("e2fsck")
	if e2fsck == "" {
		return
	}
	whole, err := os.ReadFile(d.path)
	must(d.t, err)
	start, length := repairParts[index][0]*mib, repairParts[index][1]*mib
	image := filepath.Join(d.dir, "check.img")
	must(d.t, os.WriteFile(image, whole[start:start+length], 0o600))
	if out, err := exec.Command(e2fsck, "-f", "-n", image).CombinedOutput(); err != nil {
		d.t.Fatalf("e2fsck: %v\n%s", err, out)
	}
}

func target(root uint8, path string) RepairTarget {
	return RepairTarget{Root: root, Path: path}
}

// fakeDisk is a Platform whose recovery disk is an image file.
type fakeDisk struct{ path string }

func (fakeDisk) Inventory() ([]HID, error)          { return nil, nil }
func (fakeDisk) Open(*HID) (Reports, error)         { return nil, Error("no HID") }
func (fakeDisk) OpenControl(*HID) (Reports, error)  { return nil, Error("no HID") }
func (fakeDisk) Volumes() ([]Volume, error)         { return nil, nil }
func (fakeDisk) Mount(Volume) (Volume, error)       { return Volume{}, Error("no volumes") }
func (fakeDisk) MediaPresent(*Volume) (bool, error) { return false, nil }
func (fakeDisk) Eject(*Volume) error                { return nil }

func (d fakeDisk) RecoveryDisk() (*uint64, error) {
	info, err := os.Stat(d.path)
	if err != nil {
		return nil, err
	}
	size := uint64(info.Size())
	return &size, nil
}

func (d fakeDisk) RecoveryDevice() (*platform.RecoveryDevice, error) {
	info, err := os.Stat(d.path)
	if err != nil {
		return nil, err
	}
	return &platform.RecoveryDevice{Path: d.path, Len: uint64(info.Size())}, nil
}

const initScript = "usr/bin/rode-init-update-env.sh"

func whyContains(file DamagedFile, want string) bool {
	return file.Why != nil && strings.Contains(*file.Why, want)
}

func TestAHealthyMixerHasNothingToRepair(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	findings, err := disk.engine().repairInspect()
	must(t, err)
	if findings.Hostname != "rodecaster-pro-ii" || !findings.SettingsOK || len(findings.Roots) != 2 {
		t.Fatalf("%+v", findings)
	}
	for _, root := range findings.Roots {
		if root.Reference != "the image's file list" {
			t.Fatalf("reference %q", root.Reference)
		}
		if len(root.Damaged) != 0 || root.Unclean {
			t.Fatalf("%+v", root)
		}
	}
	if findings.Repairable() != 0 {
		t.Fatal("nothing is repairable")
	}
}

func TestAZeroFilledScriptIsRestoredFromTheOtherRootAfterASettingsBackup(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	disk.zero(0, initScript)
	engine := disk.engine()
	findings, err := engine.repairInspect()
	must(t, err)
	if d := findings.Roots[0].Damaged; len(d) != 1 || d[0].Path != initScript || d[0].Why != nil {
		t.Fatalf("%+v", d)
	}
	if len(findings.Roots[1].Damaged) != 0 {
		t.Fatalf("%+v", findings.Roots[1])
	}
	if findings.Repairable() != 1 {
		t.Fatalf("repairable %d", findings.Repairable())
	}
	result, err := engine.repair(findings.Targets())
	must(t, err)
	if !slices.Equal(result.Repaired, []string{initScript}) || len(result.NotRepaired) != 0 {
		t.Fatalf("%+v", result)
	}
	if !bytes.Equal(disk.contents(0, initScript), repairScript) {
		t.Fatal("the script was not restored")
	}
	disk.clean(0)
	// The backup is the settings partition, byte for byte.
	backup, err := os.ReadFile(result.Backup)
	must(t, err)
	whole, err := os.ReadFile(disk.path)
	must(t, err)
	start := repairParts[2][0] * mib
	if !bytes.Equal(backup, whole[start:start+repairParts[2][1]*mib]) {
		t.Fatal("the backup is not the settings partition")
	}
	again, err := engine.repairInspect()
	must(t, err)
	if len(again.Roots[0].Damaged) != 0 {
		t.Fatalf("%+v", again.Roots[0])
	}
}

func TestWithoutAnIdenticalCopyNothingIsWritten(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	disk.zero(0, initScript)
	disk.zero(1, initScript)
	engine := disk.engine()
	findings, err := engine.repairInspect()
	must(t, err)
	if findings.Repairable() != 0 || !whyContains(findings.Roots[0].Damaged[0], "no identical copy") {
		t.Fatalf("%+v", findings)
	}
	if len(findings.Targets()) != 0 {
		t.Fatal("no targets")
	}
	// Asked anyway, the helper finds no copy either and writes nothing.
	result, err := engine.repair([]RepairTarget{target(1, initScript), target(2, initScript)})
	must(t, err)
	if len(result.Repaired) != 0 || len(result.NotRepaired) != 2 {
		t.Fatalf("%+v", result)
	}
	if result.Backup != "" {
		t.Fatal("no backup when nothing is written")
	}
	if !bytes.Equal(disk.contents(0, initScript), make([]byte, len(repairScript))) {
		t.Fatal("the script was written")
	}
}

func TestAnUncleanlyStoppedSystemIsLeftAlone(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	debugfs := systemTool("debugfs")
	if debugfs == "" {
		t.Skip("skipped: debugfs is not installed")
	}
	disk.zero(0, initScript)
	// Mark root 1's journal as not replayed, in a copy written back.
	whole, err := os.ReadFile(disk.path)
	must(t, err)
	start, length := repairParts[0][0]*mib, repairParts[0][1]*mib
	image := filepath.Join(disk.dir, "p1.img")
	must(t, os.WriteFile(image, whole[start:start+length], 0o600))
	if out, err := exec.Command(debugfs, "-w", "-R", "feature needs_recovery", image).CombinedOutput(); err != nil {
		t.Fatalf("debugfs: %v\n%s", err, out)
	}
	data, err := os.ReadFile(image)
	must(t, err)
	writeAt(t, disk.path, start, data)
	engine := disk.engine()
	findings, err := engine.repairInspect()
	must(t, err)
	if !findings.Roots[0].Unclean || !whyContains(findings.Roots[0].Damaged[0], "shut down cleanly") {
		t.Fatalf("%+v", findings.Roots[0])
	}
	if findings.Repairable() != 0 {
		t.Fatal("nothing is repairable")
	}
	result, err := engine.repair([]RepairTarget{target(1, initScript)})
	must(t, err)
	if len(result.NotRepaired) == 0 || !strings.Contains(result.NotRepaired[0][1], "shut down cleanly") {
		t.Fatalf("%+v", result)
	}
	if !bytes.Equal(disk.contents(0, initScript), make([]byte, len(repairScript))) {
		t.Fatal("the script was written")
	}
}

func TestADiskThatIsNotARodecasterIsRefusedBeforeAnythingIsWritten(t *testing.T) {
	other := tree{hostname: "some-laptop", manifest: true}
	disk := newDisk(t, [2]tree{other, other})
	_, err := disk.engine().repair([]RepairTarget{target(1, initScript)})
	contains(t, err, "RØDECaster")
	if exists(filepath.Join(disk.dir, "work/repair")) {
		t.Fatal("no backup either")
	}
	// A disk without a partition table.
	writeAt(t, disk.path, 510, []byte{0, 0})
	if _, err := disk.engine().repairInspect(); err == nil {
		t.Fatal("a disk without a partition table was accepted")
	}
}

func TestARootWithoutAFileListIsComparedWithTheFlashedImageSavedHere(t *testing.T) {
	old := tree{hostname: "rodecaster-pro-ii", manifest: false}
	disk := newDisk(t, [2]tree{old, old})
	disk.zero(0, initScript)
	disk.zero(1, initScript)
	// Without the saved image there is nothing to compare with.
	findings, err := disk.engine().repairInspect()
	must(t, err)
	if !strings.HasPrefix(findings.Roots[0].Reference, "nothing") || len(findings.Roots[0].Damaged) != 0 {
		t.Fatalf("%+v", findings.Roots[0])
	}
	disk.saveFlashedImage()
	engine := disk.engine()
	findings, err = engine.repairInspect()
	must(t, err)
	if findings.Roots[0].Reference != "the firmware image saved on this computer" {
		t.Fatalf("reference %q", findings.Roots[0].Reference)
	}
	if findings.Repairable() != 2 {
		t.Fatalf("repairable %d", findings.Repairable())
	}
	result, err := engine.repair(findings.Targets())
	must(t, err)
	if len(result.Repaired) != 2 {
		t.Fatalf("%+v", result)
	}
	for i := range 2 {
		if !bytes.Equal(disk.contents(i, initScript), repairScript) {
			t.Fatalf("root %d not restored", i+1)
		}
	}
	disk.clean(0)
	disk.clean(1)
}

func TestOnlyTheFilesAskedForAreRestoredAndEachIsCheckedAgain(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	disk.zero(0, initScript)
	disk.zero(0, "usr/bin/big")
	result, err := disk.engine().repair([]RepairTarget{
		target(1, initScript),
		// Intact: nothing to write.
		target(2, initScript),
		// Not in the file list: refused, whatever the app asks.
		target(1, "etc/shadow"),
	})
	must(t, err)
	if !bytes.Equal(disk.contents(0, initScript), repairScript) {
		t.Fatal("the script was not restored")
	}
	if !bytes.Equal(disk.contents(0, "usr/bin/big"), make([]byte, len(bigFile()))) {
		t.Fatal("not asked for")
	}
	if !slices.Contains(result.Repaired, initScript) {
		t.Fatalf("%+v", result)
	}
	if !slices.ContainsFunc(result.NotRepaired, func(n [2]string) bool {
		return n[0] == "etc/shadow" && strings.Contains(n[1], "not in this system's file list")
	}) {
		t.Fatalf("%+v", result.NotRepaired)
	}
	disk.clean(0)
	// The backup belongs to whoever runs the installer and only to them.
	info, err := os.Stat(result.Backup)
	must(t, err)
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("backup mode %o", info.Mode().Perm())
	}
}

func TestADamagedFileListIsNotUsed(t *testing.T) {
	good := rootManifest("rodecaster-pro-ii")
	if list, ok := parseManifest(good); !ok || len(list) != 3 {
		t.Fatalf("%v %v", list, ok)
	}
	half := []byte(good)
	clear(half[len(half)/2:])
	// Cut inside the second path: its hash is intact, its path is not.
	cut := []byte(good)
	clear(cut[strings.Index(good, "\n")+1+70:])
	for name, text := range map[string]string{
		"zero-filled": string(make([]byte, len(good))),
		"half-zeroed": string(half),
		"cut in path": string(cut),
		"empty":       "",
		"garbage":     "not a list\n",
	} {
		if list, ok := parseManifest(text); ok {
			t.Fatalf("%s: used %v", name, list)
		}
	}
}

// damagedList is the finding for a root's own damaged file list, if any.
func damagedList(root RootFindings) *DamagedFile {
	for i, file := range root.Damaged {
		if file.Path == manifestMember {
			return &root.Damaged[i]
		}
	}
	return nil
}

func TestAZeroFilledFileListIsAFindingNotAHealthySystem(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	disk.zero(0, manifestMember)
	disk.zero(0, initScript)
	// Without the saved image nothing can be checked, and that is said.
	findings, err := disk.engine().repairInspect()
	must(t, err)
	root := findings.Roots[0]
	if root.Reference != "nothing: this system's file list is damaged" {
		t.Fatalf("reference %q", root.Reference)
	}
	if list := damagedList(root); list == nil || !whyContains(*list, "can't be checked") {
		t.Fatalf("%+v", root)
	}
	if findings.Repairable() != 0 || len(findings.Roots[1].Damaged) != 0 {
		t.Fatalf("%+v", findings)
	}
	// With the image that was flashed, the files are checked against it.
	disk.saveFlashedImage()
	engine := disk.engine()
	findings, err = engine.repairInspect()
	must(t, err)
	root = findings.Roots[0]
	if root.Reference != "the firmware image saved on this computer" {
		t.Fatalf("reference %q", root.Reference)
	}
	if list := damagedList(root); list == nil || !whyContains(*list, "compared with the firmware image") {
		t.Fatalf("%+v", root)
	}
	targets := findings.Targets()
	if len(targets) != 1 || targets[0] != target(1, initScript) {
		t.Fatalf("targets %+v", targets)
	}
	result, err := engine.repair(targets)
	must(t, err)
	if !slices.Equal(result.Repaired, []string{initScript}) {
		t.Fatalf("%+v", result)
	}
	if !bytes.Equal(disk.contents(0, initScript), repairScript) {
		t.Fatal("root 1 not restored")
	}
	disk.clean(0)
}

func TestAHalfZeroedFileListIsAFinding(t *testing.T) {
	disk := newDisk(t, [2]tree{proII, proII})
	disk.zeroFrom(0, manifestMember, len(rootManifest("rodecaster-pro-ii"))/2)
	findings, err := disk.engine().repairInspect()
	must(t, err)
	root := findings.Roots[0]
	if !strings.HasPrefix(root.Reference, "nothing") || damagedList(root) == nil {
		t.Fatalf("a root with a half-zeroed file list was reported as %+v", root)
	}
}
