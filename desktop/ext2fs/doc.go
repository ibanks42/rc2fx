// Package ext2fs is libext2fs, the e2fsprogs library mkfs.ext4 and e2fsck
// are built on, for repairing a RØDECaster mixer's ext4 root partitions from
// a computer on any operating system, without mounting them.
//
// Only what the repair needs is exposed: open a filesystem at an offset in a
// disk, look a path up, read a regular file, and replace the contents of a
// regular file with the same number of bytes. Every filesystem operation is
// libext2fs's own (see shim/shim.c); this package holds no filesystem code.
// The sources are e2fsprogs 1.47.4, vendored in e2fsprogs/ by
// update-e2fsprogs.sh.
//
// # Build
//
// libext2fs and libcom_err are built with cgo from the vendored sources, with
// the configuration header of upstream's Android build (e2fsprogs/lib/config.h)
// and the source list of lib/ext2fs/Makefile.in (Android.bp's in 1.47.4 lacks
// getenv.c, orphan.c, inode_io.c, bb_compat.c and write_bb_file.c). Left out:
// sparse_io.c (libsparse), undo_io.c and tdb.c, and test_io.c, none of which
// the repair uses. unix_io.c is the I/O manager everywhere but Windows, which
// uses windows_io.c and adds e2fsprogs/include/mingw to the include path.
//
// cgo compiles only the .c files in the package directory, so each upstream
// source has a two-line file here that includes it (ext2fs_<name>.c for
// lib/ext2fs, et_<name>.c for lib/et, rc2fx_shim.c for shim/shim.c), keeping
// every source its own translation unit as upstream's build does. Upstream's
// warnings are theirs to fix; nothing here turns them into errors. Built
// without cgo, the package compiles to a stub whose Open reports that.
//
// libext2fs is licensed under the GNU Library General Public License,
// version 2, and libcom_err under an MIT-style license; the texts are in
// e2fsprogs/NOTICE. This package's own code is MIT (MIT AND LGPL-2.0-only
// as a whole).
package ext2fs

// Error is a libext2fs error, with libext2fs's own message. Code is 0 for a
// check this package makes before calling libext2fs.
type Error struct {
	Code    int64
	Message string
}

func (e *Error) Error() string { return e.Message }

func invalid(message string) *Error { return &Error{Message: message} }

// Entry is a file the lookup found.
type Entry struct {
	Inode   uint32
	Regular bool
	Size    uint64
}
