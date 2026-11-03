use super::*;
use std::sync::atomic::AtomicBool;

fn meter(frames: u32, value: f64, channels: u64) -> Meter {
    Meter {
        input_peak: value,
        output_peak: value / 2.0,
        sum_squares: (value / 2.0).powi(2) * u64::from(frames) as f64 * channels as f64,
        gain_reduction: 2.0,
        voice: 0.5,
        gate_open: 1.0,
        samples: u64::from(frames) * channels,
        clipped: 0,
    }
}
fn elapsed(wall: u64) -> Option<Elapsed> {
    Some(Elapsed::from_durations(Some(wall), Some(wall / 2)))
}
fn observed(start: Start) -> Option<Elapsed> {
    Some(Elapsed {
        start,
        ..Elapsed::from_durations(Some(10), Some(5))
    })
}

#[test]
fn cadence_separates_entry_gaps_from_execution_and_keeps_revision_continuity() {
    let mut slot = Slot::new(100, true).unwrap();
    {
        let (mut producer, mut consumer) = slot.split();
        for (revision, start) in [(1, 0), (2, 100), (2, 500), (2, 550)] {
            producer.record(
                Some(meter(2400, 0.2, 1)),
                revision,
                2400,
                observed(Start::At(start)),
            );
        }
        let value = consumer.drain_latest(2).unwrap().totals;
        assert_eq!(value.wall.max_ns, 10);
        assert_eq!(value.wall.overruns, 0);
        assert_eq!(value.cadence.expected_period_ns, 100);
        assert_eq!(value.cadence.attempts, 4);
        assert_eq!(value.cadence.baselines, 1);
        assert_eq!(value.cadence.intervals, 3);
        assert_eq!(value.cadence.total_ns, 550);
        assert_eq!(value.cadence.max_ns, 400);
        assert_eq!(value.cadence.above_period, 1);
        assert_eq!(value.cadence.above_two_periods, 1);
        assert_eq!(value.cadence.histogram, [1, 1, 0, 1, 0]);
    }
    // Returning the endpoint and opening a new registration starts a baseline,
    // rather than attributing offline time to an observed continuous interval.
    let (mut producer, _) = slot.split();
    producer.record(None, 3, 0, observed(Start::At(1_000_000)));
    let value = slot.final_snapshot().totals;
    assert_eq!(value.failed_callbacks, 1);
    assert_eq!(value.cadence.attempts, 5);
    assert_eq!(value.cadence.baselines, 2);
    assert_eq!(value.cadence.intervals, 3);
    assert_eq!(value.cadence.max_ns, 400);
}

#[test]
fn cadence_invalid_unavailable_and_unmeasured_starts_never_bridge_gaps() {
    let mut value = Cadence {
        expected_period_ns: 100,
        ..Cadence::default()
    };
    let mut previous = None;
    for start in [
        Start::At(0),
        Start::At(100),
        Start::At(100),
        Start::At(50),
        Start::At(49),
        Start::At(1000),
        Start::Invalid,
        Start::At(2000),
        Start::Unavailable,
        Start::At(3000),
        Start::Unmeasured,
        Start::At(4000),
    ] {
        value.record(start, &mut previous);
    }
    assert_eq!(value.attempts, 12);
    assert_eq!(value.intervals, 1);
    assert_eq!(value.baselines, 6);
    assert_eq!(value.unmeasured, 1);
    assert_eq!(value.unavailable, 1);
    assert_eq!(value.clock_errors, 3);
    assert_eq!(value.max_ns, 100);
    assert_eq!(
        value.intervals
            + value.baselines
            + value.unmeasured
            + value.unavailable
            + value.clock_errors,
        value.attempts
    );
}

#[test]
fn cadence_full_ring_and_final_partial_keep_cumulative_gap_and_saturation() {
    let mut slot = Slot::new(100, false).unwrap();
    let (mut producer, mut consumer) = slot.split();
    for start in (0..8).map(|n| n * 100).chain([10_000]) {
        producer.record(
            Some(meter(2400, 0.2, 1)),
            1,
            2400,
            observed(Start::At(start)),
        );
    }
    for _ in 0..CAPACITY {
        consumer.pop().unwrap();
    }
    producer.record(
        Some(meter(112, 0.2, 1)),
        1,
        112,
        observed(Start::At(10_100)),
    );
    let final_value = slot.final_snapshot();
    assert_eq!(final_value.totals.dropped_windows, 1);
    assert_eq!(final_value.totals.cadence.intervals, 9);
    assert_eq!(final_value.totals.cadence.max_ns, 9300);
    assert_eq!(final_value.totals.cadence.histogram, [0, 8, 0, 0, 1]);
    assert_eq!(final_value.partial_window.unwrap().frames, 112);
    let mut value = Cadence {
        expected_period_ns: u64::MAX,
        attempts: u64::MAX,
        intervals: u64::MAX,
        total_ns: 1,
        histogram: [0, u64::MAX, 0, 0, 0],
        ..Cadence::default()
    };
    value.record(Start::At(u64::MAX), &mut Some(0));
    assert!(value.saturated);
    assert_eq!(value.total_ns, u64::MAX);
    assert_eq!(value.intervals, u64::MAX);
    assert_eq!(value.histogram[1], u64::MAX);
    assert_eq!(value.above_period, 0);
}

#[test]
fn accumulation_matches_window_and_stereo_rms_without_losing_boundary_frames() {
    let mut slot = Slot::new(2_333_333, true).unwrap();
    let (mut producer, mut consumer) = slot.split();
    for _ in 0..21 {
        assert_eq!(
            producer.record(Some(meter(112, 0.4, 2)), 4, 112, elapsed(100)),
            RecordResult::Accumulated
        );
    }
    assert!(consumer.pop().is_none());
    assert_eq!(
        producer.record(Some(meter(112, 0.8, 2)), 4, 112, elapsed(200)),
        RecordResult::Published
    );
    let reading = consumer.pop().unwrap();
    assert_eq!(reading.window.frames, 2464);
    assert_eq!(reading.window.meter.samples, 4928);
    assert_eq!(reading.window.meter.input_peak, 0.8);
    assert_eq!(reading.window.meter.output_peak, 0.4);
    let rms = ((21.0 * 0.2_f64.powi(2) + 0.4_f64.powi(2)) / 22.0).sqrt();
    assert!((reading.window.rms() - rms).abs() < 1e-12);
    assert_eq!(reading.totals.callbacks, 22);
    assert_eq!(reading.totals.processed_frames, 2464);
    assert_eq!(reading.totals.wall.calls, 22);
    assert_eq!(reading.totals.wall.total_ns, 2300);
    assert_eq!(reading.totals.thread_cpu.unwrap().total_ns, 1150);
    assert!(slot.final_snapshot().partial_window.is_none());
}

#[test]
fn changed_revision_discards_partial_amplitudes_but_keeps_cumulative_counters() {
    let mut slot = Slot::new(1000, false).unwrap();
    let (mut producer, mut consumer) = slot.split();
    producer.record(Some(meter(2000, 0.9, 1)), 1, 2000, elapsed(100));
    producer.record(Some(meter(2400, 0.1, 1)), 2, 2400, elapsed(100));
    let reading = consumer.drain_latest(2).unwrap();
    assert_eq!(reading.window.frames, 2400);
    assert_eq!(reading.window.meter.input_peak, 0.1);
    assert_eq!(reading.totals.processed_frames, 4400);
    assert!(reading.totals.thread_cpu.is_none());
    producer.record(Some(meter(2400, 0.2, 1)), 2, 2400, elapsed(100));
    assert!(
        consumer.drain_latest(3).is_none(),
        "old amplitude must not be relabelled as new revision"
    );
    producer.record(Some(meter(112, 0.3, 1)), 3, 112, None);
    let terminal = slot.final_snapshot();
    assert_eq!(terminal.partial_window.unwrap().revision, 3);
    assert_eq!(terminal.partial_window.unwrap().frames, 112);
    assert_eq!(terminal.totals.callbacks, 4);
    assert_eq!(terminal.totals.unmeasured_callbacks, 1);
}

#[test]
fn ring_wrap_full_drop_and_quiescent_totals_preserve_rare_spikes() {
    let mut slot = Slot::new(100, true).unwrap();
    slot.queue.written.store(usize::MAX - 3, Ordering::Relaxed);
    slot.queue.read.store(usize::MAX - 3, Ordering::Relaxed);
    let (mut producer, mut consumer) = slot.split();
    for revision in 1..=8 {
        assert_eq!(
            producer.record(Some(meter(2400, 0.2, 1)), revision, 2400, elapsed(10)),
            RecordResult::Published
        );
    }
    assert_eq!(
        producer.record(Some(meter(2400, 0.2, 1)), 9, 2400, elapsed(400)),
        RecordResult::Dropped
    );
    for revision in 1..=8 {
        assert_eq!(consumer.pop().unwrap().window.revision, revision);
    }
    assert!(consumer.pop().is_none());
    producer.record(Some(meter(2400, 0.2, 1)), 10, 2400, elapsed(10));
    let recovered = consumer.pop().unwrap();
    assert_eq!(recovered.totals.dropped_windows, 1);
    assert_eq!(recovered.totals.wall.overruns, 1);
    assert_eq!(recovered.totals.wall.max_ns, 400);
    assert_eq!(recovered.totals.wall.histogram[7], 1);
    producer.record(Some(meter(1, 0.2, 1)), 10, 1, elapsed(900));
    let terminal = slot.final_snapshot();
    assert_eq!(terminal.totals.wall.max_ns, 900);
    assert_eq!(terminal.totals.wall.calls, 11);
    assert_eq!(terminal.totals.wall.overruns, 2);
    assert_eq!(terminal.partial_window.unwrap().frames, 1);
}

#[test]
fn timing_bins_errors_and_saturation_are_explicit() {
    let mut timing = Timing::default();
    for duration in [
        1, 10, 11, 20, 21, 40, 41, 80, 81, 120, 121, 160, 161, 320, 321,
    ] {
        timing.add(NonZeroU64::new(duration), 160);
    }
    assert_eq!(timing.histogram, [2, 2, 2, 2, 2, 2, 2, 1]);
    assert_eq!(timing.overruns, 3);
    timing.add(None, 160);
    assert_eq!(timing.calls, 15);
    assert_eq!(timing.clock_errors, 1);
    assert_eq!(timing.to_core().histogram, timing.histogram);
    timing.calls = u64::MAX;
    timing.total_ns = u64::MAX - 1;
    timing.overruns = u64::MAX;
    timing.histogram[7] = u64::MAX;
    timing.add(NonZeroU64::new(400), 160);
    assert!(timing.saturated);
    assert_eq!(timing.calls, u64::MAX);
    assert_eq!(timing.total_ns, u64::MAX);
    assert_eq!(timing.histogram[7], u64::MAX);
    assert_eq!(timing.overruns, u64::MAX);
    let mut large_budget = Timing::default();
    large_budget.add(NonZeroU64::new(u64::MAX), u64::MAX);
    assert_eq!(large_budget.histogram[5], 1);
    assert_eq!(large_budget.overruns, 0);
}

#[test]
fn failed_callbacks_clock_errors_and_invalid_meter_are_not_zero_successes() {
    let mut slot = Slot::new(100, true).unwrap();
    let (mut producer, _) = slot.split();
    producer.record(
        None,
        1,
        112,
        Some(Elapsed::from_durations(Some(0), Some(0))),
    );
    producer.record(
        Some(meter(112, 0.1, 1)),
        1,
        112,
        Some(Elapsed::from_durations(Some(10), Some(11))),
    );
    let mut bad = meter(112, 0.1, 1);
    bad.sum_squares = f64::NAN;
    assert_eq!(
        producer.record(Some(bad), 1, 112, None),
        RecordResult::Rejected
    );
    let totals = slot.final_snapshot().totals;
    assert_eq!(totals.callback_attempts, 3);
    assert_eq!(totals.callbacks, 1);
    assert_eq!(totals.processed_frames, 112);
    assert_eq!(totals.failed_callbacks, 1);
    assert_eq!(totals.invalid_meters, 1);
    assert_eq!(totals.unmeasured_callbacks, 1);
    assert_eq!(totals.wall.calls, 1);
    assert_eq!(totals.wall.clock_errors, 1);
    assert_eq!(totals.thread_cpu.unwrap().calls, 0);
    assert_eq!(totals.thread_cpu.unwrap().clock_errors, 2);
    assert!(delta(Some(10), Some(9)).is_none());
    assert!(delta(Some(10), Some(10)).is_none());
    assert!(delta(None, Some(10)).is_none());
    assert_eq!(delta(Some(0), Some(1)), Some(1));
}

#[test]
fn nonfinite_accumulation_and_counter_overflow_are_bounded() {
    let mut slot = Slot::new(100, false).unwrap();
    slot.accumulator.totals.processed_frames = u64::MAX - 1;
    let (mut producer, _) = slot.split();
    let mut huge = meter(1, 0.2, 1);
    huge.sum_squares = f64::MAX;
    assert_eq!(
        producer.record(Some(huge), 1, 1, None),
        RecordResult::Accumulated
    );
    assert_eq!(
        producer.record(Some(huge), 1, 1, None),
        RecordResult::Rejected
    );
    assert_eq!(
        producer.record(Some(meter(2, 0.2, 1)), 1, 2, None),
        RecordResult::Accumulated
    );
    let terminal = slot.final_snapshot();
    assert_eq!(terminal.totals.processed_frames, u64::MAX);
    assert!(terminal.totals.saturated);
    assert_eq!(terminal.totals.invalid_meters, 1);
    assert!(
        terminal
            .partial_window
            .unwrap()
            .meter
            .sum_squares
            .is_finite()
    );
    assert!(Slot::new(0, false).is_err());
}

#[test]
fn concurrent_copy_ownership_never_tears_and_drop_count_is_exact_after_join() {
    const COUNT: u64 = 50_000;
    let mut slot = Slot::new(100, true).unwrap();
    let (mut producer, mut consumer) = slot.split();
    let finished = AtomicBool::new(false);
    let received = std::thread::scope(|scope| {
        scope.spawn(|| {
            for revision in 1..=COUNT {
                producer.record(
                    Some(meter(2400, revision as f64, 2)),
                    revision,
                    2400,
                    elapsed(revision),
                );
            }
            finished.store(true, Ordering::Release);
        });
        let mut received = 0;
        let mut last = 0;
        loop {
            if let Some(reading) = consumer.pop() {
                let revision = reading.window.revision;
                assert!(revision > last);
                assert_eq!(reading.window.meter.input_peak, revision as f64);
                assert_eq!(reading.window.meter.output_peak, revision as f64 / 2.0);
                assert_eq!(reading.window.meter.samples, 4800);
                assert_eq!(reading.totals.callbacks, revision);
                assert_eq!(reading.totals.wall.calls, revision);
                assert_eq!(reading.totals.wall.max_ns, revision);
                last = revision;
                received += 1;
            } else if finished.load(Ordering::Acquire) {
                // Acquire completion before a final queue check, covering a
                // publication that followed our earlier empty observation.
                if let Some(reading) = consumer.pop() {
                    assert!(reading.window.revision > last);
                    last = reading.window.revision;
                    received += 1;
                } else {
                    break;
                }
            } else {
                std::thread::yield_now();
            }
        }
        received
    });
    let totals = slot.final_snapshot().totals;
    assert_eq!(totals.callbacks, COUNT);
    assert_eq!(totals.dropped_windows + received, COUNT);
    assert_eq!(totals.wall.max_ns, COUNT);
}

#[test]
fn clocks_are_explicitly_available_or_unavailable() {
    let clock = Clock::probe(true);
    let started = clock.start();
    let mut sum = 0_u64;
    for index in 0..10_000 {
        sum = std::hint::black_box(sum.wrapping_add(index));
    }
    std::hint::black_box(sum);
    let elapsed = clock.finish(started);
    #[cfg(target_os = "linux")]
    {
        assert!(clock.info().wall_resolution_ns.is_some());
        assert!(clock.info().thread_cpu_resolution_ns.is_some());
        assert!(elapsed.wall_ns().is_some());
        assert!(elapsed.thread_cpu_ns().is_some());
        assert!(elapsed.thread_cpu_ns() <= elapsed.wall_ns());
        assert!(matches!(elapsed.start, Start::At(_)));
        let invalid = clock.finish(Stamp {
            wall: None,
            cpu: None,
            _same_thread: PhantomData,
        });
        assert_eq!(invalid.start, Start::Invalid);
        assert!(
            time_ns(libc::timespec {
                tv_sec: -1,
                tv_nsec: 0
            })
            .is_none()
        );
        assert!(
            time_ns(libc::timespec {
                tv_sec: 1,
                tv_nsec: 1_000_000_000
            })
            .is_none()
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(clock.info(), ClockInfo::default());
        assert_eq!(elapsed.wall_ns(), None);
        assert_eq!(elapsed.thread_cpu_ns(), None);
        assert_eq!(elapsed.start, Start::Unavailable);
    }
}
