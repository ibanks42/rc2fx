//! Project tasks that work the same on Windows, Linux and macOS. Run through
//! `just`, or directly as `cargo xtask <command>`.
mod bench;
mod elf;
mod mixer;
mod mixer_release;
mod mixer_refresh;
mod mixer_keys;
mod package;
mod setup;
mod xruns;

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const HELP: &str = "cargo xtask <command>

  setup                 Install the Rust target and cargo-zigbuild, and check
                        for zig
  mixer-build           Cross-build the mixer binaries and profile checker into target/mixer
  mixer-package DIR     Package a standalone release payload and notices in a new directory
  mixer-keys DIR        Create signing keys and a public trust root outside the checkout
  mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY
                        Sign mixer update metadata with existing release keys (never root keys)
  mixer-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY
                        Renew discovery metadata without changing approved targets
  mixer <action>        Manage RC2FX on the mixer over SSH:
      run               run this build from RAM (a reboot returns to the installed build or stock)
      install           install to /opt/rc2fx and start at every boot
      uninstall [--purge]  remove the install (--purge also deletes settings)
      stop              stop RC2FX and hand audio back to stock
      status | logs     show what is running / recent logs
      xruns [SECONDS]   what the JACK error recorder records while you watch
                        (default 60 s; 0: everything since jackd started)
      api METHOD PATH [BODY|@file]  one controller API request over USB 1 (docs/reference/api.md)
      enable | disable  allow or skip starting at boot
  bench-build           Cross-build the suppression benchmark into target/bench
  bench WAV [--unpaced] [--policy production|inherited] [--cpus LIST] [--priority N]
                        [--weights FILE]
                        Time every suppression model on the mixer over a 48 kHz
                        recording and fetch the denoised audio
  check-elf FILE [--shared]  Check a mixer binary against the mixer ABI baseline
  licenses FILE         Write consolidated licence notices to FILE
  package [--target TRIPLE] [--skip-build] [--embedded]
                        Build the download-only desktop app (or embed the mixer with --embedded) and bundle
                        documentation, licences and SHA256SUMS in target/package

Mixer connection: .rc2fx-mixer.conf (see .rc2fx-mixer.conf.example) or the
RC2FX_MIXER_IP, RC2FX_MIXER_USER, RC2FX_MIXER_KEY and RC2FX_MIXER_KNOWN_HOSTS
environment variables.";

pub type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let outcome = match args.as_slice() {
        ["setup"] => setup::run(),
        ["mixer-build"] => setup::build_mixer(),
        ["mixer-package", destination] => mixer_release::package(Path::new(destination)),
        ["mixer-keys", destination] => mixer_keys::run(Path::new(destination)),
        ["mixer-sign", rest @ ..] => mixer_release::sign(rest),
        ["mixer-refresh", rest @ ..] => mixer_refresh::run(rest),
        ["mixer", action, rest @ ..] => mixer::run(action, rest),
        ["bench-build"] => bench::build(),
        ["bench", input, flags @ ..] => bench::run(input, flags),
        ["check-elf", file] => elf::check(Path::new(file), false),
        ["check-elf", file, "--shared"] => elf::check(Path::new(file), true),
        ["licenses", destination] => package::write_licenses(Path::new(destination)),
        ["package", rest @ ..] => package::run_package(rest),
        [] | ["help" | "--help" | "-h"] => {
            println!("{HELP}");
            Ok(())
        }
        _ => Err(format!("unknown command; see `cargo xtask help`\n\n{HELP}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The repository root (this crate lives one level below it).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Run a command to completion, failing on a non-zero exit.
pub fn run(command: &mut Command) -> Result {
    let status = command
        .status()
        .map_err(|error| format!("could not start {:?}: {error}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({status})", command.get_program()))
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
