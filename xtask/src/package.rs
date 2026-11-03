//! `cargo xtask package`: the self-contained desktop executable, documentation,
//! licences and a SHA256SUMS manifest. Installer inputs are embedded in the
//! executable; packaging verifies those bytes, including for --skip-build.

use crate::{Result, root, run, sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

/// Files copied from the repository unchanged, at their repository paths. The
/// installer and user pages come first; the rest is what their relative links
/// (and the crate chapters' `{{#include}}`s) reach, so every link inside the
/// package resolves (`packaged_documentation_links_resolve`).
const DOCUMENTS: [&str; 47] = [
    "docs/user/getting-started.md",
    "docs/user/how-it-works.md",
    "docs/user/installing.md",
    "docs/user/using-the-app.md",
    "docs/user/effects.md",
    "docs/user/noise-suppression.md",
    "docs/user/settings-and-diagnostics.md",
    "docs/user/troubleshooting.md",
    "docs/developer/architecture.md",
    "docs/developer/building.md",
    "docs/developer/commands.md",
    "docs/developer/linux.md",
    "docs/developer/mixer-workflow.md",
    "docs/developer/releases.md",
    "docs/reference/api.md",
    "docs/reference/duo.md",
    "docs/reference/duo-routing.md",
    "docs/reference/eq-format.md",
    "docs/reference/firmware.md",
    "docs/reference/source-name-evidence.md",
    "docs/reference/suppression.md",
    "docs/reference/usb-paths.md",
    "docs/firmware-audits/usb-paths.json",
    "docs/firmware-audits/rodecaster-pro-ii-ui-1.7.7.json",
    "docs/reference/hardware.md",
    "docs/reference/installer.md",
    "docs/reference/usb-control.md",
    "docs/crates/rc2fx-native.md",
    "docs/crates/rc2fx-dfn.md",
    "crates/rc2fx-dfn/model/README.md",
    "crates/rc2fx-dfn/model/la1/README.md",
    "crates/rc2fx-dfn/model/PROVENANCE.md",
    "docs/crates/rc2fx-native-control.md",
    "docs/crates/rc2fx-suppression.md",
    "docs/firmware-audits/rodecaster-pro-ii-1.7.7.json",
    "docs/firmware-audits/rodecaster-duo-1.7.7.json",
    "docs/firmware-audits/rodecaster-duo-routes.json",
    "crates/rc2fx-native/README.md",
    "crates/rc2fx-native/jackerr/README.md",
    "crates/rc2fx-native-control/README.md",
    "crates/rc2fx-suppression/README.md",
    "crates/rc2fx-dfn/LICENSE-DEEPFILTERNET",
    "experiments/device-panel/README.md",
    "experiments/device-panel/ui-1.7.7-map.md",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "THIRD_PARTY_NOTICES.md",
];

/// Files only the Linux package needs: the USB permission rule.
const LINUX_FILES: [&str; 1] = ["config/udev/70-rc2fx.rules"];

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

fn readme(os: &str, arch: &str) -> String {
    let start = match os {
        "windows" => "Run rc2fx.exe.",
        "macos" => {
            "Run ./rc2fx from Terminal. This build is unsigned, so macOS may ask you to\napprove it in System Settings > Privacy & Security."
        }
        _ => {
            "First let the app reach the mixer over USB, once:\n\n  \
             sudo cp config/udev/70-rc2fx.rules /etc/udev/rules.d/\n  \
             sudo udevadm control --reload-rules\n  \
             sudo udevadm trigger --action=add --subsystem-match=hidraw\n\n\
             Then run ./rc2fx. The installer asks for administrator permission once through\npkexec (polkit), because raw USB, mounting and media ejection need it; the\napp itself runs as you."
        }
    };
    format!(
        "RC2FX - {os} / {arch}\n\n\
         Extract the whole archive, then {start}\n\n\
         The executable embeds its mixer runtime, catalog and installer scripts;\n\
         no runtime/config/xtask folders are required beside it for flashing.\n\
         Official RODE firmware is downloaded when needed.\n\n\
         To install or update RC2FX on a RODECaster Pro II, choose Open installer.\n\
         It saves any RC2FX settings, builds official RODE firmware with RC2FX and\n\
         those settings in it, and flashes it after one confirmation. Keep mixer\n\
         power and USB 1 connected until RC2FX reports that it is running.\n\n\
         Progress, downloads and backups are kept in RC2FX/installer in your\n\
         user data folder (%LOCALAPPDATA% on Windows, ~/.local/share on Linux).\n\
         See docs/user/installing.md and docs/user/troubleshooting.md for help,\n\
         docs/reference/installer.md for what the installer does and how far it\n\
         has been tested, and\n\
         THIRD_PARTY_NOTICES.md and licenses/ for third-party licences.\n"
    )
}

/// Official release builds embed the quiet-diagnostics launcher policy.
fn release_build() -> bool {
    std::env::var_os("RC2FX_RELEASE_BUILD").is_some_and(|value| value == "1")
}

/// Everything in the package except SHA256SUMS. `licenses` maps file names
/// under `licenses/` to their contents.
pub fn collect(
    root: &Path,
    os: &str,
    arch: &str,
    executable: Vec<u8>,
    licenses: Vec<(String, Vec<u8>)>,
) -> Result<Files> {
    collect_with(root, os, arch, executable, licenses, release_build())
}

fn collect_with(
    root: &Path,
    os: &str,
    arch: &str,
    executable: Vec<u8>,
    licenses: Vec<(String, Vec<u8>)>,
    release: bool,
) -> Result<Files> {
    let mut files = Files::new();
    let embedded = rc2fx_installer::Envelope::find(&executable)?;
    let expected = rc2fx_installer::Envelope::expected(root, release)?;
    embedded.require_matches(&expected)?;
    for name in DOCUMENTS {
        files.insert(name.to_owned(), file(read(&root.join(name))?));
    }
    if os == "linux" {
        for name in LINUX_FILES {
            files.insert(name.to_owned(), file(read(&root.join(name))?));
        }
    }
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
    files.insert("README.txt".into(), file(readme(os, arch).into_bytes()));
    Ok(files)
}

pub fn manifest(files: &Files) -> String {
    files
        .iter()
        .filter(|(name, _)| *name != "SHA256SUMS")
        .map(|(name, m)| format!("{}  {name}\n", sha256(&m.data)))
        .collect()
}

/// Write the executable and accompanying documentation as an unpacked package.
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

pub fn write_tar(out: impl Write, files: &Files) -> Result<()> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        out,
        flate2::Compression::best(),
    ));
    for (name, m) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(m.data.len() as u64);
        header.set_mode(m.mode);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, format!("{TOP}/{name}"), m.data.as_slice())
            .map_err(|e| e.to_string())?;
    }
    builder
        .into_inner()
        .and_then(|gz| gz.finish())
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn write_zip(out: impl Write + std::io::Seek, files: &Files) -> Result<()> {
    let mut zip = zip::ZipWriter::new(out);
    for (name, m) in files {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            // Record a Unix host on every OS so unzip and macOS keep the
            // app's executable bit.
            .system(zip::System::Unix)
            .unix_permissions(m.mode);
        zip.start_file(format!("{TOP}/{name}"), options)
            .map_err(|e| e.to_string())?;
        zip.write_all(&m.data).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Third-party licence reports from cargo-about: the desktop workspace
/// (desktop app, rc2fxd and the profile checker) and the separate
/// mixer-library workspace.
fn licence_reports(root: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut reports = Vec::new();
    for (name, manifest) in [
        ("rust-crates.html", "Cargo.toml"),
        (
            "rust-crates-mixer-library.html",
            "crates/rc2fx-native/Cargo.toml",
        ),
    ] {
        let output = Command::new("cargo")
            .current_dir(root)
            .args([
                "about",
                "generate",
                "--manifest-path",
                manifest,
                "about.hbs",
            ])
            .output()
            .map_err(|e| format!("could not run cargo-about: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo about failed for {manifest}. Install it with `cargo install --locked cargo-about`.\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        reports.push((name.to_owned(), output.stdout));
    }
    Ok(reports)
}

fn usage() -> String {
    "usage: cargo xtask package [--target TRIPLE] [--skip-build]".into()
}

pub fn run_package(args: &[&str]) -> Result {
    let mut target = None;
    let mut build = true;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match *arg {
            "--target" => target = Some(rest.next().ok_or_else(usage)?.to_string()),
            "--skip-build" => build = false,
            _ => return Err(usage()),
        }
    }
    let root = root();
    let host = host_target()?;
    let triple = target.clone().unwrap_or_else(|| host.clone());
    let (os, arch) = platform(&triple)?;
    if build {
        let mut cargo = Command::new("cargo");
        cargo
            .current_dir(&root)
            .args(["build", "--locked", "--release", "-p", "rc2fx-app"]);
        if let Some(t) = &target {
            cargo.args(["--target", t]);
        }
        run(&mut cargo)?;
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

    let mut files = collect(&root, os, arch, executable, licence_reports(&root)?)?;
    let sums = manifest(&files);
    files.insert("SHA256SUMS".into(), file(sums.into_bytes()));

    let out = root.join("target").join("package");
    let name = format!("rc2fx-{os}-{arch}");
    let folder = out.join(&name).join(TOP);
    write_folder(&folder, &files)?;
    // collect() checked the actual executable's embedded payload against the
    // staged build inputs; the package deliberately has no payload sidecars.

    let archive = out.join(if os == "linux" {
        format!("{name}.tar.gz")
    } else {
        format!("{name}.zip")
    });
    let part = PathBuf::from(format!("{}.part", archive.display()));
    let result = (|| {
        let f = fs::File::create(&part).map_err(|e| e.to_string())?;
        if os == "linux" {
            write_tar(std::io::BufWriter::new(&f), &files)?;
        } else {
            write_zip(std::io::BufWriter::new(&f), &files)?;
        }
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&part, &archive).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result?;
    let bytes = read(&archive)?;
    println!(
        "Package ready:\n  {}\n  sha256 {}\n  {} files; unpacked copy in {}",
        archive.display(),
        sha256(&bytes),
        files.len(),
        folder.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
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
        for name in ["librc2fx_native.so", "rc2fxd", "rc2fx-firmware"] {
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
        put("target/mixer/SHA256SUMS", sums.as_bytes());
        for name in SCRIPTS {
            let script: &[u8] = if name == "rc2fx.sh" {
                b"#!/bin/bash\r\nrc2fx_build=development\r\nexit 0\r\n"
            } else {
                b"#!/bin/sh\r\nexit 0\r\n"
            };
            put(&format!("xtask/mixer/{name}"), script);
        }
        for name in DOCUMENTS.iter().chain(&LINUX_FILES) {
            put(name, name.as_bytes());
        }
        put("config/firmware-profiles.json", catalog);
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
    fn release_and_development_executables_keep_their_embedded_policy() {
        for release in [false, true] {
            let (root, _) = fixture();
            let exe = synthetic_executable(&root, release);
            let files = collect_with(&root, "linux", "x64", exe.clone(), vec![], release).unwrap();
            assert_eq!(
                files["rc2fx"].data, exe,
                "packaging must not rewrite the executable"
            );
            let envelope = rc2fx_installer::Envelope::find(&files["rc2fx"].data).unwrap();
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
        let old =
            collect_with(&root, "linux", "x64", b"old app".to_vec(), vec![], false).unwrap_err();
        assert!(old.contains("embed"), "{old}");

        let mut corrupt = exe.clone();
        let marker = b"rc2fx_build=development";
        let offset = corrupt
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap();
        corrupt[offset] ^= 1;
        assert!(collect_with(&root, "linux", "x64", corrupt, vec![], false).is_err());

        let wrong_policy =
            collect_with(&root, "linux", "x64", exe.clone(), vec![], true).unwrap_err();
        assert!(wrong_policy.contains("rc2fx.sh"), "{wrong_policy}");
        let script_path = root.join("xtask/mixer/rc2fx.sh");
        let mut script = fs::read(&script_path).unwrap();
        script.extend_from_slice(b"\n# changed after desktop compilation\n");
        fs::write(script_path, script).unwrap();
        let stale = collect_with(&root, "linux", "x64", exe, vec![], false).unwrap_err();
        assert!(stale.contains("rc2fx.sh"), "{stale}");
        fs::remove_dir_all(root).unwrap();
    }

    fn package(os: &str) -> (PathBuf, Files) {
        let (root, exe) = fixture();
        let mut files = collect_with(
            &root,
            os,
            "x64",
            exe,
            vec![("rust-crates.html".into(), b"<html>".to_vec())],
            false,
        )
        .unwrap();
        let sums = manifest(&files);
        files.insert("SHA256SUMS".into(), file(sums.into_bytes()));
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
    fn package_has_self_contained_executable_licences_and_complete_manifest() {
        let (root, files) = package("windows");
        assert_eq!(files["rc2fx.exe"].mode, 0o755);
        assert!(!files.contains_key("config/udev/70-rc2fx.rules"));
        assert!(files.contains_key("licenses/rust-crates.html"));
        let envelope = rc2fx_installer::Envelope::find(&files["rc2fx.exe"].data).unwrap();
        let bundle = envelope.bundle().unwrap();
        for name in rc2fx_installer::PAYLOAD_NAMES {
            assert!(bundle.contains_key(name), "{name}");
        }
        let sums = String::from_utf8(files["SHA256SUMS"].data.clone()).unwrap();
        assert_eq!(sums.lines().count(), files.len() - 1);
        for (name, m) in &files {
            if name != "SHA256SUMS" {
                assert!(
                    sums.contains(&format!("{}  {name}\n", sha256(&m.data))),
                    "{name}"
                );
            }
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
        assert!(collect_with(&root, "linux", "x64", copied_executable, vec![], false).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archives_round_trip_under_one_folder() {
        let (root, files) = package("linux");
        assert!(files.contains_key("config/udev/70-rc2fx.rules"));
        let mut tar_bytes = Vec::new();
        write_tar(&mut tar_bytes, &files).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_bytes.as_slice()));
        let mut seen = Files::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mode = entry.header().mode().unwrap();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            let name = name
                .strip_prefix("rc2fx/")
                .expect("single top folder")
                .to_owned();
            assert!(seen.insert(name, Member { data, mode }).is_none());
        }
        assert_eq!(seen, files);

        let mut zip_bytes = std::io::Cursor::new(Vec::new());
        write_zip(&mut zip_bytes, &files).unwrap();
        let mut zip = zip::ZipArchive::new(zip_bytes).unwrap();
        assert_eq!(zip.len(), files.len());
        for (name, m) in &files {
            let mut entry = zip.by_name(&format!("rc2fx/{name}")).unwrap();
            assert_eq!(entry.unix_mode().map(|mode| mode & 0o777), Some(m.mode));
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            assert_eq!(data, m.data, "{name}");
        }
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

    /// The relative targets of a Markdown file's links and mdBook includes,
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
            let links = prose.match_indices("](").map(|(at, _)| &prose[at + 2..]);
            let includes = prose
                .match_indices("{{#include ")
                .map(|(at, _)| &prose[at + 11..]);
            for rest in links.chain(includes) {
                let end = rest.find([')', '}', ' ']).unwrap_or(rest.len());
                let target = rest[..end].split('#').next().unwrap_or_default();
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
                    ```\n[e](e.md)\n```\n{{#include ../../crates/f/README.md}}\n";
        assert_eq!(local_targets(text), ["../a.md", "../../crates/f/README.md"]);
        assert_eq!(resolve("docs/user/x.md", "../a.md"), "docs/a.md");
    }

    /// Every local link in the packaged documentation points at a file the
    /// package also ships, at the same relative path.
    #[test]
    fn packaged_documentation_links_resolve() {
        let shipped: Vec<&str> = DOCUMENTS.iter().chain(&LINUX_FILES).copied().collect();
        let mut broken = Vec::new();
        for name in DOCUMENTS.iter().filter(|name| name.ends_with(".md")) {
            let text = fs::read_to_string(root().join(name)).unwrap();
            for target in local_targets(&text) {
                let path = resolve(name, &target);
                if !shipped.contains(&path.as_str()) {
                    broken.push(format!("{name}: {target}"));
                }
            }
        }
        assert!(
            broken.is_empty(),
            "links to files the package doesn't ship: {broken:#?}"
        );
    }
}
