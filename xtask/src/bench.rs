//! Times DeepFilterNet on the mixer over a recording (crates/rc2fx-bench)
//! and fetches the denoised audio; optionally compares a candidate weights file.
use crate::{Result, mixer::Mixer, root, setup};
use std::{fs, io::Read, path::Path, process::Command};

/// The fixed suppression engine and an optional candidate weights comparison.
const MODELS: &[&str] = &["deepfilternet3-h128"];
/// A DeepFilterNet weights file given with --weights.
const CUSTOM: &str = "deepfilternet3-custom";
/// Longest recording accepted; outputs live in the mixer's RAM.
const MAX_SECONDS: u64 = 60;

pub fn build() -> Result {
    let root = root();
    setup::check_zig()?;
    let target_dir = root.join("target/bench-build");
    crate::run(
        Command::new("cargo")
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", &target_dir)
            .env("RUSTFLAGS", setup::mixer_rustflags())
            .args([
                "zigbuild",
                "--locked",
                "--release",
                "--target",
                setup::ZIG_TARGET,
            ])
            .args(["--manifest-path", "crates/rc2fx-bench/Cargo.toml"]),
    )?;
    let out = root.join("target/bench");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let binary = out.join("rc2fx-bench");
    fs::copy(
        target_dir.join(setup::TARGET).join("release/rc2fx-bench"),
        &binary,
    )
    .map_err(|e| e.to_string())?;
    crate::elf::check(&binary, false)
}

pub fn run(input: &str, flags: &[&str]) -> Result {
    run_in(&root().join("target/bench"), Mixer::load, input, flags)
}

/// `bench` is target/bench; a test passes its own, and a stand-in mixer.
pub fn run_in(
    bench: &Path,
    mixer: impl FnOnce() -> Result<Mixer>,
    input: &str,
    flags: &[&str],
) -> Result {
    // Flags pass through to rc2fx-bench except --weights, whose file is
    // uploaded and run alongside the embedded weights.
    let mut passed = Vec::new();
    let mut weights = None;
    let mut rest = flags.iter().copied();
    while let Some(flag) = rest.next() {
        match flag {
            "--unpaced" => passed.push(flag.to_string()),
            "--policy" | "--cpus" | "--priority" => {
                let value = rest.next().ok_or(format!("{flag} needs a value"))?;
                if !value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '-'))
                {
                    return Err(format!("bad {flag} value {value}"));
                }
                passed.push(flag.to_string());
                passed.push(value.to_string());
            }
            "--weights" => {
                let path = rest.next().ok_or("--weights needs a file")?;
                weights = Some(fs::read(path).map_err(|e| format!("{path}: {e}"))?);
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    let input = Path::new(input);
    let recording = fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let (rate, seconds) = wav_header(&recording)?;
    if rate != 48_000 {
        return Err(format!("the recording is {rate} Hz; use 48 kHz"));
    }
    if seconds > MAX_SECONDS {
        return Err(format!(
            "the recording is {seconds} s; keep it under {MAX_SECONDS} s"
        ));
    }
    let binary = fs::read(bench.join("rc2fx-bench"))
        .map_err(|_| "target/bench/rc2fx-bench is missing; run `just bench-build`")?;
    let mut archive = tar::Builder::new(Vec::new());
    let mut files = vec![
        ("input.wav", &recording, 0o600),
        ("rc2fx-bench", &binary, 0o700),
    ];
    if let Some(weights) = &weights {
        files.push(("weights.bin", weights, 0o600));
    }
    for (name, bytes, mode) in files {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        archive
            .append_data(&mut header, name, bytes.as_slice())
            .map_err(|e| e.to_string())?;
    }
    let archive = archive.into_inner().map_err(|e| e.to_string())?;

    let mixer = mixer()?;
    let mixer = mixer.arm_power_recovery("bench")?;
    println!("Uploading to the mixer");
    mixer.ssh(
        "set -e; rm -rf /run/rc2fx-bench; mkdir -m 700 /run/rc2fx-bench; tar -xf - -C /run/rc2fx-bench",
        &archive,
    )?;
    let flags = passed.join(" ");
    let mut models: Vec<&str> = MODELS.to_vec();
    if weights.is_some() {
        models.push(CUSTOM);
    }
    let mut overruns = Vec::new();
    let mut outcome = Ok(());
    for (index, model) in models.iter().enumerate() {
        println!("Running {model}...");
        let custom = if *model == CUSTOM {
            " --weights weights.bin"
        } else {
            ""
        };
        let script = format!(
            "cd /run/rc2fx-bench && ./rc2fx-bench {model} input.wav {:02}-{model}.wav {flags}{custom} \
             > {model}.json 2> {model}.log || echo \"  {model} failed: $(tail -n 3 {model}.log)\"",
            index + 1
        );
        if let Err(error) = mixer.ssh(&script, b"") {
            outcome = Err(error);
            break;
        }
        // The controller keeps only the last ~20 s, so read it right away.
        let window = mixer
            .capture(&format!("cat /run/rc2fx-bench/{model}.json"))
            .unwrap_or_default();
        overruns.push(overruns_during(&window));
    }
    // Bulk uncompressed SSH downloads disturb the live audio graph. Device
    // comparisons found compressed downloads clean, at the cost of transfer
    // time. Uploads did not benefit, so keep their existing path unchanged.
    let results =
        mixer.capture("cd /run/rc2fx-bench && tar -czf - *.wav *.json *.log 2>/dev/null; true");
    let _ = mixer.ssh("rm -rf /run/rc2fx-bench", b"");
    outcome?;

    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let out = bench.join(&*stem);
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    unpack_results(&results?, &out)?;
    fs::rename(out.join("input.wav"), out.join("00-input.wav")).map_err(|e| e.to_string())?;
    report(&out, &models, &overruns)
}

fn unpack_results(bytes: &[u8], out: &Path) -> Result {
    // Read through the gzip footer before touching the output directory. A tar
    // reader can stop at its end marker without checking a truncated gzip tail.
    let mut archive = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut archive)
        .map_err(|e| format!("decoding benchmark results: {e}"))?;
    tar::Archive::new(archive.as_slice())
        .unpack(out)
        .map_err(|e| e.to_string())
}

/// RC2FX audio-callback overruns while the benchmark's timed loop ran, and the
/// seconds observed, from the controller's per-second history (mixer clock),
/// read over USB 1. None when RC2FX is not running or not reachable.
fn overruns_during(report: &[u8]) -> Option<(u64, f64)> {
    use rc2fx_installer::control::{Client, Request};
    let report: serde_json::Value = serde_json::from_slice(report).ok()?;
    let (start, end) = (
        report["startUnixMs"].as_u64()?,
        report["endUnixMs"].as_u64()?,
    );
    let request = Request {
        method: "GET".into(),
        path: "api/v1/diagnostics".into(),
        body: None,
    };
    let value = Client::open().ok()?.request(&request).ok()?.body;
    let points: Vec<(u64, u64)> = value["samples"]
        .as_array()?
        .iter()
        .filter_map(|sample| {
            let at = sample["observation"]["at"]["unixTimeMs"].as_u64()?;
            let overruns =
                sample["telemetry"]["nativeDiagnostics"][0]["timing"]["callbackWall"]["overruns"]
                    .as_u64()?;
            (start..=end).contains(&at).then_some((at, overruns))
        })
        .collect();
    let (first, last) = (points.first()?, points.last()?);
    (last.0 > first.0).then(|| {
        (
            last.1.saturating_sub(first.1),
            (last.0 - first.0) as f64 / 1e3,
        )
    })
}

/// Sample rate and whole seconds from a WAV header.
fn wav_header(bytes: &[u8]) -> Result<(u32, u64)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("the recording must be a WAV file".into());
    }
    let (mut rate, mut byte_rate, mut data) = (0, 0, 0u64);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..bytes.len().min(at + 8 + size)];
        if id == b"fmt " && body.len() >= 12 {
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            byte_rate = u32::from_le_bytes(body[8..12].try_into().unwrap());
        } else if id == b"data" {
            data = size as u64;
        }
        at += 8 + size + (size & 1);
    }
    if rate == 0 || byte_rate == 0 {
        return Err("the WAV file has no format chunk".into());
    }
    Ok((rate, data / u64::from(byte_rate)))
}

fn report(out: &Path, models: &[&str], overruns: &[Option<(u64, f64)>]) -> Result {
    println!(
        "\n{:<26} {:>8} {:>9} {:>9} {:>9} {:>9} {:>6} {:>5} {:>8} {:>6} {:>9}",
        "model",
        "frame",
        "cpu mean",
        "cpu max",
        "wall p99",
        "wall max",
        "load",
        "late",
        "startup",
        "RSS",
        "mixer ovr"
    );
    for (index, model) in models.iter().enumerate() {
        let text = fs::read_to_string(out.join(format!("{model}.json"))).unwrap_or_default();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
            println!("{model:<26} failed; see {model}.log");
            continue;
        };
        let number = |path: &[&str]| {
            path.iter()
                .fold(&value, |value, key| &value[*key])
                .as_f64()
                .unwrap_or(f64::NAN)
        };
        let overrun = match overruns.get(index).copied().flatten() {
            Some((count, seconds)) => format!("{count} in {seconds:.0}s"),
            None => "-".into(),
        };
        println!(
            "{:<26} {:>6.2}ms {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>7.2}ms {:>5.0}% {:>5} {:>6.0}ms {:>4.0}MB {:>9}",
            value["name"].as_str().unwrap_or(model),
            number(&["frameMs"]),
            number(&["cpu", "meanMs"]),
            number(&["cpu", "maxMs"]),
            number(&["wall", "p99Ms"]),
            number(&["wall", "maxMs"]),
            number(&["realtimeFactor"]) * 100.0,
            number(&["lateFrames"]),
            number(&["startupMs"]),
            number(&["peakRssKiB"]) / 1024.0,
            overrun,
        );
    }
    println!(
        "\nLoad is CPU time per frame as a share of the frame's duration; above 100% cannot run live.\n\
         Mixer ovr counts RC2FX audio-callback overruns while each model ran (- when RC2FX is not running).\n\
         Denoised recordings and full results: {}",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    fn compressed_results() -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (name, bytes) in [
            ("input.wav", &b"RIFF\0\xff\x80binary audio"[..]),
            ("model.json", &b"{\"frames\":200}"[..]),
            ("model.log", &b"completed\n"[..]),
        ] {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn compressed_results_preserve_audio_and_reports() {
        let out = tempfile::tempdir().unwrap();
        unpack_results(&compressed_results(), out.path()).unwrap();
        assert_eq!(
            fs::read(out.path().join("input.wav")).unwrap(),
            b"RIFF\0\xff\x80binary audio"
        );
        assert_eq!(
            fs::read(out.path().join("model.json")).unwrap(),
            b"{\"frames\":200}"
        );
        assert_eq!(
            fs::read(out.path().join("model.log")).unwrap(),
            b"completed\n"
        );
    }

    #[test]
    fn incomplete_or_corrupt_gzip_is_rejected_before_unpacking() {
        let good = compressed_results();
        let mut bad_crc = good.clone();
        let crc = bad_crc.len() - 8;
        bad_crc[crc] ^= 1;
        for bytes in [&good[..good.len() - 8], bad_crc.as_slice()] {
            let out = tempfile::tempdir().unwrap();
            assert!(unpack_results(bytes, out.path()).is_err());
            assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
        }
    }
}
