//! One-time setup and the mixer cross-build. The mixer build uses zig as the
//! C compiler and linker (through cargo-zigbuild), so it runs natively on any
//! host OS and links against the mixer's glibc 2.35 without a sysroot.
use crate::{Result, elf, root, sha256};
use std::{fs, process::Command};

pub const TARGET: &str = "aarch64-unknown-linux-gnu";
/// Firmware 1.7.6 ships glibc 2.35; zig links against exactly that version.
pub const ZIG_TARGET: &str = "aarch64-unknown-linux-gnu.2.35";
pub fn run() -> Result {
    println!("Adding the Rust {TARGET} target");
    crate::run(Command::new("rustup").args(["target", "add", TARGET]))?;
    if !succeeds(Command::new("cargo-zigbuild").arg("--version")) {
        println!("Installing cargo-zigbuild");
        crate::run(Command::new("cargo").args(["install", "--locked", "cargo-zigbuild"]))?;
    }
    check_zig()?;
    println!("Setup complete. Build the mixer side with `just mixer-build`.");
    Ok(())
}

fn succeeds(command: &mut Command) -> bool {
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn check_zig() -> Result {
    let output = Command::new("zig").arg("version").output();
    match output {
        Ok(output) if output.status.success() => {
            println!("Using zig {}", String::from_utf8_lossy(&output.stdout).trim());
            Ok(())
        }
        _ => Err("zig is not on PATH. Install it (0.13 or newer), then rerun setup:\n  \
              Windows: scoop install zig   or   winget install zig.zig\n  \
              macOS:   brew install zig\n  \
              Linux:   snap install zig --classic --beta, your package manager, or pip install ziglang\n  \
              https://ziglang.org/download/"
            .into()),
    }
}

/// The mixer's RUSTFLAGS: code generation for its Cortex-A53, exactly as the
/// Pro II profile was measured. They replace any RUSTFLAGS of the caller's,
/// so a host setting never reaches the mixer build.
pub fn mixer_rustflags() -> String {
    "-C target-cpu=cortex-a53".into()
}

fn check_mixer_staging(path: &std::path::Path) -> Result {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(
            "target/mixer is a link to another bundle; refusing to replace it. Use `cargo xtask mixer install` to deploy that bundle, or move the link aside to rebuild."
                .into(),
        ),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err("target/mixer is not a directory; refusing to replace it".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot inspect mixer staging: {error}")),
    }
}

pub fn build_mixer() -> Result {
    rc2fx_firmware::Catalog::parse(rc2fx_firmware::DEFAULT_CATALOG)?;
    let root = root();
    check_mixer_staging(&root.join("target/mixer"))?;
    check_zig()?;
    let target_dir = root.join("target/mixer-build");
    let zigbuild = || {
        let mut command = Command::new("cargo");
        command
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", &target_dir)
            .env("RUSTFLAGS", mixer_rustflags())
            .args(["zigbuild", "--locked", "--release", "--target", ZIG_TARGET]);
        command
    };
    println!("Building librc2fx_native.so");
    crate::run(zigbuild().args(["--manifest-path", "crates/rc2fx-native/Cargo.toml", "--lib"]))?;
    println!("Building rc2fxd");
    crate::run(zigbuild().args([
        "-p",
        "rc2fx-device",
        "--features",
        "native-pipeline",
        "--bin",
        "rc2fxd",
    ]))?;
    println!("Building rc2fx-supervisor");
    crate::run(zigbuild().args(["-p", "rc2fx-supervisor", "--bin", "rc2fx-supervisor"]))?;
    println!("Building firmware profile checker");
    crate::run(zigbuild().args(["-p", "rc2fx-firmware", "--bin", "rc2fx-firmware"]))?;
    let gui = build_gui_module(&root, &target_dir)?;
    let recorder = build_jack_recorder(&root, &target_dir)?;

    let release = target_dir.join(TARGET).join("release");
    // Staged beside target/mixer and swapped in only if something changed, so
    // an unchanged build does not make the desktop app embed it again.
    let staging = root.join("target/mixer");
    let out = root.join("target/mixer.new");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut sums = String::new();
    for (name, shared) in [
        ("librc2fx_native.so", true),
        ("rc2fxd", false),
        ("rc2fx-supervisor", false),
        ("rc2fx-firmware", false),
    ] {
        let file = out.join(name);
        fs::copy(release.join(name), &file).map_err(|e| format!("{name}: {e}"))?;
        elf::check(&file, shared)?;
        let bytes = fs::read(&file).map_err(|e| e.to_string())?;
        sums += &format!("{}  {name}\n", sha256(&bytes));
    }
    let profiles = rc2fx_firmware::DEFAULT_CATALOG.replace("\r\n", "\n");
    fs::write(out.join("firmware-profiles.json"), &profiles).map_err(|e| e.to_string())?;
    sums += &format!("{}  firmware-profiles.json\n", sha256(profiles.as_bytes()));
    let file = out.join(GUI_MODULE);
    fs::copy(&gui, &file).map_err(|e| format!("{GUI_MODULE}: {e}"))?;
    elf::check(&file, true)?;
    let bytes = fs::read(&file).map_err(|e| e.to_string())?;
    sums += &format!("{}  {GUI_MODULE}\n", sha256(&bytes));
    for (source, name, shared) in [
        (&recorder[0], JACKERR_MODULE, true),
        (&recorder[1], JACKERR_SNAPSHOT, false),
    ] {
        let file = out.join(name);
        fs::copy(source, &file).map_err(|e| format!("{name}: {e}"))?;
        elf::check(&file, shared)?;
        sums += &format!(
            "{}  {name}\n",
            sha256(&fs::read(&file).map_err(|e| e.to_string())?)
        );
    }
    // The runtime's release identity, pinned last; the installer refuses a
    // runtime whose manifest differs from config/mixer-release.json.
    let release = fs::read_to_string(root.join("config/mixer-release.json"))
        .map_err(|e| format!("config/mixer-release.json: {e}"))?
        .replace("\r\n", "\n");
    rc2fx_installer::MixerRelease::parse(release.as_bytes()).map_err(|e| e.0)?;
    fs::write(out.join("mixer-release.json"), &release).map_err(|e| e.to_string())?;
    sums += &format!("{}  mixer-release.json\n", sha256(release.as_bytes()));
    fs::write(out.join("SHA256SUMS"), &sums).map_err(|e| e.to_string())?;
    if same_files(&out, &staging) {
        fs::remove_dir_all(&out).map_err(|e| e.to_string())?;
        println!("Mixer build unchanged in target/mixer/\n{sums}");
    } else {
        let _ = fs::remove_dir_all(&staging);
        fs::rename(&out, &staging).map_err(|e| format!("target/mixer: {e}"))?;
        println!("Mixer build ready in target/mixer/\n{sums}");
    }
    Ok(())
}

/// Whether directories `a` and `b` hold the same files with the same bytes.
fn same_files(a: &std::path::Path, b: &std::path::Path) -> bool {
    let list = |dir: &std::path::Path| -> Option<Vec<(std::ffi::OsString, Vec<u8>)>> {
        let mut files = fs::read_dir(dir)
            .ok()?
            .map(|entry| {
                let entry = entry.ok()?;
                Some((entry.file_name(), fs::read(entry.path()).ok()?))
            })
            .collect::<Option<Vec<_>>>()?;
        files.sort();
        Some(files)
    };
    matches!((list(a), list(b)), (Some(a), Some(b)) if a == b)
}

/// The GUI's partial GE2D conversion module (LD_AUDIT; started by
/// `rc2fx.sh gui-launch`).
pub const GUI_MODULE: &str = "librc2fx_gui.so";

/// Builds the partial-conversion module from `crates/rc2fx-gui/gui_pacing.c`
/// with the three-job rectangle planner, as the Pro II profile was measured.
/// The source is passed by its absolute path, so the debug info names it.
fn build_gui_module(
    root: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<std::path::PathBuf> {
    println!("Building {GUI_MODULE}");
    let dir = target_dir.join("gui");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(GUI_MODULE);
    let mut command = Command::new("zig");
    command.current_dir(root).arg("cc");
    crate::run(
        command
            .args([
                "-target",
                "aarch64-linux-gnu.2.35",
                "-mcpu=cortex_a53",
                "-mno-outline-atomics",
                "-std=c11",
                "-O2",
                "-g",
                "-fPIC",
                "-shared",
                "-fvisibility=hidden",
                "-fno-omit-frame-pointer",
                "-mbranch-protection=standard",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-DRC2FX_PACING_DIRTY",
                "-DRC2FX_PACING_PARTIAL",
                "-DRC2FX_GUI_RECT_PLAN3",
            ])
            .arg(root.join("crates/rc2fx-gui/gui_pacing.c"))
            .arg("-o")
            .arg(&out),
    )?;
    Ok(out)
}

/// The JACK error recorder and its snapshot reader (`crates/rc2fx-native/jackerr`).
pub const JACKERR_MODULE: &str = "librc2fx_jackerr.so";
pub const JACKERR_SNAPSHOT: &str = "rc2fx_jackerr_snapshot";

fn build_jack_recorder(
    root: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<[std::path::PathBuf; 2]> {
    let directory = target_dir.join("jackerr");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let outputs = [
        directory.join(JACKERR_MODULE),
        directory.join(JACKERR_SNAPSHOT),
    ];
    for (source, output, shared) in [
        ("rc2fx_jackerr.c", &outputs[0], true),
        ("rc2fx_jackerr_snapshot.c", &outputs[1], false),
    ] {
        let mut command = Command::new("zig");
        command.current_dir(root).args([
            "cc",
            "-target",
            "aarch64-linux-gnu.2.35",
            "-mcpu=cortex_a53",
            "-mno-outline-atomics",
            "-std=c11",
            "-O2",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
        ]);
        if shared {
            command.args(["-shared", "-fPIC", "-fvisibility=hidden"]);
        } else {
            command.args(["-fPIE", "-pie"]);
        }
        crate::run(
            command
                .arg(root.join("crates/rc2fx-native/jackerr").join(source))
                .arg("-o")
                .arg(output),
        )?;
        elf::check(output, shared)?;
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;

    #[test]
    #[ignore = "requires zig; builds only the two recorder payloads, never audio/app"]
    fn recorder_builder_produces_both_checked_arm_payloads() {
        let root = super::root();
        let outputs =
            super::build_jack_recorder(&root, &root.join("target/recorder-builder-test")).unwrap();
        assert_eq!(outputs[0].file_name().unwrap(), super::JACKERR_MODULE);
        assert_eq!(outputs[1].file_name().unwrap(), super::JACKERR_SNAPSHOT);
        assert!(outputs.iter().all(|path| path.is_file()));
    }

    #[test]
    #[cfg(unix)]
    fn mixer_build_refuses_to_replace_a_promoted_bundle_link() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("rc2fx-staging-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let staging = root.join("mixer");
        let bundle = root.join("verified");
        fs::create_dir(&bundle).unwrap();
        fs::write(bundle.join("identity"), b"preserve").unwrap();
        assert!(super::check_mixer_staging(&staging).is_ok());
        std::os::unix::fs::symlink(&bundle, &staging).unwrap();
        let rejected = super::check_mixer_staging(&staging).unwrap_err();
        assert!(rejected.contains("cargo xtask mixer install"));
        assert!(staging.is_symlink());
        assert_eq!(fs::read(bundle.join("identity")).unwrap(), b"preserve");
        fs::remove_file(staging).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
