//! Deterministic endpoint deadlines, not a scheduler or device qualification.
use super::*;
use std::collections::VecDeque;

/// Fake model service publishes at the beginning of the third native block
/// after submission. The real endpoint still assembles, queues, selects and
/// accounts for each sample; no sleeps or OS scheduling determine the result.
fn three_block_service(lead: usize) -> (Snapshot, Vec<usize>) {
    let (mut worker, mut input, mut output) = endpoint();
    assert_eq!(worker.geometry.frame_samples, 240);
    worker.geometry.worker_lead_samples = lead;
    let start = 240 + lead;
    let mut pending = VecDeque::<(usize, Frame)>::new();
    let mut fallback = Vec::new();
    for at in 0..start + 7 * 240 {
        if at % 112 == 0 {
            while pending.front().is_some_and(|(block, _)| *block <= at / 112) {
                let (_, frame) = pending.pop_front().unwrap();
                assert!(output.push(&frame));
            }
        }
        let value = worker.process_sample(0., 1.);
        if at >= start {
            if worker.voice().is_none() {
                fallback.push(at);
                assert_eq!(value, 0.);
            } else {
                assert_eq!(value, 0.25);
            }
        }
        if let Some(mut frame) = input.pop() {
            frame.samples.fill(0.25);
            frame.voice = 0.75;
            pending.push_back((at / 112 + 3, frame));
        }
    }
    (worker.telemetry().snapshot(), fallback)
}

#[test]
fn admitted_and_longer_leads_deliver_all_seven_phases() {
    for lead in [320, 336] {
        let (s, fallback) = three_block_service(lead);
        assert!(fallback.is_empty(), "lead {lead}: {fallback:?}");
        assert_eq!(s.missed_deadlines, 0);
        assert_eq!(s.late_results, 0);
        assert_eq!(s.input_drops, 0);
        assert_eq!(s.output_drops, 0);
        assert_eq!(s.stale_results, 0);
    }
}

#[test]
fn lead_319_exposes_the_earlier_due_block_and_counts_recovery() {
    let (s, fallback) = three_block_service(319);
    // First input ends at sample 239 (block 2). Result arrives at sample
    // 560 (block 5), one sample after the endpoint selects it at sample 559.
    assert_eq!(fallback, vec![559]);
    assert_eq!(s.missed_deadlines, 1);
    assert_eq!(s.late_results, 1);
    assert_eq!(s.stale_results, 0);
}

#[test]
fn lead_320_sequence_gaps_reset_history_and_invalidate_only_recovery_frames() {
    struct History {
        resets: Arc<std::sync::atomic::AtomicUsize>,
        value: usize,
    }
    impl Model for History {
        fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
            self.value += 1;
            samples.fill(self.value as f32 / 8.);
            Ok(0.5)
        }
        fn reset(&mut self) -> Result<(), Error> {
            self.resets.fetch_add(1, Ordering::Relaxed);
            self.value = 0;
            Ok(())
        }
    }
    let resets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (mut input, consumer) = queue::channel();
    let (producer, mut output) = queue::channel();
    for sequence in [0, 2, 3, 6, 7] {
        assert!(input.push(&Frame {
            sequence,
            valid: true,
            ..Frame::default()
        }));
    }
    let shared = Shared::default();
    shared.stop.store(2, Ordering::Release);
    let mut geometry = geometry();
    assert_eq!(geometry.frame_samples, 240);
    geometry.worker_lead_samples = 320;
    run(
        Box::new(History {
            resets: resets.clone(),
            value: 0,
        }),
        consumer,
        producer,
        &shared,
        geometry,
    );
    for (sequence, valid, value) in [
        (0, true, 0.125),
        (2, false, 0.125),
        (3, true, 0.25),
        (6, false, 0.125),
        (7, true, 0.25),
    ] {
        let frame = output.pop().unwrap();
        assert_eq!(frame.sequence, sequence);
        assert_eq!(frame.valid, valid);
        assert_eq!(frame.samples[0], value);
    }
    assert!(output.pop().is_none());
    assert_eq!(resets.load(Ordering::Relaxed), 2);
    assert_eq!(shared.completed.load(Ordering::Acquire), 5);
}
