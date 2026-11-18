package fix

import (
	"bytes"
	"encoding/binary"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

func setEntry(b []byte, i int, typ byte, start, sectors uint32) []byte {
	o := 446 + 16*i
	b[o+4] = typ
	binary.LittleEndian.PutUint32(b[o+8:], start)
	binary.LittleEndian.PutUint32(b[o+12:], sectors)
	return b
}

func TestDecide(t *testing.T) {
	mod := func(f func(b []byte)) []byte { b := factory(); f(b); return b }
	tests := []struct {
		name    string
		sector0 []byte
		disk    uint64
		verdict report.Verdict
		reason  string
	}{
		{"factory", factory(), testSectors, report.NeedsFix, ""},
		{"fixed", fixedTable(), testSectors, report.Fixed, ""},
		{"short", factory()[:100], testSectors, report.Unrecognized, "could not be read"},
		{"no signature", mod(func(b []byte) { b[511] = 0 }), testSectors, report.Unrecognized, "no partition table"},
		{"empty table", mod(func(b []byte) { clear(b[446:510]) }), testSectors, report.Unrecognized, "partition 1 is missing"},
		{"p1 start", mod(func(b []byte) { setEntry(b, 0, 0x83, 2048, 1039458) }), testSectors, report.Unrecognized, "partition 1 starts at sector 2048, expected 8192"},
		{"p1 size", mod(func(b []byte) { setEntry(b, 0, 0x83, 8192, 1100000) }), testSectors, report.Unrecognized, "partition 1 has 1100000 sectors"},
		{"p1 type", mod(func(b []byte) { b[446+4] = 0x0c }), testSectors, report.Unrecognized, "partition 1 has type 0x0c, expected 0x83"},
		{"p2 start", mod(func(b []byte) { setEntry(b, 1, 0x83, 2000897, 1193984) }), testSectors, report.Unrecognized, "partition 2 starts at sector 2000897, expected 2000896"},
		{"p2 size", mod(func(b []byte) { setEntry(b, 1, 0x83, 2000896, 1193983) }), testSectors, report.Unrecognized, "partition 2 has 1193983 sectors, expected 1193984"},
		{"p3 type", mod(func(b []byte) { b[446+32+4] = 0x0b }), testSectors, report.Unrecognized, "partition 3 has type 0x0b"},
		{"p3 missing", mod(func(b []byte) { clear(b[478:494]) }), testSectors, report.Unrecognized, "partition 3 is missing"},
		{"p4 empty", mod(func(b []byte) { setEntry(b, 3, 0x83, 4941824, 0) }), testSectors, report.Unrecognized, "partition 4 is empty"},
		{"p4 past end", factory(), testSectors - 1, report.Unrecognized, "partition 4 ends at sector"},
		{"p4 start", mod(func(b []byte) { setEntry(b, 3, 0x83, 4941825, 1000) }), testSectors, report.Unrecognized, "partition 4 starts at sector 4941825"},
		{"p4 any size", mod(func(b []byte) { setEntry(b, 3, 0x83, 4941824, 1) }), testSectors, report.NeedsFix, ""},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			d := Decide(tt.sector0, tt.disk)
			if d.Verdict != tt.verdict {
				t.Fatalf("verdict %q, want %q (reason %q)", d.Verdict, tt.verdict, d.Reason)
			}
			if !strings.Contains(d.Reason, tt.reason) {
				t.Fatalf("reason %q, want it to contain %q", d.Reason, tt.reason)
			}
			if tt.verdict != report.Unrecognized && (d.Layout == nil || len(d.Partitions) != 4) {
				t.Fatalf("known verdict without layout or table: %+v", d)
			}
		})
	}
}

func TestGrowEntryOtherCHSChangesOnlyTheCount(t *testing.T) {
	var old [16]byte
	for i := range old {
		old[i] = byte(0xa0 + i)
	}
	e := growEntry(&layouts[0], old)
	for i := range old {
		if (i < 12 || i > 15) && e[i] != old[i] {
			t.Fatalf("byte %d changed", i)
		}
	}
	if got := binary.LittleEndian.Uint32(e[12:]); got != 1193984 {
		t.Fatalf("count %d", got)
	}
}

// The entry bytes sfdisk wrote when the fix was proven on a mixer.
var (
	hwBefore = [16]byte{0x00, 0x00, 0x01, 0x40, 0x83, 0x03, 0xe0, 0xff, 0x00, 0x20, 0x00, 0x00, 0x62, 0xdc, 0x0f, 0x00}
	hwAfter  = [16]byte{0x00, 0x00, 0x01, 0x80, 0x83, 0x03, 0x50, 0x5f, 0x00, 0x20, 0x00, 0x00, 0x00, 0x38, 0x12, 0x00}
)

func TestGrowEntryHardwareGolden(t *testing.T) {
	old := factory()
	copy(old[446:462], hwBefore[:])
	if d := Decide(old, testSectors); d.Verdict != report.NeedsFix {
		t.Fatalf("the hardware entry is not the factory layout: %+v", d)
	}
	got := grown(old, &layouts[0])
	if !bytes.Equal(got[446:462], hwAfter[:]) {
		t.Fatalf("entry 1 % x, want % x", got[446:462], hwAfter[:])
	}
	assertOnlyEntry1Differs(t, old, got)
	if d := Decide(got, testSectors); d.Verdict != report.Fixed {
		t.Fatalf("grown hardware entry is not fixed: %+v", d)
	}
}

func TestGrownSectorDiffersOnlyInEntry1(t *testing.T) {
	old := factory()
	assertOnlyEntry1Differs(t, old, grown(old, &layouts[0]))
	if !bytes.Equal(grown(old, &layouts[0]), fixedTable()) {
		t.Fatal("grown table differs from the reference fixed table")
	}
}

func assertOnlyEntry1Differs(t *testing.T, a, b []byte) {
	t.Helper()
	for i := range a {
		if a[i] != b[i] && (i < 446 || i > 461) {
			t.Fatalf("byte %d differs", i)
		}
	}
}

// grow range of the reference layout
const growFrom, growLast = 1047650, 1202175

func TestCheck(t *testing.T) {
	tests := []struct {
		name    string
		sector0 []byte
		dirty   int64 // non-zero sector in the grow range, 0 for none
		readErr bool
		ok      bool
		verdict report.Verdict
		code    string
	}{
		{name: "needs fix", sector0: factory(), ok: true, verdict: report.NeedsFix},
		{name: "fixed", sector0: fixedTable(), ok: true, verdict: report.Fixed},
		{name: "unrecognized", sector0: setEntry(factory(), 2, 0x83, 3223552, 5), ok: true, verdict: report.Unrecognized},
		{name: "not a mixer", sector0: setEntry(factory(), 1, 0x83, 1, 1), verdict: report.Unrecognized, code: "not-a-mixer"},
		{name: "dirty start", sector0: factory(), dirty: growFrom, verdict: report.NeedsFix, code: "not-empty"},
		{name: "dirty middle", sector0: factory(), dirty: (growFrom + growLast) / 2, verdict: report.NeedsFix, code: "not-empty"},
		{name: "dirty last", sector0: factory(), dirty: growLast, verdict: report.NeedsFix, code: "not-empty"},
		{name: "fixed ignores data after p1", sector0: fixedTable(), dirty: growFrom, ok: true, verdict: report.Fixed},
		{name: "read error", sector0: factory(), readErr: true, code: "read-failed"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := newFake(tt.sector0)
			if tt.dirty != 0 {
				b := make([]byte, 512)
				b[511] = 1
				f.put(tt.dirty, b)
			}
			if tt.readErr {
				f.readErr = errInjected
			}
			rep := Check(f)
			if rep.OK != tt.ok || rep.Code != tt.code || rep.Verdict != tt.verdict {
				t.Fatalf("got ok=%v code=%q verdict=%q (%s), want ok=%v code=%q verdict=%q", rep.OK, rep.Code, rep.Verdict, rep.Error, tt.ok, tt.code, tt.verdict)
			}
			if rep.Command != "check" || f.writes != 0 || rep.Changed {
				t.Fatalf("check wrote or misreported: %+v writes=%d", rep, f.writes)
			}
			if tt.code == "not-empty" && !strings.Contains(rep.Error, "sector ") {
				t.Fatalf("error does not name the sector: %s", rep.Error)
			}
			if !tt.ok && rep.Error == "" {
				t.Fatal("a failed check has no error")
			}
			if !tt.readErr && (rep.Sector0 != sum(tt.sector0) || rep.Sectors != testSectors || len(rep.Partitions) != 4) {
				t.Fatalf("report fields: %+v", rep)
			}
		})
	}
}

func fixedNow(t *testing.T) {
	t.Helper()
	old := now
	now = func() time.Time { return time.Date(2026, 11, 17, 9, 8, 7, 0, time.FixedZone("x", 3600)) }
	t.Cleanup(func() { now = old })
}

const wantBackupName = "rcfix-sector0-12345678-20261117T080807Z.bin"

func TestFixSuccess(t *testing.T) {
	fixedNow(t)
	old := factory()
	f := newFake(old)
	dir := filepath.Join(t.TempDir(), "backups")
	rep := Fix(f, Options{Expect: strings.ToUpper(sum(old)), BackupDir: dir})
	if !rep.OK || !rep.Changed || rep.Code != "" || rep.Verdict != report.NeedsFix {
		t.Fatalf("fix: %+v", rep)
	}
	assertOnlyEntry1Differs(t, old, f.sector0())
	if !f.equal(fixedTable()) {
		t.Fatal("sector 0 is not the fixed table")
	}
	if len(rep.After) != 4 || rep.After[0].Sectors != 1193984 {
		t.Fatalf("after: %+v", rep.After)
	}
	if filepath.Base(rep.Backup) != wantBackupName || filepath.Dir(rep.Backup) != dir {
		t.Fatalf("backup path %s", rep.Backup)
	}
	saved, err := os.ReadFile(rep.Backup)
	if err != nil || !bytes.Equal(saved, old) {
		t.Fatalf("backup differs from the old sector 0 (%v)", err)
	}
	if runtime.GOOS != "windows" {
		if st, _ := os.Stat(rep.Backup); st.Mode().Perm() != 0o600 {
			t.Fatalf("backup mode %v", st.Mode())
		}
	}
	if Check(f).Verdict != report.Fixed {
		t.Fatal("check after fix is not fixed")
	}
}

func TestFixRefusals(t *testing.T) {
	tests := []struct {
		name    string
		sector0 []byte
		setup   func(f *fakeDisk)
		opts    func(dir string) Options
		ok      bool
		code    string
		msg     string
	}{
		{name: "already fixed", sector0: fixedTable(), ok: true},
		{name: "unrecognized", sector0: setEntry(factory(), 2, 0x83, 3223552, 5), code: "unrecognized", msg: "partition 3 has 5 sectors"},
		{name: "changed", sector0: factory(), opts: func(dir string) Options { return Options{Expect: sum(fixedTable()), BackupDir: dir} }, code: "changed"},
		{name: "changed beats fixed", sector0: fixedTable(), opts: func(dir string) Options { return Options{Expect: sum(factory()), BackupDir: dir} }, code: "changed"},
		{name: "not empty", sector0: factory(), setup: func(f *fakeDisk) { f.put(growLast, []byte{9}) }, code: "not-empty"},
		{name: "read error", sector0: factory(), setup: func(f *fakeDisk) { f.readErr = errInjected }, code: "read-failed"},
		{name: "damaged", sector0: setEntry(factory(), 2, 0, 0, 0), setup: addEvidence(factoryFS), code: "damaged", msg: "rcfix recover"},
		{name: "not a mixer", sector0: setEntry(factory(), 2, 0, 0, 0), code: "not-a-mixer"},
		{name: "backup cannot be written", sector0: factory(), opts: func(dir string) Options {
			file := filepath.Join(dir, "file")
			os.WriteFile(file, nil, 0o600)
			return Options{BackupDir: filepath.Join(file, "sub")}
		}, code: "write-failed", msg: "could not be saved"},
		{name: "no backup dir", sector0: factory(), opts: func(string) Options { return Options{} }, code: "write-failed"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := newFake(tt.sector0)
			if tt.setup != nil {
				tt.setup(f)
			}
			dir := t.TempDir()
			opts := Options{BackupDir: filepath.Join(dir, "backups")}
			if tt.opts != nil {
				opts = tt.opts(dir)
			}
			rep := Fix(f, opts)
			if rep.OK != tt.ok || rep.Code != tt.code || !strings.Contains(rep.Error, tt.msg) {
				t.Fatalf("got ok=%v code=%q error=%q", rep.OK, rep.Code, rep.Error)
			}
			if f.writes != 0 || rep.Changed || rep.Backup != "" {
				t.Fatalf("refusal wrote: writes=%d report=%+v", f.writes, rep)
			}
			if !f.equal(tt.sector0) {
				t.Fatal("sector 0 changed")
			}
			if entries, _ := os.ReadDir(filepath.Join(dir, "backups")); len(entries) != 0 {
				t.Fatal("a backup was written")
			}
		})
	}
}

func TestFixWriteFailures(t *testing.T) {
	tests := []struct {
		name     string
		setup    func(f *fakeDisk)
		code     string
		restored bool
	}{
		{"write error", func(f *fakeDisk) { f.writeErr = map[int]error{1: errInjected} }, "write-failed", true},
		{"short write", func(f *fakeDisk) { f.shortWrite = map[int]int{1: 460} }, "write-failed", true},
		{"sync error", func(f *fakeDisk) { f.syncErr = map[int]error{1: errInjected} }, "write-failed", true},
		{"read-back error", func(f *fakeDisk) { f.readFailsAfter = 1 }, "verify-failed", true},
		{"read-back differs", func(f *fakeDisk) { f.corruptAfter = 1 }, "verify-failed", true},
		{"rollback write fails", func(f *fakeDisk) {
			f.syncErr = map[int]error{1: errInjected}
			f.writeErr = map[int]error{2: errInjected}
		}, "write-failed", false},
		{"rollback read-back differs", func(f *fakeDisk) { f.corruptAfter = 2 }, "verify-failed", false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			old := factory()
			f := newFake(old)
			tt.setup(f)
			rep := Fix(f, Options{BackupDir: t.TempDir()})
			if rep.OK || rep.Code != tt.code || rep.Changed {
				t.Fatalf("got %+v", rep)
			}
			saved, err := os.ReadFile(rep.Backup)
			if err != nil || !bytes.Equal(saved, old) {
				t.Fatalf("backup missing or wrong before the write (%v)", err)
			}
			if tt.restored {
				if !f.equal(old) {
					t.Fatal("the disk was not put back as it was")
				}
				if !strings.Contains(rep.Error, "back to how it was") {
					t.Fatalf("error does not say the storage is back: %s", rep.Error)
				}
			} else {
				if !strings.Contains(rep.Error, "may be left changed") || !strings.Contains(rep.Error, rep.Backup) {
					t.Fatalf("error does not warn or name the backup: %s", rep.Error)
				}
			}
		})
	}
}

func TestBackupNeverOverwrites(t *testing.T) {
	fixedNow(t)
	dir := t.TempDir()
	existing := filepath.Join(dir, wantBackupName)
	os.WriteFile(existing, []byte("keep"), 0o600)
	if _, err := writeBackup(dir, "rcfix-sector0-", factory()); err == nil {
		t.Fatal("overwrote an existing backup")
	}
	if b, _ := os.ReadFile(existing); string(b) != "keep" {
		t.Fatal("existing file changed")
	}
}

func writeFile(t *testing.T, b []byte) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "backup.bin")
	if err := os.WriteFile(p, b, 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestRestore(t *testing.T) {
	foreign := factory()
	foreign[440] ^= 1 // another disk id
	tests := []struct {
		name    string
		current []byte
		backup  []byte
		noFile  bool
		setup   func(f *fakeDisk)
		ok      bool
		changed bool
		code    string
		msg     string
	}{
		{name: "restores", current: fixedTable(), backup: factory(), ok: true, changed: true},
		{name: "already restored", current: factory(), backup: factory(), ok: true},
		{name: "missing file", current: fixedTable(), noFile: true, code: "bad-backup"},
		{name: "wrong size", current: fixedTable(), backup: append(factory(), 0), code: "bad-backup"},
		{name: "backup is fixed", current: fixedTable(), backup: fixedTable(), code: "bad-backup"},
		{name: "backup unrecognized", current: fixedTable(), backup: setEntry(factory(), 1, 0x83, 7, 7), code: "bad-backup"},
		{name: "other disk", current: fixedTable(), backup: foreign, code: "bad-backup"},
		{name: "write error", current: fixedTable(), backup: factory(), setup: func(f *fakeDisk) { f.writeErr = map[int]error{1: errInjected} }, code: "write-failed"},
		{name: "read-back differs", current: fixedTable(), backup: factory(), setup: func(f *fakeDisk) { f.corruptAfter = 1 }, code: "verify-failed"},
		{name: "read error", current: fixedTable(), backup: factory(), setup: func(f *fakeDisk) { f.readErr = errInjected }, code: "read-failed"},
		{name: "slot A already uses the grown partition", current: fixedTable(), backup: factory(), setup: addEvidence(grownFS), code: "bad-backup", msg: "already been installed onto the larger partition 1"},
		{name: "slot A still fits", current: fixedTable(), backup: factory(), setup: addEvidence(factoryFS), ok: true, changed: true},
		{name: "current table damaged", current: setEntry(fixedTable(), 3, 0, 0, 0), backup: factory(), setup: addEvidence(factoryFS), code: "damaged", msg: "recover --backup"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			f := newFake(tt.current)
			if tt.setup != nil {
				tt.setup(f)
			}
			path := filepath.Join(t.TempDir(), "absent.bin")
			if !tt.noFile {
				path = writeFile(t, tt.backup)
			}
			rep := Restore(f, path)
			if rep.OK != tt.ok || rep.Changed != tt.changed || rep.Code != tt.code || rep.Command != "restore" || !strings.Contains(rep.Error, tt.msg) {
				t.Fatalf("got %+v", rep)
			}
			if !rep.OK && rep.Error == "" {
				t.Fatal("a failed restore has no error")
			}
			switch {
			case tt.changed:
				if !f.equal(tt.backup) || len(rep.After) != 4 {
					t.Fatal("not restored")
				}
			case tt.code == "write-failed" || tt.code == "verify-failed":
				if !f.equal(tt.current) || !strings.Contains(rep.Error, "back to how it was") {
					t.Fatalf("failed restore did not leave the disk as it was: %s", rep.Error)
				}
			default:
				if f.writes != 0 || !f.equal(tt.current) {
					t.Fatal("refusal wrote")
				}
			}
		})
	}
}

func TestFixThenRestoreRoundTrip(t *testing.T) {
	old := factory()
	f := newFake(old)
	rep := Fix(f, Options{BackupDir: t.TempDir()})
	if !rep.OK {
		t.Fatal(rep.Error)
	}
	if rep := Restore(f, rep.Backup); !rep.OK || !rep.Changed {
		t.Fatalf("restore: %+v", rep)
	}
	if !f.equal(old) {
		t.Fatal("round trip changed sector 0")
	}
}
