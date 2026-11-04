//! Stage an AppImage helper on ordinary storage before asking polkit to run it.
//! Root cannot enter the desktop user's FUSE mount. Copy the complete AppDir,
//! including its loader libraries, rather than extracting the outer image as
//! root into the AppImage runtime's predictable shared temporary directory.

use crate::{Error, Result};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use tempfile::TempDir;

pub(super) struct Launch {
    pub executable: PathBuf,
    // The caller must retain this until the elevated helper exits.
    pub guard: Option<TempDir>,
}

struct Budget {
    entries: usize,
    bytes: u64,
}

const MAX_ENTRIES: usize = 16_384;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

/// Packaging hints are not authority. Only use an AppDir containing this exact
/// running executable and the expected launcher; unrelated inherited hints
/// leave standalone launches unchanged.
pub(super) fn prepare(exe: &Path, appdir: Option<&OsStr>) -> Result<Launch> {
    let plain = || Launch {
        executable: exe.to_path_buf(),
        guard: None,
    };
    let Some(hint) = appdir else {
        return Ok(plain());
    };
    let Ok(root) = Path::new(hint).canonicalize() else {
        return Ok(plain());
    };
    let exe_path = exe.canonicalize()?;
    if !exe_path.starts_with(&root) {
        return Ok(plain());
    }
    if !Path::new(hint).is_absolute() || root.parent().is_none() {
        return Err(Error("invalid AppImage directory".into()));
    }
    if root.join("usr/bin/rc2fx").canonicalize()? != exe_path {
        return Err(Error("unrecognized AppImage executable layout".into()));
    }
    let launcher = root.join("AppRun");
    let metadata = fs::symlink_metadata(&launcher)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(Error(
            "the AppImage needs a regular executable AppRun".into(),
        ));
    }
    // Do not inherit TMPDIR: it can name the FUSE mount (or another unusual
    // filesystem). /tmp may be noexec, in which case launching fails normally
    // rather than falling back to unsafe root extraction.
    let guard = tempfile::Builder::new()
        .prefix("rc2fx-helper-")
        .tempdir_in("/tmp")?;
    fs::set_permissions(guard.path(), fs::Permissions::from_mode(0o700))?;
    let destination = guard.path().join("AppDir");
    let mut budget = Budget {
        entries: MAX_ENTRIES,
        bytes: MAX_BYTES,
    };
    copy_entry(&root, &destination, &root, &destination, 0, &mut budget)?;
    Ok(Launch {
        executable: destination.join("AppRun"),
        guard: Some(guard),
    })
}

fn copy_entry(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
    depth: usize,
    budget: &mut Budget,
) -> Result<()> {
    if depth > MAX_DEPTH || budget.entries == 0 {
        return Err(Error(
            "AppImage helper staging exceeds its entry/depth limit".into(),
        ));
    }
    budget.entries -= 1;
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = source.canonicalize()?;
        let relative = target
            .strip_prefix(source_root)
            .map_err(|_| Error("AppImage symlink escapes its AppDir".into()))?;
        let link = fs::read_link(source)?;
        // Relative links retain their exact spelling; contained absolute links
        // must point into the copy, not back into the inaccessible FUSE mount.
        symlink(
            if link.is_absolute() {
                destination_root.join(relative)
            } else {
                link
            },
            destination,
        )?;
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(
                &entry.path(),
                &destination.join(entry.file_name()),
                source_root,
                destination_root,
                depth + 1,
                budget,
            )?;
        }
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
        )?;
    } else if metadata.is_file() {
        budget.bytes = budget
            .bytes
            .checked_sub(metadata.len())
            .ok_or_else(|| Error("AppImage helper staging exceeds its byte limit".into()))?;
        // Limit reads as well as the initial stat: a changing source cannot
        // make this copy unbounded. Source AppImages are read-only mounts.
        use std::io::{Read, Write};
        let mut input = fs::File::open(source)?.take(metadata.len() + 1);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        if std::io::copy(&mut input, &mut output)? != metadata.len() {
            return Err(Error(
                "AppImage file changed while staging its helper".into(),
            ));
        }
        output.flush()?;
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
        )?;
    } else {
        return Err(Error(
            "unsupported file type in AppImage helper staging".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn executable(path: &Path, contents: &[u8]) {
        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn layout() -> (TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("App Dir ' ; $(false)");
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        fs::create_dir_all(root.join("usr/lib")).unwrap();
        let exe = root.join("usr/bin/rc2fx");
        executable(&exe, b"fixture executable");
        executable(
            &root.join("AppRun"),
            br##"#!/bin/sh
APPDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export APPDIR
export LD_LIBRARY_PATH="$APPDIR/usr/lib"
exec "$APPDIR/usr/bin/rc2fx" "$@"
"##,
        );
        (temp, root, exe)
    }

    #[test]
    fn standalone_launches_ignore_unrelated_or_missing_packaging_hints() {
        let (temp, root, _) = layout();
        let exe = temp.path().join("standalone");
        executable(&exe, b"plain");
        for dir in [
            None,
            Some(root.as_os_str()),
            Some(OsStr::new("/nonexistent/appdir")),
        ] {
            let launch = prepare(&exe, dir).unwrap();
            assert_eq!(launch.executable, exe);
            assert!(launch.guard.is_none());
        }
        assert!(prepare(&exe, Some(OsStr::new("/"))).is_err());
    }

    #[test]
    fn invalid_launchers_and_layouts_are_rejected() {
        let (_temp, root, exe) = layout();
        fs::set_permissions(root.join("AppRun"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(prepare(&exe, Some(root.as_os_str())).is_err());
        fs::remove_file(root.join("AppRun")).unwrap();
        symlink("usr/bin/rc2fx", root.join("AppRun")).unwrap();
        assert!(prepare(&exe, Some(root.as_os_str())).is_err());
        let other = root.join("other");
        executable(&other, b"other");
        assert!(prepare(&other, Some(root.as_os_str())).is_err());
    }

    #[test]
    fn copy_preserves_contained_links_modes_and_guard_lifetime() {
        let (_temp, root, exe) = layout();
        fs::write(root.join("usr/lib/library"), b"library").unwrap();
        fs::set_permissions(
            root.join("usr/lib/library"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        symlink("library", root.join("usr/lib/relative")).unwrap();
        symlink(root.join("usr/lib/library"), root.join("usr/lib/absolute")).unwrap();
        let launch = prepare(&exe, Some(root.as_os_str())).unwrap();
        let guard_path = launch.guard.as_ref().unwrap().path().to_path_buf();
        assert_eq!(
            fs::metadata(&guard_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let copied = launch.executable.parent().unwrap();
        assert_eq!(
            fs::read_link(copied.join("usr/lib/relative")).unwrap(),
            Path::new("library")
        );
        assert_eq!(
            fs::read(copied.join("usr/lib/absolute")).unwrap(),
            b"library"
        );
        assert!(
            fs::canonicalize(copied.join("usr/lib/absolute"))
                .unwrap()
                .starts_with(&guard_path)
        );
        assert_eq!(
            fs::metadata(copied.join("usr/lib/library"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert!(launch.executable.exists());
        drop(launch);
        assert!(!guard_path.exists());
    }

    #[test]
    fn escaping_symlinks_and_special_files_are_rejected() {
        let (temp, root, exe) = layout();
        fs::write(temp.path().join("outside"), b"outside").unwrap();
        symlink("../../../outside", root.join("usr/lib/escape")).unwrap();
        assert!(prepare(&exe, Some(root.as_os_str())).is_err());
        fs::remove_file(root.join("usr/lib/escape")).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(root.join("socket")).unwrap();
        assert!(prepare(&exe, Some(root.as_os_str())).is_err());
        drop(socket);
    }

    #[test]
    fn copy_budgets_bound_entries_and_bytes() {
        let (_temp, root, _) = layout();
        for (entries, bytes) in [(1, MAX_BYTES), (MAX_ENTRIES, 1)] {
            let output = tempfile::tempdir().unwrap();
            let destination = output.path().join("copy");
            assert!(
                copy_entry(
                    &root,
                    &destination,
                    &root,
                    &destination,
                    0,
                    &mut Budget { entries, bytes }
                )
                .is_err()
            );
        }
    }

    #[test]
    fn copied_launcher_loads_bundled_library_and_forwards_literal_arguments() {
        let (temp, root, exe) = layout();
        // Real ELF fixtures ensure this tests dynamic loading, not merely an
        // environment variable. No pkexec, GUI, USB, or privileged process.
        let library = temp.path().join("library.c");
        fs::write(&library, "int fixture(void) { return 42; }\n").unwrap();
        let main = temp.path().join("main.c");
        fs::write(
            &main,
            r#"#include <stdio.h>
#include <stdlib.h>
extern int fixture(void);
int main(int argc, char **argv) {
    printf("library=%d\nappdir=%s\n", fixture(), getenv("APPDIR"));
    for (int i=1; i<argc; i++) printf("arg=%s\n", argv[i]);
    return argc > 2 ? 17 : 0;
}
"#,
        )
        .unwrap();
        assert!(
            Command::new("cc")
                .args(["-shared", "-fPIC"])
                .arg(&library)
                .arg("-o")
                .arg(root.join("usr/lib/libfixture.so"))
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("cc")
                .arg(&main)
                .arg("-L")
                .arg(root.join("usr/lib"))
                .arg("-lfixture")
                .arg("-o")
                .arg(&exe)
                .status()
                .unwrap()
                .success()
        );
        let launch = prepare(&exe, Some(root.as_os_str())).unwrap();
        let appdir = launch.executable.parent().unwrap();
        for args in [
            vec!["--install-usb-rule"],
            vec![
                "--installer-helper",
                "root ' ; $(false)",
                "work\nsecond line",
            ],
        ] {
            let output = Command::new(&launch.executable)
                .args(&args)
                .env_remove("APPDIR")
                .env_remove("APPIMAGE")
                .env_remove("LD_LIBRARY_PATH")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(if args.len() > 2 { 17 } else { 0 })
            );
            assert!(output.stderr.is_empty(), "{:?}", output.stderr);
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "library=42\nappdir={}\n{}",
                    appdir.display(),
                    args.iter()
                        .map(|arg| format!("arg={arg}\n"))
                        .collect::<String>()
                )
            );
        }
    }
}
