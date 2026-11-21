package ext2fs

import (
	"encoding/binary"
	"math"
)

const (
	modeMask = 0o170000
	modeReg  = 0o100000
	modeDir  = 0o040000
	modeLnk  = 0o120000

	inodeComprFl      = 0x00000004
	inodeEncryptFl    = 0x00000800
	inodeExtentsFl    = 0x00080000
	inodeVerityFl     = 0x00100000
	inodeInlineDataFl = 0x10000000

	inodeInlineBlocksOffset = 0x28
	inodeInlineBlocksSize   = 60

	extMagic          = 0xF30A
	extMaxDepth       = 5
	extInitMaxLen     = 32768
	extHeaderSize     = 12
	extEntrySize      = 12
	extTailSize       = 4
	extInodeMaxCounts = 4
)

type inode struct {
	ino        uint32
	mode       uint16
	size       uint64
	flags      uint32
	generation uint32
	raw        []byte
}

func (in *inode) isRegular() bool { return in.mode&modeMask == modeReg }
func (in *inode) isDir() bool     { return in.mode&modeMask == modeDir }
func (in *inode) isSymlink() bool { return in.mode&modeMask == modeLnk }

func (f *Filesystem) inodeLocation(ino uint32) (blk uint64, offInBlk uint32, err error) {
	if ino == 0 || ino > f.inodesCount {
		return 0, 0, errf("inode %d out of range", ino)
	}
	group := uint64(ino-1) / uint64(f.inodesPerGroup)
	index := uint64(ino-1) % uint64(f.inodesPerGroup)
	itable := f.inodeTables[group]
	if itable < uint64(f.firstDataBlock) || itable+f.inodeBlocksPerGroup > f.blocksCount {
		return 0, 0, errf("corrupt inode table block %d for group %d", itable, group)
	}
	byteOff := index * uint64(f.inodeSize)
	return itable + byteOff/uint64(f.blockSize), uint32(byteOff % uint64(f.blockSize)), nil
}

func inodeHasChecksumHi(raw []byte) bool {
	if len(raw) <= minInodeSize {
		return false
	}
	// EXT4_INODE_CSUM_HI_EXTRA_END = offsetof(i_checksum_hi)+2 - 128 = 4.
	return binary.LittleEndian.Uint16(raw[0x80:0x82]) >= 4
}

func (f *Filesystem) inodeChecksum(ino uint32, raw []byte, hasHi bool) uint32 {
	buf := make([]byte, len(raw))
	copy(buf, raw)
	buf[0x7c] = 0
	buf[0x7d] = 0
	if hasHi {
		buf[0x82] = 0
		buf[0x83] = 0
	}
	crc := crc32cU32(f.csumSeed, ino)
	crc = crc32cRaw(crc, buf[0x64:0x68]) // i_generation (little-endian)
	return crc32cRaw(crc, buf)
}

func (f *Filesystem) readInode(ino uint32) (*inode, error) {
	blk, offInBlk, err := f.inodeLocation(ino)
	if err != nil {
		return nil, err
	}
	blkBuf := make([]byte, f.blockSize)
	if err := f.readBlock(blk, blkBuf); err != nil {
		return nil, errf("read inode %d: %v", ino, err)
	}
	raw := make([]byte, f.inodeSize)
	copy(raw, blkBuf[offInBlk:offInBlk+f.inodeSize])

	le := binary.LittleEndian
	if f.hasMetadataCsum() {
		hasHi := inodeHasChecksumHi(raw)
		want := uint32(le.Uint16(raw[0x7c:0x7e]))
		got := f.inodeChecksum(ino, raw, hasHi)
		if hasHi {
			want |= uint32(le.Uint16(raw[0x82:0x84])) << 16
		} else {
			got &= 0xFFFF
		}
		if got != want {
			// Match e2fsprogs: an all-zero uninitialized base inode slot has no checksum set yet.
			allZero := true
			for _, b := range raw[:minInodeSize] {
				if b != 0 {
					allZero = false
					break
				}
			}
			if !allZero {
				return nil, errf("inode %d checksum mismatch: got 0x%08x, want 0x%08x", ino, got, want)
			}
		}
	}

	size := uint64(le.Uint32(raw[0x04:0x08])) | (uint64(le.Uint32(raw[0x6c:0x70])) << 32)
	return &inode{
		ino:        ino,
		mode:       le.Uint16(raw[0x00:0x02]),
		size:       size,
		flags:      le.Uint32(raw[0x20:0x24]),
		generation: le.Uint32(raw[0x64:0x68]),
		raw:        raw,
	}, nil
}

func (f *Filesystem) writeInode(in *inode) error {
	blk, offInBlk, err := f.inodeLocation(in.ino)
	if err != nil {
		return err
	}
	if f.hasMetadataCsum() {
		hasHi := inodeHasChecksumHi(in.raw)
		crc := f.inodeChecksum(in.ino, in.raw, hasHi)
		binary.LittleEndian.PutUint16(in.raw[0x7c:0x7e], uint16(crc))
		if hasHi {
			binary.LittleEndian.PutUint16(in.raw[0x82:0x84], uint16(crc>>16))
		}
	}
	blkBuf := make([]byte, f.blockSize)
	if err := f.readBlock(blk, blkBuf); err != nil {
		return errf("read inode %d block: %v", in.ino, err)
	}
	copy(blkBuf[offInBlk:offInBlk+f.inodeSize], in.raw)
	return f.writeBlock(blk, blkBuf)
}

type fileExtent struct {
	logical   uint32
	length    uint32
	physical  uint64
	unwritten bool
	leafBlock uint64 // 0 when stored inline inside inode i_block
	entryIdx  int
}

func (f *Filesystem) extentBlockChecksum(in *inode, blk []byte, maxEntries int) uint32 {
	tailOff := extHeaderSize + maxEntries*extEntrySize
	crc := crc32cU32(f.csumSeed, in.ino)
	crc = crc32cU32(crc, in.generation)
	return crc32cRaw(crc, blk[:tailOff])
}

func (f *Filesystem) collectExtents(in *inode) ([]fileExtent, error) {
	root := in.raw[inodeInlineBlocksOffset : inodeInlineBlocksOffset+inodeInlineBlocksSize]
	visited := make(map[uint64]bool)
	var out []fileExtent
	var prevEnd uint64
	if err := f.walkExtentNode(in, root, 0, -1, visited, &prevEnd, &out); err != nil {
		return nil, err
	}
	return out, nil
}

func (f *Filesystem) walkExtentNode(
	in *inode,
	node []byte,
	leafBlock uint64,
	expectedDepth int,
	visited map[uint64]bool,
	prevEnd *uint64,
	out *[]fileExtent,
) error {
	le := binary.LittleEndian
	magic := le.Uint16(node[0:2])
	entries := int(le.Uint16(node[2:4]))
	maxEntries := int(le.Uint16(node[4:6]))
	depth := int(le.Uint16(node[6:8]))

	if magic != extMagic {
		return errf("inode %d bad extent magic: 0x%04x", in.ino, magic)
	}
	if depth > extMaxDepth || (expectedDepth >= 0 && depth != expectedDepth) {
		return errf("inode %d invalid extent depth %d", in.ino, depth)
	}
	if entries > maxEntries || maxEntries == 0 {
		return errf("inode %d invalid extent counts (%d/%d)", in.ino, entries, maxEntries)
	}
	if leafBlock == 0 {
		if maxEntries > extInodeMaxCounts {
			return errf("inode %d invalid inline extent capacity %d", in.ino, maxEntries)
		}
	} else {
		tailOff := extHeaderSize + maxEntries*extEntrySize
		need := tailOff
		if f.hasMetadataCsum() {
			need += extTailSize
		}
		if need > len(node) {
			return errf("inode %d extent block %d capacity %d overflows block", in.ino, leafBlock, maxEntries)
		}
		if f.hasMetadataCsum() {
			got := f.extentBlockChecksum(in, node, maxEntries)
			want := le.Uint32(node[tailOff : tailOff+extTailSize])
			if got != want {
				return errf("inode %d extent block %d checksum mismatch: got 0x%08x, want 0x%08x", in.ino, leafBlock, got, want)
			}
		}
	}

	if depth == 0 {
		for i := 0; i < entries; i++ {
			rec := node[extHeaderSize+i*extEntrySize : extHeaderSize+(i+1)*extEntrySize]
			logical := le.Uint32(rec[0:4])
			rawLen := le.Uint16(rec[4:6])
			physHi := le.Uint16(rec[6:8])
			physLo := le.Uint32(rec[8:12])
			if rawLen == 0 {
				return errf("inode %d zero-length extent at logical %d", in.ino, logical)
			}
			unwritten := rawLen > extInitMaxLen
			length := uint32(rawLen)
			if unwritten {
				length -= extInitMaxLen
			}
			if uint64(logical) < *prevEnd {
				return errf("inode %d overlapping or unsorted extents at logical %d", in.ino, logical)
			}
			physical := uint64(physLo) | (uint64(physHi) << 32)
			if physical == 0 || physical+uint64(length) > f.blocksCount {
				return errf("inode %d extent physical range [%d, +%d) out of bounds", in.ino, physical, length)
			}
			*prevEnd = uint64(logical) + uint64(length)
			*out = append(*out, fileExtent{
				logical:   logical,
				length:    length,
				physical:  physical,
				unwritten: unwritten,
				leafBlock: leafBlock,
				entryIdx:  i,
			})
		}
		return nil
	}

	childBuf := make([]byte, f.blockSize)
	for i := 0; i < entries; i++ {
		rec := node[extHeaderSize+i*extEntrySize : extHeaderSize+(i+1)*extEntrySize]
		childBlock := uint64(le.Uint32(rec[4:8])) | (uint64(le.Uint16(rec[8:10])) << 32)
		if childBlock == 0 || childBlock >= f.blocksCount {
			return errf("inode %d extent index child block %d out of range", in.ino, childBlock)
		}
		if visited[childBlock] {
			return errf("inode %d cycle detected in extent tree at block %d", in.ino, childBlock)
		}
		visited[childBlock] = true
		if err := f.readBlock(childBlock, childBuf); err != nil {
			return errf("inode %d read extent block %d: %v", in.ino, childBlock, err)
		}
		if err := f.walkExtentNode(in, childBuf, childBlock, depth-1, visited, prevEnd, out); err != nil {
			return err
		}
	}
	return nil
}

type indirectCache struct {
	fs          *Filesystem
	addrsPerBlk uint64
	indBlk      uint64
	indBuf      []byte
	dindBlk     uint64
	dindBuf     []byte
	tindBlk     uint64
	tindBuf     []byte
}

func newIndirectCache(fs *Filesystem) *indirectCache {
	return &indirectCache{
		fs:          fs,
		addrsPerBlk: uint64(fs.blockSize) / 4,
	}
}

func (c *indirectCache) readPtr(blk uint64, cachedBlk *uint64, cachedBuf *[]byte, index uint64) (uint64, error) {
	if blk == 0 {
		return 0, nil
	}
	if *cachedBuf == nil {
		*cachedBuf = make([]byte, c.fs.blockSize)
	}
	if *cachedBlk != blk {
		if err := c.fs.readBlock(blk, *cachedBuf); err != nil {
			return 0, err
		}
		*cachedBlk = blk
	}
	phys := uint64(binary.LittleEndian.Uint32((*cachedBuf)[index*4 : (index+1)*4]))
	if phys >= c.fs.blocksCount {
		return 0, errf("indirect block pointer %d out of range", phys)
	}
	return phys, nil
}

func (c *indirectCache) lookup(in *inode, lblk uint64) (uint64, error) {
	le := binary.LittleEndian
	iBlock := in.raw[inodeInlineBlocksOffset : inodeInlineBlocksOffset+inodeInlineBlocksSize]
	if lblk < 12 {
		phys := uint64(le.Uint32(iBlock[lblk*4 : (lblk+1)*4]))
		if phys >= c.fs.blocksCount {
			return 0, errf("direct block pointer %d out of range", phys)
		}
		return phys, nil
	}
	lblk -= 12
	n := c.addrsPerBlk
	if lblk < n {
		ind := uint64(le.Uint32(iBlock[12*4 : 13*4]))
		return c.readPtr(ind, &c.indBlk, &c.indBuf, lblk)
	}
	lblk -= n
	if lblk < n*n {
		dind := uint64(le.Uint32(iBlock[13*4 : 14*4]))
		ind, err := c.readPtr(dind, &c.dindBlk, &c.dindBuf, lblk/n)
		if err != nil || ind == 0 {
			return 0, err
		}
		return c.readPtr(ind, &c.indBlk, &c.indBuf, lblk%n)
	}
	lblk -= n * n
	if lblk < n*n*n {
		tind := uint64(le.Uint32(iBlock[14*4 : 15*4]))
		dind, err := c.readPtr(tind, &c.tindBlk, &c.tindBuf, lblk/(n*n))
		if err != nil || dind == 0 {
			return 0, err
		}
		rem := lblk % (n * n)
		ind, err := c.readPtr(dind, &c.dindBlk, &c.dindBuf, rem/n)
		if err != nil || ind == 0 {
			return 0, err
		}
		return c.readPtr(ind, &c.indBlk, &c.indBuf, rem%n)
	}
	return 0, invalid("logical block offset exceeds indirect addressing range")
}

// Read returns the full contents of a regular file.
func (f *Filesystem) Read(entry Entry) ([]byte, error) {
	if err := f.checkOpen(); err != nil {
		return nil, err
	}
	if !entry.Regular {
		return nil, invalid("not a regular file")
	}
	if entry.Size > math.MaxInt {
		return nil, invalid("file too large")
	}
	in, err := f.readInode(entry.Inode)
	if err != nil {
		return nil, err
	}
	if !in.isRegular() {
		return nil, invalid("not a regular file")
	}
	if in.size != entry.Size {
		return nil, errf("inode %d size changed: got %d, expected %d", entry.Inode, in.size, entry.Size)
	}
	if in.flags&(inodeInlineDataFl|inodeEncryptFl|inodeComprFl) != 0 {
		return nil, errf("inode %d uses unsupported flags 0x%08x", entry.Inode, in.flags)
	}

	data := make([]byte, int(entry.Size))
	if len(data) == 0 {
		return data, nil
	}
	if err := f.readInodeInto(in, data); err != nil {
		return nil, err
	}
	return data, nil
}

func (f *Filesystem) readInodeInto(in *inode, dst []byte) error {
	if in.flags&inodeExtentsFl != 0 {
		extents, err := f.collectExtents(in)
		if err != nil {
			return err
		}
		bs := uint64(f.blockSize)
		total := uint64(len(dst))
		for _, ext := range extents {
			if ext.unwritten {
				continue
			}
			startByte := uint64(ext.logical) * bs
			if startByte >= total {
				break
			}
			n := uint64(ext.length) * bs
			if startByte+n > total {
				n = total - startByte
			}
			off, err := f.blockOffset(ext.physical)
			if err != nil {
				return err
			}
			if err := readFullAt(f.file, off, dst[startByte:startByte+n]); err != nil {
				return errf("read inode %d extent data: %v", in.ino, err)
			}
		}
		return nil
	}

	bs := uint64(f.blockSize)
	numBlocks := (uint64(len(dst)) + bs - 1) / bs
	ind := newIndirectCache(f)
	for lblk := uint64(0); lblk < numBlocks; lblk++ {
		phys, err := ind.lookup(in, lblk)
		if err != nil {
			return err
		}
		if phys == 0 {
			continue
		}
		start := lblk * bs
		end := start + bs
		if end > uint64(len(dst)) {
			end = uint64(len(dst))
		}
		off, err := f.blockOffset(phys)
		if err != nil {
			return err
		}
		if err := readFullAt(f.file, off, dst[start:end]); err != nil {
			return errf("read inode %d block %d: %v", in.ino, phys, err)
		}
	}
	return nil
}

// Overwrite replaces a regular file's contents in place. len(data) must match
// entry.Size exactly; metadata and file size are left untouched (except for
// clearing the unwritten flag on preallocated extents whose blocks are written).
func (f *Filesystem) Overwrite(entry Entry, data []byte) error {
	if err := f.checkOpen(); err != nil {
		return err
	}
	if !f.writable {
		return invalid("the filesystem is open read-only")
	}
	if !entry.Regular || entry.Size != uint64(len(data)) {
		return invalid("only a regular file's contents of the same size are replaced")
	}
	in, err := f.readInode(entry.Inode)
	if err != nil {
		return err
	}
	if !in.isRegular() {
		return invalid("not a regular file")
	}
	if in.size != entry.Size {
		return errf("inode %d size changed: got %d, expected %d", entry.Inode, in.size, entry.Size)
	}
	if in.flags&(inodeInlineDataFl|inodeEncryptFl|inodeComprFl|inodeVerityFl) != 0 {
		return errf("inode %d uses unsupported flags 0x%08x", entry.Inode, in.flags)
	}
	if len(data) == 0 {
		return nil
	}

	if in.flags&inodeExtentsFl != 0 {
		return f.overwriteExtents(in, data)
	}
	return f.overwriteIndirect(in, data)
}

func (f *Filesystem) overwriteExtents(in *inode, data []byte) error {
	extents, err := f.collectExtents(in)
	if err != nil {
		return err
	}
	bs := uint64(f.blockSize)
	total := uint64(len(data))
	numBlocks := (total + bs - 1) / bs

	// Validate full contiguous coverage [0, numBlocks) and any straddling split capacity before writing.
	var covered uint64
	var active []fileExtent
	for _, ext := range extents {
		if uint64(ext.logical) >= numBlocks {
			break
		}
		if uint64(ext.logical) > covered {
			return invalid("cannot overwrite file with sparse hole")
		}
		active = append(active, ext)
		covered = uint64(ext.logical) + uint64(ext.length)
	}
	if covered < numBlocks {
		return invalid("cannot overwrite file with sparse hole")
	}

	if len(active) > 0 {
		last := active[len(active)-1]
		if last.unwritten && uint64(last.logical)+uint64(last.length) > numBlocks {
			node, err := f.loadExtentNode(in, last.leafBlock)
			if err != nil {
				return err
			}
			entries := binary.LittleEndian.Uint16(node[2:4])
			maxEntries := binary.LittleEndian.Uint16(node[4:6])
			if entries >= maxEntries {
				return invalid("cannot split straddling unwritten extent without allocating extent block")
			}
		}
	}

	// Write data blocks in place; always zero-pad any partial tail block to full blockSize.
	for _, ext := range active {
		extEnd := uint64(ext.logical) + uint64(ext.length)
		if extEnd > numBlocks {
			extEnd = numBlocks
		}
		coverBlocks := extEnd - uint64(ext.logical)
		startByte := uint64(ext.logical) * bs
		endByte := startByte + coverBlocks*bs

		if endByte <= total {
			off, err := f.blockOffset(ext.physical)
			if err != nil {
				return err
			}
			if err := writeFullAt(f.file, off, data[startByte:endByte]); err != nil {
				return errf("write inode %d extent data: %v", in.ino, err)
			}
			continue
		}

		if coverBlocks > 1 {
			fullEnd := startByte + (coverBlocks-1)*bs
			off, err := f.blockOffset(ext.physical)
			if err != nil {
				return err
			}
			if err := writeFullAt(f.file, off, data[startByte:fullEnd]); err != nil {
				return errf("write inode %d extent data: %v", in.ino, err)
			}
		}
		tailStart := startByte + (coverBlocks-1)*bs
		tailBuf := make([]byte, f.blockSize)
		copy(tailBuf, data[tailStart:total])
		if err := f.writeBlock(ext.physical+coverBlocks-1, tailBuf); err != nil {
			return errf("write inode %d tail block: %v", in.ino, err)
		}
	}

	return f.markExtentsInitialized(in, active, numBlocks)
}

func (f *Filesystem) loadExtentNode(in *inode, leafBlock uint64) ([]byte, error) {
	if leafBlock == 0 {
		return in.raw[inodeInlineBlocksOffset : inodeInlineBlocksOffset+inodeInlineBlocksSize], nil
	}
	buf := make([]byte, f.blockSize)
	if err := f.readBlock(leafBlock, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

func (f *Filesystem) markExtentsInitialized(in *inode, active []fileExtent, numBlocks uint64) error {
	// Group overlapping unwritten extents by leaf block (0 = inline inode).
	var order []uint64
	grouped := make(map[uint64][]fileExtent)
	for _, ext := range active {
		if !ext.unwritten {
			continue
		}
		if _, ok := grouped[ext.leafBlock]; !ok {
			order = append(order, ext.leafBlock)
		}
		grouped[ext.leafBlock] = append(grouped[ext.leafBlock], ext)
	}
	if len(order) == 0 {
		return nil
	}

	le := binary.LittleEndian
	for _, leafBlock := range order {
		node, err := f.loadExtentNode(in, leafBlock)
		if err != nil {
			return err
		}
		// Apply in reverse entry order so splitting the last overlapping extent keeps earlier indices valid.
		list := grouped[leafBlock]
		for i := len(list) - 1; i >= 0; i-- {
			ext := list[i]
			if uint64(ext.logical)+uint64(ext.length) > numBlocks {
				entries := int(le.Uint16(node[2:4]))
				writtenLen := uint16(numBlocks - uint64(ext.logical))
				remainLen := uint16(ext.length - uint32(writtenLen))

				// Shift trailing entries right by one slot.
				for j := entries - 1; j > ext.entryIdx; j-- {
					src := node[extHeaderSize+j*extEntrySize : extHeaderSize+(j+1)*extEntrySize]
					dst := node[extHeaderSize+(j+1)*extEntrySize : extHeaderSize+(j+2)*extEntrySize]
					copy(dst, src)
				}
				le.PutUint16(node[2:4], uint16(entries+1))

				first := node[extHeaderSize+ext.entryIdx*extEntrySize : extHeaderSize+(ext.entryIdx+1)*extEntrySize]
				le.PutUint16(first[4:6], writtenLen)

				second := node[extHeaderSize+(ext.entryIdx+1)*extEntrySize : extHeaderSize+(ext.entryIdx+2)*extEntrySize]
				le.PutUint32(second[0:4], uint32(numBlocks))
				le.PutUint16(second[4:6], extInitMaxLen+remainLen)
				remPhys := ext.physical + uint64(writtenLen)
				le.PutUint16(second[6:8], uint16(remPhys>>32))
				le.PutUint32(second[8:12], uint32(remPhys))
			} else {
				rec := node[extHeaderSize+ext.entryIdx*extEntrySize : extHeaderSize+(ext.entryIdx+1)*extEntrySize]
				le.PutUint16(rec[4:6], uint16(ext.length))
			}
		}

		if leafBlock == 0 {
			if err := f.writeInode(in); err != nil {
				return err
			}
			continue
		}
		if f.hasMetadataCsum() {
			maxEntries := int(le.Uint16(node[4:6]))
			tailOff := extHeaderSize + maxEntries*extEntrySize
			crc := f.extentBlockChecksum(in, node, maxEntries)
			le.PutUint32(node[tailOff:tailOff+extTailSize], crc)
		}
		if err := f.writeBlock(leafBlock, node); err != nil {
			return err
		}
	}
	return nil
}

func (f *Filesystem) overwriteIndirect(in *inode, data []byte) error {
	bs := uint64(f.blockSize)
	total := uint64(len(data))
	numBlocks := (total + bs - 1) / bs
	physBlocks := make([]uint64, numBlocks)
	ind := newIndirectCache(f)
	for lblk := uint64(0); lblk < numBlocks; lblk++ {
		phys, err := ind.lookup(in, lblk)
		if err != nil {
			return err
		}
		if phys == 0 {
			return invalid("cannot overwrite file with sparse hole")
		}
		physBlocks[lblk] = phys
	}

	var tailBuf []byte
	for lblk, phys := range physBlocks {
		start := uint64(lblk) * bs
		if start+bs <= total {
			off, err := f.blockOffset(phys)
			if err != nil {
				return err
			}
			if err := writeFullAt(f.file, off, data[start:start+bs]); err != nil {
				return errf("write inode %d block %d: %v", in.ino, phys, err)
			}
			continue
		}
		if tailBuf == nil {
			tailBuf = make([]byte, f.blockSize)
		}
		copy(tailBuf, data[start:total])
		if err := f.writeBlock(phys, tailBuf); err != nil {
			return errf("write inode %d tail block %d: %v", in.ino, phys, err)
		}
	}
	return nil
}
