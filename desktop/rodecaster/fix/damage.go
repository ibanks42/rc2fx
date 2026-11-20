package fix

import (
	"encoding/binary"
	"fmt"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

// Fallback MBR disk ID used when rebuilding a wiped sector 0 (0xed3a9324, the
// factory Pro II image ID) in case anything references partitions by PARTUUID.
var rebuildDiskID = [4]byte{0x24, 0x93, 0x3a, 0xed}

// Sentinel CHS tuple indicating LBA addressing.
var chsLBA = [3]byte{0xfe, 0xff, 0xff}

// Offsets into the ext4 superblock (located 1024 bytes into the partition).
const (
	sbOffset          = 1024
	sbBlocksLo        = 4
	sbLogBlockSize    = 24
	sbMagic           = 56
	sbFeatureIncompat = 0x60
	sbBlocksHi        = 0x150
	incompat64bit     = 0x80
)

type sectorReader struct{ d disk.Disk }

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

// ext4Size returns the ext4 filesystem size in 512-byte sectors at start, or 0
// if no valid ext4 superblock is found there.
func ext4Size(r sectorReader, start uint64) (uint64, error) {
	// All fields we inspect live in the first 512 bytes of the superblock.
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
	// ext4 block sizes range from 1 KiB (log=0) to 64 KiB (log=6).
	logSize := binary.LittleEndian.Uint32(sb[sbLogBlockSize:])
	if blocks == 0 || logSize > 6 || blocks > (1<<40) {
		return 0, nil
	}
	return blocks * (1024 << logSize) / disk.SectorSize, nil
}

func fatBoot(b []byte) bool {
	return b != nil && b[510] == 0x55 && b[511] == 0xAA &&
		(string(b[54:57]) == "FAT" || string(b[82:85]) == "FAT")
}

// evidence probes for the Amlogic bootloader in sector 1, ext4 superblocks on
// partitions 1-2, and FAT boot sectors on partitions 3-4. Returns the probes
// and the ext4 size of partition 1 (0 if absent).
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

func intact(l *Layout, i int, p report.Partition, diskSectors uint64) bool {
	return p.Type == l.Entries[i].Type && p.Start == l.Entries[i].Start && knownSize(l, i, p.Start, p.Sectors, diskSectors)
}

// otherLayout detects a plausible non-standard partition size at a standard
// start offset so recover leaves custom layouts untouched.
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

type damage struct {
	layout   *Layout
	p1       uint64 // ext4 size on partition 1, in sectors
	evidence []report.Evidence
	rebuilt  []byte
}

// examine inspects the rest of the disk when sector 0 is unrecognized to
// distinguish recoverable damage from a non-mixer or custom layout.
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

// rebuild constructs a replacement sector 0, preserving any intact boot code,
// disk ID, and valid partition entries while regenerating damaged ones.
func rebuild(l *Layout, sector0 []byte, parts []report.Partition, p1, diskSectors uint64) ([]byte, string) {
	out := make([]byte, disk.SectorSize)
	// The Amlogic bootloader lives in sector 1 and doesn't execute MBR boot
	// code, so leave missing boot code zeroed.
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

func tooLarge(l *Layout, p1 uint64) string {
	if p1 <= uint64(l.Entries[0].Sectors) {
		return ""
	}
	return fmt.Sprintf("A firmware update has already been installed onto the larger partition 1: the system on it fills %d sectors, "+
		"more than the %d of the original table. Putting the original table back would shrink partition 1 under that system "+
		"and stop system slot A from starting.", p1, l.Entries[0].Sectors)
}
