//! The callback endpoint against its original per-sample form, and its host
//! cost.
//!
//! The endpoint fixture has no OS worker thread. A fake model on the same
//! thread services the input queue between blocks and publishes each frame
//! immediately (scaled by 0.5, voice 0.75), so neither DFN timing nor
//! scheduling enters. Timings are host x86, NOT A53.
//!
//! `reference_sample` is `Worker::process_sample` as it was before the
//! output phase and frame became counters: it divides the sample count for
//! both at every sample. `Worker::process_sample` and `Worker::process_block`
//! must reproduce it bit for bit: outputs, voices, state and counters.
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

/// `Worker::process_sample` as first written, kept as the reference: the
/// output phase and the due frame come from a modulo and a division of the
/// sample count at every sample. It neither reads nor advances the counters
/// that replaced them, so a worker must be driven by this alone.
fn reference_sample(w: &mut Worker, input: f32, wet: f32) -> f32 {
    let input = if input.is_finite() { input } else { 0. };
    let dry = w.dry[w.dry_position];
    w.dry[w.dry_position] = input;
    w.dry_position += 1;
    if w.dry_position == w.latency_samples() {
        w.dry_position = 0;
    }
    if w.finished {
        w.voice = None;
        return dry;
    }
    let start = (w.geometry.frame_samples + w.geometry.worker_lead_samples) as u64;
    let phase = if w.sample >= start {
        ((w.sample - start) % w.geometry.frame_samples as u64) as usize
    } else {
        0
    };
    if w.sample >= start {
        let due = (w.sample - start) / w.geometry.frame_samples as u64;
        if phase == 0 {
            w.pending = None;
            if !w.choose(due) {
                w.shared.missed.fetch_add(1, Ordering::Relaxed);
                w.pending = Some(due);
                let at = metrics::now(false).unwrap_or(0);
                let event = EventRecord {
                    sequence: due,
                    at_ns: at,
                };
                w.shared.endpoint_outliers.missed(event);
                w.shared.capture.missed(event);
            }
        } else if w.pending == Some(due) && w.choose(due) {
            w.pending = None;
            w.shared.late.fetch_add(1, Ordering::Relaxed);
            let at = metrics::now(false).unwrap_or(0);
            w.shared.endpoint_outliers.late(EventRecord {
                sequence: due,
                at_ns: at,
            });
        }
    }
    w.assembling.samples[w.phase] = input;
    let available = w.sample >= start
        && w.playing.valid
        && w.playing.samples[phase].is_finite()
        && !w.shared.failed.load(Ordering::Acquire);
    let output = if available {
        w.voice = Some(w.playing.voice);
        w.playing.samples[phase]
    } else {
        w.voice = None;
        dry
    };
    w.sample += 1;
    w.phase += 1;
    if w.phase == w.geometry.frame_samples {
        w.assembling.sequence = w.frame;
        w.assembling.valid = true;
        w.assembling.submitted_ns = metrics::now(false);
        if !w.input.push(&w.assembling) {
            w.shared.input_drops.fetch_add(1, Ordering::Relaxed);
        } else if !w.shared.wake.notify() {
            w.shared.failed.store(true, Ordering::Release);
        }
        w.phase = 0;
        w.frame += 1;
    }
    dry + (output - dry) * wet
}

/// How a run drives the endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    /// `reference_sample` for every sample.
    Reference,
    /// `Worker::process_sample` for every sample.
    Sample,
    /// `Worker::process_block` for every block.
    Block,
    /// Blocks and single samples in turn, as `Dsp` may mix them.
    Mixed,
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
    /// Signed zeros, the largest and smallest magnitudes and huge values in
    /// the input.
    extremes: bool,
    /// A wet that is not a blend factor at some samples: NaN, infinities,
    /// negative zero, values outside 0..1, a subnormal.
    wild_wet: bool,
    nan_input_every: Option<usize>,
    nan_result_at: Option<u64>,
    fail_at: Option<usize>,
    finish_at: Option<usize>,
}

/// One run through a path; returns every output bit, every voice and the
/// endpoint's state and counters.
fn run(
    geometry: Geometry,
    path: Path,
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
                *x = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][(done + i) % 3];
            }
            if sc.extremes {
                *x = match (done + i) % 5 {
                    0 => -0.,
                    1 => f32::MAX.copysign(*x),
                    2 => f32::from_bits(1 + (done + i) as u32 % 1000).copysign(*x),
                    3 => *x * 1e30,
                    _ => *x,
                };
            }
            // A smoothed wet that moves every sample, as `Dsp::block` passes.
            *wet = 0.5 + 0.5 * ((done + i) as f32 * 0.001).sin();
            if sc.wild_wet {
                *wet = match (done + i) % 11 {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => f32::NEG_INFINITY,
                    3 => -0.,
                    4 => 2.5,
                    5 => -1.,
                    6 => 1e30,
                    7 => f32::from_bits(3),
                    _ => *wet,
                };
            }
        }
        if sc.fail_at.is_some_and(|f| (done..done + n).contains(&f)) {
            w.shared.failed.store(true, Ordering::Release);
        }
        if sc.finish_at.is_some_and(|f| (done..done + n).contains(&f)) {
            w.finish(StopMode::Cancel, Duration::ZERO).unwrap();
        }
        let block = match path {
            Path::Block => true,
            Path::Mixed => blocks % 2 == 0,
            _ => false,
        };
        if block {
            out.copy_from_slice(x);
            w.process_block(out, wet, voice);
        } else {
            for i in 0..n {
                out[i] = match path {
                    Path::Reference => reference_sample(&mut w, x[i], wet[i]),
                    _ => w.process_sample(x[i], wet[i]),
                };
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

/// `Worker::process_sample` (counters in place of the division) and
/// `Worker::process_block`, alone and mixed, against `reference_sample`:
/// every output bit, every voice, and the endpoint's state and counters, over
/// block sizes, geometries, service patterns (results on time, late, stale,
/// never) and input and failure scenarios.
#[test]
fn endpoint_paths_match_the_reference_bit_for_bit() {
    let mut rng = Rng(99);
    let random: Vec<usize> = (0..257).map(|_| 1 + (rng.next() % 128) as usize).collect();
    let size_sets: [&[usize]; 8] = [
        &[PERIOD],
        &[1],
        &[128],
        &[112, 1, 479 % 128, 17, 128],
        &random,
        // Blocks of one frame and longer: several output frames start and
        // several input frames complete inside one call.
        &[240],
        &[480],
        &[512, 241, 239, 1, 500],
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
            extremes: true,
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
        Scenario {
            wild_wet: true,
            nan_input_every: Some(53),
            nan_result_at: Some(9),
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
                    let reference = run(g, Path::Reference, service, sizes, sc, 12_000);
                    for path in [Path::Sample, Path::Block, Path::Mixed] {
                        let actual = run(g, path, service, sizes, sc, 12_000);
                        assert_eq!(reference.2, actual.2, "state, {path:?} {g:?}");
                        assert!(reference.0 == actual.0, "output bits, {path:?} {g:?}");
                        assert!(reference.1 == actual.1, "voices, {path:?} {g:?}");
                    }
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
    assert_eq!(cases, 4 * 8 * 7 * 4);
}

/// When the fake model publishes, as sample indices: fixed before the run,
/// whatever sizes the stream is then cut into.
#[derive(Clone, Copy, Debug)]
enum Schedule {
    /// Random gaps of 1 to `n` samples between publications.
    Gaps(u64),
    /// `k` samples after each input frame completes.
    After(usize),
    Never,
}

/// `serve` with results a worker can really produce besides good ones: a
/// voice score that names the frame, a NaN at a place that moves from frame
/// to frame, infinities at both ends, frames marked invalid (as during
/// recovery) and frames never published. A full output queue drops the
/// result, as the worker does.
fn serve_faulty(input: &mut Consumer, output: &mut Producer, frame_samples: usize) {
    while let Some(mut frame) = input.pop() {
        let seq = frame.sequence;
        for x in &mut frame.samples {
            *x *= 0.5;
        }
        frame.voice = 0.125 + (seq % 7) as f32 * 0.125;
        match seq % 13 {
            5 => frame.samples[(seq as usize * 37) % frame_samples] = f32::NAN,
            9 => {
                frame.samples[0] = f32::INFINITY;
                frame.samples[frame_samples - 1] = f32::NEG_INFINITY;
            }
            _ => {}
        }
        if seq % 17 == 3 {
            frame.valid = false;
        }
        if seq % 19 != 7 {
            let _ = output.push(&frame);
        }
    }
}

/// One run with the publications, a worker failure and a `finish` placed by
/// sample index. With a real worker a result, or a failure, can arrive in
/// the middle of a block. The endpoint looks for either only at fixed
/// samples (a result where an output frame starts and at every sample while
/// one is awaited, the failure flag at every sample), so an arrival is the
/// same thing as a publication just before the sample that first looks: a
/// call here never spans a publication, and the calls are otherwise cut by
/// `sizes` alone.
#[allow(clippy::too_many_arguments)]
fn run_scheduled(
    geometry: Geometry,
    path: Path,
    schedule: Schedule,
    sizes: &[usize],
    wild: bool,
    fail_at: Option<usize>,
    finish_at: Option<usize>,
    total: usize,
) -> (Vec<u32>, Vec<Option<u32>>, String) {
    let (mut w, mut input, mut output) = worker(geometry);
    let frame = geometry.frame_samples;
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut x: Vec<f32> = (0..total).map(|_| rng.sample()).collect();
    let mut wet: Vec<f32> = (0..total)
        .map(|i| 0.5 + 0.5 * (i as f32 * 0.001).sin())
        .collect();
    if wild {
        for i in 0..total {
            x[i] = match i % 97 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                2 => -0.,
                3 => f32::MAX.copysign(x[i]),
                4 => f32::from_bits(1 + i as u32 % 1000),
                _ => x[i],
            };
            wet[i] = match i % 11 {
                0 => f32::NAN,
                1 => f32::INFINITY,
                3 => -0.,
                4 => 2.5,
                5 => -1.,
                7 => f32::from_bits(3),
                _ => wet[i],
            };
        }
    }
    // serve_before[i]: the fake model publishes before sample i is processed.
    let mut serve_before = vec![false; total + 1];
    match schedule {
        Schedule::Gaps(n) => {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ n);
            let mut at = 1 + (rng.next() % n) as usize;
            while at <= total {
                serve_before[at] = true;
                at += 1 + (rng.next() % n) as usize;
            }
        }
        Schedule::After(k) => {
            for at in (frame + k..=total).step_by(frame) {
                serve_before[at] = true;
            }
        }
        Schedule::Never => {}
    }
    let mut cut = serve_before.clone();
    for at in [fail_at, finish_at].into_iter().flatten() {
        cut[at] = true;
    }
    let mut out = x.clone();
    let mut voice = vec![Some(-7f32); total];
    let (mut done, mut calls) = (0, 0);
    let mut sizes = sizes.iter().cycle();
    while done < total {
        if serve_before[done] {
            serve_faulty(&mut input, &mut output, frame);
        }
        if fail_at == Some(done) {
            w.shared.failed.store(true, Ordering::Release);
        }
        if finish_at == Some(done) {
            w.finish(StopMode::Cancel, Duration::ZERO).unwrap();
        }
        let mut n = (*sizes.next().unwrap()).min(total - done);
        if let Some(next) = (done + 1..done + n).find(|&i| cut[i]) {
            n = next - done;
        }
        let range = done..done + n;
        let block = match path {
            Path::Block => true,
            Path::Mixed => calls % 2 == 0,
            _ => false,
        };
        if block {
            w.process_block(
                &mut out[range.clone()],
                &wet[range.clone()],
                &mut voice[range],
            );
        } else {
            for i in range {
                out[i] = match path {
                    Path::Reference => reference_sample(&mut w, x[i], wet[i]),
                    _ => w.process_sample(x[i], wet[i]),
                };
                voice[i] = w.voice;
            }
        }
        done += n;
        calls += 1;
    }
    let s = w.telemetry().snapshot();
    let state = format!(
        "sample {} frame {} phase {} dry {} pending {:?} voice {:?} finished {} in_drops {} missed {} late {} stale {} failed {}",
        w.sample,
        w.frame,
        w.phase,
        w.dry_position,
        w.pending,
        w.voice.map(f32::to_bits),
        w.finished,
        s.input_drops,
        s.missed_deadlines,
        s.late_results,
        s.stale_results,
        s.failed
    );
    (
        out.iter().map(|v| v.to_bits()).collect(),
        voice.iter().map(|v| v.map(f32::to_bits)).collect(),
        state,
    )
}

/// The three paths against `reference_sample` with results arriving at any
/// sample, not only between blocks, and exactly around each deadline: the
/// last sample at which a result is on time, the first at which it is late,
/// the last at which a late result still plays and the first at which it is
/// stale. Over geometries whose output frames start on, next to and away
/// from the sample that completes an input frame, other frame lengths (down
/// to one sample, up to the queue's largest), the longest dry delay, and
/// blocks from one sample to several frames that never align with a frame.
#[test]
fn endpoint_paths_match_the_reference_around_every_deadline() {
    let mut rng = Rng(77);
    let random: Vec<usize> = (0..257).map(|_| 1 + (rng.next() % 600) as usize).collect();
    let size_sets: [&[usize]; 6] = [
        &[PERIOD],
        &[3],
        &[7, 113, 1, 240, 239, 241, 2, 479],
        &random,
        &[480],
        &[1024],
    ];
    let lead = |worker_lead_samples, intrinsic_delay_samples| Geometry {
        worker_lead_samples,
        intrinsic_delay_samples,
        ..PRODUCTION
    };
    let geometries = [
        PRODUCTION,
        // An output frame starts on the sample that completes an input
        // frame, on the one after it, and on the one after that.
        lead(239, 120),
        lead(240, 7),
        lead(1, 0),
        lead(113, 360),
        Geometry {
            frame_samples: 120,
            intrinsic_delay_samples: 0,
            worker_lead_samples: 56,
            ..PRODUCTION
        },
        // The queue's largest frame and the longest dry delay.
        Geometry {
            frame_samples: queue::MAX_FRAME,
            intrinsic_delay_samples: MAX_SUPPRESSION_DELAY - 2 * queue::MAX_FRAME,
            worker_lead_samples: queue::MAX_FRAME,
            ..PRODUCTION
        },
        Geometry {
            frame_samples: 2,
            intrinsic_delay_samples: 1,
            worker_lead_samples: 3,
            ..PRODUCTION
        },
        Geometry {
            frame_samples: 1,
            intrinsic_delay_samples: 0,
            worker_lead_samples: 1,
            ..PRODUCTION
        },
    ];
    // (wild input and wet, worker failure at, finish at)
    let scenarios = [
        (false, None, None),
        (true, None, None),
        (false, Some(1_777), None),
        (true, Some(2_640), Some(3_001)),
    ];
    let total = 4_000;
    let mut cases = 0;
    // Late, stale, missed, dropped input, a result awaited at the end.
    let mut exercised = [false; 5];
    for g in geometries {
        let (f, l) = (g.frame_samples, g.worker_lead_samples);
        // A result is on time when it is published at most `l` samples
        // after its input frame completed, and still plays, late, for
        // `f - 1` samples more.
        let schedules = [
            Schedule::Gaps(150),
            Schedule::Gaps(900),
            Schedule::Never,
            Schedule::After(0),
            Schedule::After(l - 1),
            Schedule::After(l),
            Schedule::After(l + 1),
            Schedule::After(l + f / 2),
            Schedule::After(l + f - 1),
            Schedule::After(l + f),
            Schedule::After(l + 3 * f + 5),
        ];
        for sizes in size_sets {
            for (wild, fail_at, finish_at) in scenarios {
                for schedule in schedules {
                    let run = |path| {
                        run_scheduled(g, path, schedule, sizes, wild, fail_at, finish_at, total)
                    };
                    let reference = run(Path::Reference);
                    for path in [Path::Sample, Path::Block, Path::Mixed] {
                        let actual = run(path);
                        let what = format!("{path:?} {g:?} {schedule:?}");
                        assert_eq!(reference.2, actual.2, "state, {what}");
                        assert!(reference.0 == actual.0, "output bits, {what}");
                        assert!(reference.1 == actual.1, "voices, {what}");
                    }
                    exercised[0] |= !reference.2.contains("late 0 ");
                    exercised[1] |= !reference.2.contains("stale 0 ");
                    exercised[2] |= !reference.2.contains("missed 0 ");
                    exercised[3] |= !reference.2.contains("in_drops 0 ");
                    exercised[4] |= !reference.2.contains("pending None");
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(exercised, [true; 5]);
    assert_eq!(cases, 9 * 6 * 4 * 11);
}

/// The counters hold the divided sample count at every sample, however the
/// endpoint is driven. `process_sample` asserts it at every sample in debug
/// builds; this checks it from outside, across blocks too, in any build.
#[test]
fn output_counters_follow_the_sample_count() {
    let mut rng = Rng(3);
    for g in [
        PRODUCTION,
        Geometry {
            worker_lead_samples: 208,
            intrinsic_delay_samples: 360,
            ..PRODUCTION
        },
    ] {
        let (mut w, mut input, mut output) = worker(g);
        let start = (g.frame_samples + g.worker_lead_samples) as u64;
        let (mut x, wet, mut voice) = ([0f32; 200], [1f32; 200], [None; 200]);
        for round in 0..400 {
            let n = 1 + (rng.next() % 200) as usize;
            x[..n].iter_mut().for_each(|v| *v = rng.sample());
            if round % 3 == 0 {
                for v in &mut x[..n] {
                    *v = w.process_sample(*v, 1.);
                }
            } else {
                w.process_block(&mut x[..n], &wet[..n], &mut voice[..n]);
            }
            let expected = match w.sample.checked_sub(start) {
                Some(since) => (
                    (since % g.frame_samples as u64) as usize,
                    since / g.frame_samples as u64,
                ),
                None => (0, 0),
            };
            assert_eq!((w.out_phase, w.due), expected, "sample {}", w.sample);
            if round % 5 != 4 {
                serve(&mut input, &mut output, None);
            }
        }
    }
}

/// A model that halves its input and reports a voice score of 0.75.
struct Halve;
impl Model for Halve {
    fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
        samples.iter_mut().for_each(|x| *x *= 0.5);
        Ok(0.75)
    }
    fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

/// `Endpoint::process_block` against `Endpoint::process_sample` and
/// `Endpoint::voice` for both kinds of endpoint: every output bit and every
/// voice, over uneven block sizes, non-finite input and a moving wet. The
/// inline endpoint runs its model inside the call that completes a frame.
#[test]
fn endpoint_blocks_match_endpoint_samples_for_both_kinds() {
    let inline = || {
        let geometry = Geometry {
            worker_lead_samples: 0,
            ..PRODUCTION
        };
        Endpoint::Inline(Inline::with_model(geometry, Box::new(Halve)).unwrap())
    };
    // The threadless worker fixture; nothing serves it, so it plays dry and
    // counts misses, which both paths must do alike.
    let threaded = || Endpoint::Worker(worker(PRODUCTION).0);
    for (kind, make) in [
        ("inline", &inline as &dyn Fn() -> Endpoint),
        ("worker", &threaded),
    ] {
        let (mut by_sample, mut by_block) = (make(), make());
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let (mut done, mut played) = (0usize, false);
        while done < 6_000 {
            let n = 1 + (rng.next() % 300) as usize;
            let x: Vec<f32> = (0..n)
                .map(|i| match (done + i) % 131 {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => -0.,
                    _ => rng.sample(),
                })
                .collect();
            let wet: Vec<f32> = (0..n)
                .map(|i| 0.5 + 0.5 * ((done + i) as f32 * 0.003).sin())
                .collect();
            let mut want = vec![(0u32, None); n];
            for i in 0..n {
                let y = by_sample.process_sample(x[i], wet[i]);
                want[i] = (y.to_bits(), by_sample.voice().map(f32::to_bits));
            }
            let mut out = x.clone();
            // Longer than the block: only the block's part is written.
            let mut voice = vec![Some(-1f32); n + 3];
            by_block.process_block(&mut out, &wet, &mut voice);
            let got: Vec<_> = out
                .iter()
                .zip(&voice)
                .map(|(y, v)| (y.to_bits(), v.map(f32::to_bits)))
                .collect();
            assert!(got == want, "{kind} at sample {done}, block of {n}");
            assert!(voice[n..].iter().all(|v| *v == Some(-1.)), "{kind}");
            assert_eq!(by_block.voice(), by_sample.voice(), "{kind}");
            played |= voice[..n].iter().any(|v| v.is_some());
            done += n;
        }
        // The inline endpoint played its model's results; the unserved
        // worker played none.
        assert_eq!(played, kind == "inline");
        // The counters agree; the inline endpoint's timings are its own.
        let counters = |e: &Endpoint| {
            let s = e.telemetry().snapshot();
            (
                s.completed_frames,
                s.input_drops,
                s.output_drops,
                s.missed_deadlines,
                s.late_results,
                s.stale_results,
                s.failed,
            )
        };
        assert_eq!(counters(&by_block), counters(&by_sample), "{kind}");
        assert!(counters(&by_block).0 > 0 || kind == "worker", "{kind}");
    }
}

/// Operation counts per sample of the production path, from the source.
const OPERATION_COUNTS: &str = "process_sample per sample: 1 finite check, dry ring read+write+wrap compare \
(latency_samples() = 2 adds), finished check, output phase and due frame counters (the reference: u64 % and u64 /), pending check, \
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
fn time_blocks(geometry: Geometry, path: Path, blocks: usize) -> Stats {
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
        match path {
            Path::Block | Path::Mixed => {
                out.copy_from_slice(chunk);
                w.process_block(&mut out, &wet, &mut voice);
            }
            _ => {
                for i in 0..PERIOD {
                    out[i] = match path {
                        Path::Reference => reference_sample(&mut w, chunk[i], wet[i]),
                        _ => w.process_sample(chunk[i], wet[i]),
                    };
                    voice[i] = w.voice;
                }
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
    // One path alone, for counting its instructions under an emulator:
    // RC2FX_ENDPOINT_PATH=reference|sample|block with RC2FX_ENDPOINT_BLOCKS;
    // the difference between two block counts is the steady cost.
    if let Ok(path) = std::env::var("RC2FX_ENDPOINT_PATH") {
        let path = match path.as_str() {
            "reference" => Path::Reference,
            "sample" => Path::Sample,
            "block" => Path::Block,
            other => panic!("RC2FX_ENDPOINT_PATH={other}"),
        };
        let blocks = std::env::var("RC2FX_ENDPOINT_BLOCKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .expect("RC2FX_ENDPOINT_BLOCKS=count");
        let s = time_blocks(PRODUCTION, path, blocks);
        println!("{path:?} {blocks} blocks {:.2} ns/sample", s.per_sample);
        return;
    }
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
            for (label, path) in [
                ("reference (divides)", Path::Reference),
                ("process_sample", Path::Sample),
                ("process_block", Path::Block),
            ] {
                let s = time_blocks(g, path, 200_000);
                println!(
                    "bench {name} run {repeat} {label:<19} {:>6.2} ns/sample  block median {:>7.1} p99 {:>7.1} max {:>9.1} ns  hop-end blocks {:>7.1} other {:>7.1}",
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
