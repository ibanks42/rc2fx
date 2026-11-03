use super::*;
use std::sync::{Condvar, Mutex};

mod endpoint_bench;
mod lead_phase;
struct Identity;
impl Model for Identity {
    fn process(&mut self, _: &mut [f32]) -> Result<f32, Error> {
        Ok(0.5)
    }
    fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
/// Generic test geometry: the fixed model frame and delay with a scheduling
/// allowance, without constructing a model.
fn geometry() -> Geometry {
    Geometry {
        sample_rate: 48_000,
        frame_samples: 240,
        intrinsic_delay_samples: 120,
        worker_lead_samples: 320,
    }
}
fn endpoint() -> (Worker, Consumer, Producer) {
    let (input, consumer) = queue::channel();
    let (producer, output) = queue::channel();
    (
        Worker {
            geometry: geometry(),
            shared: Arc::new(Shared::default()),
            input,
            output,
            thread: None,
            assembling: Frame::default(),
            playing: Frame::default(),
            pending: None,
            phase: 0,
            frame: 0,
            sample: 0,
            dry: [0.; MAX_SUPPRESSION_DELAY],
            dry_position: 0,
            voice: None,
            finished: false,
            retirement_mode: None,
            retirement_timed_out: false,
        },
        consumer,
        producer,
    )
}
#[test]
fn geometry_and_rejection() {
    for lookahead in [0, 1] {
        let g = Geometry::for_lookahead(lookahead).unwrap();
        assert!(g.frame_samples <= queue::MAX_FRAME);
        assert!(g.latency_samples() <= MAX_SUPPRESSION_DELAY);
    }
    let la0 = Geometry::for_lookahead(0).unwrap();
    assert_eq!((la0.frame_samples, la0.intrinsic_delay_samples), (240, 120));
    assert_eq!(la0.worker_lead_samples, 320);
    assert_eq!(la0.latency_samples(), 680);
    let la1 = Geometry::for_lookahead(1).unwrap();
    assert_eq!((la1.frame_samples, la1.intrinsic_delay_samples), (240, 360));
    assert_eq!(la1.latency_samples(), 920);
    assert_eq!(Geometry::for_lookahead(2), Err(Error::ModelGeometry));
    let invalid = [
        WorkerOptions {
            cpu_mask: Some(0),
            realtime_priority: None,
            nice: None,
        },
        WorkerOptions {
            cpu_mask: None,
            realtime_priority: Some(21),
            nice: None,
        },
        WorkerOptions {
            cpu_mask: None,
            realtime_priority: Some(0),
            nice: None,
        },
        WorkerOptions {
            cpu_mask: None,
            realtime_priority: None,
            nice: Some(20),
        },
        WorkerOptions {
            cpu_mask: None,
            realtime_priority: None,
            nice: Some(-21),
        },
        // One scheduling class at a time.
        WorkerOptions {
            cpu_mask: None,
            realtime_priority: Some(5),
            nice: Some(0),
        },
    ];
    for options in invalid {
        assert_eq!(
            scheduling::validate(options),
            Err(Error::Scheduling),
            "{options:?}"
        );
    }
    #[cfg(target_os = "linux")]
    assert_eq!(scheduling::validate(crate::PRODUCTION_WORKER), Ok(()));
}
#[test]
fn inline_endpoint_removes_only_the_worker_lead() {
    let inline = Inline::new(0).unwrap();
    assert_eq!(inline.geometry().worker_lead_samples, 0);
    assert_eq!(inline.geometry().frame_samples, 240);
    assert_eq!(inline.geometry().intrinsic_delay_samples, 120);
    assert_eq!(inline.latency_samples(), 360);
}
#[test]
fn exact_dry_delay_when_late_or_failed_in_arbitrary_chunks() {
    {
        let (mut w, _, _) = endpoint();
        let delay = w.latency_samples();
        let samples: Vec<f32> = (0..6000).map(|i| i as f32 / 8192.).collect();
        let mut i = 0;
        for chunk in [112, 1, 479, 17, 1024].into_iter().cycle() {
            for _ in 0..chunk.min(samples.len() - i) {
                if i == 3000 {
                    w.shared.failed.store(true, Ordering::Release);
                }
                assert_eq!(
                    w.process_sample(samples[i], 1.).to_bits(),
                    if i >= delay { samples[i - delay] } else { 0. }.to_bits()
                );
                i += 1;
            }
            if i == samples.len() {
                break;
            }
        }
        assert!(w.telemetry().snapshot().missed_deadlines > 0);
        assert!(w.telemetry().snapshot().input_drops > 0);
    }
}

#[test]
fn la0_selected_timing_has_no_extra_queued_frame() {
    let (mut worker, mut input, mut output) = endpoint();
    let filter = rc2fx_dfn::DeepFilter::new().unwrap();
    assert_eq!(worker.geometry.frame_samples, filter.frame());
    assert_eq!(worker.geometry.intrinsic_delay_samples, filter.delay());
    assert_eq!(filter.lookahead(), 0);
    assert_eq!(filter.delay(), 120);
    assert_eq!(worker.geometry.worker_lead_samples, 320);
    let frame_samples = filter.frame();
    let latency = frame_samples + 120 + 320;
    assert_eq!(worker.latency_samples(), latency);
    // 680 suppression samples plus one native 112-sample helper stage are
    // 792 samples (16.5 ms at 48 kHz). This is configured digital-path
    // arithmetic, not measured physical end-to-end latency.
    assert_eq!(latency + 112, 792);
    for _ in 0..frame_samples {
        assert_eq!(worker.process_sample(0., 1.), 0.);
    }
    let mut frame = input.pop().unwrap();
    // A model result with its intrinsic 120-sample offset. The worker must
    // play index 0 at frame_samples+320, not defer it by another queued frame.
    frame.samples.fill(0.);
    frame.samples[filter.delay()] = 0.5;
    frame.voice = 0.75;
    assert!(output.push(&frame));
    for at in frame_samples..=latency {
        let sample = worker.process_sample(0., 1.);
        assert_eq!(sample, if at == latency { 0.5 } else { 0. }, "sample {at}");
        assert_eq!(
            worker.voice(),
            if at >= frame_samples + 320 {
                Some(0.75)
            } else {
                None
            }
        );
    }
    assert_eq!(worker.telemetry().snapshot().missed_deadlines, 0);
}

/// Deterministic delivery, not a real-time qualification: drive the real model
/// immediately when an input frame is submitted, then consume at the configured
/// deadline. Covers many native-112/model-frame phases and lead > frame.
#[test]
fn la0_selected_stream_delivers_every_processed_sample_without_queue_faults() {
    let (mut worker, mut input, mut output) = endpoint();
    let mut model = VendorModel::new(0).unwrap();
    assert_eq!(model.geometry(), worker.geometry());
    let frame_samples = worker.geometry.frame_samples;
    let start = frame_samples + worker.geometry.worker_lead_samples;
    let samples: Vec<f32> = (0..112 * 80)
        .map(|i| {
            if i == 0 {
                0.5
            } else {
                (i as f32 * 0.071).sin() * 0.1
            }
        })
        .collect();
    let mut enhanced: Vec<f32> = Vec::new();
    for (block, chunk) in samples.chunks(112).enumerate() {
        for (offset, &sample) in chunk.iter().enumerate() {
            let at = block * 112 + offset;
            let actual = worker.process_sample(sample, 1.);
            let dry = if at >= worker.latency_samples() {
                samples[at - worker.latency_samples()]
            } else {
                0.
            };
            if at >= start {
                let wet = enhanced[at - start];
                assert_eq!(
                    actual.to_bits(),
                    (dry + (wet - dry)).to_bits(),
                    "sample {at}"
                );
                assert!(worker.voice().is_some(), "dry fallback at sample {at}");
            } else {
                assert_eq!(actual, 0.);
            }
            if let Some(mut frame) = input.pop() {
                frame.voice = model.process(&mut frame.samples[..frame_samples]).unwrap();
                enhanced.extend_from_slice(&frame.samples[..frame_samples]);
                assert!(output.push(&frame));
            }
        }
    }
    let snapshot = worker.telemetry().snapshot();
    assert_eq!(snapshot.missed_deadlines, 0);
    assert_eq!(snapshot.late_results, 0);
    assert_eq!(snapshot.stale_results, 0);
    assert_eq!(snapshot.input_drops, 0);
    assert_eq!(snapshot.output_drops, 0);
    assert_eq!(snapshot.allowance_overruns, 0);
    assert!(!snapshot.failed);
}

#[test]
fn a_late_result_serves_the_rest_of_its_frame() {
    let (mut worker, mut input, mut output) = endpoint();
    let start = worker.geometry.frame_samples + worker.geometry.worker_lead_samples;
    // Frame 0 plays from sample 560; its result lands 46 samples late.
    for _ in 0..start + 46 {
        assert_eq!(worker.process_sample(0.0, 1.0), 0.0);
        assert_eq!(worker.voice(), None);
    }
    let mut frame = input.pop().expect("assembled frame must be available");
    frame.samples.fill(0.25);
    frame.voice = 0.5;
    assert!(output.push(&frame));
    for _ in start + 46..start + worker.geometry.frame_samples {
        assert_eq!(worker.process_sample(0.0, 1.0), 0.25);
        assert_eq!(worker.voice(), Some(0.5));
    }
    let snapshot = worker.telemetry().snapshot();
    assert_eq!(snapshot.missed_deadlines, 1);
    assert_eq!(snapshot.late_results, 1);
    // The next frame is missing outright and stays dry to its end.
    for _ in 0..worker.geometry.frame_samples {
        assert_eq!(worker.process_sample(0.0, 1.0), 0.0);
        assert_eq!(worker.voice(), None);
    }
    let snapshot = worker.telemetry().snapshot();
    assert_eq!(snapshot.missed_deadlines, 2);
    assert_eq!(snapshot.late_results, 1);
}
#[test]
fn stale_invalid_and_future_results_are_distinct() {
    let (mut w, _, mut out) = endpoint();
    for (sequence, valid) in [(0, true), (1, false), (3, true)] {
        assert!(out.push(&Frame {
            sequence,
            valid,
            ..Frame::default()
        }));
    }
    assert!(w.choose(1));
    assert!(!w.playing.valid);
    assert_eq!(w.telemetry().snapshot().stale_results, 1);
    assert!(!w.choose(2));
    assert_eq!(w.output.sequence(), Some(3));
    assert!(w.choose(3));
    assert!(w.playing.valid);
}
#[test]
fn recovery_invalidates_model_history_and_then_recovers() {
    {
        let valid = [true, false, true, true];
        let (mut input, consumer) = queue::channel();
        let (producer, mut output) = queue::channel();
        for sequence in [0, 3, 4, 5] {
            assert!(input.push(&Frame {
                sequence,
                valid: true,
                ..Frame::default()
            }));
        }
        let shared = Shared::default();
        shared.stop.store(2, Ordering::Release);
        run(Box::new(Identity), consumer, producer, &shared, geometry());
        for expected in valid {
            let frame = output.pop().unwrap();
            assert_eq!(frame.valid, expected);
            assert_eq!(
                frame.voice, 0.5,
                "the model's voice score travels with its frame"
            );
        }
        assert_eq!(shared.completed.load(Ordering::Acquire), 4);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn worker_loop_capture_preserves_queue_depth_predecessors_and_counters() {
    let (mut input, consumer) = queue::channel();
    let (producer, mut output) = queue::channel();
    for sequence in 0..4 {
        assert!(input.push(&Frame {
            sequence,
            submitted_ns: metrics::now(false),
            valid: true,
            ..Frame::default()
        }));
    }
    let shared = Arc::new(Shared::default());
    let telemetry = Telemetry(shared.clone());
    shared.missed.store(7, Ordering::Relaxed);
    assert!(telemetry.arm_capture(1));
    assert_eq!(
        telemetry.snapshot().missed_deadlines,
        7,
        "arming is not a counter reset"
    );
    shared.stop.store(2, Ordering::Release);
    run(Box::new(Identity), consumer, producer, &shared, geometry());
    let Recorded::Stable(window) = telemetry.capture_window() else {
        panic!("capture")
    };
    assert_eq!(window.metadata.state, CaptureState::Stopped);
    assert_eq!(window.frames.len(), 4);
    assert_eq!(
        window
            .frames
            .iter()
            .map(|f| f.input_pending_after_pop)
            .collect::<Vec<_>>(),
        [3, 2, 1, 0]
    );
    for (i, frame) in window.frames.iter().enumerate() {
        assert_eq!(frame.sequence, i as u64);
        assert!(frame.output_published);
        assert!(frame.submitted_ns <= frame.started_ns);
        assert!(frame.started_ns <= frame.finished_ns);
        assert!(frame.finished_ns <= frame.published_ns);
        if i > 0 {
            assert_eq!(frame.previous_sequence, Some((i - 1) as u64));
            assert_eq!(
                frame.previous_published_ns,
                window.frames[i - 1].published_ns
            );
        }
        assert_eq!(output.pop().unwrap().sequence, i as u64);
    }
    assert_eq!(telemetry.snapshot().completed_frames, 4);
    assert_eq!(telemetry.snapshot().missed_deadlines, 7);
    assert_eq!(telemetry.snapshot().output_drops, 0);
}

#[test]
fn actual_endpoint_miss_triggers_the_acknowledged_capture_epoch() {
    let (mut worker, _, _) = endpoint();
    let shared = worker.shared.clone();
    assert!(worker.telemetry().arm_capture(1));
    let mut recorder = capture::Recorder::new(&shared.capture);
    recorder.begin(metrics::now(false));
    for _ in 0..750 {
        worker.process_sample(0.0, 1.0);
    }
    recorder.record(CaptureFrame {
        sequence: 0,
        ..CaptureFrame::default()
    });
    let Recorded::Stable(window) = worker.telemetry().capture_window() else {
        panic!("capture")
    };
    assert_eq!(window.metadata.state, CaptureState::Triggered);
    assert_eq!(window.metadata.trigger.unwrap().sequence, 0);
    assert_eq!(worker.telemetry().snapshot().missed_deadlines, 1);
}

#[cfg(target_os = "linux")]
#[test]
fn queued_time_is_included_in_turnaround_and_missing_stamps_are_errors() {
    let (mut input, consumer) = queue::channel();
    let (producer, mut output) = queue::channel();
    let submitted = metrics::now(false)
        .unwrap()
        .checked_sub(20_000_000)
        .unwrap();
    for (sequence, submitted_ns) in [(0, Some(submitted)), (1, None)] {
        assert!(input.push(&Frame {
            sequence,
            submitted_ns,
            valid: true,
            ..Frame::default()
        }));
    }
    let shared = Shared::default();
    shared.stop.store(2, Ordering::Release);
    run(Box::new(Identity), consumer, producer, &shared, geometry());
    let wait = shared.queue_wait.snapshot();
    let turnaround = shared.turnaround.snapshot();
    assert_eq!(wait.calls, 1);
    assert_eq!(turnaround.calls, 1);
    assert_eq!(wait.clock_errors, 1);
    assert_eq!(turnaround.clock_errors, 1);
    assert!(wait.total_ns >= 20_000_000);
    assert!(turnaround.total_ns >= wait.total_ns);
    assert_eq!(wait.overruns, 1);
    assert_eq!(turnaround.overruns, 1);
    assert_eq!(output.pop().unwrap().submitted_ns, Some(submitted));
    assert!(output.pop().is_some());
}
#[test]
fn gap_history_and_partial_drain_geometry() {
    // The generic geometry's intrinsic delay is shorter than one frame.
    let g = geometry();
    let (mut input, consumer) = queue::channel();
    let (producer, mut output) = queue::channel();
    for sequence in [0, 3, 4, 5, 6] {
        assert!(input.push(&Frame {
            sequence,
            valid: true,
            ..Frame::default()
        }));
    }
    let shared = Shared::default();
    shared.stop.store(2, Ordering::Release);
    run(Box::new(Identity), consumer, producer, &shared, g);
    assert!(output.pop().unwrap().valid);
    let invalid = g.intrinsic_delay_samples.div_ceil(g.frame_samples);
    assert_eq!(invalid, 1);
    for i in 0..4 {
        assert_eq!(output.pop().unwrap().valid, i >= invalid);
    }
    assert_eq!(shared.completed.load(Ordering::Acquire), 5);
}
struct Hold {
    state: Arc<(Mutex<(bool, bool)>, Condvar)>,
    fail: bool,
}
impl Model for Hold {
    fn process(&mut self, _: &mut [f32]) -> Result<f32, Error> {
        let (lock, cv) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.0 = true;
        cv.notify_all();
        while !state.1 {
            state = cv.wait(state).unwrap();
        }
        if self.fail {
            Err(Error::ModelFailed)
        } else {
            Ok(0.)
        }
    }
    fn reset(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
#[test]
fn held_terminal_job_timeout_late_failure_and_queued_drain() {
    {
        let g = geometry();
        for fail in [false, true] {
            let state = Arc::new((Mutex::new((false, false)), Condvar::new()));
            let mut w = Worker::spawn(
                g,
                Box::new(Hold {
                    state: state.clone(),
                    fail,
                }),
                WorkerOptions::default(),
            )
            .unwrap();
            for _ in 0..(g.frame_samples * 3 + 17) {
                w.process_sample(0.1, 0.);
            }
            let (lock, cv) = &*state;
            let s = lock.lock().unwrap();
            let (mut s, timeout) = cv
                .wait_timeout_while(s, Duration::from_secs(2), |s| !s.0)
                .unwrap();
            assert!(!timeout.timed_out());
            assert_eq!(
                w.finish(StopMode::Drain, Duration::ZERO),
                Err(Error::JoinTimeout)
            );
            assert_eq!(w.telemetry().termination(), Termination::TimedOut);
            assert!(w.telemetry().terminal().is_none());
            let samples = w.sample;
            for _ in 0..480 {
                assert!(w.process_sample(0.2, 1.).is_finite());
            }
            assert_eq!(
                w.sample, samples,
                "retirement cannot resume queue admission"
            );
            assert!(w.thread.is_some());
            // The held successful job must retain its terminal-only wall and
            // allowance overrun after release, even though callback work ended.
            thread::sleep(Duration::from_millis(20));
            s.1 = true;
            cv.notify_all();
            drop(s);
            let terminal = w.finish(StopMode::Drain, Duration::from_secs(2)).unwrap();
            assert!(terminal.joined);
            assert_eq!(terminal.stop_mode, StopMode::Drain);
            assert!(terminal.retirement_timed_out);
            assert_eq!(w.telemetry().termination(), Termination::Joined);
            assert_eq!(terminal.full_input_frames, 3);
            assert_eq!(terminal.partial_input_samples, 17);
            assert_eq!(terminal.submitted_frames, 3);
            assert_eq!(terminal.snapshot.failed, fail);
            assert_eq!(terminal.drained, !fail);
            assert_eq!(terminal.snapshot.completed_frames, if fail { 0 } else { 3 });
            assert_eq!(terminal.pending_input_frames, if fail { 2 } else { 0 });
            if !fail && cfg!(target_os = "linux") {
                assert!(terminal.snapshot.wall.overruns >= 1);
                assert!(terminal.snapshot.allowance_overruns >= 1);
            }
            assert_eq!(w.finish(StopMode::Drain, Duration::ZERO).unwrap(), terminal);
            let telemetry = w.telemetry();
            assert_eq!(telemetry.terminal(), Some(terminal));
            assert_eq!(
                w.finish(StopMode::Cancel, Duration::ZERO).unwrap(),
                terminal
            );
            drop(w);
            assert_eq!(telemetry.terminal(), Some(terminal));
        }
    }
}

#[test]
fn misses_and_late_results_are_recorded_with_their_frame_and_time() {
    let (mut worker, mut input, mut output) = endpoint();
    let empty = worker.telemetry().outliers();
    assert_eq!(empty.last_miss, Recorded::Empty);
    assert_eq!(empty.last_late, Recorded::Empty);
    let start = worker.geometry.frame_samples + worker.geometry.worker_lead_samples;
    let before = metrics::now(false).unwrap();
    // No result for frame 0 by its deadline: a miss.
    for _ in 0..=start {
        worker.process_sample(0., 1.);
    }
    let after = metrics::now(false).unwrap();
    let Recorded::Stable(miss) = worker.telemetry().outliers().last_miss else {
        panic!("miss not recorded")
    };
    assert_eq!(miss.sequence, 0);
    assert!((before..=after).contains(&miss.at_ns));
    assert_eq!(worker.telemetry().outliers().last_late, Recorded::Empty);
    // Its result arrives mid-frame: late.
    let frame = input.pop().unwrap();
    assert!(output.push(&frame));
    worker.process_sample(0., 1.);
    let Recorded::Stable(late) = worker.telemetry().outliers().last_late else {
        panic!("late not recorded")
    };
    assert_eq!(late.sequence, 0);
    assert!(late.at_ns >= miss.at_ns);
    assert_eq!(worker.telemetry().snapshot().missed_deadlines, 1);
    assert_eq!(worker.telemetry().snapshot().late_results, 1);
}

#[test]
fn the_first_misses_and_late_results_are_kept_in_order_and_the_rest_counted() {
    let (mut worker, mut input, mut output) = endpoint();
    let empty = worker.telemetry().outliers();
    assert_eq!(
        (empty.miss_history.total, empty.miss_history.events.len()),
        (0, 0)
    );
    let start = worker.geometry.frame_samples + worker.geometry.worker_lead_samples;
    let frame = worker.geometry.frame_samples;
    // Frame 0 misses, then arrives late; then 70 more frames miss outright.
    for _ in 0..=start {
        worker.process_sample(0., 1.);
    }
    let first = input.pop().unwrap();
    assert!(output.push(&first));
    worker.process_sample(0., 1.);
    for _ in 0..70 * frame {
        worker.process_sample(0., 1.);
    }
    let outliers = worker.telemetry().outliers();
    let snapshot = worker.telemetry().snapshot();
    let misses = &outliers.miss_history;
    assert_eq!(misses.total, snapshot.missed_deadlines);
    assert_eq!(misses.total, 71);
    assert_eq!(misses.capacity, HISTORY);
    // The first HISTORY, never overwritten by later ones.
    assert_eq!(misses.events.len(), HISTORY);
    assert_eq!((misses.lost, misses.unpublished), (71 - HISTORY as u64, 0));
    assert_eq!(
        misses.total,
        misses.events.len() as u64 + misses.lost + misses.unpublished
    );
    assert!(!misses.complete());
    let sequences: Vec<_> = misses.events.iter().map(|e| e.sequence).collect();
    assert_eq!(sequences, (0..HISTORY as u64).collect::<Vec<_>>());
    assert!(misses.events.windows(2).all(|w| w[0].at_ns <= w[1].at_ns));
    assert!(misses.events.iter().all(|e| e.at_ns > 0));
    // The latest is still reported on its own, as before.
    let Recorded::Stable(last) = outliers.last_miss else {
        panic!("no last miss")
    };
    assert_eq!(last.sequence, 70);
    let lates = &outliers.late_history;
    assert_eq!((lates.total, lates.lost, lates.unpublished), (1, 0, 0));
    assert!(lates.complete());
    assert_eq!(snapshot.late_results, 1);
    let Recorded::Stable(late) = outliers.last_late else {
        panic!("no last late")
    };
    assert_eq!(lates.events, vec![late]);
    assert_eq!(late.sequence, 0);
}

#[test]
fn the_worker_records_its_slowest_frame_consistently_with_its_timings() {
    let mut worker =
        Worker::spawn(geometry(), Box::new(Identity), WorkerOptions::default()).unwrap();
    let telemetry = worker.telemetry();
    assert_eq!(telemetry.outliers().worst, Recorded::Empty);
    for _ in 0..480 * 6 {
        worker.process_sample(0.1, 1.0);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while telemetry.snapshot().completed_frames < 6 {
        assert!(Instant::now() < deadline, "worker never ran");
        std::thread::sleep(Duration::from_millis(1));
    }
    let Recorded::Stable(worst) = telemetry.outliers().worst else {
        panic!("no worst frame")
    };
    assert!(worst.sequence < 6);
    assert!(worst.submitted_ns <= worst.started_ns);
    assert!(worst.started_ns <= worst.finished_ns);
    assert!(worst.finished_ns <= worst.published_ns);
    assert_eq!(worst.queue_wait_ns, worst.started_ns - worst.submitted_ns);
    assert_eq!(worst.wall_ns, worst.finished_ns - worst.started_ns);
    assert_eq!(worst.turnaround_ns, worst.published_ns - worst.submitted_ns);
    assert_eq!(worst.turnaround_ns, telemetry.snapshot().turnaround.max_ns);
    worker
        .finish(StopMode::Cancel, Duration::from_secs(2))
        .unwrap();
}

#[test]
fn cancel_receipt_cannot_be_relabelled_drain_and_survives_drop() {
    let mut worker =
        Worker::spawn(geometry(), Box::new(Identity), WorkerOptions::default()).unwrap();
    for _ in 0..17 {
        worker.process_sample(0.1, 1.0);
    }
    let telemetry = worker.telemetry();
    assert!(telemetry.terminal().is_none());
    let first = worker
        .finish(StopMode::Cancel, Duration::from_secs(2))
        .unwrap();
    assert_eq!(first.stop_mode, StopMode::Cancel);
    assert!(!first.retirement_timed_out);
    assert_eq!(first.input_samples, 17);
    assert_eq!(first.partial_input_samples, 17);
    assert_eq!(first.full_input_frames, 0);
    assert_eq!(
        worker.finish(StopMode::Drain, Duration::ZERO).unwrap(),
        first
    );
    assert_eq!(
        worker.drain_provider(Duration::ZERO),
        Err(RetirementError::Incomplete)
    );
    drop(worker);
    assert_eq!(telemetry.terminal(), Some(first));
}

#[test]
fn retirement_completion_requires_a_timely_observation_after_waiting() {
    use std::cell::Cell;
    let start = Instant::now();
    for (delay_ms, expected) in [(9, true), (10, false), (11, false)] {
        let finished = Cell::new(false);
        let elapsed = Cell::new(Duration::ZERO);
        let waits = Cell::new(0);
        let accepted = completion_observed_before_deadline(
            start + Duration::from_millis(10),
            || finished.get(),
            || start + elapsed.get(),
            || {
                waits.set(waits.get() + 1);
                // The worker finishes during the wait, but the observer can
                // resume on time, exactly at the deadline, or after it.
                finished.set(true);
                elapsed.set(Duration::from_millis(delay_ms));
            },
        );
        assert_eq!(accepted, expected, "observer delay {delay_ms}ms");
        assert_eq!(waits.get(), 1);
    }
    assert!(completion_observed_before_deadline(
        start,
        || true,
        || panic!("already-complete zero-time join needs no clock"),
        || panic!("already-complete zero-time join must not wait"),
    ));
    assert!(!completion_observed_before_deadline(
        start,
        || false,
        || start,
        || panic!("expired budget must not wait"),
    ));
}

#[test]
fn dsp_retirement_bridge_keeps_late_worker_fault_and_unresolved_timeout() {
    let state = Arc::new((Mutex::new((false, false)), Condvar::new()));
    // The DSP checks the worker against the admitted geometry, so both use
    // the fixed lookahead-0 frame with its threaded worker lead.
    let lookahead = 0;
    let worker = Worker::spawn(
        geometry(),
        Box::new(Hold {
            state: state.clone(),
            fail: true,
        }),
        WorkerOptions::default(),
    )
    .unwrap();
    let telemetry = worker.telemetry();
    let mut target = rc2fx_core::Target::new("mic-1", "Mic", rc2fx_core::TargetKind::Mic, 1);
    target.suppression.enabled = true;
    target.suppression.lookahead = lookahead;
    let prepared = rc2fx_dsp::Prepared::new(&target, 48000).unwrap();
    let mut dsp = rc2fx_dsp::Dsp::new(&prepared, [Some(Box::new(worker)), None]).unwrap();
    // Two full frames and 17 samples of a third.
    let samples = 2 * geometry().frame_samples + 17;
    let input = vec![0.125; samples];
    let mut output = vec![0.; samples];
    dsp.process(&prepared, [&input, &[]], [&mut output, &mut []])
        .unwrap();
    let (lock, cv) = &*state;
    let held = lock.lock().unwrap();
    let (mut held, timeout) = cv
        .wait_timeout_while(held, Duration::from_secs(2), |held| !held.0)
        .unwrap();
    assert!(!timeout.timed_out());
    assert_eq!(
        dsp.drain_suppression(Duration::ZERO),
        Err(RetirementError::TimedOut)
    );
    assert!(telemetry.terminal().is_none());
    assert_eq!(telemetry.termination(), Termination::TimedOut);
    let mut untouched = [7.; 1];
    assert_eq!(
        dsp.process(&prepared, [&[0.1], &[]], [&mut untouched, &mut []]),
        Err(rc2fx_dsp::DspError::Retired)
    );
    assert_eq!(untouched, [7.; 1]);
    held.1 = true;
    cv.notify_all();
    drop(held);
    assert_eq!(
        dsp.drain_suppression(Duration::from_secs(2)),
        Err(RetirementError::TimedOut)
    );
    let terminal = telemetry.terminal().unwrap();
    assert_eq!(terminal.stop_mode, StopMode::Drain);
    assert!(terminal.joined && terminal.retirement_timed_out && terminal.snapshot.failed);
    assert!(!terminal.drained);
    assert_eq!(terminal.input_samples, samples as u64);
    assert_eq!(terminal.full_input_frames, 2);
    assert_eq!(terminal.partial_input_samples, 17);
    assert_eq!(terminal.pending_input_frames, 1);
    assert_eq!(terminal.snapshot.completed_frames, 0);
    drop(dsp);
    assert_eq!(telemetry.terminal(), Some(terminal));
}

#[test]
fn output_overflow_is_retained_in_joined_verdict() {
    let (mut input, consumer) = queue::channel();
    let (mut producer, mut output) = queue::channel();
    for _ in 0..8 {
        assert!(producer.push(&Frame::default()));
    }
    assert!(input.push(&Frame {
        valid: true,
        ..Frame::default()
    }));
    let shared = Shared::default();
    shared.stop.store(2, Ordering::Release);
    run(Box::new(Identity), consumer, producer, &shared, geometry());
    assert_eq!(shared.output_drops.load(Ordering::Acquire), 1);
    assert_eq!(shared.completed.load(Ordering::Acquire), 1);
    assert!(output.pop().is_some());
}
