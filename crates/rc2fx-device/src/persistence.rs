//! Linux/Unix durable state storage, anchored to an open directory descriptor.
//! A successful commit includes both the file and containing directory sync.
use std::io;
use std::path::Path;

#[derive(Debug)]
pub(crate) enum CommitError {
    BeforeRename(io::Error),
    #[cfg(unix)]
    AfterRename(io::Error),
}

#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitPhase {
    Write,
    FileSync,
    Rename,
    DirectorySync,
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    pub(crate) struct Directory {
        directory: File,
        // The descriptor, rather than the pathname, owns the flock for this lifetime.
        _lock: File,
        #[cfg(test)]
        failure: Option<CommitPhase>,
    }

    fn create_directory(path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            Ok(_) => return Err(io::Error::other("data path must be a real directory")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        create_directory(parent)?;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => File::open(parent)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::symlink_metadata(path)?.is_dir() {
                    Ok(())
                } else {
                    Err(io::Error::other("data path must be a real directory"))
                }
            }
            Err(error) => Err(error),
        }
    }

    fn open_at(directory: &File, name: &CStr, flags: libc::c_int) -> io::Result<File> {
        // SAFETY: the directory descriptor and NUL-terminated basename stay valid
        // through openat; a successful new descriptor transfers ownership to File.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }

    impl Directory {
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            if path.as_os_str().is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "data path is required",
                ));
            }
            create_directory(path)?;
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)?;
            directory.set_permissions(fs::Permissions::from_mode(0o700))?;
            let lock = open_at(&directory, c"daemon.lock", libc::O_CREAT | libc::O_RDWR)?;
            if !lock.metadata()?.is_file() {
                return Err(io::Error::other("daemon.lock must be a regular file"));
            }
            lock.set_permissions(fs::Permissions::from_mode(0o600))?;
            // SAFETY: flock only uses this valid descriptor, which remains owned
            // by Directory until the service drops its store.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                directory,
                _lock: lock,
                #[cfg(test)]
                failure: None,
            })
        }

        pub(crate) fn read(&self, maximum: usize) -> io::Result<Option<Vec<u8>>> {
            let file = match open_at(&self.directory, c"state.json", libc::O_RDONLY) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let metadata = file.metadata()?;
            if !metadata.is_file() {
                return Err(io::Error::other("state.json must be a regular file"));
            }
            let mut bytes = Vec::new();
            (&file).take(maximum as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "state.json exceeds its size limit",
                ));
            }
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            Ok(Some(bytes))
        }

        #[cfg(test)]
        pub(crate) fn fail_next(&mut self, phase: CommitPhase) {
            self.failure = Some(phase);
        }

        #[cfg(test)]
        fn checkpoint(&mut self, phase: CommitPhase) -> io::Result<()> {
            if self.failure == Some(phase) {
                self.failure = None;
                Err(io::Error::other(format!("injected {phase:?} failure")))
            } else {
                Ok(())
            }
        }

        pub(crate) fn commit(&mut self, bytes: &[u8]) -> Result<(), CommitError> {
            use CommitError::{AfterRename, BeforeRename};
            let name = CString::new(format!(".state-{}", random_id().map_err(BeforeRename)?))
                .expect("hex basename");
            let mut file = open_at(
                &self.directory,
                &name,
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
            )
            .map_err(BeforeRename)?;
            // The temporary basename belongs only to this invocation. Never remove
            // the destination after rename, including when directory sync fails.
            let result = (|| {
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(BeforeRename)?;
                #[cfg(test)]
                self.checkpoint(CommitPhase::Write).map_err(BeforeRename)?;
                file.write_all(bytes).map_err(BeforeRename)?;
                #[cfg(test)]
                self.checkpoint(CommitPhase::FileSync)
                    .map_err(BeforeRename)?;
                file.sync_all().map_err(BeforeRename)?;
                let fd = file.into_raw_fd();
                // SAFETY: ownership was removed from File. Close exactly once;
                // retrying close after EINTR can close a reused descriptor on Linux.
                if unsafe { libc::close(fd) } != 0 {
                    return Err(BeforeRename(io::Error::last_os_error()));
                }
                #[cfg(test)]
                self.checkpoint(CommitPhase::Rename).map_err(BeforeRename)?;
                // SAFETY: both basenames and the held directory descriptor are valid.
                if unsafe {
                    libc::renameat(
                        self.directory.as_raw_fd(),
                        name.as_ptr(),
                        self.directory.as_raw_fd(),
                        c"state.json".as_ptr(),
                    )
                } != 0
                {
                    return Err(BeforeRename(io::Error::last_os_error()));
                }
                #[cfg(test)]
                self.checkpoint(CommitPhase::DirectorySync)
                    .map_err(AfterRename)?;
                self.directory.sync_all().map_err(AfterRename)
            })();
            // SAFETY: only our randomly created temporary basename is addressed.
            // After rename this is ENOENT, leaving state.json intact.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0);
            }
            result
        }
    }

    pub(crate) fn random_id() -> io::Result<String> {
        let mut bytes = [0_u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let mut id = String::with_capacity(32);
        use std::fmt::Write as _;
        for byte in bytes {
            write!(&mut id, "{byte:02x}").expect("writing to String");
        }
        Ok(id)
    }
}

#[cfg(not(unix))]
mod platform {
    use super::*;
    pub(crate) struct Directory;
    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "device state requires Unix directory durability and file locking",
        )
    }
    impl Directory {
        pub(crate) fn open(_: &Path) -> io::Result<Self> {
            Err(unsupported())
        }
        pub(crate) fn read(&self, _: usize) -> io::Result<Option<Vec<u8>>> {
            Err(unsupported())
        }
        pub(crate) fn commit(&mut self, _: &[u8]) -> Result<(), CommitError> {
            Err(CommitError::BeforeRename(unsupported()))
        }
    }
    pub(crate) fn random_id() -> io::Result<String> {
        Err(unsupported())
    }
}

pub(crate) use platform::{Directory, random_id};
