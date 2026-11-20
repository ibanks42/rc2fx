// Package ext2fs wraps libext2fs (from e2fsprogs) so the desktop app can repair
// a RØDECaster mixer's ext4 root partitions offline on any OS without mounting
// them.
//
// It exposes only the minimal surface needed for repair: open a filesystem at a
// byte offset, look up a path, read a regular file, and overwrite a regular
// file in place with the same byte length. All filesystem logic lives in
// libext2fs via shim/shim.c. The vendored sources are e2fsprogs 1.47.4, managed
// by update-e2fsprogs.sh.
//
// # Build
//
// libext2fs and libcom_err are compiled via cgo using upstream's Android
// config header (e2fsprogs/lib/config.h) and the source list from
// lib/ext2fs/Makefile.in (Android.bp in 1.47.4 omits getenv.c, orphan.c,
// inode_io.c, bb_compat.c, and write_bb_file.c). Unused I/O backends
// (sparse_io.c, undo_io.c, tdb.c, test_io.c) are excluded. Non-Windows builds
// use unix_io.c; Windows builds use windows_io.c with e2fsprogs/include/mingw.
//
// Because cgo only compiles .c files directly in the package directory, tiny
// wrapper files (ext2fs_*.c, et_*.c, rc2fx_shim.c) #include each upstream file
// so every source stays its own translation unit. Without cgo, the package
// compiles to a stub whose Open returns an error.
//
// libext2fs is LGPL-2.0-only and libcom_err is MIT-style (see e2fsprogs/NOTICE).
// The Go/shim code in this package is MIT (MIT AND LGPL-2.0-only overall).
package ext2fs

// Error wraps a libext2fs error code and message. Code is 0 for validation
// errors caught in Go before calling into C.
type Error struct {
	Code    int64
	Message string
}

func (e *Error) Error() string { return e.Message }

func invalid(message string) *Error { return &Error{Message: message} }

// Entry describes a filesystem entry returned by Lookup.
type Entry struct {
	Inode   uint32
	Regular bool
	Size    uint64
}
