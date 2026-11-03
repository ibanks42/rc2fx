//! The audit's live processing counters (`pipeline::processing_counters`).
//!
//! The existing `inputBlocks`/`outputBlocks`/`dspFaults`/`skippedOutputs` fields
//! are only summed when a client closes, so they cannot show what the DSP is
//! doing while callbacks run (and `Pipeline::counters` waits for the helper, so
//! it must not be called then). These tests pin the live mirrors to their
//! increment sites: one increment per actual block or event, faults kept apart
//! from successes, incomplete frames apart from both, startup warm-up not
//! misreported as a failure, and the retirement aggregation adding nothing.
use super::*;
use std::sync::{Mutex, atomic::Ordering::Relaxed};

/// The counters are process-global, so these tests take turns: an exact window
/// is only meaningful when no other one of them is running.
static COUNTER_TESTS: Mutex<()> = Mutex::new(());
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    COUNTER_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// The six live counters, as one comparable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Live {
    input: u64,
    output: u64,
    faults: u64,
    skipped: u64,
    no_frame: u64,
    raw_frame: u64,
}
fn live() -> Live {
    Live {
        input: LIVE_INPUT_BLOCKS.load(Relaxed),
        output: LIVE_OUTPUT_BLOCKS.load(Relaxed),
        faults: LIVE_FAULTS.load(Relaxed),
        skipped: LIVE_SKIPPED_OUTPUTS.load(Relaxed),
        no_frame: LIVE_OUTPUT_NO_FRAME.load(Relaxed),
        raw_frame: LIVE_OUTPUT_RAW_FRAME.load(Relaxed),
    }
}
fn delta(before: Live, after: Live) -> Live {
    Live {
        input: after.input - before.input,
        output: after.output - before.output,
        faults: after.faults - before.faults,
        skipped: after.skipped - before.skipped,
        no_frame: after.no_frame - before.no_frame,
        raw_frame: after.raw_frame - before.raw_frame,
    }
}
/// The four counters both sides own moved identically: other tests' pipelines
/// only ever inflate the live side, never this pipeline's own counters.
fn agrees(live: &Live, counters: &Counters) -> bool {
    live.input == counters.input_blocks
        && live.output == counters.output_blocks
        && live.faults == counters.faults
        && live.skipped == counters.skipped_outputs
}
/// A mono source and one stereo hardware output (the C tests' fixture).
fn hardware_plan(mode: PathMode) -> Plan {
    let mut source = Target::new("source", "source", TargetKind::Mic, 1);
    source.limiter.enabled = false;
    source.gain.enabled = false;
    source.eq.enabled = true;
    source.eq.input_gain_db = 20.0 * 0.5_f64.log10();
    let mut output = Target::new("headphones", "headphones", TargetKind::Output, 2);
    output.limiter.enabled = false;
    output.gain.enabled = false;
    output.eq.enabled = true;
    output.eq.input_gain_db = 20.0 * 0.5_f64.log10();
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
                ports: vec!["out_1".into(), "out_2".into()],
                target: output,
                mode,
                hardware: true,
            },
        ],
    }
}
fn build(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
    build_with(plan, buffers, false)
}
fn build_with(
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
/// One callback with both channels requested, the mixer's own processing in the
/// output buffers, and `Pipeline::counters` called afterwards as retirement does.
fn block(pipeline: &Pipeline, buffers: &Buffers) -> Counters {
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let raw = buffers.input(0);
    let source = pipeline
        .buffer(raw as usize, FRAMES, raw.cast())
        .cast::<f32>();
    let voice = unsafe { *source };
    for channel in 0..2 {
        let raw = buffers.output(channel);
        let output = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        unsafe { std::slice::from_raw_parts_mut(output, FRAMES) }.fill(voice);
    }
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    // The retirement aggregation: reading it must not touch the live mirrors.
    pipeline.counters()
}

/// Run `script` on a fresh pipeline, retrying until no other context's events
/// fall inside the same window (they only inflate the live side). Under a
/// pathological schedule the caller reports the impasse instead of failing.
fn measured<F, Q>(make: impl Fn() -> Plan, quiet: Q, script: F) -> Option<(Live, Counters)>
where
    F: Fn(&Pipeline, &Buffers),
    Q: Fn(&Live) -> bool,
{
    for attempt in 0..2000 {
        if attempt > 0 {
            if attempt % 8 == 0 {
                std::thread::sleep(std::time::Duration::from_micros(500));
            } else {
                std::thread::yield_now();
            }
        }
        let plan = make();
        let buffers = Buffers::new();
        let (pipeline, _controls) = build(&plan, &buffers);
        let before = live();
        script(&pipeline, &buffers);
        let counters = pipeline.counters();
        let delta = delta(before, live());
        if agrees(&delta, &counters) && quiet(&delta) {
            return Some((delta, counters));
        }
        std::thread::yield_now();
    }
    None
}

/// Positive: one increment per processed block, on both sides of the pipe, with
/// the retirement sum adding nothing (it is called after every frame here).
#[test]
fn live_counters_track_each_processed_block_once() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Processed),
        |live| live.raw_frame == 1 && live.no_frame == 0,
        |pipeline, buffers| {
            for _ in 0..5 {
                block(pipeline, buffers);
            }
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    // One processed block per frame on each side. The helper's own output work
    // happens in every frame, including the first; what lags by a frame is the
    // *delivery* of that first result, which is the fallback counted below.
    assert_eq!((live.input, live.output), (5, 5));
    assert_eq!(
        (counters.input_blocks, counters.output_blocks),
        (5, 5),
        "the same events, per pipeline"
    );
    assert_eq!((live.faults, live.skipped), (0, 0));
    // Startup is visible as a fallback delivery, not as a fault or a skip. The
    // slot it selects is still zero-initialised, so this is a slot selection,
    // not evidence about what the mixer captured.
    assert_eq!(
        live.raw_frame, 1,
        "the first frame has no result and selects the unwritten slot"
    );
    assert_eq!(live.no_frame, 0);
}

/// Callback-owned (direct) outputs: output blocks are counted live from the
/// callback, one per frame, with no copied-frame fallback at all.
#[test]
fn a_callback_owned_output_counts_every_block_and_no_fallback() {
    let _exclusive = exclusive();
    for attempt in 0..2000 {
        if attempt > 0 {
            if attempt % 8 == 0 {
                std::thread::sleep(std::time::Duration::from_micros(500));
            } else {
                std::thread::yield_now();
            }
        }
        let plan = hardware_plan(PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, _controls) = build_with(&plan, &buffers, true);
        let before = live();
        for _ in 0..5 {
            block(&pipeline, &buffers);
        }
        let counters = pipeline.counters();
        let delta = delta(before, live());
        if !agrees(&delta, &counters) || delta.raw_frame != 0 || delta.no_frame != 0 {
            std::thread::yield_now();
            continue;
        }
        assert_eq!((delta.output, counters.output_blocks), (5, 5));
        assert_eq!((delta.input, counters.input_blocks), (5, 5));
        assert_eq!(
            (delta.no_frame, delta.raw_frame, delta.faults),
            (0, 0, 0),
            "the callback always has the frame it is processing"
        );
        return;
    }
    panic!("the process never went quiet for an exact counter window");
}

/// Negative: a plan whose DSP cannot process at all (a Chat voice without its
/// captured stem) counts faults only, one per frame, and no processed block.
#[test]
fn an_unprocessable_plan_counts_faults_not_blocks() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || {
            let mut plan = hardware_plan(PathMode::Processed);
            let mut chat = Target::new("send-chat", "Chat", TargetKind::Output, 2);
            chat.limiter.enabled = false;
            chat.gain.enabled = false;
            chat.suppression.enabled = true;
            plan.paths[1].target = chat;
            plan
        },
        |live| live.raw_frame == 3 && live.no_frame == 0,
        |pipeline, buffers| {
            for _ in 0..3 {
                block(pipeline, buffers);
            }
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    assert_eq!(
        (live.faults, counters.faults),
        (3, 3),
        "one per copied frame"
    );
    assert_eq!((live.output, counters.output_blocks), (0, 0));
    assert_eq!(
        live.raw_frame, 3,
        "each failed frame also delivers the copied slot: a fallback, counted apart from dspFaults"
    );
}

/// Negative: an incomplete stereo output is counted as skipped, not as a
/// processed block and not as a fault.
#[test]
fn an_incomplete_output_is_counted_skipped_not_processed() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Processed),
        |live| live.raw_frame == 0 && live.no_frame == 0,
        |pipeline, buffers| {
            unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
            let raw = buffers.input(0);
            pipeline.buffer(raw as usize, FRAMES, raw.cast());
            // Only one of the two output channels is requested.
            let left = buffers.output(0);
            pipeline.buffer(left as usize, FRAMES, left.cast());
            unsafe { pipeline.end(FRAMES, true) };
            pipeline.settle();
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    assert_eq!((live.skipped, counters.skipped_outputs), (1, 1));
    assert_eq!((live.output, live.faults), (0, 0));
}

/// A meter-only hardware output never counts as a processed block, whatever the
/// meter and count records it does produce.
#[test]
fn a_meter_only_output_is_not_a_processed_block() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Passthrough),
        |live| live.raw_frame == 0 && live.no_frame == 0,
        |pipeline, buffers| {
            for _ in 0..8 {
                block(pipeline, buffers);
            }
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    assert_eq!((live.output, counters.output_blocks), (0, 0));
    assert_eq!((live.faults, live.skipped), (0, 0));
    // Only the source is processed.
    assert_eq!((live.input, counters.input_blocks), (8, 8));
}

/// The report exposes the live view under its own name, next to the
/// retirement-only fields it must not be added to.
#[test]
fn the_audit_report_exposes_live_processing_counters() {
    let _exclusive = exclusive();
    let report = super::super::processing_counters();
    for key in ["scope", "inputBlocks", "outputBlocks", "dspFaults"] {
        assert!(report.get(key).is_some(), "missing {key}: {report}");
    }
    assert!(report["skippedOutputs"].is_u64());
    assert!(
        report["scope"]
            .as_str()
            .unwrap()
            .contains("retired pipelines")
    );
    let without = &report["hardwareOutputsWithoutResult"];
    assert!(without["noCopiedFrame"].is_u64());
    assert!(without["copiedFrameRaw"].is_u64());
    let note = without["note"].as_str().unwrap();
    assert!(note.contains("NOT the dspFaults error counter"), "{note}");
    assert!(note.contains("audibility not established"), "{note}");
    assert!(note.contains("qualification failure"), "{note}");
    assert!(note.contains("zero-initialised startup slot"), "{note}");
    // The labels must not claim that fallback deliveries are harmless.
    let json = super::super::processing_counters().to_string();
    for claim in [
        "not a fault",
        "not faults",
        "never a fault",
        "no changed audio",
        "mixer's own audio is what is heard",
        "not a dropout",
    ] {
        assert!(
            !json.contains(claim),
            "unjustified label in the report: {claim}"
        );
    }
}
