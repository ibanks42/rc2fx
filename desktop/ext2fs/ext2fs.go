//go:build cgo

package ext2fs

/*
#cgo CFLAGS: -I${SRCDIR}/e2fsprogs/lib -I${SRCDIR}/e2fsprogs/lib/ext2fs -I${SRCDIR}/e2fsprogs/lib/et -fno-strict-aliasing
#cgo windows CFLAGS: -I${SRCDIR}/e2fsprogs/include/mingw

#include <stdlib.h>

// shim/shim.c, with plain C types only, so this side never depends on
// libext2fs's structure layouts. errcode_t is long; ext2_ino_t is 32 bits.
long rc2fx_ext2fs_open(const char *device, unsigned long long offset, int writable, void **fs);
int rc2fx_ext2fs_needs_recovery(void *fs);
long rc2fx_ext2fs_close(void *fs);
long rc2fx_ext2fs_lookup(void *fs, const char *path, unsigned int *ino, int *regular, unsigned long long *size);
long rc2fx_ext2fs_read(void *fs, unsigned int ino, unsigned char *buf, unsigned long long len);
long rc2fx_ext2fs_overwrite(void *fs, unsigned int ino, const unsigned char *buf, unsigned long long len);
int rc2fx_ext2fs_not_found(long err);
const char *rc2fx_ext2fs_error(long err);
*/
import "C"

import (
	"math"
	"runtime"
	"strings"
	"sync"
	"unsafe"
)

// opening serializes Open: the shim adds libext2fs's error table on the
// first open, which is not safe to race.
var opening sync.Mutex

func check(code C.long) error {
	if code == 0 {
		return nil
	}
	// error_message returns a static or thread-local NUL-terminated string
	// for any code. `long` is 64 bits on Linux and macOS, 32 on Windows.
	return &Error{Code: int64(code), Message: C.GoString(C.rc2fx_ext2fs_error(code))}
}

// Filesystem is an open ext4 filesystem. Close flushes what was written; a
// Filesystem dropped without Close is still closed by the garbage
// collector, ignoring errors. It is not safe for concurrent use.
type Filesystem struct {
	fs       unsafe.Pointer
	writable bool
}

// Open opens the filesystem that starts offset bytes into device (a disk
// device or an image file). Read-only unless writable.
func Open(device string, offset uint64, writable bool) (*Filesystem, error) {
	if strings.IndexByte(device, 0) >= 0 {
		return nil, invalid("invalid device path")
	}
	name := C.CString(device)
	defer C.free(unsafe.Pointer(name))
	flag := C.int(0)
	if writable {
		flag = 1
	}
	var fs unsafe.Pointer
	opening.Lock()
	code := C.rc2fx_ext2fs_open(name, C.ulonglong(offset), flag, &fs)
	opening.Unlock()
	if err := check(code); err != nil {
		return nil, err
	}
	f := &Filesystem{fs: fs, writable: writable}
	runtime.SetFinalizer(f, func(f *Filesystem) { _ = f.Close() })
	return f, nil
}

func (f *Filesystem) handle() (unsafe.Pointer, error) {
	if f == nil || f.fs == nil {
		return nil, invalid("the filesystem is closed")
	}
	return f.fs, nil
}

// NeedsRecovery is whether the journal holds changes not yet replayed: the
// filesystem was not shut down cleanly, and what libext2fs reads may be out
// of date.
func (f *Filesystem) NeedsRecovery() bool {
	fs, err := f.handle()
	if err != nil {
		return false
	}
	defer runtime.KeepAlive(f)
	return C.rc2fx_ext2fs_needs_recovery(fs) != 0
}

// Lookup looks up a path relative to the filesystem's root; nil when it
// doesn't exist. A final symlink is not followed.
func (f *Filesystem) Lookup(path string) (*Entry, error) {
	fs, err := f.handle()
	if err != nil {
		return nil, err
	}
	defer runtime.KeepAlive(f)
	if strings.IndexByte(path, 0) >= 0 {
		return nil, invalid("invalid path")
	}
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	var (
		inode   C.uint
		regular C.int
		size    C.ulonglong
	)
	code := C.rc2fx_ext2fs_lookup(fs, cpath, &inode, &regular, &size)
	if code != 0 && C.rc2fx_ext2fs_not_found(code) != 0 {
		return nil, nil
	}
	if err := check(code); err != nil {
		return nil, err
	}
	return &Entry{Inode: uint32(inode), Regular: regular != 0, Size: uint64(size)}, nil
}

// Read is the contents of a regular file.
func (f *Filesystem) Read(entry Entry) ([]byte, error) {
	fs, err := f.handle()
	if err != nil {
		return nil, err
	}
	defer runtime.KeepAlive(f)
	if !entry.Regular {
		return nil, invalid("not a regular file")
	}
	if entry.Size > math.MaxInt {
		return nil, invalid("file too large")
	}
	data := make([]byte, int(entry.Size))
	// The buffer holds exactly entry.Size bytes.
	code := C.rc2fx_ext2fs_read(fs, C.uint(entry.Inode), (*C.uchar)(unsafe.SliceData(data)), C.ulonglong(entry.Size))
	if err := check(code); err != nil {
		return nil, err
	}
	return data, nil
}

// Overwrite replaces a regular file's contents with data, which must be
// exactly as long as the file. Nothing else about the file changes.
func (f *Filesystem) Overwrite(entry Entry, data []byte) error {
	fs, err := f.handle()
	if err != nil {
		return err
	}
	defer runtime.KeepAlive(f)
	if !f.writable {
		return invalid("the filesystem is open read-only")
	}
	if !entry.Regular || entry.Size != uint64(len(data)) {
		return invalid("only a regular file's contents of the same size are replaced")
	}
	code := C.rc2fx_ext2fs_overwrite(fs, C.uint(entry.Inode), (*C.uchar)(unsafe.SliceData(data)), C.ulonglong(entry.Size))
	return check(code)
}

// Close flushes everything written and closes. Later calls do nothing.
func (f *Filesystem) Close() error {
	if f == nil || f.fs == nil {
		return nil
	}
	fs := f.fs
	f.fs = nil
	runtime.SetFinalizer(f, nil)
	// The handle is closed once, here.
	return check(C.rc2fx_ext2fs_close(fs))
}
