//! Host-only tests for callback-owned hardware outputs (`Owner::Callback`),
//! where DSP runs in place after the mixer callback returns.
use super::*;

/// Deterministic per-frame, per-channel sample in [-0.5, 0.5) so bitwise
/// comparisons across pipelines stay meaningful.
fn sample(index: usize, channel: usize) -> f32 {
    let x = (index as u32)
        .wrapping_mul(2_654_435_761)
        .wrapping_add((channel as u32 + 1).wrapping_mul(0x9e37_79b9));
    ((x >> 9) & 0xffff) as f32 / 65_535.0 - 0.5
}
/// Mono source (`in_1`) plus a hardware output with stateful high-pass and
/// linear EQ input gain `gain` (e.g. 0.5 = -6.02 dB).
pub(super) fn hardware_plan(channels: u8, mode: PathMode, gain: f64) -> Plan {
    let mut source = Target::new("source", "source", TargetKind::Mic, 1);
    source.limiter.enabled = false;
    source.gain.enabled = false;
    source.eq.enabled = true;
    source.eq.input_gain_db = 20.0 * 0.5_f64.log10();
    let mut output = Target::new("headphones", "headphones", TargetKind::Output, channels);
    output.limiter.enabled = false;
    output.gain.enabled = false;
    output.eq.enabled = true;
    output.eq.input_gain_db = 20.0 * gain.log10();
    output.highpass.enabled = true;
    output.highpass.frequency = 120.0;
    Plan {
        version: 1,
        sample_rate: 48_000,
        frames: FRAMES,
        worker: Some(WorkerPlan {
            cpu_mask: None,
            realtime_priority: None,
            nice: None,
        }),
        paths: vec![
            PathPlan {
                direction: Direction::Input,
                ports: vec!["in_1".into()],
                target: source,
                mode: PathMode::Processed,
                hardware: false,
            },
            PathPlan {
                direction: Direction::Output,
                ports: (1..=channels).map(|c| format!("out_{c}")).collect(),
                target: output,
                mode,
                hardware: true,
            },
        ],
    }
}
pub(super) fn build(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
    let mut registry = Registry::default();
    for channel in 0..2 {
        assert!(registry.register(
            1,
            buffers.input(channel) as usize,
            format!("in_{}", channel + 1).as_bytes(),
            1
        ));
        assert!(registry.register(
            1,
            buffers.output(channel) as usize,
            format!("out_{}", channel + 1).as_bytes(),
            2
        ));
    }
    Pipeline::new(plan, &registry, 1).unwrap()
}
fn contents(buffers: &Buffers) -> [[f32; FRAMES]; 2] {
    [unsafe { *buffers.output[0].get() }, unsafe {
        *buffers.output[1].get()
    }]
}
/// Output buffers captured before (`raw`, as written by the mixer) and after
/// (`out`, post-DSP) `pipeline.end()`.
pub(super) struct Frame {
    pub(super) raw: [[f32; FRAMES]; 2],
    pub(super) out: [[f32; FRAMES]; 2],
}
pub(super) fn run(
    pipeline: &Pipeline,
    buffers: &Buffers,
    index: usize,
    ask: [bool; 2],
    success: bool,
) -> Frame {
    unsafe {
        (*buffers.input[0].get()).fill(sample(index, 0));
        (*buffers.input[1].get()).fill(sample(index, 1));
        pipeline.begin(FRAMES, test_buffer, 0);
    }
    let raw = buffers.input(0);
    let source = pipeline
        .buffer(raw as usize, FRAMES, raw.cast())
        .cast::<f32>();
    let voice = unsafe { *source };
    let mix = [voice * 2.0, voice * -1.5];
    for (channel, asked) in ask.into_iter().enumerate() {
        if !asked {
            continue;
        }
        let raw = buffers.output(channel);
        let written = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        let values = unsafe { std::slice::from_raw_parts_mut(written, FRAMES) };
        values.fill(mix[channel]);
    }
    let raw = contents(buffers);
    unsafe { pipeline.end(FRAMES, success) };
    pipeline.settle();
    Frame {
        raw,
        out: contents(buffers),
    }
}
fn assert_bits_same(a: &[[f32; FRAMES]; 2], b: &[[f32; FRAMES]; 2], what: &str) {
    for channel in 0..2 {
        assert_eq!(
            a[channel].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b[channel].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "{what}, channel {channel}"
        );
    }
}
fn silent(frame: &[[f32; FRAMES]; 2]) -> bool {
    frame.iter().flatten().all(|value| value.to_bits() == 0)
}

/// Verify the mixer's own output buffer is returned on request and only
/// processed in place once `end()` runs.
#[test]
fn a_callback_owned_hardware_output_is_processed_only_after_the_mixer_returns() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers);
    for index in 0..4 {
        run(&pipeline, &buffers, index, [true, true], true);
    }
    let index = 4;
    unsafe {
        (*buffers.input[0].get()).fill(sample(index, 0));
        pipeline.begin(FRAMES, test_buffer, 0);
    }
    let raw = buffers.input(0);
    let source = pipeline
        .buffer(raw as usize, FRAMES, raw.cast())
        .cast::<f32>();
    let voice = unsafe { *source };
    let mut written = Vec::new();
    for channel in 0..2 {
        let raw = buffers.output(channel);
        let output = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        assert_eq!(output as usize, raw as usize, "in place: same buffer");
        unsafe { std::slice::from_raw_parts_mut(output, FRAMES) }.fill(voice * 2.0);
        written.push(output);
    }
    // Untouched until `end()` runs.
    assert_eq!(contents(&buffers), [[voice * 2.0; FRAMES]; 2]);
    unsafe { pipeline.end(FRAMES, true) };
    // Processed in place through the same pointers the mixer wrote to.
    for (channel, &output) in written.iter().enumerate() {
        let values = unsafe { std::slice::from_raw_parts(output, FRAMES) };
        assert!(
            values.iter().all(|value| *value != voice * 2.0),
            "channel {channel}"
        );
        assert_eq!(values, &contents(&buffers)[channel]);
    }
    pipeline.settle();
}

/// Settings and mode revisions published between callbacks take effect on the
/// next frame without dropping or repeating blocks.
#[test]
fn output_revisions_are_adopted_at_a_frame_boundary_without_dropping_a_frame() {
    let mut plan = hardware_plan(2, PathMode::Processed, 0.5);
    // Keep the path memoryless so output values are just raw * gain.
    plan.paths[1].target.highpass.enabled = false;
    let buffers = Buffers::new();
    let (direct, mut direct_controls) = build(&plan, &buffers);
    let near = |frame: &[[f32; FRAMES]; 2], raw: &[[f32; FRAMES]; 2], gain: f64, what: &str| {
        for channel in 0..2 {
            let expected = f64::from(raw[channel][0]) * gain;
            for value in &frame[channel] {
                assert!(
                    (f64::from(*value) - expected).abs() < 1e-6,
                    "{what}: channel {channel}: {value} vs {expected}"
                );
            }
        }
    };
    // Let gain smoothing settle at 0.5.
    for _ in 0..8 {
        run(&direct, &buffers, 0, [true, true], true);
    }
    let frame = run(&direct, &buffers, 0, [true, true], true);
    near(&frame.out, &frame.raw, 0.5, "before");
    let blocks = direct.counters().output_blocks;
    // Bump gain from 0.5 to 2.0 and give smoothing time to settle.
    let mut revised = plan.paths[1].target.clone();
    revised.eq.input_gain_db = 20.0 * 2.0_f64.log10();
    direct_controls[1].publish(&revised, 2).unwrap();
    const SETTLE: u64 = 96;
    for _ in 0..SETTLE {
        run(&direct, &buffers, 0, [true, true], true);
    }
    let status = direct_controls[1].status().unwrap();
    assert_eq!((status.published, status.seen, status.applied), (2, 2, 2));
    assert_eq!(direct.counters().output_blocks, blocks + SETTLE);
    let frame = run(&direct, &buffers, 0, [true, true], true);
    near(&frame.out, &frame.raw, 2.0, "after");
    // Switch to passthrough; the very next frame should pass untouched.
    let staged = direct_controls[1]
        .prepare_mode(&revised, 3, PathMode::Passthrough)
        .unwrap();
    direct_controls[1].reserve(staged).unwrap().commit();
    let blocks = direct.counters().output_blocks;
    let frame = run(&direct, &buffers, 0, [true, true], true);
    assert_bits_same(&frame.out, &frame.raw, "passthrough at once");
    assert_eq!(direct.counters().output_blocks, blocks, "nothing processed");
    assert_eq!(direct.counters().faults, 0);
}

/// Meter-only hardware outputs stay callback-owned (ownership is fixed at
/// construction): audio passes untouched, metering still runs, and the helper
/// does no work for them.
#[test]
fn meter_only_hardware_outputs_keep_their_meter_and_leave_the_helper_free() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (direct, mut direct_controls) = build(&plan, &buffers);
    // Run long enough to cover METER_STRIDE (8) and one timing sample (17),
    // ending right after a visit so the completed meter window is published.
    for index in 0..72 {
        let frame = run(&direct, &buffers, index, [true, true], true);
        assert_bits_same(&frame.out, &frame.raw, "meter-only untouched");
    }
    let meter = direct_controls[1].status().unwrap().meter.unwrap();
    assert!(meter.input_peak > 0.0);
    assert_eq!(direct.counters().output_blocks, 0, "nothing processed");
    assert_eq!(direct.counters().faults, 0);
}

/// Each path's reader must be owned by either the callback or the helper,
/// never both.
#[test]
fn every_reader_is_owned_by_exactly_one_side() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers);
    // SAFETY: construction only; no callback has run.
    let front = unsafe { &*pipeline.front.get() };
    let handoff = unsafe { &*pipeline.handoff.get() };
    for (index, port) in front.ports.iter().enumerate() {
        let work = &handoff.work[index];
        assert_ne!(port.reader.is_some(), work.is_some(), "path {index}");
    }
    assert_eq!(front.ports[0].owner, Owner::Helper);
    let output = &front.ports[1];
    assert_eq!(output.owner, Owner::Callback);
    assert!(handoff.work[1].is_none(), "no helper work at all");
    assert!(output.chat.is_none());
}

/// Frame 0 is processed regardless of port registration order.
#[test]
fn a_hardware_output_is_processed_from_the_first_frame() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let mut registry = Registry::default();
    for channel in 0..2 {
        assert!(registry.register(
            1,
            buffers.output(channel) as usize,
            format!("out_{}", channel + 1).as_bytes(),
            2
        ));
    }
    assert!(registry.register(1, buffers.input(0) as usize, b"in_1", 1));
    let (pipeline, _controls) = Pipeline::new(&plan, &registry, 1).unwrap();
    let frame = run(&pipeline, &buffers, 0, [true, true], true);
    assert!(!silent(&frame.out), "processed on frame 0");
}

#[test]
fn a_mono_hardware_output_is_processed_in_place() {
    let plan = hardware_plan(1, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (direct, _) = build(&plan, &buffers);
    for index in 0..6 {
        let frame = run(&direct, &buffers, index, [true, false], true);
        assert_ne!(frame.out[0], frame.raw[0], "frame {index}: processed");
        assert_eq!(frame.out[1], frame.raw[1], "frame {index}: not its buffer");
    }
    assert_eq!(direct.counters().output_blocks, 6);
    assert_eq!(direct.counters().faults, 0);
}

/// Plan validation normally rejects a Chat-voice target marked as a hardware
/// output (the Chat mix must be USB-visible). Even when constructed directly,
/// the callback faults instead of running suppression without its captured stem.
#[test]
fn a_hardware_chat_voice_output_faults_instead_of_processing_without_its_stem() {
    let mut plan = hardware_plan(2, PathMode::Processed, 0.5);
    let mut chat = Target::new("send-chat", "Chat", TargetKind::Output, 2);
    chat.limiter.enabled = false;
    chat.gain.enabled = false;
    chat.suppression.enabled = true;
    plan.paths[1].target = chat;
    let buffers = Buffers::new();
    let (direct, _) = build(&plan, &buffers);
    let mut direct_frames = Vec::new();
    for index in 0..4 {
        direct_frames.push(run(&direct, &buffers, index, [true, true], true));
    }
    assert_eq!(
        direct.counters().faults,
        1,
        "the callback's port latches its fault once"
    );
    assert_bits_same(
        &direct_frames[3].out,
        &direct_frames[3].raw,
        "the Chat mix is left alone",
    );
}

fn clock_reads() -> u64 {
    crate::timing::CLOCK_READS.with(|reads| reads.get()[0])
}
/// Runs a callback via `begin_at` so the test harness itself doesn't read the clock.
fn run_at(pipeline: &Pipeline, buffers: &Buffers, index: usize) -> [[f32; FRAMES]; 2] {
    unsafe {
        (*buffers.input[0].get()).fill(sample(index, 0));
        pipeline.begin_at(FRAMES, test_buffer, 0, 1);
    }
    let raw = buffers.input(0);
    let source = pipeline
        .buffer(raw as usize, FRAMES, raw.cast())
        .cast::<f32>();
    let voice = unsafe { *source };
    for (channel, gain) in [2.0, -1.5].into_iter().enumerate() {
        let raw = buffers.output(channel);
        let written = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        unsafe { std::slice::from_raw_parts_mut(written, FRAMES) }.fill(voice * gain);
    }
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    contents(buffers)
}

/// Enabling bench gauges must not alter any output sample, counter, or meter
/// across settings/mode changes, failed callbacks, and skipped channels.
#[test]
fn the_bench_gauges_change_no_output_sample_counter_or_meter() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (plain, mut plain_controls) = build(&plan, &buffers);
    let (gauged, mut gauged_controls) = build(&plan, &buffers);
    plain.set_gauged(false);
    gauged.set_gauged(true);
    let costs = || {
        PATH_COST_NS
            .iter()
            .map(|cost| cost.load(Ordering::Relaxed))
            .sum::<u64>()
    };
    let wakes = || {
        [&WAKE_CALL, &WAKE_LATENCY, &END, &PLACE].map(|gauge| gauge.calls.load(Ordering::Relaxed))
    };
    let (costs_before, wakes_before) = (costs(), wakes());
    let frames = 240;
    let mut sources = [stats::here::SourceCounts::default(); 2];
    for index in 0..frames {
        let mut revised = plan.paths[1].target.clone();
        match index {
            60 => {
                revised.eq.input_gain_db = 6.0;
                plain_controls[1].publish(&revised, 2).unwrap();
                gauged_controls[1].publish(&revised, 2).unwrap();
            }
            120 | 170 => {
                let (revision, mode) = if index == 120 {
                    (3, PathMode::Passthrough)
                } else {
                    (4, PathMode::Processed)
                };
                for controls in [&mut plain_controls, &mut gauged_controls] {
                    let staged = controls[1].prepare_mode(&revised, revision, mode).unwrap();
                    controls[1].reserve(staged).unwrap().commit();
                }
            }
            _ => {}
        }
        let ask = [index % 11 != 3, index % 17 != 5];
        let success = index % 13 != 7;
        let start = stats::here::sources();
        let a = run(&plain, &buffers, index, ask, success);
        let between = stats::here::sources();
        let b = run(&gauged, &buffers, index, ask, success);
        sources[0] = sources[0].add(between.since(start));
        sources[1] = sources[1].add(stats::here::sources().since(between));
        assert_bits_same(&a.raw, &b.raw, &format!("frame {index}: the mixer's audio"));
        assert_bits_same(&a.out, &b.out, &format!("frame {index}: output"));
    }
    let (a, b) = (plain.counters(), gauged.counters());
    assert_eq!(
        (a.input_blocks, a.output_blocks, a.faults, a.skipped_outputs),
        (b.input_blocks, b.output_blocks, b.faults, b.skipped_outputs)
    );
    assert_eq!(sources[0], sources[1]);
    assert!(a.output_blocks > 100 && a.skipped_outputs > 0);
    for path in 0..2 {
        let meter = |controls: &mut Vec<parameters::Control>| {
            let value = controls[path].status().unwrap().meter.unwrap();
            (
                value.callbacks,
                value.processed_frames,
                value.input_peak.to_bits(),
                value.output_peak.to_bits(),
                value.rms.to_bits(),
                value.clipped,
                value.latency_samples,
            )
        };
        assert_eq!(
            meter(&mut plain_controls),
            meter(&mut gauged_controls),
            "path {path}"
        );
    }
    assert!(costs() > costs_before);
    for (after, before) in wakes().into_iter().zip(wakes_before) {
        assert!(after >= before + frames as u64, "{after} vs {before}");
    }
    assert_eq!(plain.helper.shared.queued_at.load(Ordering::Relaxed), 0);
    assert_ne!(gauged.helper.shared.queued_at.load(Ordering::Relaxed), 0);
}

/// With gauges off, `begin_at` and `end` must not read the clock at all.
#[test]
fn without_the_gauges_the_pipeline_reads_no_clock_in_the_callback() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (plain, _plain_controls) = build(&plan, &buffers);
    let (gauged, _gauged_controls) = build(&plan, &buffers);
    plain.set_gauged(false);
    gauged.set_gauged(true);
    // Stay under the first placement sample (cycle 64), which timestamps new placements.
    let frames = 48;
    let before = clock_reads();
    let mut plain_out = Vec::new();
    for index in 0..frames {
        plain_out.push(run_at(&plain, &buffers, index));
    }
    assert_eq!(clock_reads(), before, "no clock read with the gauges off");
    let before = clock_reads();
    for (index, expected) in plain_out.iter().enumerate() {
        let out = run_at(&gauged, &buffers, index);
        assert_bits_same(&out, expected, &format!("frame {index}"));
    }
    // Per callback with gauges on: resolve (2) + 2 visited paths (2 each) + end/place/wake (4).
    assert_eq!(clock_reads() - before, frames as u64 * (2 + 2 * 2 + 4));
    // Margin is relative to the callback start stamp (1 here), so it saturates at 0.
    assert_eq!(HELPER_MIN_MARGIN_NS.load(Ordering::Relaxed), 0);
}
