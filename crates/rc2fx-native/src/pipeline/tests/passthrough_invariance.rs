//! Regression tests ensuring mic suppression never delays or alters unrelated
//! `Passthrough` inputs.
//!
//! Regardless of what happens on the mic path (suppression toggled, worker
//! late, mic unrequested, callback failed, helper busy), a passthrough input
//! must always return the caller's own buffer pointer with its current samples
//! and report zero added latency.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stress {
    /// Normal callback: both sources requested and callback succeeds.
    None,
    /// Mixer skips requesting the mic buffer.
    MicSilent,
    /// Mixer callback reports failure.
    Failed,
    /// Helper is busy at `begin()`, so the frame handoff is refused.
    HelperBusy,
}

/// Processed mic (`in_1`) plus a stereo passthrough USB input (`in_2`/`in_3`).
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
/// `in_1` is the mic; `in_2` and `in_3` are the passthrough stereo pair
/// (borrowing `output(0)` as a third distinct live buffer).
fn build(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
    let mut registry = Registry::default();
    assert!(registry.register(1, buffers.input(0) as usize, b"in_1", 1));
    assert!(registry.register(1, buffers.input(1) as usize, b"in_2", 1));
    assert!(registry.register(1, buffers.output(0) as usize, b"in_3", 1));
    Pipeline::new(plan, &registry, 1).unwrap()
}
/// Unique per-frame, per-channel sample value so stale blocks can't match.
fn value(index: usize, channel: usize) -> f32 {
    let x = (index as u32)
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add((channel as u32 + 3).wrapping_mul(0x85eb_ca6b));
    ((x >> 8) & 0xffff) as f32 / 65_535.0 - 0.5
}
struct Seen {
    before: [[f32; FRAMES]; 2],
    after: [[f32; FRAMES]; 2],
}
fn read_back(pointers: [*mut f32; 2]) -> [[f32; FRAMES]; 2] {
    std::array::from_fn(|channel| {
        unsafe { std::slice::from_raw_parts(pointers[channel], FRAMES) }
            .try_into()
            .unwrap()
    })
}
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

#[test]
fn mic_suppression_does_not_delay_a_passthrough_input() {
    // Busy-helper frames emit probe events; serialize with event-counting tests.
    let _events = crate::probe::EVENT_TESTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Period 5 is coprime with METER_STRIDE (8) so meter reads hit every stress state.
    let stress_at = |index: usize| match index % 5 {
        1 => Stress::MicSilent,
        2 => Stress::Failed,
        3 => Stress::HelperBusy,
        _ => Stress::None,
    };
    let mut arms = Vec::new();
    for (suppressed, gauged) in [(false, false), (true, false), (false, true), (true, true)] {
        let plan = plan(suppressed);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = build(&plan, &buffers);
        pipeline.set_gauged(gauged);
        let mic = buffers.input(0);
        let passthrough = [buffers.input(1), buffers.output(0)];
        let mut seen = Vec::new();
        for index in 0..96 {
            let stress = stress_at(index);
            let frame = frame(&pipeline, mic, passthrough, index, stress);
            assert_current(&frame, index, stress);
            seen.push(frame);
        }
        let counters = pipeline.counters();
        assert_eq!(counters.skipped_outputs, 0);
        assert_eq!(
            controls[1].status().unwrap().meter.unwrap().latency_samples,
            0,
            "suppressed {suppressed}: the passthrough path reports no added latency"
        );
        // Sanity check that suppression is actually active on the mic path.
        if suppressed {
            let meter = controls[0].status().unwrap().meter.unwrap();
            assert!(
                meter.latency_samples > 0,
                "suppression enabled but the microphone reports no latency"
            );
        }
        if !suppressed {
            assert_eq!(counters.faults, 0);
        }
        arms.push(((suppressed, gauged), seen));
    }
    // Every arm must produce bit-identical passthrough samples before and after `end()`.
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

/// Even when the helper is busy for a sustained stretch and mic results never
/// arrive, the passthrough input stays current and keeps reporting zero latency.
#[test]
fn a_missing_mic_result_leaves_a_passthrough_input_current() {
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
    // Let the helper run again so the passthrough meter window publishes.
    for index in 24..72 {
        let frame = frame(&pipeline, mic, passthrough, index, Stress::None);
        assert_current(&frame, index, Stress::None);
    }
    assert_eq!(pipeline.counters().skipped_outputs, 0);
    assert_eq!(
        controls[1].status().unwrap().meter.unwrap().latency_samples,
        0
    );
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
