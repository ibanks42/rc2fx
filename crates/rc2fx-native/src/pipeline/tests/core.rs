use super::*;
use super::{helper::*, lookup::*, placement::*, plan::*, stats::*};
use rc2fx_core::{Target, TargetKind};
use rc2fx_dsp::Dsp;
use rc2fx_native_control::MAX_PATHS;
use std::{cell::UnsafeCell, collections::HashSet, sync::atomic::AtomicBool};
#[path = "direct_output.rs"]
mod direct_output;
#[path = "helper_wake.rs"]
mod helper_wake;
#[path = "passthrough_invariance.rs"]
mod passthrough_invariance;
#[path = "processing_counters.rs"]
mod processing_counters;
#[path = "skipped_paths.rs"]
mod skipped_paths;

/// The unprocessed-path meter as it was: one flattened iterator.
fn flattened_meter(buffers: [&[f32]; 2]) -> rc2fx_dsp::Meter {
    let mut meter = rc2fx_dsp::Meter::default();
    for sample in buffers.into_iter().flatten().copied() {
        let value = f64::from(sample);
        if value.is_finite() {
            meter.input_peak = meter.input_peak.max(value.abs());
            meter.sum_squares += value * value;
            meter.clipped += u64::from(value.abs() >= 1.0);
        }
        meter.samples += 1;
    }
    meter.output_peak = meter.input_peak;
    meter
}
fn meter_bits(meter: rc2fx_dsp::Meter) -> [u64; 8] {
    [
        meter.input_peak.to_bits(),
        meter.output_peak.to_bits(),
        meter.sum_squares.to_bits(),
        meter.gain_reduction.to_bits(),
        meter.voice.to_bits(),
        meter.gate_open.to_bits(),
        meter.samples,
        meter.clipped,
    ]
}
/// The meter shown for unprocessed paths is the same value bit for bit:
/// over random samples of every scale, NaNs, infinities, denormals,
/// signed zeros, full scale and just beside it, huge values and silence,
/// mono and stereo, and every length up to a block.
#[test]
fn the_unprocessed_meter_is_bit_identical_to_the_flattened_loop() {
    let special = [
        0.0f32,
        -0.0,
        f32::NAN,
        -f32::NAN,
        f32::from_bits(0x7fa0_0001),
        f32::from_bits(0xffc0_1234),
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        f32::from_bits(1),
        f32::from_bits(0x8000_0001),
        f32::from_bits(0x007f_ffff),
        1.0,
        -1.0,
        f32::from_bits(1.0f32.to_bits() - 1),
        f32::from_bits(1.0f32.to_bits() + 1),
        -f32::from_bits(1.0f32.to_bits() - 1),
        f32::MAX,
        f32::MIN,
        1.0e30,
        -3.0e38,
        0.5,
        -0.25,
    ];
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut compared = 0;
    for round in 0..6000 {
        let frames = match round % 5 {
            0 => FRAMES,
            1 => (random() % (FRAMES as u64 + 1)) as usize,
            2 => round % 4,
            _ => FRAMES,
        };
        let mut block = [[0.0f32; FRAMES]; 2];
        for channel in &mut block {
            for sample in channel.iter_mut() {
                let bits = random();
                *sample = match (round / 5) % 6 {
                    // Every bit pattern: NaNs, infinities and denormals.
                    0 => f32::from_bits(bits as u32),
                    // Program-level audio.
                    1 => (bits >> 40) as f32 / (1u64 << 23) as f32 - 1.0,
                    // Mostly special values.
                    2 => special[(bits % special.len() as u64) as usize],
                    // Audio with the odd special value in it.
                    3 if bits % 9 == 0 => special[(bits >> 8) as usize % special.len()],
                    3 => (bits >> 40) as f32 / (1u64 << 24) as f32 - 0.5,
                    // Silence, and denormal noise.
                    4 => 0.0,
                    _ => f32::from_bits((bits as u32) & 0x807f_ffff),
                };
            }
        }
        for stereo in [false, true] {
            let buffers = view(&block, frames, stereo);
            assert_eq!(
                meter_bits(native_meter(buffers)),
                meter_bits(flattened_meter(buffers)),
                "round {round}, {frames} frames, stereo {stereo}"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 12_000);
    // Every special value alone, in each channel position.
    for value in special {
        let block = [[value; FRAMES], [-value; FRAMES]];
        for stereo in [false, true] {
            let buffers = view(&block, FRAMES, stereo);
            assert_eq!(
                meter_bits(native_meter(buffers)),
                meter_bits(flattened_meter(buffers)),
                "{value:e} ({:#x}), stereo {stereo}",
                value.to_bits()
            );
        }
    }
    // The fixture is not vacuous: the meters see clips, sums and peaks.
    let loud = native_meter(view(&[[1.5; FRAMES], [-0.5; FRAMES]], FRAMES, true));
    assert_eq!(loud.samples, 2 * FRAMES as u64);
    assert_eq!(loud.clipped, FRAMES as u64);
    assert_eq!(loud.input_peak, 1.5);
    assert!(loud.sum_squares > 0.0);
}

/// The fine histogram's bins are microsecond powers of two, inclusive at
/// the top of each, with the count, total and longest alongside.
#[test]
fn the_fine_gauge_bins_by_microseconds() {
    let gauge = Fine::new();
    for (elapsed, bin) in [
        (0, 0),
        (1_000, 0),
        (1_001, 1),
        (2_000, 1),
        (4_000, 2),
        (4_001, 3),
        (8_000, 3),
        (16_000, 4),
        (32_000, 5),
        (64_000, 6),
        (64_001, 7),
        (5_000_000, 7),
    ] {
        let before = gauge.bins[bin].load(Ordering::Relaxed);
        gauge.record(elapsed);
        assert_eq!(
            gauge.bins[bin].load(Ordering::Relaxed),
            before + 1,
            "{elapsed} ns"
        );
    }
    let report = gauge.snapshot();
    assert_eq!(report["calls"], 12);
    assert_eq!(report["maxNs"], 5_000_000);
    assert_eq!(
        report["totalNs"],
        5_000_000u64
            + 64_001
            + 64_000
            + 32_000
            + 16_000
            + 8_000
            + 4_001
            + 4_000
            + 2_000
            + 1_001
            + 1_000
    );
    assert_eq!(report["histogram"].as_array().unwrap().len(), 8);
    assert_eq!(std::mem::align_of_val(&WAKE_CALL), 64);
}

#[test]
fn expanded_route_plan_uses_the_shared_bounded_path_capacity() {
    let paths: Vec<_> = (0..MAX_PATHS)
        .map(|index| {
            serde_json::json!({
                "direction": "input", "ports": [format!("in_{}", index + 1)],
                "target": Target::new(format!("path-{index}"), "Playback", TargetKind::Playback, 1),
                "mode": "passthrough", "hardware": false,
            })
        })
        .collect();
    let mut value = serde_json::json!({
        "version": 1, "sampleRate": 48000, "frames": FRAMES, "paths": paths,
    });
    let plan = Plan::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(plan.paths.len(), MAX_PATHS);
    assert!(
        plan.paths.len() >= 25,
        "the expanded Pro II catalog must fit"
    );
    let extra = value["paths"][0].clone();
    value["paths"].as_array_mut().unwrap().push(extra);
    assert!(
        Plan::parse(&serde_json::to_vec(&value).unwrap())
            .err()
            .unwrap()
            .contains("geometry/count")
    );
}

struct Buffers {
    input: [UnsafeCell<[f32; FRAMES]>; 2],
    output: [UnsafeCell<[f32; FRAMES]>; 2],
}
impl Buffers {
    fn new() -> Self {
        Self {
            input: [
                UnsafeCell::new([0.25; FRAMES]),
                UnsafeCell::new([-0.125; FRAMES]),
            ],
            output: [const { UnsafeCell::new([0.0; FRAMES]) }; 2],
        }
    }
    fn input(&self, channel: usize) -> *mut f32 {
        self.input[channel].get().cast()
    }
    fn output(&self, channel: usize) -> *mut f32 {
        self.output[channel].get().cast()
    }
}
unsafe extern "C" fn test_buffer(port: *mut c_void, _: u32) -> *mut c_void {
    port
}
fn plan_with_source_and_output(highpass: bool) -> Plan {
    let mut paths = Vec::new();
    for (id, kind, channels, direction, ports) in [
        (
            "source",
            TargetKind::Mic,
            1,
            Direction::Input,
            vec!["in_1".into()],
        ),
        (
            "headphones",
            TargetKind::Output,
            2,
            Direction::Output,
            vec!["out_1".into(), "out_2".into()],
        ),
    ] {
        let mut target = Target::new(id, id, kind, channels);
        target.limiter.enabled = false;
        target.gain.enabled = false;
        target.eq.enabled = true;
        target.eq.input_gain_db = 20.0 * 0.5_f64.log10();
        target.highpass.enabled = direction == Direction::Input && highpass;
        target.highpass.frequency = 120.0;
        paths.push(PathPlan {
            direction,
            ports,
            target,
            mode: PathMode::Processed,
            hardware: direction == Direction::Output,
        });
    }
    Plan {
        version: 1,
        sample_rate: 48_000,
        frames: FRAMES,
        worker: None,
        paths,
    }
}
fn pipeline(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
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
fn block(pipeline: &mut Pipeline, buffers: &Buffers, native_success: bool) {
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    for channel in 0..2 {
        let raw = buffers.input(channel);
        let input = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        let raw = buffers.output(channel);
        let output = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        for index in 0..FRAMES {
            unsafe { output.add(index).write(input.add(index).read() * 2.0) };
        }
    }
    unsafe { pipeline.end(FRAMES, native_success) };
    pipeline.settle();
}

#[test]
fn chat_without_a_verified_stem_leaves_the_completed_stereo_mix_untouched() {
    let mut target = Target::new("send-chat", "Chat", TargetKind::Output, 2);
    target.suppression.enabled = true;
    let plan = Plan {
        version: 1,
        sample_rate: 48_000,
        frames: FRAMES,
        worker: Some(WorkerPlan {
            cpu_mask: None,
            realtime_priority: None,
            nice: None,
        }),
        paths: vec![PathPlan {
            direction: Direction::Output,
            ports: vec!["out_1".into(), "out_2".into()],
            target,
            mode: PathMode::Processed,
            hardware: false,
        }],
    };
    let buffers = Buffers::new();
    let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
    block(&mut pipeline, &buffers, true);
    assert!(
        unsafe { &*buffers.output[0].get() }
            .iter()
            .all(|&v| v == 0.5)
    );
    assert!(
        unsafe { &*buffers.output[1].get() }
            .iter()
            .all(|&v| v == -0.25)
    );
    assert_eq!(pipeline.counters().faults, 1);
    assert_eq!(controls[0].status().unwrap().applied, 0);
    assert_eq!(
        controls[0].worker_status().unwrap().completed_frames,
        0,
        "the mixed output never becomes model input"
    );
}

#[test]
fn live_source_and_output_updates_remain_independent_and_keep_raw_fanout() {
    let plan = plan_with_source_and_output(false);
    let buffers = Buffers::new();
    let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
    block(&mut pipeline, &buffers, true);
    assert_eq!(controls[0].status().unwrap().applied, 1);
    assert_eq!(controls[1].status().unwrap().applied, 1);
    let mut source = plan.paths[0].target.clone();
    source.eq.input_gain_db = 20.0 * 0.25_f64.log10();
    controls[0].publish(&source, 2).unwrap();
    for _ in 0..100 {
        block(&mut pipeline, &buffers, true);
    }
    assert_eq!(controls[0].status().unwrap().applied, 2);
    assert_eq!(controls[1].status().unwrap().published, 1);
    for channel in 0..2 {
        let expected = [0.0625, -0.125][channel];
        assert!(
            unsafe { &*buffers.output[channel].get() }
                .iter()
                .all(|value| (value - expected).abs() < 2e-6)
        );
        assert!(
            unsafe { &*buffers.input[channel].get() }
                .iter()
                .all(|value| *value == [0.25, -0.125][channel])
        );
    }
    let mut output = plan.paths[1].target.clone();
    output.eq.input_gain_db = 0.0;
    controls[1].publish(&output, 2).unwrap();
    for _ in 0..100 {
        block(&mut pipeline, &buffers, true);
    }
    assert_eq!(controls[1].status().unwrap().applied, 2);
    assert_eq!(controls[0].status().unwrap().published, 2);
    for channel in 0..2 {
        let expected = [0.125, -0.25][channel];
        assert!(
            unsafe { &*buffers.output[channel].get() }
                .iter()
                .all(|value| (value - expected).abs() < 2e-6)
        );
    }
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn native_pass_and_replacement_change_only_one_bound_path_without_callback_memory_work() {
    use super::super::{
        CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
    };
    let plan = plan_with_source_and_output(false);
    let buffers = Buffers::new();
    let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
    for _ in 0..8 {
        block(&mut pipeline, &buffers, true);
    }
    let output_epoch = controls[1].status().unwrap().runtime_epoch;
    let staged = controls[0]
        .prepare_mode(&plan.paths[0].target, 2, PathMode::Passthrough)
        .unwrap();
    controls[0].reserve(staged).unwrap().commit();
    let memory = [
        CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
        CALLBACK_FREES.load(Ordering::Relaxed),
        CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
    ];
    let previous = INSIDE.with(|inside| {
        inside.replace(CallbackFrame {
            owner: 1,
            processing: true,
            pipeline: 0,
            started: 0,
            jack: crate::jack_cache::Snapshot::INACTIVE,
        })
    });
    for _ in 0..100 {
        block(&mut pipeline, &buffers, true);
    }
    INSIDE.with(|inside| inside.set(previous));
    assert_eq!(
        [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
        ],
        memory
    );
    assert_eq!(controls[0].status().unwrap().runtime_epoch, 0);
    assert_eq!(controls[1].status().unwrap().runtime_epoch, output_epoch);
    assert_eq!(
        controls[0].status().unwrap().meter.unwrap().latency_samples,
        0
    );
    for (channel, expected) in [0.25_f32, -0.125].into_iter().enumerate() {
        assert!(
            unsafe { &*buffers.output[channel].get() }
                .iter()
                .all(|value| (*value - expected).abs() < 1e-7)
        );
        assert!(
            unsafe { &*buffers.input[channel].get() }
                .iter()
                .all(|value| value.to_bits() == [0.25_f32, -0.125][channel].to_bits())
        );
    }
    let staged = controls[0]
        .prepare_mode(&plan.paths[0].target, 3, PathMode::Processed)
        .unwrap();
    controls[0].reserve(staged).unwrap().commit();
    let previous = INSIDE.with(|inside| {
        inside.replace(CallbackFrame {
            owner: 1,
            processing: true,
            pipeline: 0,
            started: 0,
            jack: crate::jack_cache::Snapshot::INACTIVE,
        })
    });
    for _ in 0..100 {
        block(&mut pipeline, &buffers, true);
    }
    INSIDE.with(|inside| inside.set(previous));
    assert_eq!(
        [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
        ],
        memory
    );
    assert_eq!(controls[0].status().unwrap().runtime_epoch, 2);
    assert_eq!(controls[1].status().unwrap().runtime_epoch, output_epoch);
    for (channel, expected) in [0.125_f32, -0.125].into_iter().enumerate() {
        assert!(
            unsafe { &*buffers.output[channel].get() }
                .iter()
                .all(|value| (*value - expected).abs() < 1e-7)
        );
    }
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn live_parameters_preserve_filter_tail_and_native_failure_does_not_apply() {
    let plan = plan_with_source_and_output(true);
    let buffers = Buffers::new();
    unsafe {
        (*buffers.input[0].get()).fill(0.0);
        buffers.input(0).write(0.25);
    }
    let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
    let prepared = Prepared::new(&plan.paths[0].target, 48_000).unwrap();
    let mut reference = Dsp::without_suppression(&prepared).unwrap();
    let mut expected = [0.0; FRAMES];
    reference
        .process(
            &prepared,
            [unsafe { &*buffers.input[0].get() }, &[]],
            [&mut expected, &mut []],
        )
        .unwrap();
    block(&mut pipeline, &buffers, true);
    let mut source = plan.paths[0].target.clone();
    source.eq.input_gain_db = 20.0 * 0.25_f64.log10();
    controls[0].publish(&source, 2).unwrap();
    let prepared = Prepared::new(&source, 48_000).unwrap();
    unsafe {
        (*buffers.input[0].get()).fill(0.0);
    }
    reference
        .process(
            &prepared,
            [unsafe { &*buffers.input[0].get() }, &[]],
            [&mut expected, &mut []],
        )
        .unwrap();
    // The mixer hears a processed source one frame later; its hardware
    // output is processed in the same callback.
    block(&mut pipeline, &buffers, true);
    block(&mut pipeline, &buffers, true);
    assert!(expected.iter().any(|value| value.abs() > 1e-6));
    for (actual, expected) in unsafe { &*buffers.output[0].get() }.iter().zip(expected) {
        assert!((actual - expected).abs() < 1e-7, "filter history changed");
    }
    controls[0].publish(&source, 3).unwrap();
    block(&mut pipeline, &buffers, false);
    let status = controls[0].status().unwrap();
    assert_eq!(status.seen, 3);
    assert_eq!(status.applied, 2);
    controls[0].publish(&source, 4).unwrap();
    block(&mut pipeline, &buffers, true);
    assert_eq!(controls[0].status().unwrap().applied, 4);
}

#[test]
fn output_settings_are_not_acknowledged_when_native_callback_skips_output() {
    let plan = plan_with_source_and_output(false);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline(&plan, &buffers);
    controls[1].publish(&plan.paths[1].target, 2).unwrap();
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let input = buffers.input(0);
    pipeline.buffer(input as usize, FRAMES, input.cast());
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    let source = controls[0].status().unwrap();
    let output = controls[1].status().unwrap();
    assert_eq!(source.applied, 1);
    assert_eq!(output.published, 2);
    assert_eq!(output.seen, 2);
    assert_eq!(output.applied, 0);
    assert_eq!(pipeline.counters().skipped_outputs, 1);
    controls[1].publish(&plan.paths[1].target, 3).unwrap();
}

/// A frame where the mixer asked for only one of two output channels is
/// neither processed nor rewritten, and is not a fault.
#[test]
fn a_stereo_output_never_processes_only_one_observed_channel() {
    let plan = plan_with_source_and_output(false);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline(&plan, &buffers);
    controls[1].publish(&plan.paths[1].target, 2).unwrap();
    unsafe {
        (*buffers.output[0].get()).fill(0.7);
        (*buffers.output[1].get()).fill(-0.4);
        pipeline.begin(FRAMES, test_buffer, 0);
    }
    let left = buffers.output(0);
    pipeline.buffer(left as usize, FRAMES, left.cast());
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    assert_eq!(controls[1].status().unwrap().applied, 0);
    assert_eq!(pipeline.counters().skipped_outputs, 1);
    assert_eq!(pipeline.counters().faults, 0);
    assert_eq!(unsafe { &*buffers.output[0].get() }, &[0.7; FRAMES]);
    assert_eq!(unsafe { &*buffers.output[1].get() }, &[-0.4; FRAMES]);
}

#[test]
fn a_missing_monitor_port_is_not_replaced_by_the_next_registered_output() {
    let mut plan = plan_with_source_and_output(false);
    plan.paths[1].ports = vec!["out_5".into(), "out_6".into()];
    let buffers = Buffers::new();
    let mut registry = Registry::default();
    assert!(registry.register(1, buffers.input(0) as usize, b"in_1", 1));
    assert!(registry.register(1, buffers.output(0) as usize, b"out_5", 2));
    assert!(registry.register(1, buffers.output(1) as usize, b"out_7", 2));
    assert!(Pipeline::new(&plan, &registry, 1).is_err());
}

/// One source path (mono mic or stereo playback) on in_1/in_2, EQ input
/// gain 0.5, optionally a 120 Hz high-pass (stateful).
fn source_plan(channels: u8, highpass: bool, mode: PathMode) -> Plan {
    let kind = if channels == 1 {
        TargetKind::Mic
    } else {
        TargetKind::Playback
    };
    let mut target = Target::new("source", "source", kind, channels);
    target.limiter.enabled = false;
    target.gain.enabled = false;
    target.eq.enabled = true;
    target.eq.input_gain_db = 20.0 * 0.5_f64.log10();
    target.highpass.enabled = highpass;
    target.highpass.frequency = 120.0;
    Plan {
        version: 1,
        sample_rate: 48_000,
        frames: FRAMES,
        worker: Some(WorkerPlan {
            cpu_mask: None,
            realtime_priority: None,
            nice: None,
        }),
        paths: vec![PathPlan {
            direction: Direction::Input,
            ports: (1..=channels).map(|c| format!("in_{c}")).collect(),
            target,
            mode,
            hardware: false,
        }],
    }
}
fn pipeline_owned(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
    let mut registry = Registry::default();
    for channel in 0..2 {
        assert!(registry.register(
            1,
            buffers.input(channel) as usize,
            format!("in_{}", channel + 1).as_bytes(),
            1
        ));
    }
    Pipeline::new(plan, &registry, 1).unwrap()
}
/// One callback in which the mixer requests the source channels in
/// `order` (repeats allowed) and copies what each request returned.
fn source_block(
    pipeline: &Pipeline,
    buffers: &Buffers,
    order: &[usize],
    native_success: bool,
) -> [[f32; FRAMES]; 2] {
    let mut seen = [[f32::NAN; FRAMES]; 2];
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    for &channel in order {
        let raw = buffers.input(channel);
        let got = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
        seen[channel].copy_from_slice(values);
    }
    unsafe { pipeline.end(FRAMES, native_success) };
    pipeline.settle();
    seen
}
/// One callback as `source_block`, without waiting for the helper.
fn source_block_unsettled(
    pipeline: &Pipeline,
    buffers: &Buffers,
    order: &[usize],
    native_success: bool,
) {
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    for &channel in order {
        let raw = buffers.input(channel);
        pipeline.buffer(raw as usize, FRAMES, raw.cast());
    }
    unsafe { pipeline.end(FRAMES, native_success) };
}

/// The helper-owned source adds one frame; the callback-owned hardware
/// output, processed in place before the callback returns, adds none.
#[test]
fn only_the_helper_owned_source_adds_a_frame() {
    let plan = plan_with_source_and_output(false);
    let buffers = Buffers::new();
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
    let (mut pipeline, _controls) = Pipeline::new(&plan, &registry, 1).unwrap();
    // Source 0.25 x 0.5, mixer x 2, output x 0.5: 0.125 once both
    // stages have been applied.
    let mut first = None;
    for frame in 0..4 {
        block(&mut pipeline, &buffers, true);
        let left = unsafe { &*buffers.output[0].get() };
        if first.is_none() && left.iter().all(|v| (v - 0.125).abs() < 1e-6) {
            first = Some(frame);
        }
    }
    assert_eq!(first, Some(1));
    assert_eq!(pipeline.counters().faults, 0);
}

/// The mixer may keep every pointer a callback's requests returned and
/// read them all afterwards, until the callback returns: later and
/// repeated requests never move or rewrite what an earlier one got.
#[test]
fn every_returned_source_pointer_stays_valid_through_the_callback() {
    let plan = source_plan(2, true, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
    let prepared = Prepared::new(&plan.paths[0].target, 48_000).unwrap();
    let mut reference = Dsp::without_suppression(&prepared).unwrap();
    let mut expected = [[0.0; FRAMES]; 2];
    for frame in 0..4 {
        unsafe {
            (*buffers.input[0].get()).fill(0.0);
            (*buffers.input[1].get()).fill(0.0);
            buffers.input(0).add(5 * frame).write(0.5);
            buffers.input(1).add(7 * frame + 2).write(-0.25);
        }
        // The helper hands the mixer the previous frame's result.
        let current = expected;
        let [left, right] = &mut expected;
        reference
            .process(
                &prepared,
                unsafe { [&*buffers.input[0].get(), &*buffers.input[1].get()] },
                [left, right],
            )
            .unwrap();
        let wanted = current;
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        let mut held = Vec::new();
        for channel in [1, 0, 1, 0, 0, 1] {
            let raw = buffers.input(channel);
            let got = pipeline
                .buffer(raw as usize, FRAMES, raw.cast())
                .cast::<f32>();
            held.push((channel, got));
        }
        // Read every held pointer only after all requests.
        for &(channel, got) in &held {
            let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
            if frame > 0 {
                assert_eq!(
                    values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    wanted[channel].map(f32::to_bits).to_vec(),
                    "frame {frame} channel {channel}"
                );
            }
        }
        let first = held.iter().find(|(c, _)| *c == 0).unwrap().1;
        assert!(
            held.iter().all(|&(c, p)| c != 0 || p == first),
            "one pointer per channel"
        );
        unsafe { pipeline.end(FRAMES, true) };
        // Still readable after end, until the next callback begins.
        for &(channel, got) in &held {
            let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
            if frame > 0 {
                assert_eq!(
                    values[..].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    wanted[channel].map(f32::to_bits).to_vec()
                );
            }
        }
        pipeline.settle();
    }
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn a_processed_frame_the_mixer_failed_is_counted_apart() {
    let start = here::sources();
    let plan = source_plan(2, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..2 {
        source_block(&pipeline, &buffers, &[0, 1], true);
    }
    let before = here::sources().since(start);
    controls[0].publish(&plan.paths[0].target, 2).unwrap();
    // Processed audio delivered, but the mixer's callback failed.
    source_block(&pipeline, &buffers, &[0, 1], false);
    // Processed audio delivered for one channel; the other never asked.
    source_block(&pipeline, &buffers, &[0], true);
    let after = here::sources().since(start);
    assert_eq!(after.processed, before.processed);
    assert_eq!(after.raw_invalid, before.raw_invalid);
    // The helper is handed only completed frames: the failed one
    // still delivered last frame's result, but the next has none.
    assert_eq!(
        after.processed_unacknowledged,
        before.processed_unacknowledged + 1
    );
    assert_eq!(after.raw_no_handoff, before.raw_no_handoff + 1);
    assert_eq!(after.raw_returns, before.raw_returns + 1);
    let status = controls[0].status().unwrap();
    assert_eq!((status.seen, status.applied), (2, 1));
    source_block(&pipeline, &buffers, &[0, 1], true);
    assert_eq!(controls[0].status().unwrap().applied, 2);
    let last = here::sources().since(start);
    // Its previous frame was incomplete, so not handed off either.
    assert_eq!(last.processed, before.processed);
    assert_eq!(last.raw_no_handoff, after.raw_no_handoff + 1);
}

#[test]
fn the_helper_never_shares_the_suppression_workers_core() {
    // Pinned callback: the other audio cores.
    assert_eq!(helper_mask(0b0100), 0b1010);
    // Before RØDE pins the callback (every core), or with no mask known,
    // the helper still keeps to the audio cores, never CPU 0, where the
    // suppression worker runs (rc2fx_suppression::PRODUCTION_WORKER).
    for callback in [0b1111, 0b1110, 0] {
        let mask = helper_mask(callback);
        assert_ne!(mask, 0, "callback {callback:#b}");
        assert_eq!(mask & !AUDIO_CPUS, 0, "callback {callback:#b}: {mask:#b}");
    }
    assert_eq!(
        rc2fx_suppression::PRODUCTION_WORKER.cpu_mask.unwrap() & AUDIO_CPUS,
        0
    );
}

#[test]
fn placement_is_sampled_fast_until_settled_then_at_the_steady_cadence() {
    let rr = libc::SCHED_RR;
    let mut sampler = Sampler::default();
    let samples = |sampler: &Sampler, cycles: std::ops::Range<u32>| {
        cycles
            .filter(|&cycle| sampler.due(cycle))
            .collect::<Vec<_>>()
    };
    // Startup: every 64 callbacks.
    assert_eq!(&samples(&sampler, 1..300), &[64, 128, 192, 256]);
    // Real-time but not yet pinned (every core): not settled.
    sampler.observe(rr, 0b1111, 1, 1);
    assert!(!sampler.settled);
    // Pinned and real-time, but the helper has not applied it yet.
    sampler.observe(rr, 0b0100, 2, 1);
    assert!(!sampler.settled);
    // Ordinary policy: not settled.
    sampler.observe(libc::SCHED_OTHER, 0b0100, 2, 2);
    assert!(!sampler.settled);
    // A promotion at callback 100 is seen at 128, not 2048.
    let promoted = 100;
    let seen = (promoted..).find(|&cycle| sampler.due(cycle)).unwrap();
    assert!(seen - promoted <= PLACE_FAST_EVERY, "seen at {seen}");
    sampler.observe(rr, 0b0100, 2, 2);
    assert!(sampler.settled);
    // Settled: the steady cadence, unchanged.
    assert_eq!(&samples(&sampler, 1..8193), &[2048, 4096, 6144, 8192]);
    // Never settled: fast only until PLACE_FAST_UNTIL, then steady.
    let unsettled = Sampler::default();
    assert!(unsettled.due(PLACE_FAST_UNTIL - PLACE_FAST_EVERY));
    assert!(!unsettled.due(PLACE_FAST_UNTIL + PLACE_FAST_EVERY));
    assert!(unsettled.due(4 * PLACE_EVERY));
}

/// A placement boundary whose callback finds the helper busy has no
/// handoff to sample with; the sample is taken at the next handoff, not
/// a whole interval later.
#[test]
fn a_placement_boundary_without_a_handoff_is_sampled_at_the_next_one() {
    let _events = super::super::probe::EVENT_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let plan = source_plan(1, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
    let shared = &pipeline.helper.shared;
    let deferrals = placement()["deferred"].as_u64().unwrap();
    for _ in 1..PLACE_FAST_EVERY {
        source_block(&pipeline, &buffers, &[0], true);
    }
    assert_eq!(shared.generation.load(Ordering::Acquire), 0);
    // Callback 64: neither idle nor queued, so the helper stays parked
    // and the callback gives up waiting for it.
    shared.state.store(QUEUED + 1, Ordering::Release);
    source_block_unsettled(&pipeline, &buffers, &[0], true);
    assert_eq!(unsafe { &*pipeline.front.get() }.cycle, PLACE_FAST_EVERY);
    assert_eq!(shared.generation.load(Ordering::Acquire), 0);
    shared.state.store(IDLE, Ordering::Release);
    // Callback 65 hands off and takes the deferred sample.
    source_block(&pipeline, &buffers, &[0], true);
    let generation = shared.generation.load(Ordering::Acquire);
    assert!(
        generation >= 1,
        "sample not taken after the missed boundary"
    );
    source_block(&pipeline, &buffers, &[0], true);
    assert_eq!(shared.applied.load(Ordering::Acquire), generation);
    let sampler = unsafe { &*pipeline.front.get() }.sampler;
    assert_eq!(sampler.pending, None);
    // Other tests' pipelines may defer samples too.
    assert!(
        placement()["deferred"].as_u64().unwrap() > deferrals,
        "the deferral is counted"
    );
}

#[test]
fn a_missed_boundary_stays_pending_until_sampled_and_counts_once() {
    let mut sampler = Sampler::default();
    for cycle in 60..64 {
        sampler.mark(cycle);
        assert_eq!(sampler.pending, None);
    }
    // Boundary 64 and the callbacks after it without a handoff.
    for cycle in 64..70 {
        sampler.mark(cycle);
        assert_eq!(sampler.pending, Some(64));
    }
    assert_eq!(sampler.take(70), Some(true));
    assert_eq!(sampler.pending, None);
    assert_eq!(sampler.take(71), None);
    // On time: taken at the boundary, not a deferral.
    sampler.mark(128);
    assert_eq!(sampler.take(128), Some(false));
    // Missed through the next boundary: still the first boundary's
    // sample, deferred, counted once.
    sampler.mark(192);
    for cycle in 193..256 {
        sampler.mark(cycle);
    }
    sampler.mark(256);
    assert_eq!(sampler.take(256), Some(true));
    assert_eq!(sampler.take(257), None);
    // The callback counter wraps onto a boundary.
    sampler.mark(u32::MAX);
    sampler.mark(0);
    assert_eq!(sampler.take(1), Some(true));
}

/// Runs a pipeline past its first fast placement sample; the report's
/// history shows what the callback had and what the helper then got.
#[test]
fn placement_changes_are_recorded_with_actual_masks() {
    let plan = source_plan(1, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..PLACE_FAST_EVERY + 2 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    let report = placement();
    assert!(report["samples"].as_u64().unwrap() >= 1);
    let history = report["history"].as_array().unwrap();
    assert!(!history.is_empty());
    for record in history {
        assert!(record["generation"].as_u64().unwrap() >= 1);
        // On a boundary, unless its callback had no handoff.
        let boundary = record["cycle"]
            .as_u64()
            .unwrap()
            .is_multiple_of(u64::from(PLACE_FAST_EVERY));
        assert!(boundary || record["deferred"] == true, "{record}");
        assert!(record["atNs"].as_u64().unwrap() > 0);
        if record["appliedAtNs"].as_u64().unwrap() != 0 {
            let requested = record["helperRequestedCpus"].as_u64().unwrap();
            assert_eq!(requested & !AUDIO_CPUS, 0, "never CPU 0: {record}");
            assert!(record["appliedAtNs"].as_u64() >= record["atNs"].as_u64());
        }
    }
}

#[test]
fn a_helper_source_reaches_the_mixer_a_frame_later() {
    let start = here::sources();
    let plan = source_plan(1, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    let first = source_block(&pipeline, &buffers, &[0], true);
    let second = source_block(&pipeline, &buffers, &[0], true);
    // Enough frames for one telemetry window.
    for _ in 0..48 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    // The first frame passes raw; processing arrives a frame (112
    // samples) later.
    assert!(first[0].iter().all(|v| (v - 0.25).abs() < 1e-6));
    assert!(second[0].iter().all(|v| (v - 0.125).abs() < 1e-6));
    let status = controls[0].status().unwrap();
    assert_eq!(status.applied, 1);
    let latency = status.meter.unwrap().latency_samples;
    assert_eq!(latency, FRAMES as u32);
    let counts = here::sources().since(start);
    assert_eq!(counts.processed, 49);
    assert_eq!(counts.raw_no_handoff, 1);
    assert_eq!(counts.raw_returns, 1);
    assert_eq!(pipeline.counters().input_blocks, 50);
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn a_later_raw_request_counts_the_whole_frame_raw() {
    let start = here::sources();
    let plan = source_plan(2, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..2 {
        source_block(&pipeline, &buffers, &[0, 1], true);
    }
    let before = here::sources().since(start);
    assert_eq!(before.processed, 1);
    // Left is served processed, then right arrives with a buffer that
    // is not the one resolved for this frame.
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let left = buffers.input(0);
    let got = pipeline.buffer(left as usize, FRAMES, left.cast());
    assert_ne!(got, left.cast(), "left processed");
    let right = buffers.input(1);
    let mut elsewhere = [0.0f32; FRAMES];
    let got = pipeline.buffer(right as usize, FRAMES, elsewhere.as_mut_ptr().cast());
    assert_eq!(got, elsewhere.as_mut_ptr().cast(), "right raw");
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    let after = here::sources().since(start);
    assert_eq!(after.processed, before.processed);
    assert_eq!(after.raw_invalid, before.raw_invalid + 1);
    assert_eq!(after.raw_returns, before.raw_returns + 1);
    // The failed path stays raw, and each frame says so.
    source_block(&pipeline, &buffers, &[0, 1], true);
    let failed = here::sources().since(start);
    assert_eq!(failed.raw_invalid, after.raw_invalid + 1);
    assert_eq!(failed.raw_returns, after.raw_returns + 2);
    assert_eq!(pipeline.counters().faults, 1);
}

#[test]
fn an_unrequested_source_frame_is_counted_and_not_acknowledged() {
    let start = here::sources();
    let plan = source_plan(1, false, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    source_block(&pipeline, &buffers, &[0], true);
    controls[0].publish(&plan.paths[0].target, 2).unwrap();
    source_block(&pipeline, &buffers, &[], true);
    let status = controls[0].status().unwrap();
    assert_eq!((status.seen, status.applied), (2, 1));
    let counts = here::sources().since(start);
    assert_eq!(counts.unobserved, 1);
    source_block(&pipeline, &buffers, &[0], true);
    assert_eq!(controls[0].status().unwrap().applied, 2);
}

/// The mic to headphones path of the Pro II profile: DeepFilterNet3's
/// 240-sample frame, 120 intrinsic, the 320 lead and the helper's period,
/// 792 samples (16.5 ms).
#[test]
fn a_deepfilternet_mic_reports_the_profile_latency() {
    let mut plan = source_plan(1, false, PathMode::Processed);
    plan.paths[0].target.suppression.enabled = true;
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..40 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    assert_eq!(
        controls[0].status().unwrap().meter.unwrap().latency_samples,
        240 + 120 + 320 + FRAMES as u32
    );
    assert_eq!(pipeline.counters().faults, 0);
}

/// DeepFilterNet3's lookahead is a live setting: a transaction that
/// changes it replaces only that path's suppression worker (the old
/// runtime is admitted out and a new one published), the callback does no
/// memory work meanwhile, and the reported latency follows by one hop.
#[test]
fn a_live_lookahead_switch_rebuilds_the_worker_and_moves_the_latency_by_one_hop() {
    let start = here::sources();
    use super::super::{
        CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
    };
    let mut plan = source_plan(1, false, PathMode::Processed);
    plan.paths[0].target.suppression.enabled = true;
    plan.paths[0].target.suppression.lookahead = 0;
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..40 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    let hop = rc2fx_core::DFN_HOP_SAMPLES;
    let base = rc2fx_suppression::Geometry::for_lookahead(0)
        .unwrap()
        .latency_samples() as u32;
    let helper = FRAMES as u32;
    let latency = |controls: &mut [parameters::Control]| {
        controls[0].status().unwrap().meter.unwrap().latency_samples
    };
    assert_eq!(latency(&mut controls), base + helper);
    // Every switch is a live transaction on this path alone, and the
    // callbacks either side of it do no memory work (the transaction
    // itself is control work and may allocate).
    let memory = [
        CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
        CALLBACK_FREES.load(Ordering::Relaxed),
        CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
    ];
    // Revisions must climb past the one the plan was built with.
    let first = controls[0].status().unwrap().applied + 1;
    let mut previous_epoch = controls[0].status().unwrap().runtime_epoch;
    for (revision, lookahead, expected) in [
        (first, 1, base + hop + helper),
        (first + 1, 0, base + helper),
        (first + 2, 1, base + hop + helper),
        (first + 3, 0, base + helper),
    ] {
        let retiring = controls[0].worker_telemetry().unwrap().unwrap().1;
        let mut target = plan.paths[0].target.clone();
        target.suppression.lookahead = lookahead;
        let staged = controls[0]
            .prepare_mode(&target, revision, PathMode::Processed)
            .unwrap();
        controls[0].reserve(staged).unwrap().commit();
        let previous = INSIDE.with(|inside| {
            inside.replace(CallbackFrame {
                owner: 1,
                processing: true,
                pipeline: 0,
                started: 0,
                jack: crate::jack_cache::Snapshot::INACTIVE,
            })
        });
        for _ in 0..40 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        INSIDE.with(|inside| inside.set(previous));
        assert_eq!(latency(&mut controls), expected, "lookahead {lookahead}");
        let status = controls[0].status().unwrap();
        assert_eq!(status.applied, revision);
        assert!(
            status.runtime_epoch > previous_epoch,
            "a fresh runtime was published"
        );
        previous_epoch = status.runtime_epoch;
        let retired = controls[0].retired_workers();
        assert_eq!(retired.len(), 1);
        let worker = &retired[0].worker;
        assert!(worker.completed_frames > 0);
        let terminal = retiring.snapshot();
        for (reported, raw) in [
            (worker.wall, terminal.wall),
            (worker.cpu, terminal.cpu),
            (worker.turnaround, terminal.turnaround),
        ] {
            assert_eq!(
                reported.calls, raw.calls,
                "retirement must contain only this worker's timing"
            );
            assert_eq!(reported.total_ns, raw.total_ns);
        }
    }
    assert_eq!(
        [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
        ],
        memory,
        "no callback allocation across the switches"
    );
    let counters = pipeline.counters();
    assert_eq!(counters.faults, 0);
    let sources = here::sources().since(start);
    assert!(sources.processed >= 100, "{sources:?}");
}

#[test]
fn a_suppressed_mic_reports_its_dsp_delay_and_the_helper_frame() {
    let start = here::sources();
    let mut plan = source_plan(1, false, PathMode::Processed);
    plan.paths[0].target.suppression.enabled = true;
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..40 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    let worker = rc2fx_suppression::Geometry::for_lookahead(0)
        .unwrap()
        .latency_samples() as u32;
    let latency = controls[0].status().unwrap().meter.unwrap().latency_samples;
    assert_eq!(latency, worker + FRAMES as u32);
    let counters = pipeline.counters();
    assert_eq!(counters.faults, 0);
    assert_eq!(here::sources().since(start).processed, 39);
    // The worker runs asynchronously; give it time to catch up.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while controls[0].worker_status().unwrap().completed_frames == 0 {
        assert!(std::time::Instant::now() < deadline, "worker never ran");
        thread::sleep(std::time::Duration::from_millis(1));
    }
    // The report's outlier records come from the same worker.
    let outliers = controls[0].worker_outliers().unwrap();
    assert_eq!(outliers["worst"]["state"], "stable");
    assert!(outliers["worst"]["turnaroundNs"].as_u64().unwrap() > 0);
    // Cumulative turnaround counts above 5.5, 6.0, 6.5 and 7.0 ms,
    // alongside the worst; each threshold counts at most its
    // predecessor, and the worst must exceed the top one it counts.
    let above = &outliers["turnaroundsAboveMs"];
    let counts: Vec<u64> = ["5.5", "6.0", "6.5", "7.0"]
        .iter()
        .map(|key| above[key].as_u64().unwrap())
        .collect();
    assert!(
        counts.windows(2).all(|pair| pair[1] <= pair[0]),
        "{counts:?}"
    );
    assert!(["empty", "stable"].contains(&outliers["lastMiss"]["state"].as_str().unwrap()));
    for kind in ["missHistory", "lateHistory"] {
        let history = &outliers[kind];
        let count = |key: &str| history[key].as_u64().unwrap();
        assert_eq!(
            count("total"),
            count("recorded") + count("lost") + count("unpublished")
        );
        assert_eq!(
            count("recorded"),
            history["events"].as_array().unwrap().len() as u64
        );
    }
    // The history holds the latest miss whenever it has room.
    if outliers["lastMiss"]["state"] == "stable" && outliers["missHistory"]["lost"] == 0 {
        let events = outliers["missHistory"]["events"].as_array().unwrap();
        assert_eq!(
            events.last().unwrap()["sequence"],
            outliers["lastMiss"]["sequence"]
        );
    }
}

fn full_registry() -> Registry {
    let mut registry = Registry::default();
    for pointer in 1..=512 {
        assert!(registry.register(1, pointer, format!("in_{pointer}").as_bytes(), 1));
    }
    registry
}

#[test]
fn replacement_at_capacity_retires_old_identity_before_admitting_new_one() {
    let mut registry = full_registry();
    let retired = Arc::clone(&registry.ports[0]);
    assert!(registry.register(1, retired.pointer, b"replacement", 1));
    assert_eq!(registry.ports.len(), 512);
    assert!(!retired.alive.load(Ordering::Acquire));
    let replacement = registry.ports.last().unwrap();
    assert!(replacement.alive.load(Ordering::Acquire));
    assert_eq!(replacement.pointer, retired.pointer);
    assert_eq!(replacement.name, "replacement");
    assert!(!Arc::ptr_eq(replacement, &retired));
}

#[test]
fn registry_capacity_refuses_new_identity_without_retiring_other_ports() {
    let mut registry = full_registry();
    assert!(!registry.register(1, 513, b"extra", 1));
    assert_eq!(registry.ports.len(), 512);
    assert!(
        registry
            .ports
            .iter()
            .all(|pin| pin.alive.load(Ordering::Acquire))
    );
}

#[test]
fn untrackable_replacement_does_not_leave_old_pointer_identity_alive() {
    let mut registry = Registry::default();
    assert!(registry.register(1, 1, b"source", 1));
    let retired = Arc::clone(&registry.ports[0]);
    assert!(!registry.register(1, 1, &[0xff], 1));
    assert!(!retired.alive.load(Ordering::Acquire));
    assert!(registry.ports.is_empty());
}

thread_local! {
    /// The pins `begin` resolved, in order (`recording_buffer`).
    static RESOLVED: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}
unsafe extern "C" fn recording_buffer(port: *mut c_void, _: u32) -> *mut c_void {
    RESOLVED.with(|resolved| resolved.borrow_mut().push(port as usize));
    port
}

#[test]
fn unprocessed_paths_meter_once_in_the_stride_on_staggered_cycles() {
    let start = here::sources();
    // Three meter-only paths: two mics and a hardware output.
    let mut plan = plan_with_source_and_output(false);
    let second = PathPlan {
        direction: Direction::Input,
        ports: vec!["in_2".into()],
        target: Target::new("second", "second", TargetKind::Mic, 1),
        mode: PathMode::Passthrough,
        hardware: false,
    };
    plan.paths.insert(1, second);
    for path in &mut plan.paths {
        path.mode = PathMode::Passthrough;
    }
    let buffers = Buffers::new();
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
    let (pipeline, _controls) = Pipeline::new(&plan, &registry, 1).unwrap();
    let first_pin = [buffers.input(0), buffers.input(1), buffers.output(0)];
    let mut metered = [const { Vec::new() }; 3];
    for cycle in 0..2 * METER_STRIDE {
        RESOLVED.with(|resolved| resolved.borrow_mut().clear());
        unsafe { pipeline.begin(FRAMES, recording_buffer, 0) };
        // The mixer asks for every buffer every cycle and always gets
        // its own: metering never changes the audio.
        for pin in [
            buffers.input(0),
            buffers.input(1),
            buffers.output(0),
            buffers.output(1),
        ] {
            assert_eq!(
                pipeline.buffer(pin as usize, FRAMES, pin.cast()),
                pin.cast()
            );
        }
        unsafe { pipeline.end(FRAMES, true) };
        pipeline.settle();
        let resolved = RESOLVED.with(|resolved| resolved.borrow().clone());
        for (path, pin) in first_pin.iter().enumerate() {
            if resolved.contains(&(*pin as usize)) {
                metered[path].push(cycle);
            }
        }
    }
    for (path, cycles) in metered.iter().enumerate() {
        assert_eq!(cycles.len(), 2, "path {path}: {cycles:?}");
        assert_eq!(cycles[1] - cycles[0], METER_STRIDE);
        let expected: Vec<u32> = (0..2 * METER_STRIDE)
            .filter(|&cycle| meters(cycle, path))
            .collect();
        assert_eq!(*cycles, expected);
    }
    for a in 0..3 {
        for b in a + 1..3 {
            assert!(
                metered[a].iter().all(|cycle| !metered[b].contains(cycle)),
                "paths {a} and {b} meter on the same cycle: {metered:?}"
            );
        }
    }
    let counters = pipeline.counters();
    assert_eq!(counters.faults, 0);
    assert_eq!(here::sources().since(start), here::SourceCounts::default());
}

#[test]
fn the_meter_stagger_survives_the_cycle_counter_wrapping() {
    for path in 0..MAX_PATHS {
        let hits = (0..2 * METER_STRIDE)
            .filter(|&k| meters((u32::MAX - METER_STRIDE + 1).wrapping_add(k), path))
            .count();
        assert_eq!(hits, 2, "path {path}");
    }
}

/// Fills the mono source with `x`; one settled callback; what the mixer
/// read (every sample equal).
fn source_owner(pipeline: &Pipeline) -> Owner {
    unsafe { &*pipeline.front.get() }.ports[0].owner
}
fn commit_mode(control: &mut parameters::Control, plan: &Plan, revision: u64, mode: PathMode) {
    let staged = control
        .prepare_mode(&plan.paths[0].target, revision, mode)
        .unwrap();
    control.reserve(staged).unwrap().commit();
}
/// The source plan's EQ halves the input.
fn processed(x: f32) -> f32 {
    0.5 * x
}
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

/// Sources stay with the helper: it processes the source one frame later,
/// before and after a live change.
#[test]
fn a_helper_source_keeps_its_owner_across_live_mode_changes() {
    let plan = source_plan(1, false, PathMode::Passthrough);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    let mut out = Vec::new();
    for k in 0..30 {
        if k == 5 {
            commit_mode(&mut controls[0], &plan, 2, PathMode::Processed);
        }
        if k == 20 {
            commit_mode(&mut controls[0], &plan, 3, PathMode::Passthrough);
        }
        let x = 0.1 + 0.001 * k as f32;
        unsafe { (*buffers.input[0].get()).fill(x) };
        out.push((x, source_block(&pipeline, &buffers, &[0], true)[0]));
        assert!(source_owner(&pipeline) == Owner::Helper);
        let port = &unsafe { &*pipeline.front.get() }.ports[0];
        assert!(port.reader.is_none());
    }
    // Adopted under frame 5; frame 6 has no processed frame yet, then
    // each frame gets the previous one processed. With no callbacks
    // during the commit (this test commits between them), the
    // unprocessed audio starts fading only at frame 5, and frame 6 stays
    // silent; the new runtime fades in from silence over the next frames.
    // (`a_helper_source_switch_dips_without_a_step` runs the fade
    // between callbacks, as the mixer does.)
    assert!(out[6].1.iter().all(|value| *value == 0.0));
    let fading = (rc2fx_dsp::STAGE_FADE_SAMPLES as usize).div_ceil(FRAMES);
    for k in 7..20 {
        let wet = processed(out[k - 1].0);
        for value in out[k].1 {
            if k >= 7 + fading {
                assert!(close(value, wet), "frame {k}: {value} != {wet}");
            } else {
                assert!(
                    (-1e-6..=wet + 1e-6).contains(&value),
                    "frame {k}: {value} outside 0..{wet}"
                );
            }
        }
    }
    assert_eq!(pipeline.counters().faults, 0);
}

/// Switches a helper-owned source's mode the way the mixer sees it: the
/// hand-off starts, callbacks run until it is done (or for at most
/// `limit` frames), then the publication commits. Returns the frames.
fn switch_between_callbacks(
    control: &mut parameters::Control,
    plan: &Plan,
    revision: u64,
    mode: PathMode,
    mut frame: impl FnMut(),
) -> usize {
    let staged = control
        .prepare_mode(&plan.paths[0].target, revision, mode)
        .unwrap();
    let handing = control.hand_off(&staged).unwrap();
    let mut frames = 0;
    while !handing.done() {
        assert!(frames < 100, "the hand-off never finished");
        frame();
        frames += 1;
    }
    control.reserve_after(staged, handing).unwrap().commit();
    frames
}
/// A constant through a helper-owned source switched to Processed and
/// back, with fades between callbacks: returns every sample the mixer
/// read.
fn source_switches(plan: &Plan, level: f32) -> Vec<f32> {
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(plan, &buffers);
    let mut seen = Vec::new();
    let mut frame = || {
        unsafe { (*buffers.input[0].get()).fill(level) };
        seen.extend_from_slice(&source_block(&pipeline, &buffers, &[0], true)[0]);
    };
    for _ in 0..20 {
        frame();
    }
    let first = controls[0].status().unwrap().applied + 1;
    for (revision, mode) in [
        (first, PathMode::Processed),
        (first + 1, PathMode::Passthrough),
    ] {
        let frames = switch_between_callbacks(&mut controls[0], plan, revision, mode, &mut frame);
        assert!(frames >= 4, "{mode:?} published after {frames} frames");
        for _ in 0..40 {
            frame();
        }
    }
    assert_eq!(pipeline.counters().faults, 0);
    seen
}
fn largest_step(samples: &[f32]) -> f32 {
    samples
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

/// A helper-owned source plays its processor's output one frame later than
/// its unprocessed audio, so neither can take over from the other sample
/// for sample. Switching its processing on fades the unprocessed audio
/// out first, and the processor in after; switching it off fades the
/// processor out first, and the unprocessed audio in after: a short dip,
/// never a step, and never a repeated frame.
#[test]
fn a_helper_source_switch_dips_without_a_step() {
    let plan = source_plan(1, false, PathMode::Passthrough);
    let seen = source_switches(&plan, 0.2);
    let step = largest_step(&seen);
    assert!(step < 2e-3, "a step of {step}; a cut is at least 0.1");
    // Unprocessed, processed (the EQ halves it), unprocessed again, each
    // reached exactly, with silence between.
    assert_eq!(seen[0], 0.2);
    assert!(seen.contains(&0.0));
    assert!(seen.iter().any(|v| close(*v, processed(0.2))));
    assert_eq!(*seen.last().unwrap(), 0.2);
}

/// With suppression on, the processor's output is also delayed by its
/// model: switching processing on or off dips the same way, the
/// processor fading in once its delay holds real audio.
#[test]
fn a_suppressed_source_switch_dips_without_a_step() {
    let mut plan = source_plan(1, false, PathMode::Passthrough);
    plan.paths[0].target.suppression.enabled = true;
    // The model's dry signal, so the processed level is known.
    plan.paths[0].target.suppression.wet = 0.0;
    let seen = source_switches(&plan, 0.2);
    let step = largest_step(&seen);
    assert!(step < 2e-3, "a step of {step}; a cut is at least 0.1");
    assert!(seen.iter().any(|v| close(*v, processed(0.2))));
    assert_eq!(*seen.last().unwrap(), 0.2);
}

/// A suppressed mic switched live to Processed reports its DSP delay plus
/// the helper frame, as a mic planned Processed does.
#[test]
fn a_suppressed_mic_switched_live_reports_the_helper_frame() {
    let mut plan = source_plan(1, false, PathMode::Passthrough);
    plan.paths[0].target.suppression.enabled = true;
    let worker = rc2fx_suppression::Geometry::for_lookahead(0)
        .unwrap()
        .latency_samples() as u32;
    let buffers = Buffers::new();
    let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
    for _ in 0..5 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    commit_mode(&mut controls[0], &plan, 2, PathMode::Processed);
    for _ in 0..40 {
        source_block(&pipeline, &buffers, &[0], true);
    }
    let latency = controls[0].status().unwrap().meter.unwrap().latency_samples;
    assert_eq!(latency, worker + FRAMES as u32);
    assert!(source_owner(&pipeline) == Owner::Helper);
    assert_eq!(pipeline.counters().faults, 0);
}
