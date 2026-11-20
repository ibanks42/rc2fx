package fix

import (
	"bytes"
	"encoding/binary"
	"errors"
	"io"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
)

// testSectors is the size of an 8 GB eMMC; any size past partition 4's
// start works.
const testSectors = 15269888

// refSector0 is a Pro II's sector 0 with partition 1 at p1 sectors.
func refSector0(p1 uint32) []byte {
	b := make([]byte, disk.SectorSize)
	for i := range 440 {
		b[i] = byte(i*7 + 3) // stand-in boot code
	}
	copy(b[440:444], []byte{0x78, 0x56, 0x34, 0x12})
	entries := [4][2]uint32{
		{8192, p1},
		{2000896, 1193984},
		{3223552, 1710080},
		{4941824, testSectors - 4941824},
	}
	for i, e := range entries {
		o := 446 + 16*i
		copy(b[o+1:o+4], []byte{0xfe, 0xff, 0xff})
		b[o+4] = 0x83
		copy(b[o+5:o+8], []byte{0xfe, 0xff, 0xff})
		binary.LittleEndian.PutUint32(b[o+8:], e[0])
		binary.LittleEndian.PutUint32(b[o+12:], e[1])
	}
	b[510], b[511] = 0x55, 0xAA
	return b
}

func factory() []byte    { return refSector0(1039458) }
func fixedTable() []byte { return refSector0(1193984) }

var errInjected = errors.New("injected failure")

// fakeDisk is a sparse in-memory disk that can fail on cue.
type fakeDisk struct {
	sectors uint64
	data    map[int64][]byte

	writes, syncs int
	wrote         bool

	readErr        error         // every read fails
	readFailsAfter int           // after the first write, this many reads of sector 0 fail
	corruptAfter   int           // after the first write, this many reads of sector 0 come back altered
	writeErr       map[int]error // by write number, from 1
	shortWrite     map[int]int   // write number → bytes actually stored
	syncErr        map[int]error // by sync number, from 1
}

func newFake(sector0 []byte) *fakeDisk {
	f := &fakeDisk{sectors: testSectors, data: map[int64][]byte{}}
	f.put(0, sector0)
	return f
}

func (f *fakeDisk) put(sector int64, b []byte) {
	s := make([]byte, disk.SectorSize)
	copy(s, b)
	f.data[sector] = s
}

func (f *fakeDisk) sector(n int64) []byte {
	if s, ok := f.data[n]; ok {
		return s
	}
	return make([]byte, disk.SectorSize)
}

func (f *fakeDisk) ReadAt(p []byte, off int64) (int, error) {
	if off%disk.SectorSize != 0 || len(p)%disk.SectorSize != 0 {
		panic("unaligned read")
	}
	if f.readErr != nil {
		return 0, f.readErr
	}
	if off == 0 && f.wrote {
		if f.readFailsAfter > 0 {
			f.readFailsAfter--
			return 0, errInjected
		}
	}
	n := 0
	for n < len(p) {
		s := (off + int64(n)) / disk.SectorSize
		if uint64(s) >= f.sectors {
			return n, io.EOF
		}
		copy(p[n:], f.sector(s))
		n += disk.SectorSize
	}
	if off == 0 && f.wrote && f.corruptAfter > 0 {
		f.corruptAfter--
		p[0] ^= 0xff
	}
	return n, nil
}

func (f *fakeDisk) WriteAt(p []byte, off int64) (int, error) {
	if off%disk.SectorSize != 0 {
		panic("unaligned write")
	}
	f.writes++
	if err := f.writeErr[f.writes]; err != nil {
		return 0, err
	}
	n := len(p)
	if k, ok := f.shortWrite[f.writes]; ok {
		n = k
	}
	f.wrote = true
	for i := 0; i < n; i += disk.SectorSize {
		s := (off + int64(i)) / disk.SectorSize
		cur := append([]byte(nil), f.sector(s)...)
		copy(cur, p[i:n])
		f.data[s] = cur
	}
	return n, nil
}

func (f *fakeDisk) Sync() error {
	f.syncs++
	return f.syncErr[f.syncs]
}

func (f *fakeDisk) Sectors() uint64 { return f.sectors }
func (f *fakeDisk) Close() error    { return nil }

func (f *fakeDisk) sector0() []byte { return f.sector(0) }

func (f *fakeDisk) equal(b []byte) bool { return bytes.Equal(f.sector0(), b) }
