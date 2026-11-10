//! The invariant: enabling the microphone's suppression must not delay
//! unrelated channels.
//!
//! A Passthrough input is never processed by anything, so whatever happens on
//! the microphone side — suppression on or off, its worker late, its block
//! never requested, its result missing, the helper busy — the mixer must get
//! its own current block back: the same pointer it passed, the samples it just
//! wrote, and no reported latency of its own. Nothing here assumes a global
//! delay: the microphone path is a real target with real suppression (and a
//! real worker this untimed test never waits for), observed through the same
//! callbacks the mixer uses.
use super::*;

/// How the microphone side is treated this frame, to show the unrelated path
/// does not depend on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stress {
    /// Both sources requested; the mixer's callback succeeds.
    None,
    /// The mixer never requests the microphone's buffer.
    MicSilent,
    /// The mixer's own callback reports failure.
    Failed,
    /// The helper is neither idle nor queued when the callback begins, so this
    /// frame's handoff is refused: the microphone's result cannot arrive.
    HelperBusy,
}

/// mic-1 (processed, suppression `suppressed`) and a stereo Passthrough input
/// standing in for a USB input heard as it arrives.
fn plan(suppressed: bool) -> Plan {
    let mut mic = Target::new("mic-1", "Mic 1", TargetKind::Mic, 1);
    mic.limiter.enabled = false;
    mic.gain.enabled = false;
    mic.eq.enabled = false;
    mic.suppression.enabled = suppressed;
    let mut main = Target::new("usb-main", "USB Main", TargetKind::Playback, 2);
    main.limiter.enabled = false;
    main.gain.enabled = false;
    main.eq.enabled = false;
    main.highpass.enabled = false;
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
                target: mic,
                mode: PathMode::Processed,
                hardware: false,
            },
            PathPlan {
                direction: Direction::Input,
                ports: vec!["in_2".into(), "in_3".into()],
                target: main,
                mode: PathMode::Passthrough,
                hardware: false,
            },
        ],
    }
}
/// in_1: microphone. in_2, in_3: the passthrough input's two channels (the
/// third only has to be its own live buffer, so one of the test's serves).
fn build(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
    let mut registry = Registry::default();
    assert!(registry.register(1, buffers.input(0) as usize, b"in_1", 1));
    assert!(registry.register(1, buffers.input(1) as usize, b"in_2", 1));
    assert!(registry.register(1, buffers.output(0) as usize, b"in_3", 1));
    Pipeline::with_ownership(plan, &registry, 1, false).unwrap()
}
/// A different block for every frame and channel, so a stale block or a stale
/// pointer cannot pass for the current one.
fn value(index: usize, channel: usize) -> f32 {
    let x = (index as u32)
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add((channel as u32 + 3).wrapping_mul(0x85eb_ca6b));
    ((x >> 8) & 0xffff) as f32 / 65_535.0 - 0.5
}
/// What the mixer saw of the passthrough path this frame.
struct Seen {
    /// Read back through the pointer the path returned, inside the callback.
    before: [[f32; FRAMES]; 2],
    /// Read back through the same pointers after the callback returned.
    after: [[f32; FRAMES]; 2],
}
fn read_back(pointers: [*mut f32; 2]) -> [[f32; FRAMES]; 2] {
    std::array::from_fn(|channel| {
        unsafe { std::slice::from_raw_parts(pointers[channel], FRAMES) }
            .try_into()
            .unwrap()
    })
}
/// One callback: the microphone's block, the passthrough path's own current
/// block, the buffer requests, and the mixer's own processing.
fn frame(
    pipeline: &Pipeline,
    mic: *mut f32,
    passthrough: [*mut f32; 2],
    index: usize,
    stress: Stress,
) -> Seen {
    let shared = &pipeline.helper.shared;
    if stress == Stress::HelperBusy {
        shared.state.store(QUEUED + 1, Ordering::Release);
    }
    unsafe {
        std::slice::from_raw_parts_mut(mic, FRAMES).fill(value(index, 7));
        for (channel, &pointer) in passthrough.iter().enumerate() {
            std::slice::from_raw_parts_mut(pointer, FRAMES).fill(value(index, channel));
        }
        pipeline.begin(FRAMES, test_buffer, 0);
    }
    if stress != Stress::MicSilent {
        pipeline.buffer(mic as usize, FRAMES, mic.cast());
    }
    let mut before = [[0.0; FRAMES]; 2];
    for (channel, &pointer) in passthrough.iter().enumerate() {
        let got = pipeline
            .buffer(pointer as usize, FRAMES, pointer.cast())
            .cast::<f32>();
        assert_eq!(
            got as usize, pointer as usize,
            "frame {index}, {stress:?}: the passthrough path must return the mixer's own pointer"
        );
        before[channel] = unsafe { std::slice::from_raw_parts(got, FRAMES) }
            .try_into()
            .unwrap();
    }
    unsafe { pipeline.end(FRAMES, stress != Stress::Failed) };
    let after = read_back(passthrough);
    if stress == Stress::HelperBusy {
        shared.state.store(IDLE, Ordering::Release);
    }
    pipeline.settle();
    Seen { before, after }
}
fn assert_current(seen: &Seen, index: usize, stress: Stress) {
    for channel in 0..2 {
        let current = value(index, channel);
        for (when, samples) in [("before end", &seen.before), ("after end", &seen.after)] {
            for sample in &samples[channel] {
                assert_eq!(
                    sample.to_bits(),
                    current.to_bits(),
                    "frame {index}, {stress:?}, channel {channel}, {when}: {sample} is not this block's {current}"
                );
            }
        }
    }
}

/// The regression: suppression on the microphone, the helper's state and the
/// microphone's own frame cannot change one sample of a Passthrough input.
#[test]
fn mic_suppression_does_not_delay_a_passthrough_input() {
    // Busy-helper frames record probe events; event-counting tests must not
    // see them.
    let _events = crate::probe::EVENT_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // A pattern coprime with the meter stride (8), so meter reads of the
    // passthrough path do not always coincide with the same microphone state.
    let stress_at = |index: usize| match index % 5 {
        1 => Stress::MicSilent,
        2 => Stress::Failed,
        3 => Stress::HelperBusy,
        _ => Stress::None,
    };
    let mut arms = Vec::new();
    // Each with the bench gauges off and on: they only measure, so they are
    // one more thing that must not move a sample of the unrelated path.
    for (suppressed, gauged) in [(false, false), (true, false), (false, true), (true, true)] {
        let plan = plan(suppressed);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = build(&plan, &buffers);
        pipeline.set_gauged(gauged);
        let mic = buffers.input(0);
        let passthrough = [buffers.input(1), buffers.output(0)];
        let mut seen = Vec::new();
        // Long enough for several telemetry windows, including frames on
        // which the microphone could not be processed at all.
        for index in 0..96 {
            let stress = stress_at(index);
            let frame = frame(&pipeline, mic, passthrough, index, stress);
            assert_current(&frame, index, stress);
            seen.push(frame);
        }
        // Nothing on the microphone's side moved the passthrough path: it
        // processed nothing, skipped nothing, and is reported with no
        // latency of its own.
        let counters = pipeline.counters();
        assert_eq!(counters.skipped_outputs, 0);
        assert_eq!(
            controls[1].status().unwrap().meter.unwrap().latency_samples,
            0,
            "suppressed {suppressed}: the passthrough path reports no added latency"
        );
        // With suppression on, the microphone's own path does carry its
        // suppression latency: the fixture really is suppressing.
        if suppressed {
            let meter = controls[0].status().unwrap().meter.unwrap();
            assert!(
                meter.latency_samples > 0,
                "suppression enabled but the microphone reports no latency"
            );
        }
        // Without suppression nothing on the microphone's side faults; with
        // it, a worker's own late or missing result is not the passthrough
        // path's business and is not asserted here.
        if !suppressed {
            assert_eq!(counters.faults, 0);
        }
        arms.push(((suppressed, gauged), seen));
    }
    // Bit for bit, the unrelated path saw the same thing in every arm: the same
    // block, through the same pointer, before and after the callback.
    let (_, baseline) = &arms[0];
    for ((suppressed, gauged), seen) in &arms[1..] {
        for (index, (frame, baseline)) in seen.iter().zip(baseline).enumerate() {
            for channel in 0..2 {
                for (when, samples, expected) in [
                    ("before end", &frame.before, &baseline.before),
                    ("after end", &frame.after, &baseline.after),
                ] {
                    assert_eq!(
                        samples[channel]
                            .iter()
                            .map(|v| v.to_bits())
                            .collect::<Vec<_>>(),
                        expected[channel]
                            .iter()
                            .map(|v| v.to_bits())
                            .collect::<Vec<_>>(),
                        "suppressed {suppressed}, gauged {gauged}: frame {index}, channel {channel}, {when}"
                    );
                }
            }
        }
    }
}

/// A run of frames on which the microphone's result can never arrive: the
/// helper is never handed the frame and the microphone's block is never
/// requested. The passthrough input stays current throughout, and once the
/// helper runs again the path is still reported with no latency.
#[test]
fn a_missing_mic_result_leaves_a_passthrough_input_current() {
    // Busy-helper frames record probe events; event-counting tests must not
    // see them.
    let _events = crate::probe::EVENT_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let plan = plan(true);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers);
    let mic = buffers.input(0);
    let passthrough = [buffers.input(1), buffers.output(0)];
    for index in 0..24 {
        let frame = frame(&pipeline, mic, passthrough, index, Stress::HelperBusy);
        assert_current(&frame, index, Stress::HelperBusy);
    }
    // The helper runs again: the passthrough path's own meter arrives, still
    // with no latency of its own.
    for index in 24..72 {
        let frame = frame(&pipeline, mic, passthrough, index, Stress::None);
        assert_current(&frame, index, Stress::None);
    }
    assert_eq!(pipeline.counters().skipped_outputs, 0);
    assert_eq!(
        controls[1].status().unwrap().meter.unwrap().latency_samples,
        0
    );
    // The passthrough path has no helper work of its own beyond metering: its
    // reader stays with the helper and is never processed.
    let front = unsafe { &*pipeline.front.get() };
    let handoff = unsafe { &*pipeline.handoff.get() };
    assert_eq!(front.ports[1].owner, Owner::Helper);
    let work = handoff.work[1]
        .as_ref()
        .expect("its reader stays with the helper");
    assert_eq!(
        work.done, [false; 2],
        "the passthrough path never produces a processed frame"
    );
}
