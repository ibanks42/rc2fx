//! `cargo xtask package`: the self-contained desktop executable and legal
//! notices. Release executables download the mixer payload; packaging checks
//! the executable's payload mode, including for --skip-build. Every platform
//! is packaged by Velopack (see `velopack`): an AppImage on Linux, an
//! installer and a portable zip on Windows and macOS, and the update package
//! and feed the app updates itself from.

use crate::{Result, root, sha256};

mod appimage;
mod licenses;
mod macos;
mod velopack;
use std::{collections::BTreeMap, fs, path::Path, process::Command};

#[cfg(test)]
const SCRIPTS: [&str; 4] = [
    "rc2fx.sh",
    "rc2fx-shim.sh",
    "rc2fx-gui-shim.sh",
    "install.sh",
];

const TOP: &str = "rc2fx";

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub data: Vec<u8>,
    pub mode: u32,
}

pub type Files = BTreeMap<String, Member>;

fn file(data: Vec<u8>) -> Member {
    Member { data, mode: 0o644 }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Run a packaging tool; on failure, report what it printed.
fn run<S: AsRef<std::ffi::OsStr>>(program: &str, args: &[S]) -> Result {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed:\n{}{}",
            args.first()
                .map(|arg| arg.as_ref().to_string_lossy())
                .unwrap_or_default(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(())
}

/// A packaging setting from the environment; empty counts as unset.
fn setting(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Operating system and architecture names for a target triple.
fn platform(target: &str) -> Result<(&'static str, &'static str)> {
    let os = if target.contains("windows") {
        "windows"
    } else if target.contains("apple") {
        "macos"
    } else if target.contains("linux") {
        "linux"
    } else {
        return Err(format!("unsupported package target {target}"));
    };
    let arch = if target.starts_with("x86_64") {
        "x64"
    } else if target.starts_with("aarch64") {
        "arm64"
    } else {
        return Err(format!("unsupported package architecture {target}"));
    };
    Ok((os, arch))
}

fn host_target() -> Result<String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("could not run rustc: {e}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc did not report its host".into())
}

/// Official release builds embed the quiet-diagnostics launcher policy.
fn release_build() -> bool {
    std::env::var_os("RC2FX_RELEASE_BUILD").is_some_and(|value| value == "1")
}

fn validate_executable(root: &Path, executable: &[u8], release: bool) -> Result {
    let download = b"RC2FX-PAYLOAD-MODE:download:1";
    if executable.windows(download.len()).any(|w| w == download) {
        if rc2fx_installer::Envelope::find(executable).is_ok() {
            return Err("download-only executable unexpectedly contains a mixer payload".into());
        }
        return Ok(());
    }
    let embedded = rc2fx_installer::Envelope::find(executable)?;
    let expected = rc2fx_installer::Envelope::expected(root, release)?;
    embedded.require_matches(&expected)
}

fn collect_with(
    root: &Path,
    os: &str,
    executable: Vec<u8>,
    licenses: Vec<(String, Vec<u8>)>,
    release: bool,
) -> Result<Files> {
    let mut files = Files::new();
    validate_executable(root, &executable, release)?;
    for (name, data) in licenses {
        files.insert(format!("licenses/{name}"), file(data));
    }
    let name = if os == "windows" {
        "rc2fx.exe"
    } else {
        "rc2fx"
    };
    files.insert(
        name.to_owned(),
        Member {
            data: executable,
            mode: 0o755,
        },
    );
    Ok(files)
}

/// Write the executable and legal notices as an unpacked package.
fn write_folder(dir: &Path, files: &Files) -> Result {
    let _ = fs::remove_dir_all(dir);
    for (name, m) in files {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().expect("member has a parent"))
            .map_err(|e| e.to_string())?;
        fs::write(&path, &m.data).map_err(|e| format!("{}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(m.mode))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// SHA256SUMS for every published Linux file, beside them.
fn write_checksums(out: &Path, published: &[std::path::PathBuf]) -> Result {
    let mut sums = String::new();
    for path in published {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("invalid package path {}", path.display()))?;
        sums.push_str(&format!("{}  {name}\n", sha256(&read(path)?)));
    }
    let path = out.join("SHA256SUMS");
    fs::write(&path, sums).map_err(|e| format!("{}: {e}", path.display()))
}

/// Generate consolidated notices for a standalone mixer release.
pub fn write_licenses(destination: &Path) -> Result {
    let document = licenses::generate(&root(), vec![])?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(destination, document.html).map_err(|e| e.to_string())
}

fn usage() -> String {
    "usage: cargo xtask package [--target TRIPLE] [--build-only | --skip-build] [--embedded]\n\
     --build-only stops after the build; --skip-build then packages that build.\n\
     Linux requires RC2FX_LINUXDEPLOY and RC2FX_PATCHELF to name the pinned tools,\n\
     vpk (RC2FX_VPK) and mksquashfs."
        .into()
}

pub fn run_package(args: &[&str]) -> Result {
    let mut target = None;
    let mut build = true;
    let mut package = true;
    let mut embedded = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--target" => target = Some(rest.next().ok_or_else(usage)?.to_string()),
            "--skip-build" => build = false,
            "--build-only" => package = false,
            "--embedded" => embedded = true,
            _ => return Err(usage()),
        }
    }
    let root = root();
    let host = host_target()?;
    let triple = target.clone().unwrap_or_else(|| host.clone());
    let (os, arch) = platform(&triple)?;
    let extra = if os == "linux" {
        appimage::license_sections(&root)?
    } else {
        vec![]
    };
    let document = licenses::generate(&root, extra)?;
    let document_path = root.join("target/package-licenses.json");
    fs::create_dir_all(root.join("target")).map_err(|e| e.to_string())?;
    fs::write(&document_path, &document.json).map_err(|e| e.to_string())?;
    if !build && !package {
        return Err(usage());
    }
    if build {
        licenses::build_app(&root, target.as_deref(), &document_path, embedded)?;
    }
    if !package {
        return Ok(());
    }
    let mut release = root.join("target");
    if let Some(t) = &target {
        release.push(t);
    }
    release.push("release");
    let exe = release.join(if os == "windows" {
        "rc2fx.exe"
    } else {
        "rc2fx"
    });
    let executable = read(&exe).map_err(|e| format!("{e}\nBuild the app or drop --skip-build."))?;
    let mode = if embedded {
        b"RC2FX-PAYLOAD-MODE:embedded:1"
    } else {
        b"RC2FX-PAYLOAD-MODE:download:1"
    };
    if !executable.windows(mode.len()).any(|w| w == mode) {
        return Err("executable payload mode differs from packaging options; rebuild with the requested mode".into());
    }

    licenses::verify_embedded(&executable, &document.json)?;
    let release = release_build();
    validate_executable(&root, &executable, release)?;
    let out = root.join("target").join("package");
    let version = env!("CARGO_PKG_VERSION");
    // No version in published names: releases/latest/download/NAME then
    // always links the newest release.
    let name = format!("rc2fx-{os}-{arch}");
    if os == "linux" {
        let tools = appimage::Tools::from_env()?;
        let licenses = vec![("LICENSES.html".into(), document.html)];
        let mut published = appimage::package(
            &root,
            &out,
            &name,
            version,
            &executable,
            licenses,
            arch,
            &tools,
        )?;
        // prepare-appimage-notices.py wrote the runtime's corresponding source.
        published.push(out.join(format!("rc2fx-{version}-appimage-runtime-sources.tar.gz")));
        write_checksums(&out, &published)?;
        return Ok(());
    }
    // These apps carry the notices in their Settings dialog.
    let files = collect_with(&root, os, executable, vec![], release)?;
    let folder = out.join(&name).join(TOP);
    let pack_dir = if os == "macos" {
        write_folder(&folder, &macos::bundle(files, version)?)?;
        folder.join(macos::APP)
    } else {
        write_folder(&folder, &files)?;
        folder.clone()
    };
    // collect_with() checked the executable's payload mode; the package
    // deliberately has no payload sidecars.
    let packages = velopack::pack(&root, os, arch, version, &name, &pack_dir, &out)?;
    println!("Packages ready:");
    for package in &packages {
        println!(
            "  {}\n  sha256 {}",
            package.display(),
            sha256(&read(package)?)
        );
    }
    println!("  unpacked app in {}", pack_dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static FIXTURE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> (PathBuf, Vec<u8>) {
        let root = loop {
            let counter = FIXTURE_COUNTER.fetch_add(1, Ordering::SeqCst);
            let root = std::env::temp_dir().join(format!(
                "rc2fx-package-{}-{}-{counter}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            match fs::create_dir(&root) {
                Ok(()) => break root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating fixture root {}: {error}", root.display()),
            }
        };
        let put = |name: &str, data: &[u8]| {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, data).unwrap();
        };
        let catalog = br#"{"schemaVersion":1,"profiles":[]}"#;
        let mut elf = vec![0u8; 64];
        elf[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
        elf[18] = 0xb7;
        let mut sums = String::new();
        for name in [
            "librc2fx_native.so",
            "rc2fxd",
            "rc2fx-supervisor",
            "rc2fx-firmware",
        ] {
            put(&format!("target/mixer/{name}"), &elf);
            sums += &format!("{}  {name}\n", sha256(&elf));
        }
        put("target/mixer/firmware-profiles.json", catalog);
        sums += &format!("{}  firmware-profiles.json\n", sha256(catalog));
        put("target/mixer/librc2fx_gui.so", &elf);
        sums += &format!("{}  librc2fx_gui.so\n", sha256(&elf));
        put("target/mixer/librc2fx_jackerr.so", &elf);
        sums += &format!("{}  librc2fx_jackerr.so\n", sha256(&elf));
        put("target/mixer/rc2fx_jackerr_snapshot", &elf);
        sums += &format!("{}  rc2fx_jackerr_snapshot\n", sha256(&elf));
        let release = br#"{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}"#;
        put("target/mixer/mixer-release.json", release);
        sums += &format!("{}  mixer-release.json\n", sha256(release));
        put("target/mixer/SHA256SUMS", sums.as_bytes());
        for name in SCRIPTS {
            let script: &[u8] = if name == "rc2fx.sh" {
                b"#!/bin/bash\r\nrc2fx_build=development\r\nexit 0\r\n"
            } else {
                b"#!/bin/sh\r\nexit 0\r\n"
            };
            put(&format!("xtask/mixer/{name}"), script);
        }
        put("config/firmware-profiles.json", catalog);
        put("config/mixer-release.json", release);
        let exe = synthetic_executable(&root, false);
        (root, exe)
    }

    // A synthetic executable container exercises packaging, not GUI execution.
    // The installer also tests a real standalone non-GUI executable separately.
    fn synthetic_executable(root: &Path, release: bool) -> Vec<u8> {
        let envelope = rc2fx_installer::Envelope::expected(root, release).unwrap();
        let mut exe = b"executable prefix\0".to_vec();
        exe.extend_from_slice(envelope.as_bytes());
        exe.extend_from_slice(b"\0executable suffix");
        exe
    }

    #[test]
    fn download_only_package_needs_no_runtime_and_rejects_a_bundled_payload() {
        let dir = tempfile::tempdir().unwrap();
        let exe = b"executable\0RC2FX-PAYLOAD-MODE:download:1\0".to_vec();
        let files = collect_with(dir.path(), "windows", exe.clone(), vec![], true).unwrap();
        assert_eq!(files["rc2fx.exe"].data, exe);
        let (root, mut embedded) = fixture();
        embedded.extend_from_slice(&exe);
        assert!(
            validate_executable(&root, &embedded, true)
                .unwrap_err()
                .contains("unexpectedly")
        );
    }

    #[test]
    fn release_and_development_executables_keep_their_embedded_policy() {
        for release in [false, true] {
            let (root, _) = fixture();
            let exe = synthetic_executable(&root, release);
            let files = collect_with(&root, "windows", exe.clone(), vec![], release).unwrap();
            assert_eq!(
                files["rc2fx.exe"].data, exe,
                "packaging must not rewrite the executable"
            );
            let envelope = rc2fx_installer::Envelope::find(&files["rc2fx.exe"].data).unwrap();
            let bundle = envelope.bundle().unwrap();
            let script = String::from_utf8(bundle["rc2fx.sh"].clone()).unwrap();
            let kind = if release { "release" } else { "development" };
            assert!(script.contains(&format!("rc2fx_build={kind}\n")));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn packaging_refuses_old_corrupt_stale_and_wrong_policy_executables() {
        let (root, exe) = fixture();
        let old = collect_with(&root, "windows", b"old app".to_vec(), vec![], false).unwrap_err();
        assert!(old.contains("embed"), "{old}");

        let mut corrupt = exe.clone();
        let marker = b"rc2fx_build=development";
        let offset = corrupt
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap();
        corrupt[offset] ^= 1;
        assert!(collect_with(&root, "windows", corrupt, vec![], false).is_err());

        let wrong_policy = collect_with(&root, "windows", exe.clone(), vec![], true).unwrap_err();
        assert!(wrong_policy.contains("rc2fx.sh"), "{wrong_policy}");
        let script_path = root.join("xtask/mixer/rc2fx.sh");
        let mut script = fs::read(&script_path).unwrap();
        script.extend_from_slice(b"\n# changed after desktop compilation\n");
        fs::write(script_path, script).unwrap();
        let stale = collect_with(&root, "windows", exe, vec![], false).unwrap_err();
        assert!(stale.contains("rc2fx.sh"), "{stale}");
        fs::remove_dir_all(root).unwrap();
    }

    fn package(os: &str) -> (PathBuf, Files) {
        let (root, exe) = fixture();
        let files = collect_with(
            &root,
            os,
            exe,
            vec![
                ("rust-crates.html".into(), b"<html>".to_vec()),
                ("LICENSE-MIT".into(), b"MIT terms".to_vec()),
            ],
            false,
        )
        .unwrap();
        (root, files)
    }

    #[test]
    fn standalone_package_does_not_ship_installer_sidecars() {
        let (root, files) = package("windows");
        let sidecars: Vec<_> = files
            .keys()
            .filter(|name| {
                name.starts_with("runtime/")
                    || name.starts_with("xtask/")
                    || name.as_str() == "config/firmware-profiles.json"
            })
            .collect();
        assert!(
            sidecars.is_empty(),
            "installer inputs must be embedded, not sidecars: {sidecars:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_has_self_contained_executable_and_licences() {
        let (root, files) = package("windows");
        assert_eq!(files["rc2fx.exe"].mode, 0o755);
        assert!(files.contains_key("licenses/rust-crates.html"));
        assert_eq!(files["licenses/LICENSE-MIT"].data, b"MIT terms");
        for forbidden in [
            "config/",
            "crates/",
            "docs/",
            "experiments/",
            "runtime/",
            "xtask/",
            "README",
        ] {
            assert!(
                files.keys().all(|name| !name.starts_with(forbidden)),
                "{forbidden} leaked into archive: {:?}",
                files.keys().collect::<Vec<_>>()
            );
        }
        let envelope = rc2fx_installer::Envelope::find(&files["rc2fx.exe"].data).unwrap();
        let bundle = envelope.bundle().unwrap();
        for name in rc2fx_installer::PAYLOAD_NAMES {
            assert!(bundle.contains_key(name), "{name}");
        }
        let folder = root.join("out").join(TOP);
        write_folder(&folder, &files).unwrap();
        let copied_executable = fs::read(folder.join("rc2fx.exe")).unwrap();
        rc2fx_installer::Envelope::find(&copied_executable)
            .unwrap()
            .require_matches(&envelope)
            .unwrap();
        // No sidecars are needed to inspect/validate the copied executable.
        assert!(!folder.join("runtime").exists());
        assert!(!folder.join("xtask").exists());
        assert!(!folder.join("config/firmware-profiles.json").exists());
        // A missing source payload is refused, not silently packaged stale.
        fs::remove_file(root.join("target/mixer/rc2fxd")).unwrap();
        assert!(collect_with(&root, "windows", copied_executable, vec![], false).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn platform_names() {
        assert_eq!(
            platform("x86_64-pc-windows-msvc").unwrap(),
            ("windows", "x64")
        );
        assert_eq!(
            platform("aarch64-apple-darwin").unwrap(),
            ("macos", "arm64")
        );
        assert_eq!(
            platform("x86_64-unknown-linux-gnu").unwrap(),
            ("linux", "x64")
        );
        assert!(platform("riscv64gc-unknown-linux-gnu").is_err());
    }

    /// The relative targets of a Markdown file's links and VitePress includes,
    /// outside code. URLs and same-page anchors are not files.
    fn local_targets(text: &str) -> Vec<String> {
        let mut targets = Vec::new();
        let mut fenced = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            // Drop inline code spans: every other backtick-separated piece.
            let prose: String = line.split('`').step_by(2).collect();
            let links = prose.match_indices("](").map(|(at, _)| {
                let rest = &prose[at + 2..];
                &rest[..rest.find([')', ' ']).unwrap_or(rest.len())]
            });
            let includes = prose.match_indices("<!--@include: ").map(|(at, _)| {
                let rest = &prose[at + 14..];
                rest[..rest.find("-->").unwrap_or(rest.len())].trim()
            });
            for target in links.chain(includes) {
                let target = target.split('#').next().unwrap_or_default();
                if !target.is_empty() && !target.contains(':') {
                    targets.push(target.to_owned());
                }
            }
        }
        targets
    }

    /// `target` as a repository path, relative to the file `from`.
    fn resolve(from: &str, target: &str) -> String {
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
        for part in target.split('/') {
            match part {
                ".." => {
                    assert!(parts.pop().is_some(), "{from}: {target} leaves the package");
                }
                "." | "" => {}
                part => parts.push(part),
            }
        }
        parts.join("/")
    }

    #[test]
    fn link_targets_skip_code_urls_and_anchors() {
        let text = "See [a](../a.md#x), [b](https://b), [c](#c) and `[d](d.md)`.\n\
                    ```\n[e](e.md)\n```\n<!--@include: ../../crates/f/README.md-->\n";
        assert_eq!(local_targets(text), ["../a.md", "../../crates/f/README.md"]);
        assert_eq!(resolve("docs/user/x.md", "../a.md"), "docs/a.md");
    }
}
