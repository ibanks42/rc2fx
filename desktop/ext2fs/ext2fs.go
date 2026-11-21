package ext2fs

import (
	"encoding/binary"
	"hash/crc32"
	"io"
	"math"
	"os"
	"strings"
)

const (
	superblockOffset = 1024
	superblockSize   = 1024
	superMagic       = 0xEF53
	rootIno          = 2
	crc32cChksum     = 1

	minBlockSize = 1024
	maxBlockSize = 65536
	minInodeSize = 128
	minDescSize  = 32
	maxDescSize  = 1024

	// Superblock compatible feature flags.
	compatSparseSuper2 = 0x0200

	// Superblock incompatible feature flags.
	incompatCompression = 0x0001
	incompatFiletype    = 0x0002
	incompatRecover     = 0x0004
	incompatJournalDev  = 0x0008
	incompatMetaBG      = 0x0010
	incompatExtents     = 0x0040
	incompat64Bit       = 0x0080
	incompatMMP         = 0x0100
	incompatFlexBG      = 0x0200
	incompatEAInode     = 0x0400
	incompatDirdata     = 0x1000
	incompatCsumSeed    = 0x2000
	incompatLargeDir    = 0x4000
	incompatInlineData  = 0x8000
	incompatEncrypt     = 0x10000
	incompatCasefold    = 0x20000

	supportedIncompat = incompatFiletype |
		incompatRecover |
		incompatMetaBG |
		incompatExtents |
		incompat64Bit |
		incompatFlexBG |
		incompatEAInode |
		incompatCsumSeed |
		incompatLargeDir

	// Superblock read-only compatible feature flags.
	roCompatSparseSuper   = 0x0001
	roCompatLargeFile     = 0x0002
	roCompatHugeFile      = 0x0008
	roCompatGDTCSum       = 0x0010
	roCompatDirNlink      = 0x0020
	roCompatExtraIsize    = 0x0040
	roCompatQuota         = 0x0100
	roCompatBigalloc      = 0x0200
	roCompatMetadataCsum  = 0x0400
	roCompatReadonly      = 0x1000
	roCompatProject       = 0x2000
	roCompatOrphanPresent = 0x10000

	supportedWritableROCompat = roCompatSparseSuper |
		roCompatLargeFile |
		roCompatHugeFile |
		roCompatGDTCSum |
		roCompatDirNlink |
		roCompatExtraIsize |
		roCompatQuota |
		roCompatMetadataCsum |
		roCompatProject |
		roCompatOrphanPresent
)

var castagnoliTable = crc32.MakeTable(crc32.Castagnoli)

// crc32cRaw computes raw un-finalized Castagnoli CRC32c matching Linux ext4's
// crc32c_le (and e2fsprogs' ext2fs_crc32c_le).
func crc32cRaw(crc uint32, p []byte) uint32 {
	return ^crc32.Update(^crc, castagnoliTable, p)
}

func crc32cU32(crc uint32, v uint32) uint32 {
	var b [4]byte
	binary.LittleEndian.PutUint32(b[:], v)
	return crc32cRaw(crc, b[:])
}

func isPowerOf2(n uint32) bool {
	return n != 0 && (n&(n-1)) == 0
}

// Filesystem represents an open ext4 filesystem. Callers should call Close to
// flush writes. Not safe for concurrent use.
type Filesystem struct {
	file     *os.File
	offset   uint64
	writable bool

	blockSize           uint32
	inodeSize           uint32
	descSize            uint32
	revLevel            uint32
	blocksCount         uint64
	inodesCount         uint32
	firstDataBlock      uint32
	blocksPerGroup      uint32
	inodesPerGroup      uint32
	inodeBlocksPerGroup uint64
	clusterRatioBits    uint32

	featureCompat   uint32
	featureIncompat uint32
	featureROCompat uint32
	csumSeed        uint32

	inodeTables []uint64
}

// Open opens the ext4 filesystem starting at byte offset inside device (a block
// device or image file). It opens read-only unless writable is true.
func Open(device string, offset uint64, writable bool) (*Filesystem, error) {
	if strings.IndexByte(device, 0) >= 0 {
		return nil, invalid("invalid device path")
	}
	if offset > math.MaxInt64-(superblockOffset+superblockSize) {
		return nil, invalid("filesystem offset overflows int64")
	}

	flags := os.O_RDONLY
	if writable {
		flags = os.O_RDWR
	}
	file, err := os.OpenFile(device, flags, 0)
	if err != nil {
		return nil, errf("open %s: %v", device, err)
	}

	fs, err := openFromFile(file, offset, writable)
	if err != nil {
		_ = file.Close()
		return nil, err
	}
	return fs, nil
}

func openFromFile(file *os.File, offset uint64, writable bool) (*Filesystem, error) {
	var sb [superblockSize]byte
	if err := readFullAt(file, int64(offset+superblockOffset), sb[:]); err != nil {
		return nil, errf("read superblock: %v", err)
	}

	le := binary.LittleEndian
	if magic := le.Uint16(sb[0x38:0x3a]); magic != superMagic {
		return nil, errf("bad ext4 superblock magic: 0x%04x", magic)
	}
	revLevel := le.Uint32(sb[0x4c:0x50])
	if revLevel > 1 {
		return nil, errf("unsupported ext4 revision level: %d", revLevel)
	}

	var featureCompat, featureIncompat, featureROCompat uint32
	if revLevel >= 1 {
		featureCompat = le.Uint32(sb[0x5c:0x60])
		featureIncompat = le.Uint32(sb[0x60:0x64])
		featureROCompat = le.Uint32(sb[0x64:0x68])
	}

	hasMetadataCsum := featureROCompat&roCompatMetadataCsum != 0
	if hasMetadataCsum {
		if sb[0x175] != crc32cChksum {
			return nil, errf("unsupported metadata checksum type: %d", sb[0x175])
		}
		got := crc32cRaw(^uint32(0), sb[:0x3fc])
		want := le.Uint32(sb[0x3fc:0x400])
		if got != want {
			return nil, errf("superblock checksum mismatch: got 0x%08x, want 0x%08x", got, want)
		}
	}

	if unsupported := featureIncompat &^ supportedIncompat; unsupported != 0 {
		return nil, errf("unsupported ext4 incompatible features: 0x%08x", unsupported)
	}
	if writable {
		if unsupported := featureROCompat &^ supportedWritableROCompat; unsupported != 0 {
			return nil, errf("unsupported ext4 read-only compatible features for write: 0x%08x", unsupported)
		}
	}

	logBlockSize := le.Uint32(sb[0x18:0x1c])
	if logBlockSize > 6 {
		return nil, errf("invalid s_log_block_size: %d", logBlockSize)
	}
	blockSize := uint32(minBlockSize) << logBlockSize

	logClusterSize := le.Uint32(sb[0x1c:0x20])
	if logClusterSize > 19 || logBlockSize > logClusterSize {
		return nil, errf("invalid s_log_cluster_size: %d", logClusterSize)
	}
	blocksPerGroup := le.Uint32(sb[0x20:0x24])
	clustersPerGroup := le.Uint32(sb[0x24:0x28])
	clusterRatioBits := logClusterSize - logBlockSize
	if featureROCompat&roCompatBigalloc == 0 {
		if logClusterSize != logBlockSize || blocksPerGroup != clustersPerGroup {
			return nil, invalid("corrupt superblock cluster geometry")
		}
	} else if uint64(blocksPerGroup) != uint64(clustersPerGroup)<<clusterRatioBits {
		return nil, invalid("corrupt superblock bigalloc geometry")
	}

	inodeSize := uint32(minInodeSize)
	if revLevel >= 1 {
		inodeSize = uint32(le.Uint16(sb[0x58:0x5a]))
	}
	if inodeSize < minInodeSize || inodeSize > blockSize || !isPowerOf2(inodeSize) {
		return nil, errf("invalid inode size: %d", inodeSize)
	}

	descSize := uint32(minDescSize)
	if featureIncompat&incompat64Bit != 0 {
		descSize = uint32(le.Uint16(sb[0xfe:0x100]))
		if descSize < 64 || descSize > maxDescSize || descSize > blockSize || !isPowerOf2(descSize) {
			return nil, errf("invalid 64-bit group descriptor size: %d", descSize)
		}
	}

	blocksCount := uint64(le.Uint32(sb[0x04:0x08]))
	if featureIncompat&incompat64Bit != 0 {
		blocksCount |= uint64(le.Uint32(sb[0x150:0x154])) << 32
	}
	inodesCount := le.Uint32(sb[0x00:0x04])
	firstDataBlock := le.Uint32(sb[0x14:0x18])
	inodesPerGroup := le.Uint32(sb[0x28:0x2c])
	firstMetaBG := le.Uint32(sb[0x104:0x108])

	if blocksPerGroup < 8 || inodesPerGroup == 0 || inodesCount == 0 || uint64(firstDataBlock) >= blocksCount {
		return nil, invalid("corrupt superblock counts")
	}
	if blocksCount > math.MaxInt64/uint64(blockSize) {
		return nil, invalid("filesystem size overflows int64")
	}

	descPerBlock := blockSize / descSize
	if descPerBlock == 0 {
		return nil, invalid("group descriptor does not fit in a block")
	}

	dataBlocks := blocksCount - uint64(firstDataBlock)
	groupCount := (dataBlocks + uint64(blocksPerGroup) - 1) / uint64(blocksPerGroup)
	if groupCount == 0 || groupCount > math.MaxUint32 {
		return nil, invalid("corrupt block group count")
	}
	if groupCount*uint64(inodesPerGroup) != uint64(inodesCount) {
		return nil, invalid("corrupt inode count in superblock")
	}

	inodeBlocksPerGroup := (uint64(inodesPerGroup)*uint64(inodeSize) + uint64(blockSize) - 1) / uint64(blockSize)

	var csumSeed uint32
	if featureIncompat&incompatCsumSeed != 0 {
		csumSeed = le.Uint32(sb[0x270:0x274])
	} else if hasMetadataCsum {
		csumSeed = crc32cRaw(^uint32(0), sb[0x68:0x78])
	}

	backupBGs := [2]uint32{
		le.Uint32(sb[0x24c:0x250]),
		le.Uint32(sb[0x250:0x254]),
	}

	fs := &Filesystem{
		file:                file,
		offset:              offset,
		writable:            writable,
		blockSize:           blockSize,
		inodeSize:           inodeSize,
		descSize:            descSize,
		revLevel:            revLevel,
		blocksCount:         blocksCount,
		inodesCount:         inodesCount,
		firstDataBlock:      firstDataBlock,
		blocksPerGroup:      blocksPerGroup,
		inodesPerGroup:      inodesPerGroup,
		inodeBlocksPerGroup: inodeBlocksPerGroup,
		clusterRatioBits:    clusterRatioBits,
		featureCompat:       featureCompat,
		featureIncompat:     featureIncompat,
		featureROCompat:     featureROCompat,
		csumSeed:            csumSeed,
		inodeTables:         make([]uint64, groupCount),
	}

	if err := fs.loadGroupDescriptors(descPerBlock, firstMetaBG, backupBGs); err != nil {
		return nil, err
	}
	return fs, nil
}

func (f *Filesystem) loadGroupDescriptors(descPerBlock, firstMetaBG uint32, backupBGs [2]uint32) error {
	groupCount := uint32(len(f.inodeTables))
	descBlocks := (groupCount + descPerBlock - 1) / descPerBlock
	hasMetaBG := f.featureIncompat&incompatMetaBG != 0
	if hasMetaBG && firstMetaBG > descBlocks {
		return invalid("corrupt s_first_meta_bg in superblock")
	}

	gdtStart := uint64(f.firstDataBlock) + 1
	if f.blockSize == minBlockSize && f.firstDataBlock == 0 {
		// Block 0 is reserved for bootloaders on 1 KiB filesystems; primary SB is block 1, GDT starts at block 2.
		gdtStart = 2
	}

	le := binary.LittleEndian
	buf := make([]byte, f.blockSize)
	zeroDesc := make([]byte, f.descSize)

	for i := uint32(0); i < descBlocks; i++ {
		var blk uint64
		if hasMetaBG && i >= firstMetaBG {
			bg := i * descPerBlock
			blk = uint64(f.firstDataBlock) + uint64(bg)*uint64(f.blocksPerGroup)
			if f.bgHasSuper(bg, backupBGs) {
				blk++
			}
			if i == 0 && f.blockSize == minBlockSize && f.clusterRatioBits > 0 {
				blk++
			}
		} else {
			blk = gdtStart + uint64(i)
		}

		if err := f.readBlock(blk, buf); err != nil {
			return errf("read group descriptor block %d: %v", blk, err)
		}

		for slot := uint32(0); slot < descPerBlock; slot++ {
			g := i*descPerBlock + slot
			if g >= groupCount {
				break
			}
			desc := buf[slot*f.descSize : (slot+1)*f.descSize]
			if f.hasMetadataCsum() {
				copy(zeroDesc, desc)
				zeroDesc[0x1e] = 0
				zeroDesc[0x1f] = 0
				crc := crc32cU32(f.csumSeed, g)
				crc = crc32cRaw(crc, zeroDesc)
				want := le.Uint16(desc[0x1e:0x20])
				if uint16(crc) != want {
					return errf("group %d descriptor checksum mismatch", g)
				}
			}

			itable := uint64(le.Uint32(desc[0x08:0x0c]))
			if f.is64Bit() {
				itable |= uint64(le.Uint32(desc[0x28:0x2c])) << 32
			}
			f.inodeTables[g] = itable
		}
	}
	return nil
}

func (f *Filesystem) bgHasSuper(group uint32, backupBGs [2]uint32) bool {
	if group == 0 {
		return true
	}
	if f.featureCompat&compatSparseSuper2 != 0 {
		return group == backupBGs[0] || group == backupBGs[1]
	}
	if group <= 1 || f.featureROCompat&roCompatSparseSuper == 0 {
		return true
	}
	if group&1 == 0 {
		return false
	}
	return isPowerOf(group, 3) || isPowerOf(group, 5) || isPowerOf(group, 7)
}

func isPowerOf(n, base uint32) bool {
	if n == 0 {
		return false
	}
	for n%base == 0 {
		n /= base
	}
	return n == 1
}

func (f *Filesystem) hasMetadataCsum() bool { return f.featureROCompat&roCompatMetadataCsum != 0 }
func (f *Filesystem) is64Bit() bool         { return f.featureIncompat&incompat64Bit != 0 }
func (f *Filesystem) hasFiletype() bool     { return f.featureIncompat&incompatFiletype != 0 }

func (f *Filesystem) checkOpen() error {
	if f == nil || f.file == nil {
		return invalid("the filesystem is closed")
	}
	return nil
}

// NeedsRecovery reports whether the journal has unreplayed transactions
// (meaning the filesystem wasn't unmounted cleanly and reads may be stale).
func (f *Filesystem) NeedsRecovery() bool {
	if f == nil || f.file == nil {
		return false
	}
	return f.featureIncompat&incompatRecover != 0
}

// Close flushes pending writes and releases the handle. Subsequent calls are
// no-ops.
func (f *Filesystem) Close() error {
	if f == nil || f.file == nil {
		return nil
	}
	file := f.file
	f.file = nil
	var syncErr error
	if f.writable {
		syncErr = file.Sync()
	}
	closeErr := file.Close()
	if syncErr != nil {
		return syncErr
	}
	return closeErr
}

func (f *Filesystem) blockOffset(blk uint64) (int64, error) {
	if blk >= f.blocksCount {
		return 0, errf("block %d out of range (blocks=%d)", blk, f.blocksCount)
	}
	byteOff := blk * uint64(f.blockSize)
	if byteOff > math.MaxInt64-f.offset || f.offset+byteOff > math.MaxInt64-uint64(f.blockSize) {
		return 0, invalid("block offset overflows int64")
	}
	return int64(f.offset + byteOff), nil
}

func (f *Filesystem) readBlock(blk uint64, dst []byte) error {
	if uint32(len(dst)) != f.blockSize {
		return invalid("block buffer size mismatch")
	}
	off, err := f.blockOffset(blk)
	if err != nil {
		return err
	}
	return readFullAt(f.file, off, dst)
}

func (f *Filesystem) writeBlock(blk uint64, src []byte) error {
	if uint32(len(src)) != f.blockSize {
		return invalid("block buffer size mismatch")
	}
	off, err := f.blockOffset(blk)
	if err != nil {
		return err
	}
	return writeFullAt(f.file, off, src)
}

func readFullAt(r io.ReaderAt, off int64, dst []byte) error {
	for len(dst) > 0 {
		n, err := r.ReadAt(dst, off)
		dst = dst[n:]
		off += int64(n)
		if err != nil {
			if err == io.EOF && len(dst) == 0 {
				return nil
			}
			return err
		}
		if n == 0 {
			return io.ErrUnexpectedEOF
		}
	}
	return nil
}

func writeFullAt(w io.WriterAt, off int64, src []byte) error {
	for len(src) > 0 {
		n, err := w.WriteAt(src, off)
		src = src[n:]
		off += int64(n)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
	}
	return nil
}
