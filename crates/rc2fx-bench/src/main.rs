//! Runs one suppression model over a recording the way the mixer's worker
//! would: one frame at a time, paced to real time, on the production worker
//! policy (SCHED_RR priority 5 on CPU 0). Writes the denoised audio, aligned
//! to the input, and prints a JSON timing summary on stdout.
//!
//!     rc2fx-bench MODEL INPUT.wav OUTPUT.wav [--unpaced] [--policy production|inherited]
//!                 [--cpus LIST] [--priority N] [--weights FILE] [--pause-us N] [--spin]
//!                 [--kernel tiled|rows|compare]
//!
//! MODEL is deepfilternet3-h128, or
//! deepfilternet3-custom with `--weights`, a DeepFilterNet weights file in the
//! engine's format whose framing the model defines. `--cpus` (for example 0 or
//! 1-3) and `--priority` (SCHED_RR 1-99) replace the production policy's
//! placement, to measure the model where the mixer could run it.
//!
//! `--kernel` chooses the int8 matrix kernel of the GRUs, which this binary
//! alone can do (`rc2fx-dfn`'s `bench-kernels` feature). `tiled` is the
//! production kernel and the default. `rows` is the row-major kernel it
//! replaced. `compare` runs the recording four times, rows, tiled, tiled,
//! rows, so that a drift in the mixer's load falls on both alike; it reports
//! the time per frame of each kernel under `kernels`, fails unless every
//! pass produced the same samples bit for bit, and also times each kernel
//! alone on one cache-resident matrix. The summary's own `cpu` and `wall`
//! are then the tiled kernel's. The two kernels compute the same bits; only
//! their speed on the mixer is in question. Because the run shares the
//! mixer with the live engine, `compare` first prints to stderr how long it
//! will take and under which scheduling, and it sleeps for two seconds
//! after the resident timing and between passes.
use rc2fx_suppression::{VendorModel, bench_kernels};
use std::time::Instant;

const SAMPLE_RATE: u32 = 48_000;
/// How long a `--kernel compare` run sleeps between its parts, so that the
/// live engine has the core to itself between them.
const COMPARE_REST: std::time::Duration = std::time::Duration::from_secs(2);

fn main() {
    if let Err(message) = run() {
        eprintln!("rc2fx-bench: {message}");
        std::process::exit(1);
    }
}

struct Options {
    weights: Option<String>,
    input: String,
    output: String,
    paced: bool,
    policy: Option<Policy>,
    /// DeepFilterNet3's pause between layers in microseconds; `None` keeps the
    /// production value, 0 removes the pause.
    pause_us: Option<u64>,
    /// Spin instead of sleep for the pause.
    spin: bool,
    kernel: Kernel,
}
/// `--kernel`: the GRUs' int8 matrix kernel, or both in turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kernel {
    Tiled,
    Rows,
    Compare,
}
/// Scheduling applied to this thread before the model is built.
#[derive(PartialEq, Eq)]
struct Policy {
    cpus: Vec<usize>,
    priority: i32,
}
/// The production suppression worker policy (`PRODUCTION_WORKER`): SCHED_RR
/// priority 5 on CPU 0.
fn production() -> Policy {
    Policy {
        cpus: vec![0],
        priority: 5,
    }
}

fn options() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: MODEL INPUT.wav OUTPUT.wav [--unpaced] [--policy production|inherited] \
                 [--cpus LIST] [--priority N] [--weights FILE] [--pause-us N] [--spin] \
                 [--kernel tiled|rows|compare]";
    let model = args.next().ok_or(usage)?;
    let custom = match model.as_str() {
        "deepfilternet3-custom" => true,
        "deepfilternet3-h128" => false,
        _ => return Err(format!("unknown model {model}")),
    };
    let input = args.next().ok_or(usage)?;
    let output = args.next().ok_or(usage)?;
    let mut paced = true;
    let mut policy = Some(production());
    let (mut cpus, mut priority, mut weights, mut pause_us) = (None, None, None, None);
    let mut spin = false;
    let mut kernel = Kernel::Tiled;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--unpaced" => paced = false,
            "--kernel" => {
                kernel = match args.next().as_deref() {
                    Some("tiled") => Kernel::Tiled,
                    Some("rows") => Kernel::Rows,
                    Some("compare") => Kernel::Compare,
                    _ => return Err(usage.into()),
                }
            }
            "--policy" => match args.next().as_deref() {
                Some("production") => policy = Some(production()),
                Some("inherited") => policy = None,
                _ => return Err(usage.into()),
            },
            "--cpus" => cpus = Some(cpu_list(&args.next().ok_or(usage)?)?),
            "--priority" => {
                priority = Some(
                    args.next()
                        .and_then(|n| n.parse::<i32>().ok())
                        .filter(|n| (1..=99).contains(n))
                        .ok_or(usage)?,
                )
            }
            "--weights" => weights = Some(args.next().ok_or(usage)?),
            "--spin" => spin = true,
            "--pause-us" => {
                pause_us = Some(
                    args.next()
                        .ok_or(usage)?
                        .parse()
                        .map_err(|_| "--pause-us takes microseconds")?,
                )
            }
            _ => return Err(usage.into()),
        }
    }
    if cpus.is_some() || priority.is_some() {
        let base = policy.unwrap_or(Policy {
            cpus: Vec::new(),
            priority: 0,
        });
        policy = Some(Policy {
            cpus: cpus.unwrap_or(base.cpus),
            priority: priority.unwrap_or(base.priority),
        });
    }
    if custom && weights.is_none() {
        return Err("deepfilternet3-custom needs --weights FILE".into());
    }
    Ok(Options {
        weights,
        input,
        output,
        paced,
        policy,
        pause_us,
        spin,
        kernel,
    })
}

/// "0", "1,3" or "1-3".
fn cpu_list(text: &str) -> Result<Vec<usize>, String> {
    let bad = || format!("bad CPU list {text}");
    let mut cpus = Vec::new();
    for part in text.split(',') {
        if let Some((a, b)) = part.split_once('-') {
            let a = a.parse::<usize>().map_err(|_| bad())?;
            let b = b.parse::<usize>().map_err(|_| bad())?;
            if a > b {
                return Err(bad());
            }
            cpus.extend(a..=b);
        } else {
            cpus.push(part.parse().map_err(|_| bad())?);
        }
    }
    Ok(cpus)
}

fn read_mono(path: &str) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        return Err(format!(
            "{path} is {} Hz; record at 48 kHz",
            spec.sample_rate
        ));
    }
    let channels = usize::from(spec.channels);
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|s| s as f32 * scale))
                .collect()
        }
    }
    .map_err(|e| format!("{path}: {e}"))?;
    // A stereo recording of one microphone: keep the first channel.
    Ok(samples.into_iter().step_by(channels).collect())
}

fn write_mono(path: &str, samples: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| format!("{path}: {e}"))?;
    for &sample in samples {
        writer.write_sample(sample).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// Pins this thread to `policy.cpus` (if any) and, for a nonzero priority,
/// makes it SCHED_RR at that priority. Returns the policy's description.
fn apply_policy(policy: &Policy) -> Result<String, String> {
    unsafe {
        if !policy.cpus.is_empty() {
            let mut cpus: libc::cpu_set_t = std::mem::zeroed();
            for &cpu in &policy.cpus {
                libc::CPU_SET(cpu, &mut cpus);
            }
            if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &cpus) != 0 {
                return Err(format!("CPU affinity: {}", std::io::Error::last_os_error()));
            }
        }
        if policy.priority != 0 {
            let param = libc::sched_param {
                sched_priority: policy.priority,
            };
            if libc::sched_setscheduler(0, libc::SCHED_RR, &param) != 0 {
                return Err(format!(
                    "SCHED_RR {}: {}",
                    policy.priority,
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    let cpus = if policy.cpus.is_empty() {
        "inherited CPUs".to_string()
    } else {
        let list: Vec<String> = policy.cpus.iter().map(ToString::to_string).collect();
        format!("CPUs {}", list.join(","))
    };
    Ok(if policy.priority != 0 {
        format!("SCHED_RR {}, {cpus}", policy.priority)
    } else {
        format!("inherited scheduling, {cpus}")
    })
}

fn clock(id: libc::clockid_t) -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(id, &mut time) };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

fn sleep_until(deadline_ns: u64) {
    let time = libc::timespec {
        tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
    };
    unsafe {
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &time,
            std::ptr::null_mut(),
        )
    };
}

fn summary(mut values: Vec<u64>) -> serde_json::Value {
    values.sort_unstable();
    let ms = |ns: u64| ns as f64 / 1e6;
    let at = |fraction: f64| ms(values[((values.len() - 1) as f64 * fraction) as usize]);
    let mean = values.iter().sum::<u64>() as f64 / values.len() as f64 / 1e6;
    serde_json::json!({
        "meanMs": mean, "p50Ms": at(0.5), "p99Ms": at(0.99), "maxMs": ms(*values.last().unwrap()),
    })
}

/// Lag (in samples) that best lines the output up with the input, as a check
/// on the declared delay.
fn measured_lag(input: &[f32], output: &[f32]) -> usize {
    // One second from one second in: past any silent lead-in, and cheap enough
    // not to disturb the mixer (the model delays less than 2048 samples).
    let start = (SAMPLE_RATE as usize).min(input.len() / 2);
    let window =
        (SAMPLE_RATE as usize).min(input.len().min(output.len()).saturating_sub(start + 2048));
    (0..2048)
        .max_by(|&a, &b| {
            let score = |lag: usize| -> f64 {
                (start..start + window)
                    .map(|i| f64::from(input[i]) * f64::from(output[i + lag]))
                    .sum()
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap_or(0)
}

/// One timed pass of the recording through a newly built model.
struct Pass {
    row_major: bool,
    startup_ms: f64,
    cpu: Vec<u64>,
    wall: Vec<u64>,
    late: usize,
    output: Vec<f32>,
    frame: usize,
    delay: usize,
    start_unix_ms: u64,
    end_unix_ms: u64,
}

/// Builds the model and runs `input` through it one frame at a time with
/// the row-major int8 kernel (`row_major`) or the tiled one.
fn pass(
    options: &Options,
    weights: Option<&[u8]>,
    input: &[f32],
    row_major: bool,
) -> Result<Pass, String> {
    bench_kernels::set_row_major_q8(row_major);
    let started = Instant::now();
    let mut model = match weights {
        Some(weights) => VendorModel::deep_filter_from_weights(weights),
        None => VendorModel::new(0),
    }
    .map_err(|e| format!("model: {e}"))?;
    if let Some(us) = options.pause_us {
        let d = std::time::Duration::from_micros(us);
        let pause = if options.spin {
            rc2fx_suppression::LayerPause::Spin(d)
        } else {
            rc2fx_suppression::LayerPause::Sleep(d)
        };
        model.set_layer_pause((us > 0).then_some(pause));
    }
    let startup_ms = started.elapsed().as_secs_f64() * 1e3;
    let geometry = model.geometry();
    let frame = geometry.frame_samples;
    let delay = geometry.intrinsic_delay_samples;
    // Pad so the delayed tail of the recording is flushed out too.
    let frames = (input.len() + delay).div_ceil(frame);
    let mut output = input.to_vec();
    output.resize(frames * frame, 0.0);
    let frame_ns = frame as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE);
    let (mut cpu, mut wall) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    let mut late = 0;
    let begin = clock(libc::CLOCK_MONOTONIC);
    let start_unix_ms = clock(libc::CLOCK_REALTIME) / 1_000_000;
    for (index, samples) in output.chunks_mut(frame).enumerate() {
        let deadline = begin + (index as u64 + 1) * frame_ns;
        let (cpu_start, wall_start) = (
            clock(libc::CLOCK_THREAD_CPUTIME_ID),
            clock(libc::CLOCK_MONOTONIC),
        );
        model
            .process(samples)
            .map_err(|e| format!("frame {index}: {e}"))?;
        let wall_end = clock(libc::CLOCK_MONOTONIC);
        cpu.push(clock(libc::CLOCK_THREAD_CPUTIME_ID) - cpu_start);
        wall.push(wall_end - wall_start);
        if wall_end > deadline {
            late += 1;
        }
        if options.paced {
            sleep_until(deadline);
        }
    }
    let end_unix_ms = clock(libc::CLOCK_REALTIME) / 1_000_000;
    Ok(Pass {
        row_major,
        startup_ms,
        cpu,
        wall,
        late,
        output,
        frame,
        delay,
        start_unix_ms,
        end_unix_ms,
    })
}

fn kernel_name(row_major: bool) -> &'static str {
    if row_major { "rows" } else { "tiled" }
}

/// One kernel's passes of a `--kernel compare` run: every frame of all of
/// them together, and each pass on its own with the mixer-clock window it
/// ran in.
fn kernel_report(passes: &[Pass], row_major: bool) -> serde_json::Value {
    let mine = || passes.iter().filter(move |p| p.row_major == row_major);
    let all = |pick: fn(&Pass) -> &Vec<u64>| -> Vec<u64> {
        mine().flat_map(|p| pick(p).iter().copied()).collect()
    };
    let each: Vec<serde_json::Value> = passes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.row_major == row_major)
        .map(|(index, p)| {
            serde_json::json!({
                "pass": index + 1,
                "cpu": summary(p.cpu.clone()),
                "wall": summary(p.wall.clone()),
                "lateFrames": p.late,
                "startUnixMs": p.start_unix_ms,
                "endUnixMs": p.end_unix_ms,
            })
        })
        .collect();
    serde_json::json!({
        "cpu": summary(all(|p| &p.cpu)),
        "wall": summary(all(|p| &p.wall)),
        "lateFrames": mine().map(|p| p.late).sum::<usize>(),
        "passes": each,
    })
}

fn run() -> Result<(), String> {
    let options = options()?;
    let input = read_mono(&options.input)?;
    let policy = match &options.policy {
        Some(policy) => apply_policy(policy)?,
        None => "inherited".to_string(),
    };
    let weights = match &options.weights {
        Some(path) => Some(std::fs::read(path).map_err(|e| format!("{path}: {e}"))?),
        None => None,
    };
    // Rows, tiled, tiled, rows: each kernel has one early and one late pass.
    let order: &[bool] = match options.kernel {
        Kernel::Tiled => &[false],
        Kernel::Rows => &[true],
        Kernel::Compare => &[true, false, false, true],
    };
    let compare = options.kernel == Kernel::Compare;
    if compare {
        // The passes and one rest after the resident timing and after every
        // pass but the last. A paced pass lasts as long as the recording; an
        // unpaced one is shorter wherever the model keeps up with real time.
        let seconds = input.len() as f64 / f64::from(SAMPLE_RATE);
        let total = order.len() as f64 * (seconds + COMPARE_REST.as_secs_f64());
        let scheduling = if options.policy == Some(production()) {
            format!("at the production worker's priority ({policy})")
        } else {
            format!("with this scheduling: {policy}")
        };
        eprintln!(
            "rc2fx-bench: --kernel compare takes {} {:.0} s ({} passes of {:.0} s, each after {} s \
             asleep) and runs {scheduling}, beside the live engine",
            if options.paced {
                "about"
            } else {
                "up to about"
            },
            total.ceil(),
            order.len(),
            seconds,
            COMPARE_REST.as_secs(),
        );
    }
    // Each kernel alone on one cache-resident 384 x 128 matrix, under the
    // same scheduling as the passes.
    let resident = compare.then(|| {
        let [rows, tiled] = bench_kernels::resident_product_ns(2_000);
        serde_json::json!({ "rows": rows, "tiled": tiled })
    });
    let mut passes = Vec::with_capacity(order.len());
    for &row_major in order {
        if compare {
            // Asleep, not spinning: the core goes back to the live engine.
            std::thread::sleep(COMPARE_REST);
        }
        passes.push(pass(&options, weights.as_deref(), &input, row_major)?);
    }
    bench_kernels::set_row_major_q8(false);
    // Back to normal scheduling before the untimed checks.
    if options.policy.as_ref().is_some_and(|p| p.priority != 0) {
        let param = libc::sched_param { sched_priority: 0 };
        unsafe { libc::sched_setscheduler(0, libc::SCHED_OTHER, &param) };
    }
    // The summary's own figures are the production kernel's when it ran.
    let main = passes.iter().find(|p| !p.row_major).unwrap_or(&passes[0]);
    let identical = passes.iter().all(|p| {
        p.output.len() == main.output.len()
            && p.output
                .iter()
                .zip(&main.output)
                .all(|(a, b)| a.to_bits() == b.to_bits())
    });
    let (frame, delay) = (main.frame, main.delay);
    let frames = main.cpu.len();
    let frame_ns = frame as u64 * 1_000_000_000 / u64::from(SAMPLE_RATE);
    let lag = measured_lag(&input, &main.output);
    write_mono(&options.output, &main.output[delay..delay + input.len()])?;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let cpu_mean = main.cpu.iter().sum::<u64>() as f64 / frames as f64;
    let mut report = serde_json::json!({
        "name": if weights.is_none() {
            "DeepFilterNet3 (RC2FX)".to_string()
        } else {
            format!("DeepFilterNet3 custom ({frame}-sample frame, {delay} delay)")
        },
        "frameSamples": frame,
        "frameMs": frame_ns as f64 / 1e6,
        "delaySamples": delay,
        "measuredLagSamples": lag,
        "frames": frames,
        "paced": options.paced,
        "policy": policy,
        "startupMs": main.startup_ms,
        "kernel": kernel_name(main.row_major),
        "cpu": summary(main.cpu.clone()),
        "wall": summary(main.wall.clone()),
        "realtimeFactor": cpu_mean / frame_ns as f64,
        "lateFrames": main.late,
        // With both kernels' weights in memory, about 480 KiB more than the
        // service holds.
        "peakRssKiB": usage.ru_maxrss,
        // Mixer clock, so the controller's per-second history can be lined up.
        "startUnixMs": passes[0].start_unix_ms,
        "endUnixMs": passes[passes.len() - 1].end_unix_ms,
    });
    if compare {
        let (rows, tiled) = (kernel_report(&passes, true), kernel_report(&passes, false));
        let mean = |kernel: &serde_json::Value| kernel["cpu"]["meanMs"].as_f64().unwrap_or(0.);
        let (rows_ms, tiled_ms) = (mean(&rows), mean(&tiled));
        eprintln!(
            "kernels: rows {:.1} us and tiled {:.1} us of CPU per frame (tiled {:+.1}%), outputs {}",
            rows_ms * 1e3,
            tiled_ms * 1e3,
            (tiled_ms / rows_ms - 1.) * 100.,
            if identical { "identical" } else { "DIFFER" }
        );
        report["kernels"] = serde_json::json!({
            "order": order.iter().map(|&r| kernel_name(r)).collect::<Vec<_>>(),
            "rows": rows,
            "tiled": tiled,
            // Negative when the tiled kernel is faster.
            "tiledCpuMeanDeltaUs": (tiled_ms - rows_ms) * 1e3,
            "tiledCpuMeanDeltaPercent": (tiled_ms / rows_ms - 1.) * 100.,
            "outputsIdentical": identical,
            "residentProductNs": resident,
        });
    }
    println!("{report}");
    if !identical {
        return Err("the kernels' outputs differ".into());
    }
    Ok(())
}
