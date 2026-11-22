//! Developer SSH access over USB 1: status and adding/removing RC2FX's single
//! managed `authorized_keys` line without reflashing.
//!
//! Runs directly inside `rc2fxd` off the async executor with serialized access,
//! component-by-component `O_NOFOLLOW | O_DIRECTORY` path verification, and
//! atomic `renameat` replacement. Exposed only on the USB control API.
//! Removing the key blocks new logins; existing sessions stay open.

use rc2fx_core::{Ed25519PublicKey, SSH_MANAGED_MARKER, SshRequest, SshStatus};
use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const DEFAULT_SSH_DIR: &str = "/home/root/.ssh";
const DEFAULT_SSH_HOST_DIR: &str = "/etc/ssh";
const DEFAULT_SSH_FIRST_BOOT: &str = "/usr/bin/first-boot";
const DEFAULT_SSH_LOGS_DIR: &str = "/opt/rc2fx/logs";
const DEFAULT_SSH_PROC_DIR: &str = "/proc";

const MAX_KEYS_BYTES: usize = 1024 * 1024;
const ED25519_PREFIX: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI";
const ED25519_TAIL_LEN: usize = 43;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshError {
    /// The request or key is invalid (400).
    Invalid(String),
    /// Another SSH change is in progress (409).
    Busy,
    /// The first boot after an update is still running (409).
    FirstBoot(String),
    /// `~/.ssh` or `authorized_keys` is not safe to touch (409).
    Unsafe(String),
    /// An I/O or system failure occurred (500).
    Failed(String),
}

impl SshError {
    pub fn status(&self) -> u16 {
        match self {
            SshError::Invalid(_) => 400,
            SshError::Busy | SshError::FirstBoot(_) | SshError::Unsafe(_) => 409,
            SshError::Failed(_) => 500,
        }
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshError::Invalid(why) => write!(f, "{why}"),
            SshError::Busy => f.write_str("Another SSH change is in progress"),
            SshError::FirstBoot(why) | SshError::Unsafe(why) | SshError::Failed(why) => {
                write!(f, "{why}")
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Paths {
    ssh_dir: PathBuf,
    host_dir: PathBuf,
    first_boot: PathBuf,
    logs_dir: PathBuf,
    proc_dir: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            ssh_dir: PathBuf::from(DEFAULT_SSH_DIR),
            host_dir: PathBuf::from(DEFAULT_SSH_HOST_DIR),
            first_boot: PathBuf::from(DEFAULT_SSH_FIRST_BOOT),
            logs_dir: PathBuf::from(DEFAULT_SSH_LOGS_DIR),
            proc_dir: PathBuf::from(DEFAULT_SSH_PROC_DIR),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Status,
    Enable,
    Disable,
}

/// The handle the USB API uses. Cloned into every client; one permit, owned
/// by the blocking task for as long as the operation runs, so a canceled
/// request cannot let a second change start while the first is still going.
pub struct DeveloperSsh {
    paths: Paths,
    permit: Arc<tokio::sync::Semaphore>,
}

impl Default for DeveloperSsh {
    fn default() -> Self {
        Self::new()
    }
}

impl DeveloperSsh {
    pub fn new() -> Self {
        Self {
            paths: Paths::default(),
            permit: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_paths(
        ssh_dir: PathBuf,
        host_dir: PathBuf,
        first_boot: PathBuf,
        logs_dir: PathBuf,
        proc_dir: PathBuf,
    ) -> Self {
        Self {
            paths: Paths {
                ssh_dir,
                host_dir,
                first_boot,
                logs_dir,
                proc_dir,
            },
            permit: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    pub async fn status(&self) -> Result<SshStatus, SshError> {
        self.run(Op::Status, None).await
    }

    /// Validate `request` completely before touching the filesystem, then
    /// enable or disable the managed key.
    pub async fn set(&self, request: SshRequest) -> Result<SshStatus, SshError> {
        let (op, key) = match (request.enabled, request.public_key) {
            (true, Some(raw)) => {
                let key = Ed25519PublicKey::parse(&raw).map_err(|e| SshError::Invalid(e.0))?;
                if !is_strict_key(key.as_str()) {
                    return Err(SshError::Invalid("invalid public key".into()));
                }
                (Op::Enable, Some(key))
            }
            (true, None) => {
                return Err(SshError::Invalid(
                    "enabling SSH needs the app's public key".into(),
                ));
            }
            (false, None) => (Op::Disable, None),
            (false, Some(_)) => {
                return Err(SshError::Invalid(
                    "disabling SSH takes no public key".into(),
                ));
            }
        };
        self.run(op, key).await
    }

    async fn run(&self, op: Op, key: Option<Ed25519PublicKey>) -> Result<SshStatus, SshError> {
        let permit = self
            .permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| SshError::Busy)?;
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            execute(&paths, op, key.as_ref())
        })
        .await
        .map_err(|_| SshError::Failed("developer SSH task failed".into()))?
    }
}

fn unsafe_path(detail: impl std::fmt::Display) -> SshError {
    SshError::Unsafe(format!("unsafe path: {detail}"))
}

fn execute(paths: &Paths, op: Op, key: Option<&Ed25519PublicKey>) -> Result<SshStatus, SshError> {
    if op != Op::Status && !paths.first_boot.as_os_str().is_empty() && path_stat(&paths.first_boot)
    {
        return Err(SshError::FirstBoot(
            "first boot after an update: try again once it finishes".into(),
        ));
    }

    // SAFETY: geteuid(2) is always safe and cannot fail.
    let me = unsafe { libc::geteuid() };
    let dir_fd = open_ssh_dir(&paths.ssh_dir, op == Op::Enable, me)?;

    let data = match &dir_fd {
        Some(fd) => read_keys(fd, &paths.ssh_dir, me)?,
        None => Vec::new(),
    };
    let (mut managed, unmanaged_keys, filtered) = filter_keys(&data, op != Op::Status);

    match op {
        Op::Status => {}
        Op::Enable => {
            let key = key.expect("validated before execute");
            let dir_fd = dir_fd.as_ref().expect("created on Enable");
            let mut out = filtered;
            if !out.is_empty() && *out.last().unwrap() != b'\n' {
                out.push(b'\n');
            }
            out.extend_from_slice(key.as_str().as_bytes());
            out.push(b' ');
            out.extend_from_slice(SSH_MANAGED_MARKER.as_bytes());
            out.push(b'\n');
            if out.len() > MAX_KEYS_BYTES {
                return Err(unsafe_path(format!(
                    "{}/authorized_keys is too large",
                    paths.ssh_dir.display()
                )));
            }
            write_keys_at(dir_fd, &out, me)?;
            log_added(&paths.logs_dir, &key.fingerprint());
            managed = Some(key.as_str().to_owned());
        }
        Op::Disable => {
            if managed.is_some() {
                let dir_fd = dir_fd.as_ref().expect("exists when managed key present");
                write_keys_at(dir_fd, &filtered, me)?;
            }
            managed = None;
        }
    }

    let managed_fingerprint = match managed {
        Some(line) => {
            let parsed = Ed25519PublicKey::parse(&line).map_err(|e| SshError::Failed(e.0))?;
            Some(parsed.fingerprint())
        }
        None => None,
    };

    Ok(SshStatus {
        managed: managed_fingerprint.is_some(),
        managed_fingerprint,
        unmanaged_keys,
        host_keys: read_host_keys(&paths.host_dir),
        server_running: check_server(&paths.proc_dir),
        detail: None,
    })
}

fn cstring_of_bytes(bytes: &[u8]) -> Option<CString> {
    CString::new(bytes).ok()
}

fn cstring_of_path(path: &Path) -> Option<CString> {
    cstring_of_bytes(path.as_os_str().as_bytes())
}

fn path_stat(path: &Path) -> bool {
    let Some(c_path) = cstring_of_path(path) else {
        return false;
    };
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: c_path is NUL-terminated and st points to valid writable storage.
    unsafe { libc::stat(c_path.as_ptr(), st.as_mut_ptr()) == 0 }
}

fn fstat_fd(fd: &OwnedFd) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is an open descriptor and st points to valid writable storage.
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat returned 0, so st is initialized.
    Ok(unsafe { st.assume_init() })
}

fn fstatat_nofollow(dir_fd: &OwnedFd, name: &CString) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: dir_fd is open, name is NUL-terminated, and st is valid writable storage.
    let rc = unsafe {
        libc::fstatat(
            dir_fd.as_raw_fd(),
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatat returned 0, so st is initialized.
    Ok(unsafe { st.assume_init() })
}

fn openat_raw(
    dir_fd: libc::c_int,
    name: &CString,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> io::Result<OwnedFd> {
    // SAFETY: name is NUL-terminated and flags/mode are valid openat arguments.
    let fd = unsafe { libc::openat(dir_fd, name.as_ptr(), flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh owned file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn is_dir_mode(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFDIR
}

fn is_file_mode(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFREG
}

fn is_symlink_mode(mode: libc::mode_t) -> bool {
    (mode & libc::S_IFMT) == libc::S_IFLNK
}

fn check_ancestor_fd(fd: &OwnedFd, path: &str, me: libc::uid_t) -> Result<(), SshError> {
    let st = fstat_fd(fd).map_err(|_| unsafe_path(path))?;
    if !is_dir_mode(st.st_mode) {
        return Err(unsafe_path(path));
    }
    if st.st_uid != 0 && st.st_uid != me {
        return Err(unsafe_path(format!("{path} owner")));
    }
    // Allow root-owned sticky directories such as /tmp; reject any other
    // group- or world-writable ancestor.
    if (st.st_mode & 0o022) != 0 && !(st.st_uid == 0 && (st.st_mode & 0o1000) != 0) {
        return Err(unsafe_path(format!("{path} is writable by others")));
    }
    Ok(())
}

/// Walk from `/` to `ssh_dir` component by component with `O_NOFOLLOW | O_DIRECTORY`
/// and `fstat` on each fd. When `create` is true, create a missing leaf with 0700.
fn open_ssh_dir(
    ssh_dir: &Path,
    create: bool,
    me: libc::uid_t,
) -> Result<Option<OwnedFd>, SshError> {
    let raw = ssh_dir.as_os_str().as_bytes();
    if raw.first() != Some(&b'/') {
        return Err(unsafe_path(ssh_dir.display()));
    }
    let mut comps: Vec<&[u8]> = Vec::new();
    for seg in raw[1..].split(|&b| b == b'/') {
        if seg.is_empty() {
            continue;
        }
        if seg == b"." || seg == b".." {
            return Err(unsafe_path(ssh_dir.display()));
        }
        comps.push(seg);
    }
    if comps.is_empty() {
        return Err(unsafe_path(ssh_dir.display()));
    }

    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let slash = CString::new("/").unwrap();
    let mut cur_fd =
        openat_raw(libc::AT_FDCWD, &slash, dir_flags, 0).map_err(|_| unsafe_path("/"))?;
    check_ancestor_fd(&cur_fd, "/", me)?;

    let mut cur_path = String::with_capacity(raw.len());
    for comp in &comps[..comps.len() - 1] {
        cur_path.push('/');
        cur_path.push_str(&String::from_utf8_lossy(comp));
        let c_comp = cstring_of_bytes(comp).ok_or_else(|| unsafe_path(&cur_path))?;
        let next_fd = openat_raw(cur_fd.as_raw_fd(), &c_comp, dir_flags, 0)
            .map_err(|_| unsafe_path(&cur_path))?;
        cur_fd = next_fd;
        check_ancestor_fd(&cur_fd, &cur_path, me)?;
    }

    let leaf = comps[comps.len() - 1];
    let c_leaf = cstring_of_bytes(leaf).ok_or_else(|| unsafe_path(ssh_dir.display()))?;
    let leaf_err = |parent: &OwnedFd| -> SshError {
        if let Ok(lst) = fstatat_nofollow(parent, &c_leaf)
            && is_symlink_mode(lst.st_mode)
        {
            return unsafe_path(format!("{} is a link", ssh_dir.display()));
        }
        unsafe_path(format!("{} is not a directory", ssh_dir.display()))
    };

    let fd = match openat_raw(cur_fd.as_raw_fd(), &c_leaf, dir_flags, 0) {
        Ok(fd) => fd,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => {
            if !create {
                return Ok(None);
            }
            // SAFETY: cur_fd is an open directory fd and c_leaf is NUL-terminated.
            let mk = unsafe { libc::mkdirat(cur_fd.as_raw_fd(), c_leaf.as_ptr(), 0o700) };
            let mk_errno = if mk == 0 {
                None
            } else {
                io::Error::last_os_error().raw_os_error()
            };
            if mk != 0 && mk_errno != Some(libc::EEXIST) {
                return Err(unsafe_path(format!("cannot create {}", ssh_dir.display())));
            }
            let fd = openat_raw(cur_fd.as_raw_fd(), &c_leaf, dir_flags, 0)
                .map_err(|_| leaf_err(&cur_fd))?;
            if mk == 0 {
                // SAFETY: fd and cur_fd are open directory descriptors.
                unsafe {
                    libc::fchmod(fd.as_raw_fd(), 0o700);
                    if me == 0 {
                        libc::fchown(fd.as_raw_fd(), 0, 0);
                    }
                    libc::fsync(cur_fd.as_raw_fd());
                }
            }
            fd
        }
        Err(_) => return Err(leaf_err(&cur_fd)),
    };

    let st = fstat_fd(&fd)
        .map_err(|_| unsafe_path(format!("{} is not a directory", ssh_dir.display())))?;
    if !is_dir_mode(st.st_mode) {
        return Err(unsafe_path(format!(
            "{} is not a directory",
            ssh_dir.display()
        )));
    }
    if st.st_uid != me {
        return Err(unsafe_path(format!("{} owner", ssh_dir.display())));
    }
    if (st.st_mode & 0o022) != 0 {
        return Err(unsafe_path(format!(
            "{} is writable by others",
            ssh_dir.display()
        )));
    }
    Ok(Some(fd))
}

fn check_keys_stat(st: &libc::stat, keys_path: &str, me: libc::uid_t) -> Result<(), SshError> {
    if is_symlink_mode(st.st_mode) || !is_file_mode(st.st_mode) {
        return Err(unsafe_path(format!("{keys_path} is not a regular file")));
    }
    if st.st_nlink != 1 {
        return Err(unsafe_path(format!("{keys_path} has other links")));
    }
    if st.st_uid != me {
        return Err(unsafe_path(format!("{keys_path} owner")));
    }
    if (st.st_mode & 0o022) != 0 {
        return Err(unsafe_path(format!("{keys_path} is writable by others")));
    }
    if st.st_size < 0 || (st.st_size as u64) > MAX_KEYS_BYTES as u64 {
        return Err(unsafe_path(format!("{keys_path} is too large")));
    }
    Ok(())
}

fn read_fd_limited(fd: &OwnedFd, max_bytes: usize) -> Result<Vec<u8>, Option<io::Error>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remain = (max_bytes + 1).saturating_sub(out.len());
        let to_read = remain.min(chunk.len());
        // SAFETY: fd is open and chunk[..to_read] is valid writable memory.
        let n = unsafe { libc::read(fd.as_raw_fd(), chunk.as_mut_ptr().cast(), to_read) };
        if n > 0 {
            out.extend_from_slice(&chunk[..n as usize]);
            if out.len() > max_bytes {
                return Err(None);
            }
        } else if n == 0 {
            return Ok(out);
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Some(err));
        }
    }
}

fn read_keys(dir_fd: &OwnedFd, ssh_dir: &Path, me: libc::uid_t) -> Result<Vec<u8>, SshError> {
    let keys_path = format!("{}/authorized_keys", ssh_dir.display());
    let name = CString::new("authorized_keys").unwrap();
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let fd = match openat_raw(dir_fd.as_raw_fd(), &name, flags, 0) {
        Ok(fd) => fd,
        Err(err) => {
            if err.raw_os_error() == Some(libc::ENOENT) {
                return Ok(Vec::new());
            }
            if let Ok(lst) = fstatat_nofollow(dir_fd, &name) {
                check_keys_stat(&lst, &keys_path, me)?;
            }
            if matches!(
                err.raw_os_error(),
                Some(libc::ELOOP | libc::EISDIR | libc::ENXIO)
            ) {
                return Err(unsafe_path(format!("{keys_path} is not a regular file")));
            }
            return Err(SshError::Failed("cannot read authorized_keys".into()));
        }
    };

    let st =
        fstat_fd(&fd).map_err(|_| unsafe_path(format!("{keys_path} is not a regular file")))?;
    check_keys_stat(&st, &keys_path, me)?;

    let bytes = match read_fd_limited(&fd, MAX_KEYS_BYTES) {
        Ok(bytes) => bytes,
        Err(None) => return Err(unsafe_path(format!("{keys_path} is too large"))),
        Err(Some(_)) => return Err(SshError::Failed("cannot read authorized_keys".into())),
    };
    if bytes.contains(&0) {
        return Err(unsafe_path(format!("{keys_path} contains NUL bytes")));
    }
    Ok(bytes)
}

fn is_b64_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'+' || b == b'/'
}

fn is_strict_key(s: &str) -> bool {
    s.len() == ED25519_PREFIX.len() + ED25519_TAIL_LEN
        && s.starts_with(ED25519_PREFIX)
        && s.as_bytes()[ED25519_PREFIX.len()..]
            .iter()
            .all(|&b| is_b64_char(b))
}

/// Split `authorized_keys` into its first managed key, count of unmanaged key
/// lines, and optionally the unmanaged bytes preserved byte for byte.
fn filter_keys(data: &[u8], keep_output: bool) -> (Option<String>, u32, Vec<u8>) {
    let suffix = format!(" {SSH_MANAGED_MARKER}");
    let mut managed: Option<String> = None;
    let mut unmanaged: u32 = 0;
    let mut out = if keep_output {
        Vec::with_capacity(data.len())
    } else {
        Vec::new()
    };

    let mut pos = 0;
    while pos < data.len() {
        let mut end = pos;
        while end < data.len() && data[end] != b'\n' {
            end += 1;
        }
        let has_nl = end < data.len();
        let line = &data[pos..end];
        pos = if has_nl { end + 1 } else { end };

        if let Ok(text) = std::str::from_utf8(line)
            && let Some(cand) = text.strip_suffix(&suffix)
            && is_strict_key(cand)
        {
            if managed.is_none() {
                managed = Some(cand.to_owned());
            }
            continue;
        }

        let first_non_ws = line
            .iter()
            .copied()
            .find(|&b| !matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c));
        if matches!(first_non_ws, Some(b) if b != b'#') {
            unmanaged = unmanaged.saturating_add(1);
        }
        if keep_output {
            out.extend_from_slice(line);
            if has_nl {
                out.push(b'\n');
            }
        }
    }
    (managed, unmanaged, out)
}

fn write_all_fd(fd: &OwnedFd, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        // SAFETY: fd is open and data points to readable memory of length data.len().
        let n = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
        if n > 0 {
            data = &data[n as usize..];
        } else if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        } else {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
        }
    }
    Ok(())
}

/// Atomically replace `authorized_keys` inside `dir_fd` (temp file, `fsync`,
/// `renameat`, best-effort directory `fsync`) with mode 0600 and root:root
/// ownership when running as root.
fn write_keys_at(dir_fd: &OwnedFd, data: &[u8], me: libc::uid_t) -> Result<(), SshError> {
    let fail = || SshError::Failed("could not write authorized_keys".into());
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let pid = std::process::id();

    let mut opened = None;
    for _ in 0..8 {
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!(".authorized_keys.{pid}.{seq}")).unwrap();
        match openat_raw(dir_fd.as_raw_fd(), &name, flags, 0o600) {
            Ok(fd) => {
                opened = Some((fd, name));
                break;
            }
            Err(err) if err.raw_os_error() == Some(libc::EEXIST) => continue,
            Err(_) => break,
        }
    }
    let Some((fd, tmp_name)) = opened else {
        return Err(fail());
    };

    struct UnlinkGuard<'a> {
        dir_fd: &'a OwnedFd,
        name: &'a CString,
        armed: bool,
    }
    impl Drop for UnlinkGuard<'_> {
        fn drop(&mut self) {
            if self.armed {
                // SAFETY: dir_fd is open and name is NUL-terminated.
                unsafe {
                    libc::unlinkat(self.dir_fd.as_raw_fd(), self.name.as_ptr(), 0);
                }
            }
        }
    }
    let mut guard = UnlinkGuard {
        dir_fd,
        name: &tmp_name,
        armed: true,
    };

    // SAFETY: fd is an open writable descriptor.
    let perm_ok = unsafe {
        libc::fchmod(fd.as_raw_fd(), 0o600) == 0
            && (me != 0 || libc::fchown(fd.as_raw_fd(), 0, 0) == 0)
    };
    if !perm_ok || write_all_fd(&fd, data).is_err() {
        return Err(fail());
    }
    // SAFETY: fd is an open descriptor.
    if unsafe { libc::fsync(fd.as_raw_fd()) } != 0 {
        return Err(fail());
    }
    // Close explicitly so close errors (e.g. ENOSPC on flush) are caught.
    let raw_fd = std::os::fd::IntoRawFd::into_raw_fd(fd);
    // SAFETY: raw_fd was transferred out of OwnedFd so it is closed once.
    if unsafe { libc::close(raw_fd) } != 0 {
        return Err(fail());
    }
    let dest = CString::new("authorized_keys").unwrap();
    // SAFETY: dir_fd is open and both names are NUL-terminated.
    if unsafe {
        libc::renameat(
            dir_fd.as_raw_fd(),
            tmp_name.as_ptr(),
            dir_fd.as_raw_fd(),
            dest.as_ptr(),
        )
    } != 0
    {
        return Err(fail());
    }
    guard.armed = false;
    // SAFETY: dir_fd is open; best-effort directory fsync.
    unsafe {
        libc::fsync(dir_fd.as_raw_fd());
    }
    Ok(())
}

fn read_fd_buf(fd: &OwnedFd, buf: &mut [u8]) -> usize {
    let mut got = 0;
    while got < buf.len() {
        // SAFETY: fd is open and buf[got..] is valid writable memory.
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf[got..].as_mut_ptr().cast(),
                buf.len() - got,
            )
        };
        if n > 0 {
            got += n as usize;
        } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    got
}

fn read_host_keys(host_dir: &Path) -> Vec<String> {
    if host_dir.as_os_str().is_empty() {
        return Vec::new();
    }
    let Some(c_dir) = cstring_of_path(host_dir) else {
        return Vec::new();
    };
    let Ok(dir_fd) = openat_raw(
        libc::AT_FDCWD,
        &c_dir,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(host_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| {
            name.len() >= "ssh_host__key.pub".len()
                && name.starts_with("ssh_host_")
                && name.ends_with("_key.pub")
        })
        .collect();
    names.sort();

    let mut hosts = Vec::with_capacity(names.len());
    let mut raw = [0u8; 1024];
    for name in names {
        let Ok(c_name) = CString::new(name) else {
            continue;
        };
        let Ok(fd) = openat_raw(
            dir_fd.as_raw_fd(),
            &c_name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0,
        ) else {
            continue;
        };
        let Ok(st) = fstat_fd(&fd) else {
            continue;
        };
        if !is_file_mode(st.st_mode) {
            continue;
        }
        let got = read_fd_buf(&fd, &mut raw);
        if got == 0 {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&raw[..got]) else {
            continue;
        };
        let line = text.split('\n').next().unwrap_or("");
        let Some((kind, rest)) = line.split_once(' ') else {
            continue;
        };
        if !matches!(
            kind,
            "ssh-ed25519"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
                | "ssh-rsa"
        ) {
            continue;
        }
        let blob = rest.split_once(' ').map_or(rest, |(b, _)| b);
        if blob.len() >= 16 && blob.bytes().all(|b| is_b64_char(b) || b == b'=') {
            hosts.push(format!("{kind} {blob}"));
        }
    }
    hosts
}

fn check_server(proc_dir: &Path) -> Option<bool> {
    if proc_dir.as_os_str().is_empty() {
        return None;
    }
    let c_dir = cstring_of_path(proc_dir)?;
    let pfd = openat_raw(
        libc::AT_FDCWD,
        &c_dir,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        0,
    )
    .ok()?;
    let entries = std::fs::read_dir(proc_dir).ok()?;
    let mut sbuf = [0u8; 512];
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > 20 || !bytes.iter().all(u8::is_ascii_digit) {
            continue;
        }
        let mut rel = Vec::with_capacity(bytes.len() + 6);
        rel.extend_from_slice(bytes);
        rel.extend_from_slice(b"/stat");
        let Ok(c_rel) = CString::new(rel) else {
            continue;
        };
        let Ok(sfd) = openat_raw(
            pfd.as_raw_fd(),
            &c_rel,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        ) else {
            continue;
        };
        let got = read_fd_buf(&sfd, &mut sbuf);
        let Ok(text) = std::str::from_utf8(&sbuf[..got]) else {
            continue;
        };
        let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else {
            continue;
        };
        if close <= open || &text[open + 1..close] != "sshd" {
            continue;
        }
        let after = text[close + 1..].trim_start();
        if matches!(after.as_bytes().first(), Some(&c) if c != b'Z' && c != b'X') {
            return Some(true);
        }
    }
    Some(false)
}

/// Best-effort log entry for an accepted key; failures here never fail the request.
fn log_added(logs_dir: &Path, fingerprint: &str) {
    if logs_dir.as_os_str().is_empty() || std::fs::create_dir_all(logs_dir).is_err() {
        return;
    }
    let Some(c_dir) = cstring_of_path(logs_dir) else {
        return;
    };
    let Ok(dfd) = openat_raw(
        libc::AT_FDCWD,
        &c_dir,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    ) else {
        return;
    };
    let name = CString::new("rc2fx.log").unwrap();
    let Ok(fd) = openat_raw(
        dfd.as_raw_fd(),
        &name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o644,
    ) else {
        return;
    };
    if !fstat_fd(&fd).is_ok_and(|st| is_file_mode(st.st_mode)) {
        return;
    }
    let ts = jiff::Timestamp::now()
        .strftime("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let line = format!("{ts} Developer SSH key added: {fingerprint}\n");
    let _ = write_all_fd(&fd, line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEPzD7gW9u5+SC5iq8gzujWEOTBQO1CbVCLO8fAOGkip";
    const KEY_FP: &str = "SHA256:QbAgxtEX0z3ZCUzJ86NO6a0PYcnvrkRmVh0+cMm05TY";
    const OTHER: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMS4sKxaX6YqY9tZ3lJgYHL8WbK9F0jUqk1E1h0a2G7K";
    const HOST_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBhostkeyhostkeyhostkeyhostkeyhostkeyhostkey0";

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        ssh_dir: PathBuf,
        keys_path: PathBuf,
        host_dir: PathBuf,
        first_boot: PathBuf,
        logs_dir: PathBuf,
        dev: DeveloperSsh,
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(temp.path()).unwrap();
            chmod(&root, 0o700);
            let home_base = root.join("home");
            let home = home_base.join("root");
            let ssh_dir = home.join(".ssh");
            let etc_dir = root.join("etc");
            let host_dir = etc_dir.join("ssh");
            for dir in [&home_base, &home, &ssh_dir, &etc_dir, &host_dir] {
                std::fs::create_dir_all(dir).unwrap();
                chmod(dir, 0o700);
            }
            write_mode(
                &host_dir.join("ssh_host_ed25519_key.pub"),
                format!("{HOST_KEY} root@rodecaster\n").as_bytes(),
                0o644,
            );
            write_mode(
                &host_dir.join("ssh_host_ed25519_key"),
                b"PRIVATE, never read\n",
                0o600,
            );
            let first_boot = root.join("first-boot");
            let logs_dir = root.join("logs");
            let keys_path = ssh_dir.join("authorized_keys");
            let dev = DeveloperSsh::with_paths(
                ssh_dir.clone(),
                host_dir.clone(),
                first_boot.clone(),
                logs_dir.clone(),
                PathBuf::from(DEFAULT_SSH_PROC_DIR),
            );
            Self {
                _temp: temp,
                root,
                home,
                ssh_dir,
                keys_path,
                host_dir,
                first_boot,
                logs_dir,
                dev,
            }
        }

        async fn enable(&self, key: &str) -> Result<SshStatus, SshError> {
            self.dev
                .set(SshRequest {
                    enabled: true,
                    public_key: Some(key.into()),
                })
                .await
        }

        async fn disable(&self) -> Result<SshStatus, SshError> {
            self.dev
                .set(SshRequest {
                    enabled: false,
                    public_key: None,
                })
                .await
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.home, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(&self.ssh_dir, std::fs::Permissions::from_mode(0o700));
        }
    }

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn write_mode(path: &Path, data: &[u8], mode: u32) {
        std::fs::write(path, data).unwrap();
        chmod(path, mode);
    }

    #[tokio::test]
    async fn status_reports_counts_host_keys_and_server() {
        let mut f = Fixture::new();
        write_mode(
            &f.keys_path,
            format!("{OTHER}\n# a comment\n\n").as_bytes(),
            0o600,
        );
        // Symlinked host pub key must be skipped.
        let sym_target = f.root.join("ext_host.pub");
        write_mode(
            &sym_target,
            format!("{OTHER} symlink@host\n").as_bytes(),
            0o644,
        );
        symlink(&sym_target, f.host_dir.join("ssh_host_rsa_key.pub")).unwrap();

        let st = f.dev.status().await.unwrap();
        assert!(!st.managed);
        assert_eq!(st.managed_fingerprint, None);
        assert_eq!(st.unmanaged_keys, 1);
        assert!(st.server_running.is_some());
        assert_eq!(st.host_keys, vec![HOST_KEY]);

        // Fake /proc covers running, zombie/stopped, and unknown server states.
        let fake_proc = f.root.join("proc");
        let pid1 = fake_proc.join("101");
        std::fs::create_dir_all(&pid1).unwrap();
        let stat1 = pid1.join("stat");
        write_mode(&stat1, b"101 (sshd) S 1 101 101 0 -1 4194560\n", 0o644);
        f.dev.paths.proc_dir = fake_proc;
        assert_eq!(f.dev.status().await.unwrap().server_running, Some(true));

        write_mode(&stat1, b"101 (sshd) Z 1 101 101 0 -1 4194560\n", 0o644);
        assert_eq!(f.dev.status().await.unwrap().server_running, Some(false));

        f.dev.paths.proc_dir = f.root.join("no-such-proc");
        assert_eq!(f.dev.status().await.unwrap().server_running, None);
    }

    #[tokio::test]
    async fn enable_logs_fingerprint_and_tolerates_log_failure() {
        let f = Fixture::new();
        f.enable(KEY).await.unwrap();
        let log_text = std::fs::read_to_string(f.logs_dir.join("rc2fx.log")).unwrap();
        assert!(log_text.contains(&format!("Developer SSH key added: {KEY_FP}")));
        assert!(!log_text.contains(&KEY["ssh-ed25519 ".len()..]));

        // A file blocking logs_dir must not fail an accepted key.
        let f2 = Fixture::new();
        write_mode(&f2.logs_dir, b"a file where the folder should be", 0o600);
        f2.enable(KEY).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&f2.keys_path).unwrap(),
            format!("{KEY} {SSH_MANAGED_MARKER}\n")
        );
    }

    #[tokio::test]
    async fn enable_and_disable_manage_single_marked_line() {
        let f = Fixture::new();
        write_mode(
            &f.keys_path,
            format!("{OTHER}\n# keep me\n").as_bytes(),
            0o600,
        );

        // Trailing comments on API input are stripped before writing.
        let st = f.enable(&format!("{KEY} user@laptop\n")).await.unwrap();
        assert!(st.managed);
        assert_eq!(st.managed_fingerprint.as_deref(), Some(KEY_FP));
        assert_eq!(st.unmanaged_keys, 1);

        let managed_line = format!("{KEY} {SSH_MANAGED_MARKER}");
        let lines: Vec<String> = std::fs::read_to_string(&f.keys_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines, vec![OTHER, "# keep me", &managed_line]);
        let mode = std::fs::symlink_metadata(&f.keys_path).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600);

        // Re-enabling replaces the existing managed line without duplicating it.
        f.enable(KEY).await.unwrap();
        let lines2: Vec<String> = std::fs::read_to_string(&f.keys_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(lines2, vec![OTHER, "# keep me", &managed_line]);

        // Disabling removes only the managed line.
        let st3 = f.disable().await.unwrap();
        assert!(!st3.managed);
        assert_eq!(st3.managed_fingerprint, None);
        assert_eq!(st3.unmanaged_keys, 1);
        assert_eq!(
            std::fs::read_to_string(&f.keys_path).unwrap(),
            format!("{OTHER}\n# keep me\n")
        );

        // Disabling with no managed line writes nothing (inode stays identical).
        let before = std::fs::symlink_metadata(&f.keys_path).unwrap();
        f.disable().await.unwrap();
        let after = std::fs::symlink_metadata(&f.keys_path).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.mtime_nsec(), after.mtime_nsec());
    }

    #[tokio::test]
    async fn lookalikes_are_never_removed() {
        let f = Fixture::new();
        let raw = format!(
            "command=\"sh\" {KEY} {SSH_MANAGED_MARKER}\n{KEY} {SSH_MANAGED_MARKER} extra\nssh-rsa AAAAB3NzaC1yc2E {SSH_MANAGED_MARKER}\n"
        );
        write_mode(&f.keys_path, raw.as_bytes(), 0o600);

        let st = f.disable().await.unwrap();
        assert!(!st.managed);
        assert_eq!(st.unmanaged_keys, 3);
        assert_eq!(std::fs::read_to_string(&f.keys_path).unwrap(), raw);
    }

    #[tokio::test]
    async fn invalid_keys_and_requests_refused_without_side_effects() {
        let f = Fixture::new();
        let orig = format!("{OTHER}\n");
        write_mode(&f.keys_path, orig.as_bytes(), 0o600);

        assert!(matches!(
            f.dev
                .set(SshRequest {
                    enabled: true,
                    public_key: None,
                })
                .await,
            Err(SshError::Invalid(_))
        ));
        assert!(matches!(
            f.dev
                .set(SshRequest {
                    enabled: false,
                    public_key: Some(KEY.into()),
                })
                .await,
            Err(SshError::Invalid(_))
        ));

        let pad600 = " ".repeat(600);
        for bad in [
            String::new(),
            format!("ssh-rsa {}", &KEY[12..]),
            format!("command=\"id\" {KEY}"),
            KEY[..KEY.len() - 1].to_owned(),
            format!("{KEY}\n{OTHER}"),
            format!("{KEY}\n\n"),
            format!("{KEY}{pad600}"),
            format!("{KEY}\0garbage\n"),
        ] {
            assert!(
                matches!(f.enable(&bad).await, Err(SshError::Invalid(_))),
                "accepted bad key {bad:?}"
            );
            assert_eq!(std::fs::read_to_string(&f.keys_path).unwrap(), orig);
        }

        assert!(is_strict_key(KEY));
        assert!(!is_strict_key(&format!("{KEY} comment")));
        assert!(!is_strict_key(&format!(" {KEY}")));

        let names: Vec<_> = std::fs::read_dir(&f.ssh_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["authorized_keys"]);
    }

    #[tokio::test]
    async fn first_boot_refuses_changes() {
        let f = Fixture::new();
        write_mode(&f.first_boot, b"", 0o600);
        let orig = format!("{KEY} {SSH_MANAGED_MARKER}\n");
        write_mode(&f.keys_path, orig.as_bytes(), 0o600);

        let err = f.enable(OTHER).await.unwrap_err();
        assert!(matches!(err, SshError::FirstBoot(_)));
        assert_eq!(err.status(), 409);
        assert_eq!(std::fs::read_to_string(&f.keys_path).unwrap(), orig);

        assert!(matches!(f.disable().await, Err(SshError::FirstBoot(_))));
        assert!(f.dev.status().await.unwrap().managed);
    }

    #[tokio::test]
    async fn unsafe_paths_are_refused() {
        let f = Fixture::new();
        let target = f.root.join("elsewhere");
        write_mode(&target, b"", 0o600);

        // Symlink, dangling symlink, and hard link at authorized_keys.
        symlink(&target, &f.keys_path).unwrap();
        let err = f.enable(KEY).await.unwrap_err();
        assert!(matches!(err, SshError::Unsafe(_)));
        assert_eq!(err.status(), 409);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
        std::fs::remove_file(&f.keys_path).unwrap();

        symlink(f.root.join("missing-target"), &f.keys_path).unwrap();
        assert!(matches!(f.dev.status().await, Err(SshError::Unsafe(_))));
        std::fs::remove_file(&f.keys_path).unwrap();

        std::fs::hard_link(&target, &f.keys_path).unwrap();
        assert!(matches!(f.enable(KEY).await, Err(SshError::Unsafe(_))));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
        std::fs::remove_file(&f.keys_path).unwrap();

        // Symlink at ssh_dir.
        std::fs::remove_dir(&f.ssh_dir).unwrap();
        symlink(&f.root, &f.ssh_dir).unwrap();
        assert!(matches!(f.dev.status().await, Err(SshError::Unsafe(_))));
        std::fs::remove_file(&f.ssh_dir).unwrap();
    }

    #[tokio::test]
    async fn missing_dir_created_private_and_status_changes_nothing() {
        let f = Fixture::new();
        std::fs::remove_dir(&f.ssh_dir).unwrap();

        let st = f.dev.status().await.unwrap();
        assert!(!f.ssh_dir.exists() && !st.managed && st.unmanaged_keys == 0);

        let st_dis = f.disable().await.unwrap();
        assert!(!f.ssh_dir.exists() && !st_dis.managed && st_dis.unmanaged_keys == 0);

        f.enable(KEY).await.unwrap();
        let meta = std::fs::symlink_metadata(&f.ssh_dir).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.mode() & 0o7777, 0o700);
        assert_eq!(
            std::fs::read_to_string(&f.keys_path).unwrap(),
            format!("{KEY} {SSH_MANAGED_MARKER}\n")
        );
    }

    #[tokio::test]
    async fn preserves_every_other_byte_and_missing_final_newline() {
        let f = Fixture::new();
        write_mode(
            &f.keys_path,
            format!("{OTHER}\r\n  \n# c\n{KEY} {SSH_MANAGED_MARKER}\nlast-line-without-newline")
                .as_bytes(),
            0o600,
        );

        f.disable().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&f.keys_path).unwrap(),
            format!("{OTHER}\r\n  \n# c\nlast-line-without-newline")
        );

        f.enable(KEY).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&f.keys_path).unwrap(),
            format!("{OTHER}\r\n  \n# c\nlast-line-without-newline\n{KEY} {SSH_MANAGED_MARKER}\n")
        );
    }

    #[tokio::test]
    async fn writable_by_others_oversized_nul_and_ancestors_refused() {
        let f = Fixture::new();
        write_mode(&f.keys_path, format!("{OTHER}\n").as_bytes(), 0o600);

        chmod(&f.ssh_dir, 0o770);
        assert!(matches!(f.enable(KEY).await, Err(SshError::Unsafe(_))));
        chmod(&f.ssh_dir, 0o700);

        chmod(&f.keys_path, 0o666);
        assert!(matches!(f.dev.status().await, Err(SshError::Unsafe(_))));
        chmod(&f.keys_path, 0o600);

        // Oversized file (> 1 MiB).
        let big = vec![b'#'; MAX_KEYS_BYTES + 1];
        write_mode(&f.keys_path, &big, 0o600);
        assert!(matches!(f.disable().await, Err(SshError::Unsafe(_))));
        assert_eq!(
            std::fs::symlink_metadata(&f.keys_path).unwrap().len() as usize,
            MAX_KEYS_BYTES + 1
        );

        // NUL bytes in file.
        let nul_content = format!("{OTHER}\n\0hidden\n{KEY} {SSH_MANAGED_MARKER}\n");
        write_mode(&f.keys_path, nul_content.as_bytes(), 0o600);
        assert!(matches!(f.disable().await, Err(SshError::Unsafe(_))));
        assert!(matches!(f.dev.status().await, Err(SshError::Unsafe(_))));
        assert_eq!(std::fs::read(&f.keys_path).unwrap(), nul_content.as_bytes());
        std::fs::remove_file(&f.keys_path).unwrap();

        // Ancestor writable by others (and sticky when not owned by root).
        chmod(&f.home, 0o777);
        assert!(matches!(f.enable(KEY).await, Err(SshError::Unsafe(_))));
        assert!(!f.keys_path.exists());
        // SAFETY: geteuid(2) is always safe.
        if unsafe { libc::geteuid() } != 0 {
            chmod(&f.home, 0o1777);
            assert!(matches!(f.dev.status().await, Err(SshError::Unsafe(_))));
        }
        chmod(&f.home, 0o755);
        assert!(f.dev.status().await.is_ok());
    }

    #[tokio::test]
    async fn busy_when_another_operation_holds_permit() {
        let f = Fixture::new();
        let _held = f.dev.permit.clone().try_acquire_owned().unwrap();
        let err = f.dev.status().await.unwrap_err();
        assert_eq!(err, SshError::Busy);
        assert_eq!(err.status(), 409);
        assert_eq!(err.to_string(), "Another SSH change is in progress");
    }

    #[tokio::test]
    async fn enable_refuses_exceeding_size_cap() {
        let f = Fixture::new();
        let make_pad = |n: usize| {
            let mut b = vec![b'#'; n];
            b[n - 1] = b'\n';
            b
        };

        let near_cap = make_pad(MAX_KEYS_BYTES - 10);
        write_mode(&f.keys_path, &near_cap, 0o600);
        let err = f.enable(KEY).await.unwrap_err();
        assert!(matches!(err, SshError::Unsafe(ref msg) if msg.contains("is too large")));
        assert_eq!(std::fs::read(&f.keys_path).unwrap().len(), near_cap.len());

        let st = f.dev.status().await.unwrap();
        assert!(!st.managed);
        f.disable().await.unwrap();

        let managed_line = format!("{KEY} {SSH_MANAGED_MARKER}\n");
        let exact_pad = make_pad(MAX_KEYS_BYTES - managed_line.len());
        write_mode(&f.keys_path, &exact_pad, 0o600);
        let st_ok = f.enable(KEY).await.unwrap();
        assert!(st_ok.managed);
        assert_eq!(std::fs::read(&f.keys_path).unwrap().len(), MAX_KEYS_BYTES);
        let st_after = f.disable().await.unwrap();
        assert!(!st_after.managed);
        assert_eq!(std::fs::read(&f.keys_path).unwrap().len(), exact_pad.len());
    }
}
