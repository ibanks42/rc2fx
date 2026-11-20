//! Tests for the process-global processing counters (`pipeline::processing_counters`),
//! cross-checked against each pipeline's local `Counters`.
use super::*;
use std::sync::{Mutex, atomic::Ordering::Relaxed};

/// Serializes tests that measure exact deltas on the process-global counters.
static COUNTER_TESTS: Mutex<()> = Mutex::new(());
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    COUNTER_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

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
fn agrees(live: &Live, counters: &Counters) -> bool {
    live.input == counters.input_blocks
        && live.output == counters.output_blocks
        && live.faults == counters.faults
        && live.skipped == counters.skipped_outputs
}
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
    // Reading per-pipeline counters must not touch the live globals.
    pipeline.counters()
}

/// Runs `script` on a fresh pipeline, retrying if concurrent tests outside
/// `COUNTER_TESTS` bumped the process-global counters during the window.
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
    assert_eq!((live.input, live.output), (5, 5));
    assert_eq!(
        (counters.input_blocks, counters.output_blocks),
        (5, 5),
        "the same events, per pipeline"
    );
    assert_eq!((live.faults, live.skipped), (0, 0));
}

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

#[test]
fn an_incomplete_output_is_counted_skipped_not_processed() {
    let _exclusive = exclusive();
    let (live, counters) = measured(
        || hardware_plan(PathMode::Processed),
        |pipeline, buffers| {
            unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
            let raw = buffers.input(0);
            pipeline.buffer(raw as usize, FRAMES, raw.cast());
            // Request only the left output channel.
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
    assert_eq!((live.input, counters.input_blocks), (8, 8));
}

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
