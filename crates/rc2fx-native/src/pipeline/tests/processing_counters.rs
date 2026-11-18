//! The audit's live processing counters (`pipeline::processing_counters`).
//!
//! These tests pin the live counters to their increment sites, checked against
//! the per-pipeline `Counters` (`Pipeline::counters`, test only, waits for the
//! helper): one increment per actual block or event, faults kept apart from
//! successes, incomplete frames apart from both, startup warm-up not
//! misreported as a failure, and reading the per-pipeline counters adding
//! nothing.
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

/// The four live counters, as one comparable value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Live {
    input: u64,
    output: u64,
    faults: u64,
    skipped: u64,
}
fn live() -> Live {
    Live {
        input: LIVE_INPUT_BLOCKS.load(Relaxed),
        output: LIVE_OUTPUT_BLOCKS.load(Relaxed),
        faults: LIVE_FAULTS.load(Relaxed),
        skipped: LIVE_SKIPPED_OUTPUTS.load(Relaxed),
    }
}
fn delta(before: Live, after: Live) -> Live {
    Live {
        input: after.input - before.input,
        output: after.output - before.output,
        faults: after.faults - before.faults,
        skipped: after.skipped - before.skipped,
    }
}
/// The counters both sides own moved identically: other tests' pipelines
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
/// One callback with both channels requested, the mixer's own processing in the
/// output buffers, and `Pipeline::counters` read afterwards.
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
    // Reading the per-pipeline counters must not touch the live mirrors.
    pipeline.counters()
}

/// Run `script` on a fresh pipeline, retrying until no other context's events
/// fall inside the same window (they only inflate the live side). Under a
/// pathological schedule the caller reports the impasse instead of failing.
fn measured<F>(make: impl Fn() -> Plan, script: F) -> Option<(Live, Counters)>
where
    F: Fn(&Pipeline, &Buffers),
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
        if agrees(&delta, &counters) {
            return Some((delta, counters));
        }
        std::thread::yield_now();
    }
    None
}

/// Positive: one increment per processed block, on both sides of the pipe, with
/// reading the per-pipeline counters adding nothing (it happens after every
/// frame here).
/// The callback processes every output frame it is handed, from the first one.
#[test]
fn live_counters_track_each_processed_block_once() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Processed),
        |pipeline, buffers| {
            for _ in 0..5 {
                block(pipeline, buffers);
            }
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    // One processed block per frame on each side, the first frame included.
    assert_eq!((live.input, live.output), (5, 5));
    assert_eq!(
        (counters.input_blocks, counters.output_blocks),
        (5, 5),
        "the same events, per pipeline"
    );
    assert_eq!((live.faults, live.skipped), (0, 0));
}

/// Negative: a plan whose DSP cannot process at all (a Chat voice without its
/// captured stem) counts a fault, latched once for the path, and no processed
/// block.
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
        |pipeline, buffers| {
            for _ in 0..3 {
                block(pipeline, buffers);
            }
        },
    )
    .expect("the live counters must agree with a pipeline's own counters");
    assert_eq!(
        (live.faults, counters.faults),
        (1, 1),
        "the callback's port latches its fault once"
    );
    assert_eq!((live.output, counters.output_blocks), (0, 0));
}

/// Negative: an incomplete stereo output is counted as skipped, not as a
/// processed block and not as a fault.
#[test]
fn an_incomplete_output_is_counted_skipped_not_processed() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Processed),
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

/// The report exposes the live view under its own name, including retired
/// pipelines.
#[test]
fn the_audit_report_exposes_live_processing_counters() {
    let _exclusive = exclusive();
    let before = live();
    let report = super::super::processing_counters();
    let after = live();
    for (key, low, high) in [
        ("inputBlocks", before.input, after.input),
        ("outputBlocks", before.output, after.output),
        ("dspFaults", before.faults, after.faults),
        ("skippedOutputs", before.skipped, after.skipped),
    ] {
        let value = report[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key}: {report}"));
        assert!(
            (low..=high).contains(&value),
            "{key}: {value} not in {low}..={high}"
        );
    }
    assert!(
        report["scope"]
            .as_str()
            .unwrap()
            .contains("retired pipelines")
    );
}
