package ext2fs

import (
	"encoding/binary"
	"strings"
)

const (
	maxSymlinkDepth = 40
	maxSymlinkBytes = 4096
	maxRecLen64K    = (1 << 16) - 1
)

// Lookup resolves path relative to the filesystem root, returning nil if it
// does not exist. A trailing symlink is not followed.
func (f *Filesystem) Lookup(path string) (*Entry, error) {
	if err := f.checkOpen(); err != nil {
		return nil, err
	}
	if strings.IndexByte(path, 0) >= 0 {
		return nil, invalid("invalid path")
	}
	ino, found, err := f.resolvePath(rootIno, path, false, 0)
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, nil
	}
	in, err := f.readInode(ino)
	if err != nil {
		return nil, err
	}
	return &Entry{
		Inode:   in.ino,
		Regular: in.isRegular(),
		Size:    in.size,
	}, nil
}

func (f *Filesystem) resolvePath(baseIno uint32, path string, followFinal bool, linkDepth int) (uint32, bool, error) {
	cur := baseIno
	if strings.HasPrefix(path, "/") {
		cur = rootIno
	}
	hasTrailingSlash := strings.HasSuffix(path, "/")
	parts := splitComponents(path)
	if len(parts) == 0 {
		return cur, true, nil
	}

	for i, name := range parts {
		dirIn, err := f.readInode(cur)
		if err != nil {
			return 0, false, err
		}
		if !dirIn.isDir() {
			return 0, false, errf("not a directory (inode %d)", cur)
		}
		if dirIn.flags&inodeInlineDataFl != 0 {
			return 0, false, errf("directory inode %d uses unsupported inline_data", cur)
		}

		childIno, found, err := f.lookupInDir(dirIn, name)
		if err != nil {
			return 0, false, err
		}
		if !found {
			return 0, false, nil
		}

		isLast := i == len(parts)-1
		if !isLast || hasTrailingSlash || followFinal {
			childIn, err := f.readInode(childIno)
			if err != nil {
				return 0, false, err
			}
			if childIn.isSymlink() {
				if linkDepth >= maxSymlinkDepth {
					return 0, false, invalid("too many levels of symbolic links")
				}
				target, err := f.readSymlinkTarget(childIn)
				if err != nil {
					return 0, false, err
				}
				resolved, ok, err := f.resolvePath(cur, target, true, linkDepth+1)
				if err != nil || !ok {
					return 0, ok, err
				}
				childIno = resolved
			}
		}
		cur = childIno
	}
	return cur, true, nil
}

func splitComponents(path string) []string {
	var parts []string
	for _, seg := range strings.Split(path, "/") {
		if seg != "" {
			parts = append(parts, seg)
		}
	}
	return parts
}

func (f *Filesystem) readSymlinkTarget(in *inode) (string, error) {
	if in.size > maxSymlinkBytes {
		return "", errf("symlink inode %d target too long (%d bytes)", in.ino, in.size)
	}
	if in.size == 0 {
		return "", nil
	}
	if in.flags&inodeInlineDataFl != 0 {
		return "", errf("symlink inode %d uses unsupported inline_data", in.ino)
	}
	if in.flags&inodeExtentsFl == 0 && in.size < inodeInlineBlocksSize {
		return string(in.raw[inodeInlineBlocksOffset : inodeInlineBlocksOffset+in.size]), nil
	}
	buf := make([]byte, in.size)
	if err := f.readInodeInto(in, buf); err != nil {
		return "", err
	}
	return string(buf), nil
}

func (f *Filesystem) lookupInDir(dirIn *inode, name string) (uint32, bool, error) {
	if len(name) == 0 || len(name) > 255 {
		return 0, false, nil
	}
	bs := uint64(f.blockSize)
	numBlocks := (dirIn.size + bs - 1) / bs
	if numBlocks == 0 {
		return 0, false, nil
	}

	blkBuf := make([]byte, f.blockSize)
	if dirIn.flags&inodeExtentsFl != 0 {
		extents, err := f.collectExtents(dirIn)
		if err != nil {
			return 0, false, err
		}
		for _, ext := range extents {
			if ext.unwritten {
				continue
			}
			for off := uint32(0); off < ext.length; off++ {
				if uint64(ext.logical)+uint64(off) >= numBlocks {
					return 0, false, nil
				}
				if err := f.readBlock(ext.physical+uint64(off), blkBuf); err != nil {
					return 0, false, err
				}
				if ino, ok, err := f.scanDirBlock(dirIn.ino, blkBuf, name); err != nil || ok {
					return ino, ok, err
				}
			}
		}
		return 0, false, nil
	}

	ind := newIndirectCache(f)
	for lblk := uint64(0); lblk < numBlocks; lblk++ {
		phys, err := ind.lookup(dirIn, lblk)
		if err != nil {
			return 0, false, err
		}
		if phys == 0 {
			continue
		}
		if err := f.readBlock(phys, blkBuf); err != nil {
			return 0, false, err
		}
		if ino, ok, err := f.scanDirBlock(dirIn.ino, blkBuf, name); err != nil || ok {
			return ino, ok, err
		}
	}
	return 0, false, nil
}

func (f *Filesystem) scanDirBlock(dirIno uint32, blk []byte, name string) (uint32, bool, error) {
	le := binary.LittleEndian
	bs := uint32(len(blk))
	var off uint32
	for off+8 <= bs {
		ino := le.Uint32(blk[off : off+4])
		rawRecLen := uint32(le.Uint16(blk[off+4 : off+6]))
		recLen := decodeDirRecLen(rawRecLen, f.blockSize)
		if recLen < 8 || recLen%4 != 0 || off+recLen > bs {
			return 0, false, errf("corrupt directory block in inode %d", dirIno)
		}
		var nameLen uint32
		if f.hasFiletype() {
			nameLen = uint32(blk[off+6])
		} else {
			nameLen = uint32(le.Uint16(blk[off+6 : off+8]))
		}
		if ino != 0 {
			if 8+nameLen > recLen {
				return 0, false, errf("corrupt directory entry in inode %d", dirIno)
			}
			if int(nameLen) == len(name) && string(blk[off+8:off+8+nameLen]) == name {
				return ino, true, nil
			}
		}
		off += recLen
	}
	return 0, false, nil
}

func decodeDirRecLen(raw uint32, blockSize uint32) uint32 {
	if blockSize < maxBlockSize {
		return raw
	}
	if raw == maxRecLen64K || raw == 0 {
		return blockSize
	}
	return (raw & 65532) | ((raw & 3) << 16)
}
