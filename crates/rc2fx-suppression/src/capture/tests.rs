use super::*;

fn frame(sequence: u64) -> CaptureFrame {
    CaptureFrame {
        sequence,
        submitted_ns: Some(sequence * 100 + 1),
        started_ns: Some(sequence * 100 + 2),
        finished_ns: Some(sequence * 100 + 5),
        published_ns: Some(sequence * 100 + 6),
        cpu_ns: Some(2),
        input_pending_after_pop: 3,
        output_published: true,
        ..CaptureFrame::default()
    }
}
fn window(c: &Capture) -> CaptureWindow {
    let Recorded::Stable(w) = c.snapshot() else {
        panic!("expected stable capture")
    };
    w
}

#[test]
fn disabled_capture_is_empty_and_epochs_only_increase() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    r.begin(Some(1));
    r.record(frame(0));
    assert_eq!(c.snapshot(), Recorded::Empty);
    assert!(!c.arm(0));
    assert!(c.arm(1));
    assert!(!c.arm(1));
    assert_eq!(
        c.metadata(),
        Recorded::Empty,
        "request is not acknowledgement"
    );
    r.begin(Some(100));
    assert_eq!(window(&c).metadata.applied_epoch, 1);
    assert!(c.arm(2));
    assert!(!c.arm(1));
    assert_eq!(window(&c).metadata.requested_epoch, 2);
    assert_eq!(window(&c).metadata.applied_epoch, 1);
}

#[test]
fn wraps_then_freezes_at_first_miss_plus_thirty_two_sequences() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    c.arm(1);
    r.begin(Some(1));
    for seq in 0..=212 {
        if seq == 180 {
            c.missed(EventRecord {
                sequence: 180,
                at_ns: 18000,
            });
            c.missed(EventRecord {
                sequence: 181,
                at_ns: 18100,
            });
        }
        r.record(frame(seq));
        if seq == 211 {
            assert_eq!(window(&c).metadata.state, CaptureState::Triggered);
        }
    }
    let w = window(&c);
    assert_eq!(w.metadata.state, CaptureState::Frozen);
    assert_eq!(w.metadata.trigger.unwrap().sequence, 180);
    assert_eq!(w.frames.len(), 128);
    assert_eq!(w.overwritten, 85);
    assert_eq!(w.frames.first().unwrap().sequence, 85);
    assert_eq!(w.frames.last().unwrap().sequence, 212);
    assert_eq!(w.frames[0].previous_sequence, Some(84));
    assert_eq!(w.frames[0].previous_published_ns, Some(8406));
    assert_eq!(w.frames[0].input_pending_after_pop, 3);
    r.record(frame(213));
    assert_eq!(window(&c), w, "frozen capture is not overwritten");
    drop(r);
    assert_eq!(
        window(&c),
        w,
        "complete window stays complete on retirement"
    );
}

#[test]
fn metadata_does_not_acknowledge_before_endpoint_epoch_is_active() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    c.arm(1);
    r.begin(Some(1));
    // Model the brief interval after worker metadata publication but before
    // endpoint activation. Control readers must not claim it is armed yet.
    c.active.store(0, Ordering::Release);
    assert_eq!(c.metadata(), Recorded::Busy);
    assert_eq!(c.snapshot(), Recorded::Busy);
    c.active.store(1, Ordering::Release);
    assert_eq!(window(&c).metadata.applied_epoch, 1);
}

#[test]
fn burst_before_worker_reads_latch_preserves_first_miss() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    c.arm(1);
    r.begin(Some(1));
    for sequence in 10..17 {
        c.missed(EventRecord {
            sequence,
            at_ns: sequence * 100,
        });
    }
    r.record(frame(20));
    assert_eq!(
        window(&c).metadata.trigger,
        Some(EventRecord {
            sequence: 10,
            at_ns: 1000
        })
    );
}

#[test]
fn rearm_rejects_old_epoch_and_pre_arm_misses() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    c.arm(1);
    r.begin(Some(1));
    c.missed(EventRecord {
        sequence: 0,
        at_ns: 2,
    });
    r.record(frame(32));
    assert_eq!(window(&c).metadata.state, CaptureState::Frozen);
    c.arm(2);
    r.begin(Some(10000));
    // Simulate old-epoch endpoint publication completing after rearm.
    c.first_miss.write([1, 99, 9999]);
    r.record(frame(100));
    assert_eq!(window(&c).metadata.trigger, None);
    c.missed(EventRecord {
        sequence: 99,
        at_ns: 9999,
    });
    c.missed(EventRecord {
        sequence: 101,
        at_ns: 10100,
    });
    r.record(frame(101));
    let w = window(&c);
    assert_eq!(w.metadata.trigger.unwrap().sequence, 101);
    assert_eq!(w.metadata.applied_epoch, 2);
    assert_eq!(w.frames.len(), 2);
    assert_eq!(w.frames[0].previous_sequence, Some(32));
}

#[test]
fn partial_retirement_gaps_and_missing_clocks_are_not_fabricated() {
    let c = Capture::default();
    {
        let mut r = Recorder::new(&c);
        c.arm(1);
        r.begin(None);
        c.missed(EventRecord {
            sequence: 4,
            at_ns: 0,
        });
        r.record(CaptureFrame {
            sequence: 4,
            ..CaptureFrame::default()
        });
        r.record(CaptureFrame {
            sequence: 9,
            output_published: true,
            ..CaptureFrame::default()
        });
    }
    let w = window(&c);
    assert_eq!(w.metadata.state, CaptureState::Stopped);
    assert_eq!(w.metadata.armed_at_ns, None);
    assert_eq!(
        w.frames.iter().map(|f| f.sequence).collect::<Vec<_>>(),
        [4, 9]
    );
    assert_eq!(w.frames[0].published_ns, None);
    assert!(!w.frames[0].output_published);
    assert_eq!(w.frames[1].published_ns, None);
    assert!(
        w.frames[1].output_published,
        "clock failure is not output failure"
    );
    assert_eq!(w.frames[1].previous_sequence, Some(4));
    assert_eq!(w.frames[1].previous_published_ns, None);
    assert!(c.arm(2));
    assert_eq!(
        window(&c).metadata.applied_epoch,
        1,
        "retired worker cannot acknowledge"
    );
    assert_ne!(c.identity().0, Capture::default().identity().0);
}

#[test]
fn missing_sequences_do_not_extend_the_post_trigger_span() {
    let c = Capture::default();
    let mut r = Recorder::new(&c);
    c.arm(1);
    r.begin(Some(1));
    c.missed(EventRecord {
        sequence: 10,
        at_ns: 100,
    });
    r.record(frame(10));
    r.record(frame(50));
    let w = window(&c);
    assert_eq!(w.metadata.state, CaptureState::Frozen);
    assert_eq!(
        w.frames.len(),
        2,
        "gap remains observable, not 32 claimed frames"
    );
}

#[test]
fn concurrent_rearm_and_reads_are_coherent_or_busy() {
    use std::sync::{Arc, atomic::AtomicBool};
    let c = Arc::new(Capture::default());
    let done = Arc::new(AtomicBool::new(false));
    c.arm(1);
    let (writer_c, writer_done) = (c.clone(), done.clone());
    let worker = std::thread::spawn(move || {
        let mut r = Recorder::new(&writer_c);
        for sequence in 0..20000 {
            r.begin(Some(sequence * 100));
            r.record(frame(sequence));
        }
        drop(r);
        writer_done.store(true, Ordering::Release);
    });
    let mut epoch = 1;
    while !done.load(Ordering::Acquire) {
        if let Recorded::Stable(w) = c.snapshot() {
            for pair in w.frames.windows(2) {
                assert_eq!(pair[1].sequence, pair[0].sequence + 1);
            }
            for f in &w.frames {
                assert_eq!(f.published_ns, Some(f.sequence * 100 + 6));
            }
            assert!(w.frames.len() <= CAPTURE_FRAMES);
            assert!(w.metadata.applied_epoch <= w.metadata.requested_epoch);
        }
        if epoch < 200 {
            epoch += 1;
            c.arm(epoch);
        }
    }
    worker.join().unwrap();
    assert_eq!(window(&c).metadata.state, CaptureState::Stopped);
}

#[test]
#[ignore = "host-only microbenchmark; no ARM performance claim"]
fn capture_recording_overhead() {
    use crate::outlier::{FrameRecord, WorkerOutliers};
    use std::{hint::black_box, time::Instant};
    for mode in ["existing", "disabled", "armed"] {
        let mut timings = Vec::new();
        for _ in 0..7 {
            let old = WorkerOutliers::default();
            let capture = Capture::default();
            let mut recorder = Recorder::new(&capture);
            if mode == "armed" {
                capture.arm(1);
            }
            let start = Instant::now();
            for sequence in 0..250000 {
                old.record(
                    black_box(FrameRecord {
                        sequence,
                        turnaround_ns: 3_000_000,
                        ..FrameRecord::default()
                    }),
                    5_000_000,
                );
                if mode != "existing" {
                    recorder.begin(Some(sequence * 100));
                    recorder.record(black_box(frame(sequence)));
                }
            }
            timings.push(start.elapsed().as_nanos() as f64 / 250000.0);
            black_box((&old, &capture));
        }
        timings.sort_by(f64::total_cmp);
        println!(
            "{mode}: median {:.1} ns/record min {:.1} max {:.1}",
            timings[3], timings[0], timings[6]
        );
    }
    println!(
        "capture storage {} bytes/worker, recorder {} bytes/worker",
        std::mem::size_of::<Capture>(),
        std::mem::size_of::<Recorder<'_>>()
    );
}
