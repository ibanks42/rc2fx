//! Private local files: journals, keys, downloads and backups.

use crate::{Result, bail};
use md5::Md5;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    let home = home.map(PathBuf::from).filter(|p| p.is_absolute());
    #[cfg(target_os = "linux")]
    {
        // sudo/pkexec can replace HOME with /root. Keys must still belong to
        // the user who launched the installer, just like its cache files.
        let elevated = unsafe { libc::geteuid() } == 0;
        let invoker = if elevated {
            invoking_user().ok()?
        } else {
            None
        };
        home_for_identity(home, elevated, invoker.map(|(uid, _)| uid))
    }
    #[cfg(not(target_os = "linux"))]
    home
}

#[cfg(target_os = "linux")]
fn home_for_identity(
    home: Option<PathBuf>,
    elevated: bool,
    invoker: Option<u32>,
) -> Option<PathBuf> {
    if elevated && let Some(uid) = invoker {
        return account(uid).map(|(_, home)| home);
    }
    home
}

#[cfg(target_os = "linux")]
fn account(uid: u32) -> Option<(u32, PathBuf)> {
    use std::{ffi::CStr, os::unix::ffi::OsStrExt};
    let mut buffer = vec![0u8; 4096];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found = std::ptr::null_mut();
        // Reentrant lookup: all returned strings remain within our buffer.
        let error = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut found,
            )
        };
        if error == libc::ERANGE && buffer.len() < 65536 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if error != 0 || found.is_null() {
            return None;
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() {
            return None;
        }
        let bytes = unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes();
        let home = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        return home.is_absolute().then_some((entry.pw_gid, home));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod helper_home_tests {
    use super::*;

    #[test]
    fn privileged_helpers_use_the_invokers_home_not_roots_environment() {
        let uid = unsafe { libc::getuid() };
        let (_, expected) = account(uid).expect("test account exists");
        assert_eq!(
            home_for_identity(Some("/wrong-helper-home".into()), true, Some(uid)),
            Some(expected)
        );
        assert_eq!(
            home_for_identity(Some("/normal-home".into()), false, Some(uid)),
            Some("/normal-home".into())
        );
        assert_eq!(
            home_for_identity(Some("/root".into()), true, None),
            Some("/root".into())
        );
        assert_eq!(
            home_for_identity(Some("/root".into()), true, Some(u32::MAX)),
            None
        );
    }
}

pub(crate) fn exists(path: &Path) -> bool {
    fs::metadata(path).is_ok()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sum(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Algorithm {
    Sha256,
    Md5,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Md5 => "md5",
        }
    }
}

/// Hash a file, reporting each block read to `progress`.
pub(crate) fn hash_reader(
    mut reader: impl Read,
    algorithm: Algorithm,
    mut progress: impl FnMut(u64),
) -> io::Result<String> {
    let mut sha = Sha256::new();
    let mut md = Md5::new();
    let mut buf = vec![0; 1 << 16];
    let mut total = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        match algorithm {
            Algorithm::Sha256 => sha.update(&buf[..n]),
            Algorithm::Md5 => md.update(&buf[..n]),
        }
        total += n as u64;
        progress(total);
    }
    Ok(match algorithm {
        Algorithm::Sha256 => hex(&sha.finalize()),
        Algorithm::Md5 => hex(&md.finalize()),
    })
}

pub(crate) fn file_hash(path: &Path, algorithm: Algorithm) -> Result<String> {
    Ok(hash_reader(File::open(path)?, algorithm, |_| {})?)
}

pub(crate) fn matches(path: &Path, algorithm: Algorithm, want: &str) -> Result<()> {
    let got = file_hash(path, algorithm)?;
    if want.is_empty() || got != want {
        bail!(
            "checksum mismatch: {} ({})",
            path.file_name().unwrap_or_default().to_string_lossy(),
            algorithm.name()
        );
    }
    Ok(())
}

/// Files created by the privileged Linux helper still belong to the invoking
/// user, especially the SSH key that the desktop client reuses later. Never
/// change ownership of an existing file or of staged USB files.
#[cfg(target_os = "linux")]
pub(crate) fn local_owner(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let Some((uid, gid)) = invoking_user()? else {
        return Ok(());
    };
    // SAFETY: the descriptor is owned by `file` for the duration of the call.
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

/// The user who started a privileged helper with sudo or pkexec.
#[cfg(target_os = "linux")]
fn invoking_user() -> Result<Option<(u32, u32)>> {
    let parse = |name: &str| -> Result<Option<u32>> {
        match std::env::var(name) {
            Ok(v) => v
                .parse::<u32>()
                .map(Some)
                .map_err(|_| crate::Error(format!("invalid {name}"))),
            Err(_) => Ok(None),
        }
    };
    if let Some(uid) = parse("SUDO_UID")? {
        let gid = parse("SUDO_GID")?.ok_or("invalid SUDO_GID")?;
        return Ok(Some((uid, gid)));
    }
    if let Some(uid) = parse("PKEXEC_UID")? {
        let (gid, _) = account(uid).ok_or("unknown PKEXEC_UID")?;
        return Ok(Some((uid, gid)));
    }
    Ok(None)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn local_owner(_: &File) -> Result<()> {
    Ok(())
}

fn create_dir_private(dir: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

pub(crate) fn private_dirs(dir: &Path) -> Result<()> {
    match fs::metadata(dir) {
        Ok(m) if m.is_dir() => return Ok(()),
        Ok(_) => bail!("mkdir {}: not a directory", dir.display()),
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        Err(_) => {}
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        private_dirs(parent)?;
    }
    match create_dir_private(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return private_dirs(dir),
        Err(e) => return Err(e.into()),
    }
    #[cfg(target_os = "linux")]
    local_owner(&File::open(dir)?)?;
    Ok(())
}

fn options(write: bool) -> OpenOptions {
    let mut o = OpenOptions::new();
    o.read(!write).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o
}

/// Create a uniquely named private file in `dir`. It is removed on drop
/// unless [`Temp::persist`] renamed it.
pub(crate) struct Temp {
    pub file: Option<File>,
    pub path: PathBuf,
}

impl Temp {
    pub fn new(dir: &Path, prefix: &str) -> Result<Self> {
        for _ in 0..64 {
            let mut nonce = [0u8; 8];
            getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
            let path = dir.join(format!("{prefix}{}", hex(&nonce)));
            match options(true).create_new(true).open(&path) {
                Ok(file) => {
                    let temp = Temp {
                        file: Some(file),
                        path,
                    };
                    local_owner(temp.file())?;
                    return Ok(temp);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        bail!("could not create a temporary file in {}", dir.display())
    }

    pub fn file(&self) -> &File {
        self.file.as_ref().expect("temporary file is open")
    }

    /// Flush data to disk and close the handle, keeping the file.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        Ok(())
    }

    /// Finish, then rename over `dest`.
    pub fn persist(mut self, dest: &Path) -> Result<()> {
        self.finish()?;
        replace_local_file(&self.path, dest)?;
        self.path = PathBuf::new();
        Ok(())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        self.file.take();
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Write for Temp {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .expect("temporary file is open")
            .write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().expect("temporary file is open").flush()
    }
}

/// Create the file privately if missing, otherwise open the existing one.
pub(crate) fn open_local_file(path: &Path, append: bool) -> Result<File> {
    let mut create = options(true);
    create.append(append).create_new(true);
    match create.open(path) {
        Ok(file) => {
            local_owner(&file)?;
            Ok(file)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let mut existing = options(true);
            existing.append(append);
            Ok(existing.open(path)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// Write a whole file through a synced temporary and an atomic rename.
pub(crate) fn atomic_file(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut temp = Temp::new(dir, ".rc2fx-")?;
    temp.write_all(data)?;
    temp.persist(path)
}

/// Create a new file exclusively; refuses to replace an existing one.
pub(crate) fn exclusive_file(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = options(true).create_new(true).open(path)?;
    local_owner(&file)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// SHA-256 and MD5 of a file in one read. `progress` gets (read, total).
pub(crate) fn hash_both(
    path: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<(String, String)> {
    let mut file = File::open(path)?;
    let total = file.metadata()?.len();
    let (mut sha, mut md) = (Sha256::new(), Md5::new());
    let mut buf = vec![0; 1 << 20];
    let mut done = 0;
    progress(0, total);
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    Ok((hex(&sha.finalize()), hex(&md.finalize())))
}

/// Copy a file to a new destination and flush it. `progress` gets
/// (written, total).
pub(crate) fn copy_with_progress(
    src: &Path,
    dst: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let mut input = File::open(src)?;
    let total = input.metadata()?.len();
    let mut out = options(true).create_new(true).open(dst)?;
    let mut buf = vec![0; 1 << 20];
    let mut done = 0;
    progress(0, total);
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    out.sync_all()?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn replace_local_file(temp: &Path, path: &Path) -> Result<()> {
    fs::rename(temp, path)?;
    // Persist the directory entry as well as file data before a USB command.
    File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn replace_local_file(temp: &Path, path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(temp), wide(path));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

/// An exclusive, non-blocking advisory lock released on drop.
pub(crate) struct LockGuard<'a>(&'a File);

pub(crate) fn lock_action(file: &File) -> io::Result<LockGuard<'_>> {
    file.try_lock().map_err(|e| match e {
        fs::TryLockError::Error(e) => e,
        fs::TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock, "locked"),
    })?;
    Ok(LockGuard(file))
}

impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether two paths name the same file, ignoring separators and, on
/// Windows, case. Neither path needs to exist.
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let text = std::path::absolute(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    norm(a) == norm(b)
}
