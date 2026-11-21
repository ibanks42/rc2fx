package fix

import (
	"bytes"
	"encoding/binary"
	"errors"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
)

const testSectors = 15269888

func makeMBR(diskID [4]byte, entries [4]Partition) []byte {
	b := make([]byte, disk.SectorSize)
	for i := range 440 {
		b[i] = byte(i*7 + 3)
	}
	copy(b[440:444], diskID[:])
	for i, p := range entries {
		o := tableOffset + entrySize*i
		b[o+4] = p.Type
		binary.LittleEndian.PutUint32(b[o+8:], p.Start)
		binary.LittleEndian.PutUint32(b[o+12:], p.Sectors)
	}
	b[510], b[511] = 0x55, 0xAA
	return b
}

func factoryEntries() [4]Partition {
	return [4]Partition{
		{Number: 1, Type: 0x83, Start: 8192, Sectors: 1039458},
		{Number: 2, Type: 0x83, Start: 2000896, Sectors: 1193984},
		{Number: 3, Type: 0x83, Start: 3223552, Sectors: 1710080},
		{Number: 4, Type: 0x83, Start: 4941824, Sectors: testSectors - 4941824},
	}
}

func grownEntries() [4]Partition {
	e := factoryEntries()
	e[0].Sectors = 1193984
	return e
}

func TestParseMBRValid(t *testing.T) {
	tight := [4]Partition{
		{Number: 1, Type: 0x83, Start: 1, Sectors: 10},
		{Number: 2, Type: 0x83, Start: 11, Sectors: 20},
		{Number: 3, Type: 0x0c, Start: 31, Sectors: 30},
		{Number: 4, Type: 0x0c, Start: 61, Sectors: 40},
	}
	high := [4]Partition{
		{Number: 1, Type: 0x83, Start: 1, Sectors: 10},
		{Number: 2, Type: 0x83, Start: 100, Sectors: 20},
		{Number: 3, Type: 0x83, Start: 200, Sectors: 30},
		{Number: 4, Type: 0x83, Start: math.MaxUint32 - 10, Sectors: 100},
	}
	highEnd := uint64(math.MaxUint32-10) + 100

	tests := []struct {
		name        string
		entries     [4]Partition
		diskSectors uint64
	}{
		{"factory", factoryEntries(), testSectors},
		{"grown", grownEntries(), testSectors},
		{"tightly packed starting at sector 1", tight, 101},
		{"uint32 sum past 4GiB boundary", high, highEnd},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			sector0 := makeMBR([4]byte{0x78, 0x56, 0x34, 0x12}, tt.entries)
			got, err := ParseMBR(sector0, tt.diskSectors)
			if err != nil {
				t.Fatalf("ParseMBR unexpected error: %v", err)
			}
			if got != tt.entries {
				t.Fatalf("got %+v, want %+v", got, tt.entries)
			}
		})
	}
}

func TestParseMBRInvalid(t *testing.T) {
	withEntry := func(mut func(*[4]Partition)) []byte {
		e := factoryEntries()
		mut(&e)
		return makeMBR([4]byte{0x78, 0x56, 0x34, 0x12}, e)
	}
	withByte := func(mut func([]byte)) []byte {
		b := makeMBR([4]byte{0x78, 0x56, 0x34, 0x12}, factoryEntries())
		mut(b)
		return b
	}

	high := [4]Partition{
		{Number: 1, Type: 0x83, Start: 1, Sectors: 10},
		{Number: 2, Type: 0x83, Start: 100, Sectors: 20},
		{Number: 3, Type: 0x83, Start: 200, Sectors: 30},
		{Number: 4, Type: 0x83, Start: math.MaxUint32 - 10, Sectors: 100},
	}
	highEnd := uint64(math.MaxUint32-10) + 100

	tests := []struct {
		name        string
		sector0     []byte
		diskSectors uint64
		wantErr     error
	}{
		{"nil sector0", nil, testSectors, ErrNoPartitionTable},
		{"empty sector0", []byte{}, testSectors, ErrNoPartitionTable},
		{"short 511", make([]byte, disk.SectorSize-1), testSectors, ErrNoPartitionTable},
		{"long 513", append(makeMBR([4]byte{}, factoryEntries()), 0), testSectors, ErrNoPartitionTable},
		{"bad sig 510", withByte(func(b []byte) { b[510] = 0x54 }), testSectors, ErrNoPartitionTable},
		{"bad sig 511", withByte(func(b []byte) { b[511] = 0xAB }), testSectors, ErrNoPartitionTable},
		{"zero type", withEntry(func(e *[4]Partition) { e[2].Type = 0 }), testSectors, ErrInvalidMBR},
		{"zero sectors", withEntry(func(e *[4]Partition) { e[3].Sectors = 0 }), testSectors, ErrInvalidMBR},
		{"p1 starts at 0", withEntry(func(e *[4]Partition) { e[0].Start = 0 }), testSectors, ErrInvalidMBR},
		{"overlapping partitions", withEntry(func(e *[4]Partition) { e[1].Start = e[0].Start + e[0].Sectors - 1 }), testSectors, ErrInvalidMBR},
		{"out of order partitions", withEntry(func(e *[4]Partition) {
			e[1].Start, e[2].Start = e[2].Start, e[1].Start
		}), testSectors, ErrInvalidMBR},
		{"ends past diskSectors", makeMBR([4]byte{}, factoryEntries()), testSectors - 1, ErrInvalidMBR},
		{"zero diskSectors", makeMBR([4]byte{}, factoryEntries()), 0, ErrInvalidMBR},
		{"high uint32 past diskSectors", makeMBR([4]byte{}, high), highEnd - 1, ErrInvalidMBR},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseMBR(tt.sector0, tt.diskSectors)
			if !errors.Is(err, tt.wantErr) {
				t.Fatalf("got err %v, want %v", err, tt.wantErr)
			}
			var zero [4]Partition
			if got != zero {
				t.Fatalf("expected zero partitions on error, got %+v", got)
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

func TestSaveSector0Success(t *testing.T) {
	fixedNow(t)
	sector0 := makeMBR([4]byte{0x78, 0x56, 0x34, 0x12}, factoryEntries())
	dir := filepath.Join(t.TempDir(), "nested", "backups")

	path, err := SaveSector0(dir, sector0)
	if err != nil {
		t.Fatalf("SaveSector0 unexpected error: %v", err)
	}
	if filepath.Dir(path) != dir || filepath.Base(path) != wantBackupName {
		t.Fatalf("backup path %q, want dir %q base %q", path, dir, wantBackupName)
	}
	saved, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile(%q): %v", path, err)
	}
	if !bytes.Equal(saved, sector0) {
		t.Fatal("saved backup bytes differ from input sector0")
	}
	if runtime.GOOS != "windows" {
		if st, err := os.Stat(path); err != nil || st.Mode().Perm() != 0o600 {
			t.Fatalf("backup file mode %v (err=%v), want 0600", st.Mode(), err)
		}
		if st, err := os.Stat(dir); err != nil || st.Mode().Perm() != 0o700 {
			t.Fatalf("backup dir mode %v (err=%v), want 0700", st.Mode(), err)
		}
	}
}

func TestSaveSector0ErrorsAndNoOverwrite(t *testing.T) {
	fixedNow(t)
	valid := makeMBR([4]byte{0x78, 0x56, 0x34, 0x12}, factoryEntries())

	t.Run("rejects wrong lengths", func(t *testing.T) {
		for _, n := range []int{0, 511, 513} {
			dir := t.TempDir()
			if path, err := SaveSector0(dir, make([]byte, n)); err == nil || path != "" {
				t.Fatalf("len=%d: expected error, got path=%q err=%v", n, path, err)
			}
			entries, err := os.ReadDir(dir)
			if err != nil || len(entries) != 0 {
				t.Fatalf("len=%d: dir not empty after error: entries=%v err=%v", n, entries, err)
			}
		}
	})

	t.Run("rejects empty dir", func(t *testing.T) {
		if path, err := SaveSector0("", valid); err == nil || path != "" {
			t.Fatalf("expected error for empty dir, got path=%q err=%v", path, err)
		}
	})

	t.Run("fails when dir parent is a file", func(t *testing.T) {
		parentFile := filepath.Join(t.TempDir(), "notadir")
		if err := os.WriteFile(parentFile, []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
		if path, err := SaveSector0(filepath.Join(parentFile, "sub"), valid); err == nil || path != "" {
			t.Fatalf("expected error when dir cannot be created, got path=%q err=%v", path, err)
		}
	})

	t.Run("never overwrites existing backup", func(t *testing.T) {
		dir := t.TempDir()
		existing := filepath.Join(dir, wantBackupName)
		if err := os.WriteFile(existing, []byte("keep"), 0o600); err != nil {
			t.Fatal(err)
		}
		if path, err := SaveSector0(dir, valid); err == nil || path != "" {
			t.Fatalf("expected error on existing file, got path=%q err=%v", path, err)
		}
		got, err := os.ReadFile(existing)
		if err != nil || string(got) != "keep" {
			t.Fatalf("existing file changed: %q (err=%v)", got, err)
		}
	})
}
