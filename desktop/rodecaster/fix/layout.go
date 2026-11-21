// Package fix parses a mixer's MBR partition table and saves durable backups
// of sector 0.
package fix

import (
	"encoding/binary"
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
)

const (
	tableOffset = 446
	entrySize   = 16
)

// Partition is one entry of the table, in 512-byte sectors.
type Partition struct {
	Number  int    `json:"number"`
	Type    byte   `json:"type"`
	Start   uint32 `json:"start"`
	Sectors uint32 `json:"sectors"`
}

func entry(sector0 []byte, i int) []byte {
	o := tableOffset + entrySize*i
	return sector0[o : o+entrySize]
}

func parse(sector0 []byte) [4]Partition {
	var parts [4]Partition
	for i := range parts {
		e := entry(sector0, i)
		parts[i] = Partition{
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
func ParseMBR(sector0 []byte, diskSectors uint64) ([4]Partition, error) {
	var parts [4]Partition
	if len(sector0) != disk.SectorSize || sector0[510] != 0x55 || sector0[511] != 0xAA {
		return parts, ErrNoPartitionTable
	}
	parsed := parse(sector0)
	var end uint64 = 1 // sector 0 holds the MBR itself
	for _, p := range parsed {
		start, count := uint64(p.Start), uint64(p.Sectors)
		if p.Type == 0 || count == 0 || start < end || start+count > diskSectors {
			return parts, ErrInvalidMBR
		}
		end = start + count
	}
	return parsed, nil
}
