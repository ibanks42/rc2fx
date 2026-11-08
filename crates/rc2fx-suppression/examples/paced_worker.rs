//! Bounded production-worker timing probe. No JACK, settings writes or routing.
//! Non-silent input is generated before measurement; this is not a sound test.
use rc2fx_suppression::{
    AffinityScope, Endpoint, PRODUCTION_WORKER, Snapshot, StopMode, Timing, WorkerOptions,
};
use serde_json::{Value, json};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};

const HELP: &str = "paced_worker dfn SECONDS [--signal silence|continuous|both] [--policy production|inherited]\nSECONDS is 1..30 per phase. 'both' runs silence then continuous in one worker.\nproduction applies the mixer worker policy (RR5 on CPU 3, KernelAllowed affinity);\ninherited (default) keeps this process's scheduling. No JACK/audio routing is opened.";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    Silence,
    Continuous,
    Both,
}
impl Signal {
    fn name(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Continuous => "continuous",
            Self::Both => "both",
        }
    }
}
#[derive(Debug)]
struct Args {
    seconds: u64,
    signal: Signal,
    options: WorkerOptions,
}
struct Phase {
    signal: Signal,
    end_block: u64,
    max_lateness: Duration,
    missed_periods: u64,
    total_callback: Duration,
    max_callback: Duration,
    worker: Snapshot,
}
fn parse(args: &[String]) -> Result<Option<Args>, String> {
    if args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    if args.len() < 2 || args[0] != "dfn" {
        return Err(HELP.into());
    }
    let seconds = args[1].parse::<u64>().map_err(|_| "invalid seconds")?;
    if !(1..=30).contains(&seconds) {
        return Err("seconds must be 1..30 per phase".into());
    }
    let mut result = Args {
        seconds,
        signal: Signal::Continuous,
        options: WorkerOptions::default(),
    };
    let mut signal_seen = false;
    let mut policy_seen = false;
    let (pairs, unpaired) = args[2..].as_chunks::<2>();
    for pair in pairs {
        match pair[0].as_str() {
            "--signal" if !signal_seen => {
                result.signal = match pair[1].as_str() {
                    "silence" => Signal::Silence,
                    "continuous" => Signal::Continuous,
                    "both" => Signal::Both,
                    _ => return Err("signal must be silence, continuous or both".into()),
                };
                signal_seen = true;
            }
            "--policy" if !policy_seen => {
                result.options = match pair[1].as_str() {
                    "production" => PRODUCTION_WORKER,
                    "inherited" => WorkerOptions::default(),
                    _ => return Err("policy must be production or inherited".into()),
                };
                policy_seen = true;
            }
            _ => return Err(format!("unknown or duplicate option: {}", pair[0])),
        }
    }
    if !unpaired.is_empty() {
        return Err("option needs a value".into());
    }
    Ok(Some(result))
}
fn timing(value: Timing) -> Value {
    json!({"calls":value.calls,"totalNs":value.total_ns,"maxNs":value.max_ns,
        "overruns":value.overruns,"clockErrors":value.clock_errors,"histogram":value.bins})
}
fn snapshot(value: Snapshot) -> Value {
    json!({"completedFrames":value.completed_frames,"inputDrops":value.input_drops,
        "outputDrops":value.output_drops,"missedDeadlines":value.missed_deadlines,
        "staleResults":value.stale_results,"failed":value.failed,
        "allowanceOverruns":value.allowance_overruns,"wall":timing(value.wall),
        "cpu":timing(value.cpu),"queueWait":timing(value.queue_wait),"turnaround":timing(value.turnaround)})
}
#[cfg(target_os = "linux")]
fn readback() -> Result<Value, String> {
    let mut workers = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if std::fs::read_to_string(entry.path().join("comm"))
            .map_err(|e| e.to_string())?
            .trim()
            == "rc2fx-ns-worker"
        {
            workers.push(
                entry
                    .file_name()
                    .to_string_lossy()
                    .parse::<i32>()
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    if workers.len() != 1 {
        return Err(format!(
            "expected one owned worker, found {}",
            workers.len()
        ));
    }
    let tid = workers[0];
    // Read-only queries against the one owned worker, outside the paced interval.
    unsafe {
        let policy = libc::sched_getscheduler(tid);
        let mut param = libc::sched_param { sched_priority: 0 };
        let mut affinity: libc::cpu_set_t = std::mem::zeroed();
        if policy < 0
            || libc::sched_getparam(tid, &mut param) != 0
            || libc::sched_getaffinity(tid, std::mem::size_of_val(&affinity), &mut affinity) != 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let cpus: Vec<_> = (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| libc::CPU_ISSET(*cpu, &affinity))
            .collect();
        Ok(json!({"workerTid":tid,"policy":policy,"priority":param.sched_priority,"cpus":cpus}))
    }
}
#[cfg(not(target_os = "linux"))]
fn readback() -> Result<Value, String> {
    Ok(Value::Null)
}
fn run(args: Args) -> Result<(), String> {
    // Generate non-silent input for feature extraction. The deterministic tone
    // plus broadband input is a timing workload, not representative speech.
    let samples: Vec<f32> = (0..48_000)
        .map(|i| {
            let noise = ((i as u32).wrapping_mul(1664525).wrapping_add(1013904223) >> 8) as f32
                / 16777216.0
                - 0.5;
            0.08 * (i as f32 * 0.037).sin() + 0.005 * noise
        })
        .collect();
    let explicit = args.options != WorkerOptions::default();
    let mut worker = Endpoint::new_with_affinity(
        0,
        args.options,
        if explicit {
            AffinityScope::KernelAllowed
        } else {
            AffinityScope::Inherited
        },
    )
    .map_err(|e| e.to_string())?;
    let telemetry = worker.telemetry();
    let geometry = worker.geometry();
    let scheduling = readback()?;
    let signals: &[Signal] = match args.signal {
        Signal::Both => &[Signal::Silence, Signal::Continuous],
        Signal::Silence => &[Signal::Silence],
        Signal::Continuous => &[Signal::Continuous],
    };
    let blocks = args.seconds * 48_000 / 112;
    let mut phases = Vec::with_capacity(signals.len());
    let mut global_block = 0_u64;
    let mut fingerprint = 0.0_f64;
    let initial = telemetry.snapshot();
    let start = Instant::now();
    for &signal in signals {
        let mut max_lateness = Duration::ZERO;
        let mut missed_periods = 0_u64;
        let mut max_callback = Duration::ZERO;
        let mut total_callback = Duration::ZERO;
        for _ in 0..blocks {
            let deadline =
                start + Duration::from_nanos(global_block * 112 * 1_000_000_000 / 48_000);
            if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let entered = Instant::now();
            let late = entered.saturating_duration_since(deadline);
            max_lateness = max_lateness.max(late);
            if late.as_nanos() > 112 * 1_000_000_000 / 48_000 {
                missed_periods += 1;
            }
            for offset in 0..112 {
                let sample = if signal == Signal::Silence {
                    0.0
                } else {
                    samples[((global_block * 112 + offset) % 48_000) as usize]
                };
                fingerprint += f64::from(worker.process_sample(black_box(sample), 1.0));
            }
            let elapsed = entered.elapsed();
            total_callback += elapsed;
            max_callback = max_callback.max(elapsed);
            global_block += 1;
        }
        phases.push(Phase {
            signal,
            end_block: global_block,
            max_lateness,
            missed_periods,
            total_callback,
            max_callback,
            worker: telemetry.snapshot(),
        });
    }
    let paced_elapsed_ns = start.elapsed().as_nanos() as u64;
    let terminal = worker
        .finish(StopMode::Drain, Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    if !terminal.joined || !terminal.drained || terminal.snapshot.failed {
        return Err(format!("worker retirement failed: {terminal:?}"));
    }
    let phases: Vec<_> = phases.into_iter().map(|phase| json!({"signal":phase.signal.name(),"blocks":blocks,"endBlock":phase.end_block,
        "pacingMaxLateNs":phase.max_lateness.as_nanos() as u64,"pacingMissedPeriods":phase.missed_periods,
        "callbackTotalNs":phase.total_callback.as_nanos() as u64,"callbackMaxNs":phase.max_callback.as_nanos() as u64,
        "worker":snapshot(phase.worker)})).collect();
    println!(
        "{}",
        json!({"model":"dfn","secondsPerPhase":args.seconds,"signal":args.signal.name(),
        "processPid":std::process::id(),"cpuMask":args.options.cpu_mask,"requestedPriority":args.options.realtime_priority,"requestedNice":args.options.nice,
        "affinityScope":if explicit {"kernelAllowed"} else {"inherited"},"scheduling":scheduling,
        "geometry":{"sampleRate":geometry.sample_rate,"frameSamples":geometry.frame_samples,
            "latencySamples":geometry.latency_samples(),"frameBudgetNs":geometry.frame_budget_ns(),"workerAllowanceNs":geometry.worker_allowance_ns()},
        "counterSemantics":"cumulativeWorkerCounters","initial":snapshot(initial),"phases":phases,
        "pacedElapsedNs":paced_elapsed_ns,"fingerprint":fingerprint,
        "terminal":{"joined":terminal.joined,"drained":terminal.drained,"inputSamples":terminal.input_samples,
            "fullInputFrames":terminal.full_input_frames,"partialInputSamples":terminal.partial_input_samples,
            "submittedFrames":terminal.submitted_frames,"pendingInputFrames":terminal.pending_input_frames,"worker":snapshot(terminal.snapshot)}})
    );
    Ok(())
}
fn main() {
    let result = parse(&std::env::args().skip(1).collect::<Vec<_>>()).and_then(|args| match args {
        Some(args) => run(args),
        None => {
            println!("{HELP}");
            Ok(())
        }
    });
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn production_policy_and_two_phases_are_bounded() {
        let value = parse(&args(&[
            "dfn",
            "30",
            "--signal",
            "both",
            "--policy",
            "production",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(value.signal, Signal::Both);
        assert_eq!(value.options, PRODUCTION_WORKER);
        assert!(parse(&args(&["dfn", "31"])).is_err());
        assert!(parse(&args(&["dfn", "0"])).is_err());
    }
    #[test]
    fn defaults_and_invalid_flags_are_explicit() {
        let value = parse(&args(&["dfn", "1"])).unwrap().unwrap();
        assert_eq!(value.signal, Signal::Continuous);
        assert_eq!(value.options, WorkerOptions::default());
        for values in [
            vec!["dfn", "1", "--policy", "other"],
            vec!["dfn", "1", "--priority", "5"],
            vec!["dfn", "1", "--signal", "bad"],
            vec!["dfn", "1", "--signal"],
            vec![
                "dfn",
                "1",
                "--policy",
                "production",
                "--policy",
                "inherited",
            ],
        ] {
            assert!(parse(&args(&values)).is_err());
        }
        assert!(parse(&args(&["--help"])).unwrap().is_none());
    }
}
