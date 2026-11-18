package fix

import (
	"encoding/binary"
	"fmt"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

// rebuildDiskID is the disk id a rebuilt sector 0 gets when the damaged one
// has none (all zero or all 0xFF): bytes 24 93 3a ed, the id of a Pro II as
// shipped. RØDE writes one factory image to every unit, so it is the best
// guess should anything on the mixer name partitions by PARTUUID.
var rebuildDiskID = [4]byte{0x24, 0x93, 0x3a, 0xed}

// chsLBA is the CHS value that means "use the LBA fields".
var chsLBA = [3]byte{0xfe, 0xff, 0xff}

// The few ext4 superblock fields read here, as offsets into the superblock,
// which starts 1024 bytes into the partition.
const (
	sbOffset          = 1024
	sbBlocksLo        = 4
	sbLogBlockSize    = 24
	sbMagic           = 56
	sbFeatureIncompat = 0x60
	sbBlocksHi        = 0x150
	incompat64bit     = 0x80
)

// sectorReader reads one sector at a time and remembers nothing.
type sectorReader struct{ d disk.Disk }

// read returns sector n, or nil when the disk is too small to have it.
func (r sectorReader) read(n uint64) ([]byte, error) {
	if n >= r.d.Sectors() {
		return nil, nil
	}
	b := make([]byte, disk.SectorSize)
	if err := readFull(r.d, b, int64(n*disk.SectorSize)); err != nil {
		return nil, err
	}
	return b, nil
}

// ext4Size is the size in sectors of the ext4 filesystem starting at sector
// start, or 0 when there is no plausible ext4 superblock there.
func ext4Size(r sectorReader, start uint64) (uint64, error) {
	// Every field read is in the superblock's first 512 bytes.
	sb, err := r.read(start + sbOffset/disk.SectorSize)
	if err != nil || sb == nil {
		return 0, err
	}
	if sb[sbMagic] != 0x53 || sb[sbMagic+1] != 0xEF {
		return 0, nil
	}
	blocks := uint64(binary.LittleEndian.Uint32(sb[sbBlocksLo:]))
	if binary.LittleEndian.Uint32(sb[sbFeatureIncompat:])&incompat64bit != 0 {
		blocks |= uint64(binary.LittleEndian.Uint32(sb[sbBlocksHi:])) << 32
	}
	// ext4 block sizes run from 1 KiB to 64 KiB.
	logSize := binary.LittleEndian.Uint32(sb[sbLogBlockSize:])
	if blocks == 0 || logSize > 6 || blocks > (1<<40) {
		return 0, nil
	}
	return blocks * (1024 << logSize) / disk.SectorSize, nil
}

// fatBoot reports whether b is a FAT boot sector.
func fatBoot(b []byte) bool {
	return b != nil && b[510] == 0x55 && b[511] == 0xAA &&
		(string(b[54:57]) == "FAT" || string(b[82:85]) == "FAT")
}

// evidence looks for what makes a disk a RØDECaster with the standard
// layout, reading one sector per item: the bootloader in sector 1, ext4 at
// the start of partitions 1 and 2, FAT at the start of 3 and 4. p1 is the
// size of the filesystem on partition 1, 0 when there is none.
func evidence(d disk.Disk, l *Layout) (ev []report.Evidence, p1 uint64, err error) {
	r := sectorReader{d}
	boot, err := r.read(1)
	if err != nil {
		return nil, 0, err
	}
	// "@AML" at absolute offset 0x210 is 0x10 into sector 1.
	ev = append(ev, report.Evidence{What: "the mixer's bootloader (@AML at byte 0x210)", Sector: 1,
		Found: boot != nil && string(boot[0x10:0x14]) == "@AML"})
	for i, spec := range l.Entries {
		start := uint64(spec.Start)
		var found bool
		if i < 2 {
			size, err := ext4Size(r, start)
			if err != nil {
				return nil, 0, err
			}
			found = size > 0
			if i == 0 {
				p1 = size
			}
			ev = append(ev, report.Evidence{What: fmt.Sprintf("ext4 filesystem at the start of partition %d", i+1), Sector: start, Found: found})
			continue
		}
		b, err := r.read(start)
		if err != nil {
			return nil, 0, err
		}
		ev = append(ev, report.Evidence{What: fmt.Sprintf("FAT filesystem at the start of partition %d", i+1), Sector: start, Found: fatBoot(b)})
	}
	return ev, p1, nil
}

func missing(ev []report.Evidence) []string {
	var out []string
	for _, e := range ev {
		if !e.Found {
			out = append(out, fmt.Sprintf("%s (sector %d)", e.What, e.Sector))
		}
	}
	return out
}

// knownSize is whether n sectors is a size this tool knows for entry i of
// the layout.
func knownSize(l *Layout, i int, start, n uint32, diskSectors uint64) bool {
	switch {
	case n == 0 || uint64(start)+uint64(n) > diskSectors:
		return false
	case i == 0:
		return n == l.Entries[0].Sectors || n == l.Grown
	case l.Entries[i].Sectors == 0:
		return true
	}
	return n == l.Entries[i].Sectors
}

// intact is whether entry i is exactly what the layout has there: type,
// start and a known size.
func intact(l *Layout, i int, p report.Partition, diskSectors uint64) bool {
	return p.Type == l.Entries[i].Type && p.Start == l.Entries[i].Start && knownSize(l, i, p.Start, p.Sectors, diskSectors)
}

// otherLayout is a reason when the table, though not one this tool knows,
// is readable rather than damaged: an entry has a standard start and a
// sensible size this tool does not know. Recover never changes the size of
// such an entry, so a table like this is left alone.
func otherLayout(l *Layout, parts []report.Partition, diskSectors uint64) string {
	for i, p := range parts {
		if p.Start != l.Entries[i].Start || p.Sectors == 0 || uint64(p.Start)+uint64(p.Sectors) > diskSectors {
			continue
		}
		if !knownSize(l, i, p.Start, p.Sectors, diskSectors) {
			return fmt.Sprintf("partition %d starts where expected but has %d sectors, a size rcfix does not know", i+1, p.Sectors)
		}
	}
	return ""
}

// damage is a damaged sector 0 and what to rebuild it as.
type damage struct {
	layout   *Layout
	p1       uint64 // ext4 size on partition 1, in sectors
	evidence []report.Evidence
	rebuilt  []byte
}

// examine decides an Unrecognized sector 0 further, from the rest of the
// disk. It returns the damage when the table can be rebuilt; otherwise a
// failure code ("not-a-mixer") and words, or only a reason when the table
// is another layout and stays Unrecognized. err is a failed read.
func examine(d disk.Disk, sector0 []byte, parts []report.Partition) (dm *damage, ev []report.Evidence, code, reason string, err error) {
	l := &layouts[0]
	if r := otherLayout(l, parts, d.Sectors()); r != "" {
		return nil, nil, "", r, nil
	}
	ev, p1, err := evidence(d, l)
	if err != nil {
		return nil, nil, "", "", err
	}
	if m := missing(ev); len(m) > 0 {
		return nil, ev, "not-a-mixer", "the rest of the disk does not show a " + l.Name + ": missing " + strings.Join(m, ", "), nil
	}
	rebuilt, reason := rebuild(l, sector0, parts, p1, d.Sectors())
	if rebuilt == nil {
		return nil, ev, "", reason, nil
	}
	return &damage{layout: l, p1: p1, evidence: ev, rebuilt: rebuilt}, ev, "", "", nil
}

// rebuild is sector 0 as recover writes it: the boot code and disk id kept
// where there are any, every intact entry kept byte for byte, every other
// entry written new at its standard start, and the 55 AA signature. It
// returns nil and a reason when the evidence contradicts the table.
func rebuild(l *Layout, sector0 []byte, parts []report.Partition, p1, diskSectors uint64) ([]byte, string) {
	out := make([]byte, disk.SectorSize)
	// The bootloader is in sector 1 and does not run this code, so a
	// missing boot code is left as zeros rather than invented.
	if !allBytes(sector0[:440], 0) && !allBytes(sector0[:440], 0xff) {
		copy(out[:440], sector0[:440])
	}
	if id := sector0[440:444]; allBytes(id, 0) || allBytes(id, 0xff) {
		copy(out[440:444], rebuildDiskID[:])
	} else {
		copy(out[440:444], id)
	}
	if p1 > uint64(l.Grown) {
		return nil, fmt.Sprintf("the filesystem on partition 1 has %d sectors, more than even the grown partition 1 (%d)", p1, l.Grown)
	}
	for i := range parts {
		dst := entry(out, i)
		if intact(l, i, parts[i], diskSectors) {
			if i == 0 && p1 > uint64(parts[0].Sectors) {
				return nil, fmt.Sprintf("partition 1's entry has %d sectors, but the filesystem on it has %d", parts[0].Sectors, p1)
			}
			copy(dst, entry(sector0, i))
			continue
		}
		if i == 0 {
			e := l.FactoryEntry1
			if p1 > uint64(l.Entries[0].Sectors) {
				e = l.GrownEntry1
			}
			copy(dst, e[:])
			continue
		}
		n := l.Entries[i].Sectors
		if n == 0 {
			// The last partition fills the disk, to a whole MiB.
			end := diskSectors / 2048 * 2048
			if end <= uint64(l.Entries[i].Start) || end-uint64(l.Entries[i].Start) > 0xffffffff {
				return nil, fmt.Sprintf("the disk (%d sectors) has no room for partition %d", diskSectors, i+1)
			}
			n = uint32(end - uint64(l.Entries[i].Start))
		}
		copy(dst[1:4], chsLBA[:])
		dst[4] = l.Entries[i].Type
		copy(dst[5:8], chsLBA[:])
		binary.LittleEndian.PutUint32(dst[8:12], l.Entries[i].Start)
		binary.LittleEndian.PutUint32(dst[12:16], n)
	}
	out[510], out[511] = 0x55, 0xAA
	// Whatever was decided must itself be a table this tool knows.
	if v := Decide(out, diskSectors).Verdict; v != report.NeedsFix && v != report.Fixed {
		return nil, "the rebuilt table would not be one rcfix knows"
	}
	return out, ""
}

func allBytes(b []byte, v byte) bool {
	for _, c := range b {
		if c != v {
			return false
		}
	}
	return true
}

// tooLarge is the words for a filesystem on partition 1 that has outgrown
// the factory partition, or "" when it has not.
func tooLarge(l *Layout, p1 uint64) string {
	if p1 <= uint64(l.Entries[0].Sectors) {
		return ""
	}
	return fmt.Sprintf("A firmware update has already been installed onto the larger partition 1: the system on it fills %d sectors, "+
		"more than the %d of the original table. Putting the original table back would shrink partition 1 under that system "+
		"and stop system slot A from starting.", p1, l.Entries[0].Sectors)
}
