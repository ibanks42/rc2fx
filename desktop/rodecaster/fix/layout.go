// Package fix reads a mixer's MBR partition table, decides whether it is a
// layout this tool knows, and grows partition 1 into the empty space after
// it. It works on any disk.Disk and has no operating-system code.
package fix

import (
	"encoding/binary"
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

const (
	tableOffset = 446
	entrySize   = 16
	countOffset = 12
)

// entrySpec is one expected table entry. Sectors 0 means any non-zero count
// that ends inside the disk (the last partition fills whatever eMMC size the
// mixer has).
type entrySpec struct {
	Type    byte
	Start   uint32
	Sectors uint32
}

// Layout is a factory partition table this tool knows. The fix always grows
// entry 1 to Grown sectors, into space the layout leaves empty after it.
type Layout struct {
	Name    string
	Entries [4]entrySpec
	Grown   uint32
	// FactoryEntry1 is entry 1 exactly as RØDE writes it, and GrownEntry1
	// what sfdisk made of it when the fix was proven on a mixer (it updates
	// the CHS fields along with the count). Fix writes GrownEntry1 over an
	// exact FactoryEntry1, and recover writes one of them for a lost entry.
	FactoryEntry1, GrownEntry1 [entrySize]byte
}

var layouts = []Layout{
	{
		Name: "RØDECaster Pro II",
		Entries: [4]entrySpec{
			{Type: 0x83, Start: 8192, Sectors: 1039458},
			{Type: 0x83, Start: 2000896, Sectors: 1193984},
			{Type: 0x83, Start: 3223552, Sectors: 1710080},
			{Type: 0x83, Start: 4941824},
		},
		Grown:         1193984,
		FactoryEntry1: [entrySize]byte{0x00, 0x00, 0x01, 0x40, 0x83, 0x03, 0xe0, 0xff, 0x00, 0x20, 0x00, 0x00, 0x62, 0xdc, 0x0f, 0x00},
		GrownEntry1:   [entrySize]byte{0x00, 0x00, 0x01, 0x80, 0x83, 0x03, 0x50, 0x5f, 0x00, 0x20, 0x00, 0x00, 0x00, 0x38, 0x12, 0x00},
	},
}

// Decision is what Decide found.
type Decision struct {
	Verdict    report.Verdict
	Reason     string
	Layout     *Layout
	Partitions []report.Partition
}

func entry(sector0 []byte, i int) []byte {
	o := tableOffset + entrySize*i
	return sector0[o : o+entrySize]
}

func parse(sector0 []byte) []report.Partition {
	parts := make([]report.Partition, 4)
	for i := range parts {
		e := entry(sector0, i)
		parts[i] = report.Partition{
			Number:  i + 1,
			Type:    e[4],
			Start:   binary.LittleEndian.Uint32(e[8:12]),
			Sectors: binary.LittleEndian.Uint32(e[12:16]),
		}
	}
	return parts
}

// ErrNoPartitionTable reports that sector 0 does not carry the 55 AA MBR
// signature.
var ErrNoPartitionTable = fmt.Errorf("the disk has no partition table")

// ErrInvalidMBR reports that an MBR does not hold four ordered,
// non-overlapping non-empty primary partitions inside the disk.
var ErrInvalidMBR = fmt.Errorf("the disk's partitions are not a RØDECaster's")

// ParseMBR reads the four primary partitions of an MBR sector, refusing
// anything that is not four ordered, non-overlapping non-empty partitions
// inside diskSectors.
func ParseMBR(sector0 []byte, diskSectors uint64) ([4]report.Partition, error) {
	var parts [4]report.Partition
	if len(sector0) != disk.SectorSize || sector0[510] != 0x55 || sector0[511] != 0xAA {
		return parts, ErrNoPartitionTable
	}
	parsed := parse(sector0)
	var end uint64 = 1 // sector 0 holds the MBR itself
	for i, p := range parsed {
		start, count := uint64(p.Start), uint64(p.Sectors)
		if p.Type == 0 || count == 0 || start < end || start+count > diskSectors {
			return parts, ErrInvalidMBR
		}
		end = start + count
		parts[i] = p
	}
	return parts, nil
}

// Decide says whether sector0 is a known layout that needs the fix, already
// has it, or is something else. A near miss is unrecognized: this tool
// never guesses.
func Decide(sector0 []byte, diskSectors uint64) Decision {
	if len(sector0) != disk.SectorSize {
		return Decision{Verdict: report.Unrecognized, Reason: "sector 0 could not be read in full"}
	}
	if sector0[510] != 0x55 || sector0[511] != 0xAA {
		return Decision{Verdict: report.Unrecognized, Reason: "there is no partition table (sector 0 does not end with the 55 AA signature)"}
	}
	d := Decision{Partitions: parse(sector0)}
	first := ""
	for i := range layouts {
		v, reason := match(&layouts[i], d.Partitions, diskSectors)
		if reason == "" {
			d.Verdict, d.Layout = v, &layouts[i]
			return d
		}
		if first == "" {
			first = reason
		}
	}
	d.Verdict, d.Reason = report.Unrecognized, first
	return d
}

// match returns the verdict for a table that is this layout, or the first
// difference in words.
func match(l *Layout, parts []report.Partition, diskSectors uint64) (report.Verdict, string) {
	verdict := report.NeedsFix
	for i, want := range l.Entries {
		got, n := parts[i], i+1
		if got.Type == 0 && got.Start == 0 && got.Sectors == 0 {
			return "", fmt.Sprintf("partition %d is missing", n)
		}
		if got.Type != want.Type {
			return "", fmt.Sprintf("partition %d has type 0x%02x, expected 0x%02x", n, got.Type, want.Type)
		}
		if got.Start != want.Start {
			return "", fmt.Sprintf("partition %d starts at sector %d, expected %d", n, got.Start, want.Start)
		}
		switch {
		case i == 0:
			switch got.Sectors {
			case want.Sectors:
			case l.Grown:
				verdict = report.Fixed
			default:
				return "", fmt.Sprintf("partition 1 has %d sectors, expected %d (or %d once fixed)", got.Sectors, want.Sectors, l.Grown)
			}
		case want.Sectors == 0:
			if got.Sectors == 0 {
				return "", fmt.Sprintf("partition %d is empty", n)
			}
		case got.Sectors != want.Sectors:
			return "", fmt.Sprintf("partition %d has %d sectors, expected %d", n, got.Sectors, want.Sectors)
		}
		if end := uint64(got.Start) + uint64(got.Sectors); end > diskSectors {
			return "", fmt.Sprintf("partition %d ends at sector %d, past the end of the disk (%d sectors)", n, end-1, diskSectors)
		}
	}
	if end := uint64(l.Entries[0].Start) + uint64(l.Grown); end > diskSectors {
		return "", fmt.Sprintf("the disk is too small (%d sectors) for the grown partition 1", diskSectors)
	}
	return verdict, ""
}

// growEntry is the new table entry for partition 1. RØDE's exact entry
// becomes the bytes proven on hardware. Any other entry with the factory
// start and count (Decide has checked those) has CHS bytes this tool has no
// tested value for, so only its sector count changes: nothing reads CHS on
// this device, and guessing new ones would be the riskier change.
func growEntry(l *Layout, old [entrySize]byte) [entrySize]byte {
	if old == l.FactoryEntry1 {
		return l.GrownEntry1
	}
	e := old
	binary.LittleEndian.PutUint32(e[countOffset:countOffset+4], l.Grown)
	return e
}

// grown is sector0 with partition 1 grown to the layout's size.
func grown(sector0 []byte, l *Layout) []byte {
	out := append([]byte(nil), sector0...)
	var old [entrySize]byte
	copy(old[:], entry(sector0, 0))
	e := growEntry(l, old)
	copy(entry(out, 0), e[:])
	return out
}

// growRange is the sectors partition 1 grows into, [from, to).
func growRange(l *Layout) (from, to uint64) {
	start := uint64(l.Entries[0].Start)
	return start + uint64(l.Entries[0].Sectors), start + uint64(l.Grown)
}
