//! Host-only cost of the callback endpoint
//! `Worker::process_sample`, and the feasibility of a block seam.
//!
//! The endpoint fixture has no OS worker thread. A fake model on the same
//! thread services the input queue between 112-sample blocks and publishes
//! each frame immediately (scaled by 0.5, voice 0.75), so neither DFN timing
//! nor scheduling enters the measurement. This is host x86 timing, NOT A53.
//!
//! `block_sketch` is a TEST-ONLY prototype of a block endpoint, used to check
//! that a block seam can keep the per-sample results bit for bit and to price
//! it. It is not a production change. Samples that hold an event (an output
//! frame boundary, the sample completing an input hop, a pending late result,
//! or a finished endpoint) go through the real `process_sample`; the samples
//! in between run a loop without the per-sample division, modulo or event
//! checks.
//!
//!     cargo test --release -p rc2fx-suppression endpoint_bench -- --include-ignored --nocapture
use super::*;
use std::hint::black_box;

/// Single-hop DeepFilterNet3 geometry with the admitted 320-sample lead, the
/// comparison point these measurements were taken against.
const PRODUCTION: Geometry = Geometry {
    sample_rate: 48_000,
    frame_samples: 240,
    intrinsic_delay_samples: 120,
    worker_lead_samples: 320,
};
/// The device period.
const PERIOD: usize = 112;

fn worker(geometry: Geometry) -> (Worker, Consumer, Producer) {
    let (mut w, input, output) = endpoint();
    w.geometry = geometry;
    (w, input, output)
}

/// The fake model: publishes every queued input frame at once.
fn serve(input: &mut Consumer, output: &mut Producer, nan_at: Option<u64>) {
    while let Some(mut frame) = input.pop() {
        for x in &mut frame.samples {
            *x *= 0.5;
        }
        if nan_at == Some(frame.sequence) {
            frame.samples[7] = f32::NAN;
        }
        frame.voice = 0.75;
        assert!(output.push(&frame));
    }
}

/// The interior of a block, between events. Mirrors `process_sample` for a
/// sample where `finished` is false, `pending` is None, the output phase is
/// not 0 (or the sample precedes `start`), the input hop does not complete
/// and the dry ring does not wrap.
#[inline(always)]
fn interior(
    w: &mut Worker,
    input: &[f32],
    wet: &[f32],
    out: &mut [f32],
    voice: &mut [Option<f32>],
    started: bool,
    phase: usize,
) {
    let failed = &w.shared.failed;
    let dry_ring = &mut w.dry[w.dry_position..w.dry_position + input.len()];
    let assembling = &mut w.assembling.samples[w.phase..w.phase + input.len()];
    let playing = &w.playing.samples[phase..phase + input.len()];
    let mut last = w.voice;
    for i in 0..input.len() {
        let x = if input[i].is_finite() { input[i] } else { 0. };
        let dry = dry_ring[i];
        dry_ring[i] = x;
        assembling[i] = x;
        // The failure flag stays a per-sample acquire load, as in
        // `process_sample`, so a concurrent failure is seen at the same sample.
        let available =
            started && w.playing.valid && playing[i].is_finite() && !failed.load(Ordering::Acquire);
        let output = if available {
            last = Some(w.playing.voice);
            playing[i]
        } else {
            last = None;
            dry
        };
        voice[i] = last;
        out[i] = dry + (output - dry) * wet[i];
    }
    w.voice = last;
    w.dry_position += input.len();
    if w.dry_position == w.latency_samples() {
        w.dry_position = 0;
    }
    w.phase += input.len();
    w.sample += input.len() as u64;
}

fn block_sketch(
    w: &mut Worker,
    input: &[f32],
    wet: &[f32],
    out: &mut [f32],
    voice: &mut [Option<f32>],
) {
    let fs = w.geometry.frame_samples;
    let start = (fs + w.geometry.worker_lead_samples) as u64;
    let latency = w.latency_samples();
    let mut t = 0;
    while t < input.len() {
        let s = w.sample;
        // Samples to the next event, exclusive: the event sample itself goes
        // through `process_sample`.
        let (started, phase, to_output) = if s >= start {
            let phase = ((s - start) % fs as u64) as usize;
            (true, phase, if phase == 0 { 0 } else { fs - phase })
        } else {
            (false, 0, (start - s) as usize)
        };
        let run = to_output
            .min(fs - 1 - w.phase)
            .min(latency - w.dry_position)
            .min(input.len() - t);
        if w.finished || w.pending.is_some() || run == 0 {
            out[t] = w.process_sample(input[t], wet[t]);
            voice[t] = w.voice;
            t += 1;
            continue;
        }
        let end = t + run;
        interior(
            w,
            &input[t..end],
            &wet[t..end],
            &mut out[t..end],
            &mut voice[t..end],
            started,
            phase,
        );
        t = end;
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn sample(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.
    }
}

#[derive(Clone, Copy)]
enum Service {
    /// Publish after every block.
    Immediate,
    /// Publish only every `n` blocks: misses, late results, stale results.
    Every(usize),
    /// Never publish: misses and input drops.
    Never,
}

#[derive(Default)]
struct Scenario {
    nan_input_every: Option<usize>,
    nan_result_at: Option<u64>,
    fail_at: Option<usize>,
    finish_at: Option<usize>,
}

/// One run through either path; returns every output bit, every voice and
/// the endpoint's state and counters.
fn run(
    geometry: Geometry,
    block: bool,
    service: Service,
    sizes: &[usize],
    sc: &Scenario,
    samples: usize,
) -> (Vec<u32>, Vec<Option<u32>>, String) {
    let (mut w, mut input, mut output) = worker(geometry);
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut outs = Vec::with_capacity(samples);
    let mut voices = Vec::with_capacity(samples);
    let (mut done, mut blocks) = (0, 0);
    let mut buffers = ([0f32; 512], [0f32; 512], [0f32; 512], [None; 512]);
    for &size in sizes.iter().cycle() {
        if done >= samples {
            break;
        }
        let n = size.min(samples - done);
        let (x, wet, out, voice) = (
            &mut buffers.0[..n],
            &mut buffers.1[..n],
            &mut buffers.2[..n],
            &mut buffers.3[..n],
        );
        for (i, (x, wet)) in x.iter_mut().zip(wet.iter_mut()).enumerate() {
            *x = rng.sample();
            if sc.nan_input_every.is_some_and(|k| (done + i) % k == 0) {
                *x = f32::NAN;
            }
            // A smoothed wet that moves every sample, as `Dsp::block` passes.
            *wet = 0.5 + 0.5 * ((done + i) as f32 * 0.001).sin();
        }
        if sc.fail_at.is_some_and(|f| (done..done + n).contains(&f)) {
            w.shared.failed.store(true, Ordering::Release);
        }
        if sc.finish_at.is_some_and(|f| (done..done + n).contains(&f)) {
            w.finish(StopMode::Cancel, Duration::ZERO).unwrap();
        }
        if block {
            block_sketch(&mut w, x, wet, out, voice);
        } else {
            for i in 0..n {
                out[i] = w.process_sample(x[i], wet[i]);
                voice[i] = w.voice;
            }
        }
        outs.extend(out.iter().map(|v| v.to_bits()));
        voices.extend(voice.iter().map(|v| v.map(f32::to_bits)));
        done += n;
        blocks += 1;
        match service {
            Service::Immediate => serve(&mut input, &mut output, sc.nan_result_at),
            Service::Every(k) if blocks % k == 0 => {
                serve(&mut input, &mut output, sc.nan_result_at)
            }
            _ => {}
        }
    }
    let s = w.telemetry().snapshot();
    let state = format!(
        "sample {} frame {} phase {} dry {} pending {:?} voice {:?} finished {} completed {} in_drops {} out_drops {} missed {} late {} stale {} failed {}",
        w.sample,
        w.frame,
        w.phase,
        w.dry_position,
        w.pending,
        w.voice.map(f32::to_bits),
        w.finished,
        s.completed_frames,
        s.input_drops,
        s.output_drops,
        s.missed_deadlines,
        s.late_results,
        s.stale_results,
        s.failed
    );
    (outs, voices, state)
}

#[test]
fn block_sketch_matches_process_sample_bit_for_bit() {
    let mut rng = Rng(99);
    let random: Vec<usize> = (0..257).map(|_| 1 + (rng.next() % 128) as usize).collect();
    let size_sets: [&[usize]; 5] = [
        &[PERIOD],
        &[1],
        &[128],
        &[112, 1, 479 % 128, 17, 128],
        &random,
    ];
    let geometries = [
        PRODUCTION,
        // A longer scheduling allowance: still seven phases, but a different
        // submission/selection spacing.
        Geometry {
            worker_lead_samples: 336,
            ..PRODUCTION
        },
        Geometry {
            worker_lead_samples: 208,
            ..PRODUCTION
        },
        // The one-hop-lookahead model with its extra intrinsic delay.
        Geometry {
            intrinsic_delay_samples: 360,
            ..PRODUCTION
        },
    ];
    let scenarios = [
        Scenario::default(),
        Scenario {
            nan_input_every: Some(97),
            ..Scenario::default()
        },
        Scenario {
            nan_result_at: Some(5),
            ..Scenario::default()
        },
        Scenario {
            fail_at: Some(4_001),
            ..Scenario::default()
        },
        Scenario {
            finish_at: Some(3_333),
            ..Scenario::default()
        },
    ];
    let services = [
        Service::Immediate,
        Service::Every(3),
        Service::Every(7),
        Service::Never,
    ];
    let mut cases = 0;
    let mut exercised = [false; 3];
    for g in geometries {
        for sizes in size_sets {
            for sc in &scenarios {
                for service in services {
                    let reference = run(g, false, service, sizes, sc, 12_000);
                    let sketch = run(g, true, service, sizes, sc, 12_000);
                    assert_eq!(reference.2, sketch.2, "state, {g:?}");
                    assert!(reference.0 == sketch.0, "output bits, {g:?}");
                    assert!(reference.1 == sketch.1, "voices, {g:?}");
                    exercised[0] |= !reference.2.contains("late 0");
                    exercised[1] |= !reference.2.contains("stale 0");
                    exercised[2] |= !reference.2.contains("in_drops 0");
                    cases += 1;
                }
            }
        }
    }
    // The comparison must have covered late, stale and dropped frames.
    assert_eq!(exercised, [true; 3]);
    assert_eq!(cases, 4 * 5 * 5 * 4);
}

/// Operation counts per sample of the production path, from the source.
const OPERATION_COUNTS: &str = "process_sample per sample: 1 finite check, dry ring read+write+wrap compare \
(latency_samples() = 2 adds), finished check, u64 % (phase) and u64 / (due) once started, pending check, \
assembling write, availability (start compare, valid, finite, 1 acquire atomic load), voice store, 3 increments, \
hop compare, blend (1 sub, 1 mul-add). Per input hop (240): clock_gettime(MONOTONIC), Frame push (1,960-byte copy), \
futex FUTEX_WAKE syscall. Per output frame: choose() = queue head peek + Frame pop (1,960-byte copy into playing). \
Late/miss: one more clock_gettime and outlier record.";

struct Stats {
    per_sample: f64,
    median_block: f64,
    p99_block: f64,
    max_block: f64,
    event_block: f64,
    quiet_block: f64,
}

/// Times 112-sample blocks; the fake model's service runs outside the timed
/// region. Input and output pass through black_box.
fn time_blocks(geometry: Geometry, block: bool, blocks: usize) -> Stats {
    let (mut w, mut input, mut output) = worker(geometry);
    let mut rng = Rng(5);
    let x: Vec<f32> = (0..PERIOD * 64).map(|_| rng.sample()).collect();
    let wet = [1f32; PERIOD];
    let mut out = [0f32; PERIOD];
    let mut voice = [None; PERIOD];
    let mut times = Vec::with_capacity(blocks);
    let mut events = Vec::with_capacity(blocks);
    let fs = geometry.frame_samples as u64;
    for b in 0..blocks + 2_000 {
        let chunk = black_box(&x[(b % 64) * PERIOD..(b % 64 + 1) * PERIOD]);
        let first = w.sample;
        let t = Instant::now();
        if block {
            block_sketch(&mut w, chunk, &wet, &mut out, &mut voice);
        } else {
            for i in 0..PERIOD {
                out[i] = w.process_sample(chunk[i], wet[i]);
                voice[i] = w.voice;
            }
        }
        black_box((&out, &voice));
        let elapsed = t.elapsed().as_nanos() as f64;
        serve(&mut input, &mut output, None);
        if b >= 2_000 {
            times.push(elapsed);
            // Does this block hold an input hop end (and so a push + futex)?
            events.push((first + PERIOD as u64) / fs != first / fs);
        }
    }
    let s = w.telemetry().snapshot();
    assert_eq!(
        (s.missed_deadlines, s.input_drops, s.output_drops),
        (0, 0, 0)
    );
    let mean = |pick: bool| {
        let v: Vec<f64> = times
            .iter()
            .zip(&events)
            .filter(|(_, e)| **e == pick)
            .map(|(t, _)| *t)
            .collect();
        v.iter().sum::<f64>() / v.len() as f64
    };
    let (event_block, quiet_block) = (mean(true), mean(false));
    let per_sample = times.iter().sum::<f64>() / (times.len() * PERIOD) as f64;
    times.sort_by(f64::total_cmp);
    Stats {
        per_sample,
        median_block: times[times.len() / 2],
        p99_block: times[times.len() * 99 / 100],
        max_block: times[times.len() - 1],
        event_block,
        quiet_block,
    }
}

fn mean_ns(calls: usize, mut f: impl FnMut()) -> f64 {
    for _ in 0..calls / 10 {
        f();
    }
    let t = Instant::now();
    for _ in 0..calls {
        f();
    }
    t.elapsed().as_nanos() as f64 / calls as f64
}

#[test]
#[ignore = "host timing; run with --release --include-ignored --nocapture"]
fn endpoint_bench() {
    println!(
        "HOST ONLY ({}), not Cortex-A53; debug_assertions {}",
        std::env::consts::ARCH,
        cfg!(debug_assertions)
    );
    println!("{OPERATION_COUNTS}");
    // The per-hop pieces on their own.
    let wake = wake::Wake::default();
    println!(
        "piece futex wake (no waiter)  {:>8.1} ns",
        mean_ns(200_000, || assert!(black_box(&wake).notify()))
    );
    println!(
        "piece clock_gettime MONOTONIC {:>8.1} ns",
        mean_ns(2_000_000, || {
            black_box(metrics::now(false));
        })
    );
    let (mut p, mut c) = queue::channel();
    let frame = Frame::default();
    println!(
        "piece Frame push+pop          {:>8.1} ns",
        mean_ns(2_000_000, || {
            assert!(p.push(black_box(&frame)));
            black_box(c.pop());
        })
    );
    for (name, g) in [
        ("single-hop 240/lead 320", PRODUCTION),
        (
            "lookahead-1 240/lead 320",
            Geometry {
                intrinsic_delay_samples: 360,
                ..PRODUCTION
            },
        ),
    ] {
        for repeat in 0..3 {
            for (path, block) in [("process_sample", false), ("block_sketch", true)] {
                let s = time_blocks(g, block, 200_000);
                println!(
                    "bench {name} run {repeat} {path:<15} {:>6.2} ns/sample  block median {:>7.1} p99 {:>7.1} max {:>9.1} ns  hop-end blocks {:>7.1} other {:>7.1}",
                    s.per_sample,
                    s.median_block,
                    s.p99_block,
                    s.max_block,
                    s.event_block,
                    s.quiet_block
                );
            }
        }
    }
}
