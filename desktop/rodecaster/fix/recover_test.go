package fix

import (
	"bytes"
	"encoding/binary"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

// fsSpec is the ext4 filesystem on partition 1: its size and block size.
type fsSpec struct {
	sectors  uint64
	logBlock uint32 // 0: 1 KiB blocks, 2: 4 KiB blocks
	bit64    bool
}

var (
	// factoryFS is what RØDE's updater makes on the factory partition 1.
	factoryFS = fsSpec{sectors: 1039458, logBlock: 0}
	// grownFS is what it makes on the grown one, as a 64-bit filesystem.
	grownFS = fsSpec{sectors: 1193984, logBlock: 2, bit64: true}
)

// ext4Superblock is the first sector of a superblock with just the fields
// rcfix reads.
func ext4Superblock(fs fsSpec) []byte {
	b := make([]byte, 512)
	blocks := fs.sectors * 512 / (1024 << fs.logBlock)
	binary.LittleEndian.PutUint32(b[4:], uint32(blocks))
	binary.LittleEndian.PutUint32(b[24:], fs.logBlock)
	b[56], b[57] = 0x53, 0xEF
	if fs.bit64 {
		binary.LittleEndian.PutUint32(b[0x60:], 0x80)
		binary.LittleEndian.PutUint32(b[0x150:], uint32(blocks>>32))
	}
	return b
}

// fatBootSector is a FAT boot sector with just the fields rcfix reads; at
// is 54 (FAT12/16) or 82 (FAT32).
func fatBootSector(at int) []byte {
	b := make([]byte, 512)
	copy(b[at:], "FAT32   ")
	b[510], b[511] = 0x55, 0xAA
	return b
}

// Each piece of evidence, by its index in the report.
const (
	evBoot = iota
	evP1
	evP2
	evP3
	evP4
)

// addEvidenceExcept puts everything that makes the fake disk a RØDECaster
// on it, except the items named.
func addEvidenceExcept(fs fsSpec, skip ...int) func(f *fakeDisk) {
	return func(f *fakeDisk) {
		has := func(i int) bool {
			for _, s := range skip {
				if s == i {
					return false
				}
			}
			return true
		}
		if has(evBoot) {
			b := make([]byte, 512)
			copy(b[0x10:], "@AML")
			f.put(1, b)
		}
		if has(evP1) {
			f.put(8192+2, ext4Superblock(fs))
		}
		if has(evP2) {
			f.put(2000896+2, ext4Superblock(fsSpec{sectors: 1193984, logBlock: 2}))
		}
		if has(evP3) {
			f.put(3223552, fatBootSector(54))
		}
		if has(evP4) {
			f.put(4941824, fatBootSector(82))
		}
	}
}

func addEvidence(fs fsSpec) func(f *fakeDisk) { return addEvidenceExcept(fs) }

func mixer(sector0 []byte, setup func(f *fakeDisk)) *fakeDisk {
	f := newFake(sector0)
	if setup != nil {
		setup(f)
	}
	return f
}

// rebuiltFromNothing is what a sector 0 with no usable bytes becomes.
func rebuiltFromNothing(entry1 [16]byte) []byte {
	b := make([]byte, 512)
	copy(b[440:444], []byte{0x24, 0x93, 0x3a, 0xed})
	copy(b[446:462], entry1[:])
	starts := [3][2]uint32{{2000896, 1193984}, {3223552, 1710080}, {4941824, testSectors - 4941824}}
	for i, e := range starts {
		o := 446 + 16*(i+1)
		copy(b[o+1:o+4], []byte{0xfe, 0xff, 0xff})
		b[o+4] = 0x83
		copy(b[o+5:o+8], []byte{0xfe, 0xff, 0xff})
		binary.LittleEndian.PutUint32(b[o+8:], e[0])
		binary.LittleEndian.PutUint32(b[o+12:], e[1])
	}
	b[510], b[511] = 0x55, 0xAA
	return b
}

func with(b []byte, f func(b []byte)) []byte { f(b); return b }

// damageCases are damaged sector 0s on a mixer, each with what the rebuild
// must be and the verdict the rebuilt table has.
func damageCases() []struct {
	name    string
	sector0 []byte
	fs      fsSpec
	want    []byte
	after   report.Verdict
} {
	zero := make([]byte, 512)
	ff := bytes.Repeat([]byte{0xff}, 512)
	return []struct {
		name    string
		sector0 []byte
		fs      fsSpec
		want    []byte
		after   report.Verdict
	}{
		{"zeroed, 1K", zero, factoryFS, rebuiltFromNothing(hwBefore), report.NeedsFix},
		{"zeroed, 4K", zero, grownFS, rebuiltFromNothing(hwAfter), report.Fixed},
		{"0xFF, 1K", ff, factoryFS, rebuiltFromNothing(hwBefore), report.NeedsFix},
		{"0xFF, 4K", ff, grownFS, rebuiltFromNothing(hwAfter), report.Fixed},
		{"signature lost, 1K", with(factory(), func(b []byte) { b[510], b[511] = 0, 0 }), factoryFS, factory(), report.NeedsFix},
		{"signature lost, 4K", with(fixedTable(), func(b []byte) { b[510], b[511] = 0, 0 }), grownFS, fixedTable(), report.Fixed},
		{"entry 3 corrupted, 1K", with(factory(), func(b []byte) { setEntry(b, 2, 0x07, 77, 99) }), factoryFS, factory(), report.NeedsFix},
		{"entry 3 corrupted, 4K", with(fixedTable(), func(b []byte) { setEntry(b, 2, 0x07, 77, 99) }), grownFS, fixedTable(), report.Fixed},
		{"entry 1 corrupted, 1K", with(factory(), func(b []byte) { clear(b[446:462]) }), factoryFS,
			with(factory(), func(b []byte) { copy(b[446:462], hwBefore[:]) }), report.NeedsFix},
		{"entry 1 corrupted, 4K", with(factory(), func(b []byte) { clear(b[446:462]) }), grownFS,
			with(factory(), func(b []byte) { copy(b[446:462], hwAfter[:]) }), report.Fixed},
		{"entry 4 past the end, 1K", with(factory(), func(b []byte) { setEntry(b, 3, 0x83, 4941824, testSectors) }), factoryFS, factory(), report.NeedsFix},
	}
}

func TestCheckDamaged(t *testing.T) {
	for _, tt := range damageCases() {
		t.Run(tt.name, func(t *testing.T) {
			f := mixer(tt.sector0, addEvidence(tt.fs))
			rep := Check(f)
			if !rep.OK || rep.Verdict != report.Damaged || rep.Code != "" || rep.Reason == "" {
				t.Fatalf("check: %+v", rep)
			}
			if f.writes != 0 {
				t.Fatal("check wrote")
			}
			if len(rep.Evidence) != 5 || rep.P1Filesystem != tt.fs.sectors {
				t.Fatalf("evidence %+v, p1 %d", rep.Evidence, rep.P1Filesystem)
			}
			want := parse(tt.want)
			for i := range want {
				if rep.Rebuild[i] != want[i] {
					t.Fatalf("rebuild %+v, want %+v", rep.Rebuild, want)
				}
			}
		})
	}
}

func TestRecoverRebuilds(t *testing.T) {
	fixedNow(t)
	for _, tt := range damageCases() {
		t.Run(tt.name, func(t *testing.T) {
			f := mixer(tt.sector0, addEvidence(tt.fs))
			dir := filepath.Join(t.TempDir(), "backups")
			rep := Recover(f, RecoverOptions{BackupDir: dir, Expect: sum(tt.sector0)})
			if !rep.OK || !rep.Changed || rep.Verdict != report.Damaged || rep.Command != "recover" || rep.Source != "" {
				t.Fatalf("recover: %+v", rep)
			}
			if !f.equal(tt.want) {
				t.Fatalf("sector 0 is\n% x\nwant\n% x", f.sector0(), tt.want)
			}
			if !strings.HasPrefix(filepath.Base(rep.Backup), "rcfix-damaged-sector0-") || filepath.Dir(rep.Backup) != dir {
				t.Fatalf("backup %s", rep.Backup)
			}
			if saved, err := os.ReadFile(rep.Backup); err != nil || !bytes.Equal(saved, tt.sector0) {
				t.Fatalf("the copy is not the damaged sector 0 (%v)", err)
			}
			if c := Check(f); !c.OK || c.Verdict != tt.after {
				t.Fatalf("check after recover: %+v", c)
			}
			if again := Recover(f, RecoverOptions{BackupDir: dir}); !again.OK || again.Changed {
				t.Fatalf("second recover: %+v", again)
			}
		})
	}
}

func TestRebuildKeepsBootCodeAndDiskID(t *testing.T) {
	old := factory()
	old[510] = 0
	f := mixer(old, addEvidence(factoryFS))
	if rep := Recover(f, RecoverOptions{BackupDir: t.TempDir()}); !rep.OK {
		t.Fatal(rep.Error)
	}
	got := f.sector0()
	if !bytes.Equal(got[:444], factory()[:444]) || got[444] != 0 || got[445] != 0 {
		t.Fatal("boot code or disk id was not kept")
	}
}

func TestEvidenceIncomplete(t *testing.T) {
	names := []string{"bootloader", "partition 1", "partition 2", "partition 3", "partition 4"}
	for item, name := range names {
		t.Run(name, func(t *testing.T) {
			f := mixer(make([]byte, 512), addEvidenceExcept(factoryFS, item))
			for _, rep := range []report.Report{Check(f), Recover(f, RecoverOptions{BackupDir: t.TempDir()})} {
				if rep.OK || rep.Code != "not-a-mixer" || rep.Verdict != report.Unrecognized || !strings.Contains(rep.Error, name) {
					t.Fatalf("%s: %+v", rep.Command, rep)
				}
				if len(rep.Evidence) != 5 || rep.Evidence[item].Found {
					t.Fatalf("evidence %+v", rep.Evidence)
				}
			}
			if f.writes != 0 {
				t.Fatal("wrote with incomplete evidence")
			}
		})
	}
	// An ext4 superblock with an impossible block size is no evidence.
	f := mixer(make([]byte, 512), addEvidence(fsSpec{sectors: 1039458, logBlock: 9}))
	if rep := Check(f); rep.Code != "not-a-mixer" {
		t.Fatalf("bad block size: %+v", rep)
	}
}

func TestRecoverRefusals(t *testing.T) {
	tests := []struct {
		name    string
		sector0 []byte
		setup   func(f *fakeDisk)
		opts    RecoverOptions
		ok      bool
		code    string
		msg     string
	}{
		{name: "factory table", sector0: factory(), setup: addEvidence(factoryFS), ok: true},
		{name: "fixed table", sector0: fixedTable(), ok: true},
		{name: "another layout", sector0: setEntry(factory(), 1, 0x83, 2000896, 5), setup: addEvidence(factoryFS), code: "unrecognized", msg: "a size rcfix does not know"},
		{name: "entry 1 too small for its filesystem", sector0: with(factory(), func(b []byte) { b[511] = 0 }), setup: addEvidence(grownFS), code: "unrecognized", msg: "the filesystem on it has 1193984"},
		{name: "filesystem larger than any partition 1", sector0: make([]byte, 512), setup: addEvidence(fsSpec{sectors: 1300000 * 2, logBlock: 2}), code: "unrecognized", msg: "more than even the grown"},
		{name: "not a mixer", sector0: make([]byte, 512), code: "not-a-mixer"},
		{name: "changed", sector0: make([]byte, 512), setup: addEvidence(factoryFS), opts: RecoverOptions{Expect: sum(factory())}, code: "changed"},
		{name: "read error", sector0: make([]byte, 512), setup: func(f *fakeDisk) { f.readErr = errInjected }, code: "read-failed"},
		{name: "no backup dir", sector0: make([]byte, 512), setup: addEvidence(factoryFS), opts: RecoverOptions{BackupDir: "-"}, code: "write-failed", msg: "could not be saved"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := mixer(tt.sector0, tt.setup)
			opts := tt.opts
			switch opts.BackupDir {
			case "":
				opts.BackupDir = t.TempDir()
			case "-":
				opts.BackupDir = ""
			}
			rep := Recover(f, opts)
			if rep.OK != tt.ok || rep.Code != tt.code || !strings.Contains(rep.Error, tt.msg) || rep.Changed {
				t.Fatalf("got %+v", rep)
			}
			if !rep.OK && rep.Error == "" {
				t.Fatal("failure without an error")
			}
			if f.writes != 0 || !f.equal(tt.sector0) {
				t.Fatal("a refusal wrote")
			}
		})
	}
}

func TestRecoverFromBackup(t *testing.T) {
	sigLost := func(b []byte) []byte { b[510] = 0; return b }
	tests := []struct {
		name    string
		current []byte
		fs      fsSpec
		backup  []byte
		size    int // non-zero: a file of this many bytes instead
		ok      bool
		code    string
		msg     string
	}{
		{name: "factory copy", current: make([]byte, 512), fs: factoryFS, backup: factory(), ok: true},
		{name: "fixed copy", current: make([]byte, 512), fs: grownFS, backup: fixedTable(), ok: true},
		{name: "fixed copy, slot A not yet reformatted", current: make([]byte, 512), fs: factoryFS, backup: fixedTable(), ok: true},
		{name: "factory copy under a grown slot A", current: make([]byte, 512), fs: grownFS, backup: factory(), code: "bad-backup", msg: "larger partition 1"},
		{name: "copy would shrink an intact entry", current: sigLost(fixedTable()), fs: factoryFS, backup: factory(), code: "bad-backup", msg: "partition 1, whose entry on the mixer is intact"},
		{name: "copy is not a known table", current: make([]byte, 512), fs: factoryFS, backup: setEntry(factory(), 1, 0x83, 7, 7), code: "bad-backup", msg: "not hold a partition table rcfix knows"},
		{name: "copy is damaged too", current: make([]byte, 512), fs: factoryFS, backup: make([]byte, 512), code: "bad-backup"},
		{name: "wrong size", current: make([]byte, 512), fs: factoryFS, size: 1024, code: "bad-backup", msg: "exactly 512 bytes"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := mixer(tt.current, addEvidence(tt.fs))
			content := tt.backup
			if tt.size != 0 {
				content = make([]byte, tt.size)
			}
			path := writeFile(t, content)
			rep := Recover(f, RecoverOptions{Backup: path, BackupDir: t.TempDir()})
			if rep.OK != tt.ok || rep.Code != tt.code || !strings.Contains(rep.Error, tt.msg) {
				t.Fatalf("got %+v", rep)
			}
			if tt.ok {
				if !rep.Changed || rep.Source != path || !f.equal(tt.backup) {
					t.Fatalf("not written: %+v", rep)
				}
				return
			}
			if f.writes != 0 || rep.Backup != "" {
				t.Fatal("a refusal wrote")
			}
		})
	}
}

func TestRecoverWriteFailures(t *testing.T) {
	tests := []struct {
		name     string
		setup    func(f *fakeDisk)
		code     string
		restored bool
	}{
		{"write error", func(f *fakeDisk) { f.writeErr = map[int]error{1: errInjected} }, "write-failed", true},
		{"read-back differs", func(f *fakeDisk) { f.corruptAfter = 1 }, "verify-failed", true},
		{"rollback fails", func(f *fakeDisk) { f.corruptAfter = 2 }, "verify-failed", false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			old := make([]byte, 512)
			f := mixer(old, addEvidence(factoryFS))
			tt.setup(f)
			rep := Recover(f, RecoverOptions{BackupDir: t.TempDir()})
			if rep.OK || rep.Code != tt.code || rep.Changed {
				t.Fatalf("got %+v", rep)
			}
			if tt.restored {
				if !f.equal(old) || !strings.Contains(rep.Error, "back to how it was") {
					t.Fatalf("not put back: %s", rep.Error)
				}
				return
			}
			if !strings.Contains(rep.Error, "may be left changed") || !strings.Contains(rep.Error, rep.Backup) {
				t.Fatalf("error does not warn or name the copy: %s", rep.Error)
			}
		})
	}
}

func TestDamageThenRecoverThenFix(t *testing.T) {
	f := mixer(make([]byte, 512), addEvidence(factoryFS))
	if rep := Check(f); rep.Verdict != report.Damaged {
		t.Fatalf("check: %+v", rep)
	}
	if rep := Recover(f, RecoverOptions{BackupDir: t.TempDir()}); !rep.OK || !rep.Changed {
		t.Fatalf("recover: %+v", rep)
	}
	if rep := Check(f); !rep.OK || rep.Verdict != report.NeedsFix {
		t.Fatalf("check after recover: %+v", rep)
	}
	if rep := Fix(f, Options{BackupDir: t.TempDir()}); !rep.OK || rep.After[0].Sectors != 1193984 {
		t.Fatalf("fix after recover: %+v", rep)
	}
	if got := f.sector0(); !bytes.Equal(got[446:462], hwAfter[:]) || !bytes.Equal(got[462:], fixedTable()[462:]) {
		t.Fatal("the recovered and fixed table is not the expected one")
	}
}
