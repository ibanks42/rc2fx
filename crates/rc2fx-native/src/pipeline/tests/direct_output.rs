//! Hardware outputs owned by the callback, which hands the mixer's own audio to
//! the DSP in place after the mixer returns, compared with the helper reading a
//! copy and writing the result one frame later (pipeline::DIRECT_OUTPUT).
//!
//! Everything here is host-only: no mixer, no real device, no timing claims.
use super::*;

/// A deterministic per-frame, per-channel sample in [-0.5, 0.5). Samples are
/// recomputed identically for both arms, so a bitwise comparison of their
/// output is meaningful.
fn sample(index: usize, channel: usize) -> f32 {
    let x = (index as u32)
        .wrapping_mul(2_654_435_761)
        .wrapping_add((channel as u32 + 1).wrapping_mul(0x9e37_79b9));
    ((x >> 9) & 0xffff) as f32 / 65_535.0 - 0.5
}
/// One mono source (in_1) and one output of `channels` channels, its
/// high-pass (stateful) and EQ input gain `gain` lin (0.5: -6.02 dB).
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
pub(super) fn build(
    plan: &Plan,
    buffers: &Buffers,
    direct_output: bool,
) -> (Pipeline, Vec<parameters::Control>) {
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
    Pipeline::with_ownership(plan, &registry, 1, direct_output).unwrap()
}
fn contents(buffers: &Buffers) -> [[f32; FRAMES]; 2] {
    [unsafe { *buffers.output[0].get() }, unsafe {
        *buffers.output[1].get()
    }]
}
/// One callback: the source request, the mixer's own processing writing its
/// output mix, then the DSP. Returns the output buffers as the mixer left them
/// (before `end`) and as the callback returns them (after `end`).
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

/// The accepted change: the same audio, bit for bit, delivered one period
/// (`FRAMES` = 112 samples) earlier.
#[test]
fn a_hardware_output_reaches_the_mixer_one_period_earlier_and_is_otherwise_bitwise_the_same() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (helper, mut helper_controls) = build(&plan, &buffers, false);
    let (direct, mut direct_controls) = build(&plan, &buffers, true);
    // A telemetry window (48 frames) so the reported latency is available.
    let frames = 48;
    let mut helper_frames = Vec::new();
    let mut direct_frames = Vec::new();
    for index in 0..frames {
        helper_frames.push(run(&helper, &buffers, index, [true, true], true));
        direct_frames.push(run(&direct, &buffers, index, [true, true], true));
    }
    // Frame k of the callback-owned output is the helper's frame k + 1.
    for k in 0..frames - 1 {
        assert_bits_same(
            &direct_frames[k].out,
            &helper_frames[k + 1].out,
            &format!("frame {k} vs helper {}", k + 1),
        );
    }
    // The helper's first frame hands back the still zero-initialised slot it
    // has to write (nothing was copied yet): an untreated delivery, not a
    // result. The callback's first frame is processed.
    assert!(silent(&helper_frames[0].out));
    assert!(!silent(&direct_frames[0].out));
    // Delivered one period earlier: the reported latency differs by FRAMES.
    let latency = |controls: &mut Vec<parameters::Control>| {
        controls[1].status().unwrap().meter.unwrap().latency_samples
    };
    assert_eq!(
        latency(&mut direct_controls) + FRAMES as u32,
        latency(&mut helper_controls)
    );
    assert_eq!(helper.counters().faults, 0);
    assert_eq!(direct.counters().faults, 0);
    assert_eq!(
        helper.counters().skipped_outputs + direct.counters().skipped_outputs,
        0
    );
}

/// The seam: the mixer's own audio is what the DSP reads, and the buffer the
/// mixer wrote is the buffer the callback rewrites, only after it returns.
#[test]
fn a_callback_owned_hardware_output_is_processed_only_after_the_mixer_returns() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers, true);
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
    // Still the mixer's audio: nothing was processed on the request path.
    assert_eq!(contents(&buffers), [[voice * 2.0; FRAMES]; 2]);
    unsafe { pipeline.end(FRAMES, true) };
    // Processed where the mixer wrote it, through the pointers it kept.
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

/// A settings revision published between callbacks is adopted by the next
/// frame, without dropping or repeating one. The callback route can do that for
/// a mode change too; the helper route always lags its own frame behind.
#[test]
fn output_revisions_are_adopted_at_a_frame_boundary_without_dropping_a_frame() {
    let mut plan = hardware_plan(2, PathMode::Processed, 0.5);
    // Memoryless, so a frame's value is readable: the mixer's mix x the gain.
    plan.paths[1].target.highpass.enabled = false;
    let buffers = Buffers::new();
    let (direct, mut direct_controls) = build(&plan, &buffers, true);
    let (helper, mut helper_controls) = build(&plan, &buffers, false);
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
    // Enough frames for the DSP's gain smoothing to settle at 0.5.
    for _ in 0..8 {
        run(&direct, &buffers, 0, [true, true], true);
        run(&helper, &buffers, 0, [true, true], true);
    }
    let frame = run(&direct, &buffers, 0, [true, true], true);
    near(&frame.out, &frame.raw, 0.5, "direct before");
    let frame = run(&helper, &buffers, 0, [true, true], true);
    near(&frame.out, &frame.raw, 0.5, "helper before");
    let blocks = (
        direct.counters().output_blocks,
        helper.counters().output_blocks,
    );
    // One property revision, both routes: gain 0.5 -> 2.0.
    let mut revised = plan.paths[1].target.clone();
    revised.eq.input_gain_db = 20.0 * 2.0_f64.log10();
    direct_controls[1].publish(&revised, 2).unwrap();
    helper_controls[1].publish(&revised, 2).unwrap();
    // The DSP smooths a gain change over thousands of samples, so allow it to
    // settle before reading the new value.
    const SETTLE: u64 = 96;
    for _ in 0..SETTLE {
        run(&direct, &buffers, 0, [true, true], true);
        run(&helper, &buffers, 0, [true, true], true);
    }
    for (name, controls) in [
        ("direct", &mut direct_controls),
        ("helper", &mut helper_controls),
    ] {
        let status = controls[1].status().unwrap();
        assert_eq!(
            (status.published, status.seen, status.applied),
            (2, 2, 2),
            "{name}"
        );
    }
    // Exactly one processed block per frame, in both routes: nothing dropped.
    assert_eq!(direct.counters().output_blocks, blocks.0 + SETTLE);
    assert_eq!(helper.counters().output_blocks, blocks.1 + SETTLE);
    let frame = run(&direct, &buffers, 0, [true, true], true);
    near(&frame.out, &frame.raw, 2.0, "direct after");
    // A mode change at a frame boundary: the mixer's own audio, untouched, from
    // the very next frame.
    let staged = direct_controls[1]
        .prepare_mode(&revised, 3, PathMode::Passthrough)
        .unwrap();
    direct_controls[1].reserve(staged).unwrap().commit();
    let blocks = direct.counters().output_blocks;
    let frame = run(&direct, &buffers, 0, [true, true], true);
    assert_bits_same(&frame.out, &frame.raw, "direct: passthrough at once");
    assert_eq!(direct.counters().output_blocks, blocks, "nothing processed");
    // The helper route: the frame after the switch is still the result it
    // already had in hand, and only the one after that is untouched.
    let staged = helper_controls[1]
        .prepare_mode(&revised, 3, PathMode::Passthrough)
        .unwrap();
    helper_controls[1].reserve(staged).unwrap().commit();
    let blocks = helper.counters().output_blocks;
    let lagging = run(&helper, &buffers, 0, [true, true], true);
    near(&lagging.out, &lagging.raw, 2.0, "helper: one frame of lag");
    let frame = run(&helper, &buffers, 0, [true, true], true);
    assert_bits_same(&frame.out, &frame.raw, "helper: passthrough one frame late");
    assert_eq!(helper.counters().output_blocks, blocks);
    assert_eq!(direct.counters().faults, 0);
    assert_eq!(helper.counters().faults, 0);
}

/// Meter-only hardware outputs move to the callback too (ownership is fixed at
/// construction): the same meter, the same untouched audio, and no helper work
/// for the path at all (see `every_reader_is_owned_by_exactly_one_side`).
#[test]
fn meter_only_hardware_outputs_keep_their_meter_and_leave_the_helper_free() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (helper, mut helper_controls) = build(&plan, &buffers, false);
    let (direct, mut direct_controls) = build(&plan, &buffers, true);
    // Enough frames to cover the meter stride (8) and one timing sample (17),
    // ending just after a visit: a callback-owned path left alone publishes a
    // finished window at its next visit, at most METER_STRIDE - 1 cycles late.
    for index in 0..72 {
        let helper_frame = run(&helper, &buffers, index, [true, true], true);
        let direct_frame = run(&direct, &buffers, index, [true, true], true);
        // Meter-only: the mixer's own audio, in place, for both.
        assert_bits_same(&helper_frame.out, &direct_frame.out, "meter-only");
        assert_bits_same(&direct_frame.out, &direct_frame.raw, "meter-only untouched");
        let _ = index;
    }
    let meter =
        |controls: &mut Vec<parameters::Control>| controls[1].status().unwrap().meter.unwrap();
    let (helper_meter, direct_meter) = (meter(&mut helper_controls), meter(&mut direct_controls));
    assert_eq!(
        helper_meter.input_peak.to_bits(),
        direct_meter.input_peak.to_bits()
    );
    assert_eq!(
        helper_meter.output_peak.to_bits(),
        direct_meter.output_peak.to_bits()
    );
    assert_eq!(helper_meter.rms.to_bits(), direct_meter.rms.to_bits());
    assert_eq!(helper_meter.clipped, direct_meter.clipped);
    assert_eq!(
        helper_meter.latency_samples, direct_meter.latency_samples,
        "a meter-only path reports no delivery delay in either route"
    );
    assert!(helper_meter.input_peak > 0.0);
    assert_eq!(direct.counters().output_blocks, 0, "nothing processed");
    assert_eq!(helper.counters().faults + direct.counters().faults, 0);
}

/// One owner per reader, never two: the helper cannot hold the same reader the
/// callback processes with, so a frame is never processed twice.
#[test]
fn every_reader_is_owned_by_exactly_one_side() {
    for direct_output in [false, true] {
        let plan = hardware_plan(2, PathMode::Processed, 0.5);
        let buffers = Buffers::new();
        let (pipeline, _controls) = build(&plan, &buffers, direct_output);
        // SAFETY: construction only; no callback has run.
        let front = unsafe { &*pipeline.front.get() };
        let handoff = unsafe { &*pipeline.handoff.get() };
        for (index, port) in front.ports.iter().enumerate() {
            let work = &handoff.work[index];
            assert_ne!(
                port.reader.is_some(),
                work.is_some(),
                "path {index}: direct_output {direct_output}"
            );
        }
        let output = &front.ports[1];
        if direct_output {
            assert_eq!(output.owner, Owner::Callback);
            assert!(handoff.work[1].is_none(), "no helper work at all");
        } else {
            assert_eq!(output.owner, Owner::Helper);
        }
        // A helper-owned hardware output that is not the Chat mix has no
        // chat capture work to do; a callback-owned one only ever uses it
        // when the reader needs a stem.
        assert!(!output.pins.is_empty());
    }
}

/// The build's own constructor, not just the test one.
#[test]
fn the_production_constructor_processes_hardware_outputs_in_the_callback() {
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

/// A mono hardware output (one port) is processed in place like a stereo one.
#[test]
fn a_mono_hardware_output_is_processed_in_place() {
    let plan = hardware_plan(1, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (helper, _) = build(&plan, &buffers, false);
    let (direct, _) = build(&plan, &buffers, true);
    let mut helper_frames = Vec::new();
    let mut direct_frames = Vec::new();
    for index in 0..6 {
        helper_frames.push(run(&helper, &buffers, index, [true, false], true));
        direct_frames.push(run(&direct, &buffers, index, [true, false], true));
    }
    for k in 0..5 {
        assert_eq!(
            direct_frames[k].out[0]
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            helper_frames[k + 1].out[0]
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            "frame {k}"
        );
    }
    assert_eq!(direct.counters().faults, 0);
    assert_eq!(helper.counters().faults, 0);
}

/// A frame where the mixer asked for only one of two output channels is neither
/// processed nor rewritten, in either route.
#[test]
fn a_stereo_hardware_output_never_processes_one_observed_channel() {
    for direct_output in [false, true] {
        let plan = hardware_plan(2, PathMode::Processed, 0.5);
        let buffers = Buffers::new();
        let (pipeline, _) = build(&plan, &buffers, direct_output);
        unsafe {
            (*buffers.output[0].get()).fill(0.7);
            (*buffers.output[1].get()).fill(-0.4);
            pipeline.begin(FRAMES, test_buffer, 0);
        }
        let left = buffers.output(0);
        pipeline.buffer(left as usize, FRAMES, left.cast());
        unsafe { pipeline.end(FRAMES, true) };
        pipeline.settle();
        assert_eq!(
            unsafe { &*buffers.output[0].get() },
            &[0.7; FRAMES],
            "direct_output {direct_output}"
        );
        assert_eq!(unsafe { &*buffers.output[1].get() }, &[-0.4; FRAMES]);
        assert_eq!(pipeline.counters().skipped_outputs, 1);
        assert_eq!(pipeline.counters().faults, 0);
    }
}

/// A Chat-voice target marked as a hardware output is not admissible (Plan
/// validation requires the Chat mix to be USB-visible). Even constructed
/// directly, neither route processes it without its captured stem: the callback
/// route asks for the stem and faults, the helper route faults in the DSP.
#[test]
fn a_hardware_chat_voice_output_faults_in_both_ownerships_instead_of_processing_without_its_stem() {
    let mut plan = hardware_plan(2, PathMode::Processed, 0.5);
    let mut chat = Target::new("send-chat", "Chat", TargetKind::Output, 2);
    chat.limiter.enabled = false;
    chat.gain.enabled = false;
    chat.suppression.enabled = true;
    plan.paths[1].target = chat;
    let buffers = Buffers::new();
    let (helper, _) = build(&plan, &buffers, false);
    let (direct, _) = build(&plan, &buffers, true);
    let mut helper_frames = Vec::new();
    let mut direct_frames = Vec::new();
    for index in 0..4 {
        helper_frames.push(run(&helper, &buffers, index, [true, true], true));
        direct_frames.push(run(&direct, &buffers, index, [true, true], true));
    }
    // Both fault, neither writes processed audio: the mixer's mix stays as it
    // is (the callback route), or is replaced by the mix of the frame before
    // (the helper route, which always hands back a copied frame).
    assert_eq!(
        direct.counters().faults,
        1,
        "the callback's port latches its fault once"
    );
    assert_eq!(
        helper.counters().faults,
        4,
        "the helper's DSP refuses every copied frame"
    );
    assert_bits_same(
        &direct_frames[3].out,
        &direct_frames[3].raw,
        "callback route leaves the Chat mix alone",
    );
    assert_bits_same(
        &helper_frames[3].out,
        &helper_frames[2].raw,
        "helper route hands back the copied frame, unprocessed",
    );
}

fn clock_reads() -> u64 {
    crate::timing::CLOCK_READS.with(|reads| reads.get()[0])
}
/// One callback through `begin_at`, so that the test itself reads no clock.
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

/// The bench gauges only measure. With them on, every output sample, counter
/// and meter is the same bit for bit, in both ownerships, through settings and
/// mode changes, failed callbacks and unrequested channels.
#[test]
fn the_bench_gauges_change_no_output_sample_counter_or_meter() {
    for direct_output in [true, false] {
        let plan = hardware_plan(2, PathMode::Processed, 0.5);
        let buffers = Buffers::new();
        let (plain, mut plain_controls) = build(&plan, &buffers, direct_output);
        let (gauged, mut gauged_controls) = build(&plan, &buffers, direct_output);
        plain.set_gauged(false);
        gauged.set_gauged(true);
        let costs = || {
            PATH_COST_NS
                .iter()
                .map(|cost| cost.load(Ordering::Relaxed))
                .sum::<u64>()
        };
        let wakes = || {
            [&WAKE_CALL, &WAKE_LATENCY, &END, &PLACE]
                .map(|gauge| gauge.calls.load(Ordering::Relaxed))
        };
        let (costs_before, wakes_before) = (costs(), wakes());
        let frames = 240;
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
            let a = run(&plain, &buffers, index, ask, success);
            let b = run(&gauged, &buffers, index, ask, success);
            assert_bits_same(&a.raw, &b.raw, &format!("frame {index}: the mixer's audio"));
            assert_bits_same(&a.out, &b.out, &format!("frame {index}: output"));
        }
        let (a, b) = (plain.counters(), gauged.counters());
        assert_eq!(
            (a.input_blocks, a.output_blocks, a.faults, a.skipped_outputs),
            (b.input_blocks, b.output_blocks, b.faults, b.skipped_outputs)
        );
        assert_eq!(a.sources, b.sources);
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
        // The gauged pipeline recorded; the plain one never stamped a wake.
        assert!(costs() > costs_before);
        for (after, before) in wakes().into_iter().zip(wakes_before) {
            assert!(after >= before + frames as u64, "{after} vs {before}");
        }
        assert_eq!(plain.helper.shared.queued_at.load(Ordering::Relaxed), 0);
        assert_ne!(gauged.helper.shared.queued_at.load(Ordering::Relaxed), 0);
    }
}

/// Without the gauges a callback's begin and end read no clock at all: not
/// for the path costs, the buffer resolve, the helper margin or the wake. With
/// them, each of those is stamped.
#[test]
fn without_the_gauges_the_pipeline_reads_no_clock_in_the_callback() {
    let plan = hardware_plan(2, PathMode::Processed, 0.5);
    let buffers = Buffers::new();
    let (plain, _plain_controls) = build(&plan, &buffers, true);
    let (gauged, _gauged_controls) = build(&plan, &buffers, true);
    plain.set_gauged(false);
    gauged.set_gauged(true);
    // Fewer callbacks than the first placement sample (64), which stamps its
    // record when the placement is new.
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
    // Per callback: the resolve (2), each visited path (2 each, the source
    // and the output every cycle), the end, the placement and the wake (4).
    assert_eq!(clock_reads() - before, frames as u64 * (2 + 2 * 2 + 4));
    // The margin is taken from the callback's start stamp, which was 1 here:
    // the helper finished long after it, so the margin reads zero.
    assert_eq!(HELPER_MIN_MARGIN_NS.load(Ordering::Relaxed), 0);
}
