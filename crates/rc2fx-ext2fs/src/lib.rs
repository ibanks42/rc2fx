//! libext2fs, the e2fsprogs library `mkfs.ext4` and `e2fsck` are built on,
//! for repairing a RØDECaster mixer's ext4 root partitions from a computer
//! on any operating system, without mounting them.
//!
//! Only what the repair needs is exposed: open a filesystem at an offset in
//! a disk, look a path up, read a regular file, and replace the contents of
//! a regular file with the same number of bytes. Every filesystem operation
//! is libext2fs's own (see `src/shim.c`); this crate holds no filesystem
//! code. The sources are e2fsprogs 1.47.4, vendored by
//! `update-e2fsprogs.sh` and built by `build.rs`.
// Linux only until libext2fs is built and tested for macOS and Windows; the
// crate is empty elsewhere.
#![cfg(target_os = "linux")]

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulonglong, c_void};
use std::fmt;
use std::path::Path;

type Errcode = c_long;
type Fs = *mut c_void;

unsafe extern "C" {
    fn rc2fx_ext2fs_open(
        device: *const c_char,
        offset: c_ulonglong,
        writable: c_int,
        fs: *mut Fs,
    ) -> Errcode;
    fn rc2fx_ext2fs_needs_recovery(fs: Fs) -> c_int;
    fn rc2fx_ext2fs_close(fs: Fs) -> Errcode;
    fn rc2fx_ext2fs_lookup(
        fs: Fs,
        path: *const c_char,
        ino: *mut c_uint,
        regular: *mut c_int,
        size: *mut c_ulonglong,
    ) -> Errcode;
    fn rc2fx_ext2fs_read(fs: Fs, ino: c_uint, buf: *mut u8, len: c_ulonglong) -> Errcode;
    fn rc2fx_ext2fs_overwrite(fs: Fs, ino: c_uint, buf: *const u8, len: c_ulonglong) -> Errcode;
    fn rc2fx_ext2fs_not_found(err: Errcode) -> c_int;
    fn rc2fx_ext2fs_error(err: Errcode) -> *const c_char;
}

/// A libext2fs error, with libext2fs's own message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: i64,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

fn check(code: Errcode) -> Result<(), Error> {
    if code == 0 {
        return Ok(());
    }
    // SAFETY: error_message returns a static or thread-local NUL-terminated
    // string for any code.
    let message = unsafe { CStr::from_ptr(rc2fx_ext2fs_error(code)) }
        .to_string_lossy()
        .into_owned();
    // `long` is 64 bits on Linux and macOS, 32 on Windows.
    #[allow(clippy::useless_conversion)]
    let code = i64::from(code);
    Err(Error { code, message })
}

fn invalid(message: &str) -> Error {
    Error {
        code: 0,
        message: message.to_owned(),
    }
}

/// A file the lookup found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub inode: u32,
    pub regular: bool,
    pub size: u64,
}

/// An open ext4 filesystem. Closing flushes what was written; dropping
/// without [`Filesystem::close`] still closes, ignoring errors.
pub struct Filesystem {
    fs: Fs,
    writable: bool,
}

// SAFETY: a libext2fs filesystem handle has no thread affinity; this type
// gives no shared access to it (every method takes &self or &mut self, and
// it is not Sync).
unsafe impl Send for Filesystem {}

impl Filesystem {
    /// Open the filesystem that starts `offset` bytes into `device` (a disk
    /// device or an image file). Read-only unless `writable`.
    pub fn open(device: &Path, offset: u64, writable: bool) -> Result<Self, Error> {
        let name = CString::new(device.to_string_lossy().as_bytes())
            .map_err(|_| invalid("invalid device path"))?;
        let mut fs: Fs = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated name and out pointer.
        check(unsafe { rc2fx_ext2fs_open(name.as_ptr(), offset, writable.into(), &mut fs) })?;
        Ok(Self { fs, writable })
    }

    /// The journal holds changes not yet replayed: the filesystem was not
    /// shut down cleanly, and what libext2fs reads may be out of date.
    pub fn needs_recovery(&self) -> bool {
        // SAFETY: open handle.
        unsafe { rc2fx_ext2fs_needs_recovery(self.fs) != 0 }
    }

    /// Look up a path relative to the filesystem's root; `None` when it
    /// doesn't exist. A final symlink is not followed.
    pub fn lookup(&self, path: &str) -> Result<Option<Entry>, Error> {
        let path = CString::new(path).map_err(|_| invalid("invalid path"))?;
        let (mut inode, mut regular, mut size) = (0, 0, 0);
        // SAFETY: open handle, NUL-terminated path, valid out pointers.
        let code = unsafe {
            rc2fx_ext2fs_lookup(self.fs, path.as_ptr(), &mut inode, &mut regular, &mut size)
        };
        // SAFETY: plain comparison of an error code.
        if code != 0 && unsafe { rc2fx_ext2fs_not_found(code) } != 0 {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(Entry {
            inode,
            regular: regular != 0,
            size,
        }))
    }

    /// The contents of a regular file.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, Error> {
        if !entry.regular {
            return Err(invalid("not a regular file"));
        }
        let len = usize::try_from(entry.size).map_err(|_| invalid("file too large"))?;
        let mut data = vec![0; len];
        // SAFETY: open handle; the buffer holds exactly entry.size bytes.
        check(unsafe { rc2fx_ext2fs_read(self.fs, entry.inode, data.as_mut_ptr(), entry.size) })?;
        Ok(data)
    }

    /// Replace a regular file's contents with `data`, which must be exactly
    /// as long as the file. Nothing else about the file changes.
    pub fn overwrite(&mut self, entry: &Entry, data: &[u8]) -> Result<(), Error> {
        if !self.writable {
            return Err(invalid("the filesystem is open read-only"));
        }
        if !entry.regular || entry.size != data.len() as u64 {
            return Err(invalid(
                "only a regular file's contents of the same size are replaced",
            ));
        }
        // SAFETY: open handle; the buffer holds exactly data.len() bytes.
        check(unsafe { rc2fx_ext2fs_overwrite(self.fs, entry.inode, data.as_ptr(), entry.size) })
    }

    /// Flush everything written and close.
    pub fn close(mut self) -> Result<(), Error> {
        let fs = std::mem::replace(&mut self.fs, std::ptr::null_mut());
        // SAFETY: the handle is closed once, here or in drop.
        check(unsafe { rc2fx_ext2fs_close(fs) })
    }
}

impl Drop for Filesystem {
    fn drop(&mut self) {
        if !self.fs.is_null() {
            // SAFETY: the handle is closed once.
            unsafe { rc2fx_ext2fs_close(self.fs) };
        }
    }
}

#[cfg(test)]
mod tests;
