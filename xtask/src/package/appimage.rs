//! Linux release payload: a minimal AppDir assembled here, shared libraries
//! deployed by the externally supplied `linuxdeploy`, and the image packaged
//! directly by the separately pinned `appimagetool`. No repository
//! documentation, source, configuration or mixer sidecars enter the image.
use crate::{Result, run, sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const APPRUN: &str = include_str!("../../assets/rc2fx.AppRun");
const DESKTOP: &str = include_str!("../../assets/rc2fx.desktop");
const ICON: &[u8] = include_bytes!("../../assets/rc2fx.svg");
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    pub linuxdeploy: PathBuf,
    pub appimagetool: PathBuf,
    pub runtime: PathBuf,
    pub patchelf: PathBuf,
}

impl Tools {
    /// The release workflow installs and verifies these binaries, then passes
    /// their absolute paths. Keeping versions out of xtask leaves one pinned
    /// dependency definition in CI.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            linuxdeploy: tool("RC2FX_LINUXDEPLOY")?,
            appimagetool: tool("RC2FX_APPIMAGETOOL")?,
            runtime: tool("RC2FX_APPIMAGE_RUNTIME")?,
            patchelf: tool("RC2FX_PATCHELF")?,
        })
    }
}

fn tool(variable: &str) -> Result<PathBuf> {
    let value = env::var_os(variable)
        .ok_or_else(|| format!("{variable} must point to the pinned AppImage tool"))?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "{variable} must be an absolute path: {}",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!("{variable} is not a file: {}", path.display()));
    }
    Ok(path)
}

fn appimage_arch(arch: &str) -> Result<&'static str> {
    match arch {
        "x64" => Ok("x86_64"),
        "arm64" => Ok("aarch64"),
        other => Err(format!("AppImage has no architecture mapping for {other}")),
    }
}

fn write_member(path: &Path, data: &[u8], mode: u32) -> Result {
    #[cfg(not(unix))]
    let _ = mode; // Windows does not have Unix file permission bits.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn license_name(name: &str) -> Result<&str> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || Path::new(name).file_name().and_then(|n| n.to_str()) != Some(name)
    {
        return Err(format!("invalid licence file name {name:?}"));
    }
    Ok(name)
}

/// Create the application payload before dependency deployment. The only
/// repository files here are the icon, desktop entry and licence reports.
pub fn stage(appdir: &Path, executable: &[u8], licenses: &[(String, Vec<u8>)]) -> Result {
    if appdir.exists() {
        fs::remove_dir_all(appdir).map_err(|e| format!("{}: {e}", appdir.display()))?;
    }
    write_member(&appdir.join("usr/bin/rc2fx"), executable, 0o755)?;
    write_member(
        &appdir.join("usr/share/applications/rc2fx.desktop"),
        DESKTOP.as_bytes(),
        0o644,
    )?;
    write_member(
        &appdir.join("usr/share/icons/hicolor/scalable/apps/rc2fx.svg"),
        ICON,
        0o644,
    )?;
    for (name, data) in licenses {
        let name = license_name(name)?;
        write_member(
            &appdir.join("usr/share/licenses/rc2fx").join(name),
            data,
            0o644,
        )?;
    }
    write_member(&appdir.join("AppRun"), APPRUN.as_bytes(), 0o755)?;
    Ok(())
}

/// AppDir root entries required by appimagetool. linuxdeploy normally makes
/// these, but keep the fallback deterministic when it does not.
fn ensure_root_entries(appdir: &Path) -> Result {
    for (source, target) in [
        ("usr/share/applications/rc2fx.desktop", "rc2fx.desktop"),
        (
            "usr/share/icons/hicolor/scalable/apps/rc2fx.svg",
            "rc2fx.svg",
        ),
    ] {
        let target = appdir.join(target);
        if !target.exists() {
            fs::copy(appdir.join(source), &target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

/// The user-private helper requires a regular, executable AppRun at the
/// AppDir root. Recheck the launcher after linuxdeploy, keeping the contract
/// explicit if a tool ever replaces it with a link.
fn ensure_regular_apprun(appdir: &Path) -> Result {
    let path = appdir.join("AppRun");
    let metadata = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() {
        let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        write_member(&path, &data, 0o755)?;
    } else if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Notices for the libraries linuxdeploy is allowed to copy. Take the exact
/// build host's copyright files; never substitute generic license templates.
pub fn license_sections(root: &Path) -> Result<Vec<(String, String)>> {
    let mut sections = Vec::new();
    for (name, locations) in [
        (
            "libxkbcommon / libxkbcommon-x11",
            [
                "/usr/share/doc/libxkbcommon0/copyright",
                "/usr/share/licenses/libxkbcommon/LICENSE",
            ],
        ),
        (
            "libXau",
            [
                "/usr/share/doc/libxau6/copyright",
                "/usr/share/doc/libXau/COPYING",
            ],
        ),
        (
            "libXdmcp",
            [
                "/usr/share/doc/libxdmcp6/copyright",
                "/usr/share/doc/libXdmcp/COPYING",
            ],
        ),
        (
            "libxcb-xkb",
            [
                "/usr/share/doc/libxcb-xkb1/copyright",
                "/usr/share/licenses/libxcb/COPYING",
            ],
        ),
        (
            "libbsd",
            [
                "/usr/share/doc/libbsd0/copyright",
                "/usr/share/licenses/libbsd/COPYING",
            ],
        ),
        (
            "libmd",
            [
                "/usr/share/doc/libmd0/copyright",
                "/usr/share/licenses/libmd/COPYING",
            ],
        ),
    ] {
        let path = locations
            .iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .ok_or_else(|| format!("missing build-host copyright file for bundled {name}"))?;
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if text.trim().is_empty() {
            return Err(format!("empty copyright file for {name}"));
        }
        sections.push((format!("AppImage library — {name}"), text));
    }
    let path = root.join("target/appimage-legal/notices.json");
    let data = fs::read(&path).map_err(|e| {
        format!(
            "{}: {e}; run python3 .github/scripts/prepare-appimage-notices.py",
            path.display()
        )
    })?;
    let document: serde_json::Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let runtime_path = tool("RC2FX_APPIMAGE_RUNTIME")?;
    let runtime_bytes = fs::read(&runtime_path).map_err(|e| e.to_string())?;
    let expected = document["runtimeSha256"]
        .as_str()
        .ok_or("AppImage notices do not identify the runtime")?;
    if sha256(&runtime_bytes) != expected {
        return Err(
            "AppImage runtime differs from the version covered by its notices and source bundle"
                .into(),
        );
    }
    if document["schemaVersion"] != 1 {
        return Err("unsupported AppImage notices schema".into());
    }
    let runtime = document["sections"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("missing AppImage runtime notices")?;
    for section in runtime {
        let title = section["title"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("missing runtime notice title")?;
        let text = section["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("missing runtime license text")?;
        sections.push((title.to_owned(), text.to_owned()));
    }
    Ok(sections)
}

fn covered_library(name: &str) -> bool {
    [
        "libxkbcommon.so",
        "libxkbcommon-x11.so",
        "libXau.so",
        "libXdmcp.so",
        "libxcb-xkb.so",
        "libbsd.so",
        "libmd.so",
    ]
    .iter()
    .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}.")))
}

fn verify_library_notices(directory: &Path) -> Result {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            verify_library_notices(&entry.path())?;
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !covered_library(&name) {
                return Err(format!(
                    "linuxdeploy bundled {name} without a reviewed license notice; add its build-host copyright to AppImage license_sections"
                ));
            }
        }
    }
    Ok(())
}

/// Deploy shared libraries with linuxdeploy, then package the AppDir with the
/// separately pinned appimagetool. linuxdeploy is never asked to produce the
/// image itself, so its bundled/older appimagetool cannot be selected.
pub fn build(appdir: &Path, out: &Path, name: &str, arch: &str, tools: &Tools) -> Result<PathBuf> {
    let arch = appimage_arch(arch)?;
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let appimage = out.join(format!("{name}.AppImage"));
    let _ = fs::remove_file(&appimage);
    let tool_dir = tools
        .appimagetool
        .parent()
        .ok_or_else(|| "appimagetool has no parent directory".to_string())?;

    let mut deploy = Command::new(&tools.linuxdeploy);
    deploy
        .current_dir(out)
        .arg("--appdir")
        .arg(appdir)
        .arg("--executable")
        .arg(appdir.join("usr/bin/rc2fx"))
        .arg("--desktop-file")
        .arg(appdir.join("usr/share/applications/rc2fx.desktop"))
        .arg("--icon-file")
        .arg(appdir.join("usr/share/icons/hicolor/scalable/apps/rc2fx.svg"))
        .env("ARCH", arch)
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .env("NO_STRIP", "1")
        .env("PATCHELF", &tools.patchelf)
        .env("PATH", tool_path(tool_dir)?);
    run(&mut deploy)?;
    verify_library_notices(&appdir.join("usr/lib"))?;
    ensure_regular_apprun(appdir)?;
    ensure_root_entries(appdir)?;

    let mut package = Command::new(&tools.appimagetool);
    package
        .current_dir(out)
        .arg("--no-appstream")
        .arg("--runtime-file")
        .arg(&tools.runtime)
        .arg(appdir)
        .arg(&appimage)
        .env("ARCH", arch)
        .env("APPIMAGE_EXTRACT_AND_RUN", "1")
        .env("PATH", tool_path(tool_dir)?);
    run(&mut package)?;
    if !appimage.is_file() || appimage.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(format!(
            "appimagetool did not create a non-empty {}",
            appimage.display()
        ));
    }
    Ok(appimage)
}

fn tool_path(tool_dir: &Path) -> Result<std::ffi::OsString> {
    let path = env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![tool_dir.to_path_buf()];
    entries.extend(env::split_paths(&path));
    env::join_paths(entries).map_err(|e| format!("building tool PATH: {e}"))
}

/// Write the checksum of the single Linux artifact beside it.
pub fn write_checksums(out: &Path, appimage: &Path) -> Result<PathBuf> {
    let name = appimage
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid AppImage path {}", appimage.display()))?;
    let data = fs::read(appimage).map_err(|e| format!("{}: {e}", appimage.display()))?;
    let sums = out.join("SHA256SUMS");
    fs::write(&sums, format!("{}  {name}\n", sha256(&data)))
        .map_err(|e| format!("{}: {e}", sums.display()))?;
    Ok(sums)
}

/// Stage, deploy, package and checksum one Linux AppImage. The temporary
/// AppDir is removed after success so `target/package` contains only the
/// AppImage and SHA256SUMS for this target.
pub fn package(
    out: &Path,
    name: &str,
    executable: &[u8],
    licenses: Vec<(String, Vec<u8>)>,
    arch: &str,
    tools: &Tools,
) -> Result {
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    // Old tar.gz/folder outputs must not survive a move to single-file Linux
    // releases.
    let _ = fs::remove_file(out.join(format!("{name}.tar.gz")));
    let _ = fs::remove_dir_all(out.join(name));
    let _ = fs::remove_dir_all(out.join(format!(".{name}.AppDir")));
    let _ = fs::remove_file(out.join("SHA256SUMS"));
    // Keep the working AppDir outside target/package: only the finished
    // AppImage and its checksum live in the published output directory.
    let work = out
        .parent()
        .unwrap_or(out)
        .join(format!("{}-appimage-work", name));
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let appdir = work.join("AppDir");
    let result = (|| {
        stage(&appdir, executable, &licenses)?;
        let appimage = build(&appdir, out, name, arch, tools)?;
        if let Ok(expected) = rc2fx_installer::Envelope::find(executable) {
            // linuxdeploy may adjust ELF search paths. The embedded installer
            // payload must still be the one validated before staging.
            let deployed = fs::read(appdir.join("usr/bin/rc2fx"))
                .map_err(|e| format!("deployed executable: {e}"))?;
            rc2fx_installer::Envelope::find(&deployed)?.require_matches(&expected)?;
        }
        let sums = write_checksums(out, &appimage)?;
        println!(
            "Package ready:\n  {}\n  sha256 {}\n  checksums {}",
            appimage.display(),
            sha256(&fs::read(&appimage).map_err(|e| e.to_string())?),
            sums.display()
        );
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[cfg(target_os = "linux")]
    use std::{collections::BTreeMap, io::Read};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = env::temp_dir().join(format!(
            "rc2fx-appimage-{label}-{}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(target_os = "linux")]
    fn fake_tools(dir: &Path) -> Tools {
        let linuxdeploy = dir.join("linuxdeploy");
        let appimagetool = dir.join("appimagetool");
        fs::write(
            &linuxdeploy,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"$0.args\"\nprintf '%s\\n' \"${NO_STRIP:-}\" > \"$0.env\"\nappdir=\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --appdir ]; then appdir=$2; fi\n  shift\ndone\nmkdir -p \"$appdir/usr/lib\"\nprintf host > \"$appdir/usr/lib/libXau.so.6\"\n",
        )
        .unwrap();
        fs::write(
            &appimagetool,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" > \"$0.args\"\nappdir=\nout=\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --runtime-file) shift ;;\n    --no-appstream) ;;\n    *) if [ -z \"$appdir\" ]; then appdir=$1; else out=$1; fi ;;\n  esac\n  shift\ndone\ntar -cf \"$out\" -C \"$appdir\" .\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for tool in [&linuxdeploy, &appimagetool] {
                fs::set_permissions(tool, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let runtime = dir.join("runtime-x86_64");
        fs::write(&runtime, b"pinned type2 runtime").unwrap();
        let patchelf = dir.join("patchelf");
        fs::write(&patchelf, b"pinned ELF patcher").unwrap();
        Tools {
            linuxdeploy,
            appimagetool,
            runtime,
            patchelf,
        }
    }

    #[test]
    fn unexpected_bundled_libraries_need_reviewed_notices() {
        assert!(super::covered_library("libxkbcommon-x11.so.0"));
        assert!(super::covered_library("libxcb-xkb.so.1.0.0"));
        assert!(super::covered_library("libXdmcp.so.6"));
        assert!(super::covered_library("libbsd.so.0"));
        assert!(super::covered_library("libmd.so.0"));
        assert!(!super::covered_library("libnew.so.1"));
        assert!(!super::covered_library("libXau.something"));
        assert!(!super::covered_library("libbsd-ctor.so.0"));
    }

    #[test]
    fn appdir_has_only_the_runtime_ui_and_licences() {
        let root = temp_dir("layout");
        let appdir = root.join("AppDir");
        let exe = b"ELF executable";
        stage(
            &appdir,
            exe,
            &[("rust-crates.html".into(), b"<html>".to_vec())],
        )
        .unwrap();
        assert_eq!(fs::read(appdir.join("usr/bin/rc2fx")).unwrap(), exe);
        assert!(
            appdir
                .join("usr/share/applications/rc2fx.desktop")
                .is_file()
        );
        assert!(
            appdir
                .join("usr/share/icons/hicolor/scalable/apps/rc2fx.svg")
                .is_file()
        );
        assert_eq!(
            fs::read(appdir.join("usr/share/licenses/rc2fx/rust-crates.html")).unwrap(),
            b"<html>"
        );
        let apprun = fs::read_to_string(appdir.join("AppRun")).unwrap();
        assert!(apprun.contains("$APPDIR/usr/bin/rc2fx"));
        assert!(apprun.contains("LD_LIBRARY_PATH"));
        assert!(!apprun.contains("APPIMAGE="));
        let metadata = fs::symlink_metadata(appdir.join("AppRun")).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert!(metadata.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(metadata.permissions().mode() & 0o111, 0);
        }
        let mut paths = Vec::new();
        fn walk(path: &Path, paths: &mut Vec<String>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let rel = entry
                    .path()
                    .strip_prefix(path)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                paths.push(rel);
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), paths);
                }
            }
        }
        walk(&appdir, &mut paths);
        for forbidden in [
            "docs",
            "crates",
            "experiments",
            "config",
            "runtime",
            "xtask",
            "target",
            "README",
            "rc2fxd",
            "librc2fx_native",
        ] {
            assert!(
                paths.iter().all(|path| !path.starts_with(forbidden)),
                "{forbidden} leaked into {paths:?}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn apprun_uses_its_own_location_and_preserves_appimage() {
        let root = temp_dir("apprun");
        let appdir = root.join("AppDir");
        let script = br#"#!/bin/sh
printf '%s\n' "$APPDIR" "$APPIMAGE" "$LD_LIBRARY_PATH" "$1" "$2"
"#;
        stage(&appdir, script, &[]).unwrap();
        let output = Command::new(appdir.join("AppRun"))
            .arg("one")
            .arg("two words")
            .env("APPDIR", "/tmp/inherited-appdir")
            .env("APPIMAGE", "/tmp/rc2fx.AppImage")
            .env("LD_LIBRARY_PATH", "/usr/local/lib")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let expected = format!(
            "{}\n/tmp/rc2fx.AppImage\n{}/usr/lib:/usr/local/lib\none\ntwo words\n",
            appdir.display(),
            appdir.display()
        );
        assert_eq!(text, expected);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn build_preserves_payload_and_writes_single_artifact_checksum() {
        let root = temp_dir("build");
        let out = root.join("out");
        let exe = b"registry-pinned executable bytes";
        let tools = fake_tools(&root);
        let name = "rc2fx-0.1.0-linux-x64";
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join(format!("{name}.tar.gz")), b"old").unwrap();
        fs::create_dir_all(out.join(name)).unwrap();
        package(
            &out,
            name,
            exe,
            vec![("rust-crates.html".into(), b"licence".to_vec())],
            "x64",
            &tools,
        )
        .unwrap();
        let appimage = out.join(format!("{name}.AppImage"));
        assert!(appimage.is_file());
        let file = fs::File::open(&appimage).unwrap();
        let mut archive = tar::Archive::new(file);
        let mut seen = BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            seen.insert(path, data);
        }
        assert_eq!(seen["./usr/bin/rc2fx"], exe);
        assert_eq!(seen["./usr/lib/libXau.so.6"], b"host");
        assert_eq!(seen["./AppRun"], APPRUN.as_bytes());
        assert!(seen.contains_key("./rc2fx.desktop"));
        assert!(seen.contains_key("./rc2fx.svg"));
        for forbidden in [
            "docs/",
            "crates/",
            "experiments/",
            "config/",
            "runtime/",
            "xtask/",
            "rc2fxd",
            "librc2fx_native",
            ".tar.gz",
            ".zip",
        ] {
            assert!(
                seen.keys().all(|path| !path.contains(forbidden)),
                "{forbidden} leaked into AppImage payload"
            );
        }
        let deploy_args = fs::read_to_string(root.join("linuxdeploy.args")).unwrap();
        for arg in ["--appdir", "--executable", "--desktop-file", "--icon-file"] {
            assert!(
                deploy_args.contains(arg),
                "{arg} missing from {deploy_args:?}"
            );
        }
        assert!(
            !deploy_args.contains("--custom-apprun"),
            "passing the staged AppRun as its own copy source makes linuxdeploy delete it"
        );
        assert!(
            !deploy_args.contains("--output"),
            "linuxdeploy must only deploy, not package: {deploy_args:?}"
        );
        assert_eq!(
            fs::read_to_string(root.join("linuxdeploy.env")).unwrap(),
            "1\n"
        );
        let package_args = fs::read_to_string(root.join("appimagetool.args")).unwrap();
        assert!(package_args.contains("--no-appstream"), "{package_args:?}");
        assert!(package_args.contains("--runtime-file"), "{package_args:?}");
        assert!(package_args.contains("runtime-x86_64"), "{package_args:?}");
        assert!(
            package_args.contains(&format!("{name}.AppImage")),
            "{package_args:?}"
        );
        let sums = out.join("SHA256SUMS");
        assert!(sums.is_file());
        let appimage_name = format!("{name}.AppImage");
        let mut artifacts: Vec<_> = fs::read_dir(&out)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        artifacts.sort();
        assert_eq!(artifacts, ["SHA256SUMS", appimage_name.as_str()]);
        assert!(
            !out.parent()
                .unwrap()
                .join(format!("{name}-appimage-work"))
                .exists()
        );
        assert!(!out.join(format!("{name}.tar.gz")).exists());
        assert_eq!(
            fs::read_to_string(sums).unwrap(),
            format!(
                "{}  {}\n",
                sha256(&fs::read(&appimage).unwrap()),
                appimage.file_name().unwrap().to_string_lossy()
            )
        );
        fs::remove_dir_all(root).unwrap();
    }
}
