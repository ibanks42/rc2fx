use rc2fx_suppression::{Endpoint, Error, StopMode, VendorModel, WorkerOptions};
use std::time::Duration;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
thread_local! {static ALLOCS:Cell<Option<usize>>=const{Cell::new(None)};}
struct Count;
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1))
            }
        });
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1))
            }
        });
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Count = Count;
fn signal(frame: usize, size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let x = ((frame * size + i) * 7919 % 65521) as f32 / 65521.;
            (x - 0.5) * 1.7
        })
        .collect()
}
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|x| x.to_bits()).collect()
}
#[test]
fn pristine_reset_rejection_headroom_and_concurrent_ownership() {
    let threads: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                let mut a = VendorModel::new(0).unwrap();
                let mut b = VendorModel::new(0).unwrap();
                let size = a.geometry().frame_samples;
                for frame in 0..24 {
                    let mut x = signal(frame, size);
                    let mut y = x.clone();
                    a.process(&mut x).unwrap();
                    b.process(&mut y).unwrap();
                    assert_eq!(bits(&x), bits(&y));
                }
                let mut bad = vec![0.; size];
                bad[size - 1] = f32::NAN;
                assert_eq!(a.process(&mut bad), Err(Error::InvalidFrame));
                assert!(bad[size - 1].is_nan());
                assert_eq!(a.process(&mut vec![0.; size + 1]), Err(Error::InvalidFrame));
                a.reset().unwrap();
                let mut fresh = VendorModel::new(0).unwrap();
                for frame in 0..6 {
                    let mut x: Vec<f32> = signal(frame, size).iter().map(|x| x * 4.).collect();
                    let mut y: Vec<f32> = x.iter().map(|x| x.clamp(-1., 1.)).collect();
                    a.process(&mut x).unwrap();
                    fresh.process(&mut y).unwrap();
                    assert_eq!(bits(&x), bits(&y), "after reset");
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}
#[test]
fn real_worker_is_off_callback_and_joins_complete_frames() {
    let mut w = Endpoint::new(0, WorkerOptions::default()).unwrap();
    let mut reference = VendorModel::new(0).unwrap();
    let size = w.geometry().frame_samples;
    let inputs: Vec<f32> = (0..4).flat_map(|frame| signal(frame, size)).collect();
    let mut processed = inputs.clone();
    for frame in processed.chunks_mut(size) {
        reference.process(frame).unwrap();
    }
    let start = w.geometry().frame_samples + w.geometry().worker_lead_samples;
    let delay = w.latency_samples();
    let telemetry = w.telemetry();
    for frame in 0..4 {
        // The worker warms its model on its own thread; nothing allocates
        // after publication.
        ALLOCS.with(|c| c.set(Some(0)));
        for i in frame * size..(frame + 1) * size {
            if i == 1 {
                ALLOCS.with(|c| c.set(Some(0)));
            }
            let dry = if i >= delay { inputs[i - delay] } else { 0. };
            let signal = if i >= start {
                processed[i - start]
            } else {
                dry
            };
            let wet = if frame == 2 { 0. } else { 0.375 };
            let expected = dry + (signal - dry) * wet;
            assert_eq!(
                w.process_sample(inputs[i], wet).to_bits(),
                expected.to_bits(),
                "exact dry/wet alignment at sample {i}"
            );
        }
        assert_eq!(ALLOCS.with(|c| c.replace(None)), Some(0));
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while telemetry.snapshot().completed_frames < (frame + 1) as u64 {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let done = w.finish(StopMode::Drain, Duration::from_secs(5)).unwrap();
    assert!(done.joined && done.drained);
    assert_eq!(done.snapshot.completed_frames, 4);
    // The worker's clocks are Unix-only; elsewhere frames go untimed.
    let timed = if cfg!(unix) { 4 } else { 0 };
    assert_eq!(done.snapshot.wall.calls, timed);
    assert_eq!(done.snapshot.cpu.calls, timed);
    // Deliberately paced functional waits, never a host deadline admission.
}

/// The model's whole per-frame path, which is the worker's (`process`, the
/// staged preparation steps after it, and a reset), allocates nothing once
/// the model is built, for both embedded models.
#[test]
fn the_model_allocates_nothing_per_frame() {
    for lookahead in [0, 1] {
        let mut model = VendorModel::new(lookahead).unwrap();
        let size = model.geometry().frame_samples;
        let mut frames: Vec<Vec<f32>> = (0..24).map(|frame| signal(frame, size)).collect();
        ALLOCS.with(|c| c.set(Some(0)));
        for (n, frame) in frames.iter_mut().enumerate() {
            model.process(frame).unwrap();
            while model.prepare_step() {}
            if n == 11 {
                model.reset().unwrap();
            }
        }
        assert_eq!(
            ALLOCS.with(|c| c.replace(None)),
            Some(0),
            "lookahead {lookahead}"
        );
        assert!(frames.iter().flatten().all(|x| x.is_finite()));
    }
}

/// `Endpoint::process_block` on the real worker and model, in device-period
/// blocks that never line up with the model's frame: the exact dry/wet
/// alignment of the per-sample test above with a wet that moves at every
/// sample, each sample's voice score, and no allocation on the calling
/// thread.
#[test]
fn real_worker_blocks_align_dry_and_wet_and_allocate_nothing() {
    const PERIOD: usize = 112;
    let mut w = Endpoint::new(0, WorkerOptions::default()).unwrap();
    let mut reference = VendorModel::new(0).unwrap();
    let size = w.geometry().frame_samples;
    let inputs: Vec<f32> = (0..7).flat_map(|frame| signal(frame, size)).collect();
    let mut processed = inputs.clone();
    let scores: Vec<f32> = processed
        .chunks_mut(size)
        .map(|frame| reference.process(frame).unwrap())
        .collect();
    let start = size + w.geometry().worker_lead_samples;
    let delay = w.latency_samples();
    let wet: Vec<f32> = (0..inputs.len())
        .map(|i| match i / 500 {
            2 => 0.,
            _ => 0.25 + (i % 7) as f32 / 14.,
        })
        .collect();
    let telemetry = w.telemetry();
    let mut out = inputs.clone();
    let mut voice = vec![Some(-1f32); inputs.len()];
    for at in (0..inputs.len()).step_by(PERIOD) {
        let block = at..(at + PERIOD).min(inputs.len());
        // The first call on a thread may set up thread-local state, as in
        // the per-sample test; every later one must allocate nothing.
        ALLOCS.with(|c| c.set(Some(0)));
        w.process_block(
            &mut out[block.clone()],
            &wet[block.clone()],
            &mut voice[block.clone()],
        );
        let allocations = ALLOCS.with(|c| c.replace(None));
        assert!(at == 0 || allocations == Some(0), "block at {at}");
        // Every completed frame's result is published before the next
        // block, well inside its lead, so each plays from its first sample.
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while telemetry.snapshot().completed_frames < (block.end / size) as u64 {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    for i in 0..inputs.len() {
        let dry = if i >= delay { inputs[i - delay] } else { 0. };
        let (signal, score) = if i >= start {
            (processed[i - start], Some(scores[(i - start) / size]))
        } else {
            (dry, None)
        };
        let expected = dry + (signal - dry) * wet[i];
        assert_eq!(out[i].to_bits(), expected.to_bits(), "sample {i}");
        assert_eq!(
            voice[i].map(f32::to_bits),
            score.map(f32::to_bits),
            "voice at sample {i}"
        );
    }
    let done = w.finish(StopMode::Drain, Duration::from_secs(5)).unwrap();
    assert!(done.joined && done.drained);
    assert_eq!(done.snapshot.completed_frames, 7);
    assert_eq!(
        (done.snapshot.missed_deadlines, done.snapshot.late_results),
        (0, 0)
    );
}

/// The endpoint behind the per-sample provider methods only.
struct PerSample(Endpoint);
impl rc2fx_dsp::Suppression for PerSample {
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        self.0.geometry().resolved()
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        self.0.process_sample(input, wet)
    }
    fn voice(&self) -> Option<f32> {
        self.0.voice()
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), rc2fx_dsp::RetirementError> {
        rc2fx_dsp::Suppression::drain(&mut self.0, timeout)
    }
}

/// `Dsp` on the real worker and model, as the mixer's microphone path runs
/// it: one state reaches the endpoint's block form, the other its
/// per-sample form. With every result published before the block that
/// plays it, both must give the same output and meters bit for bit, the
/// model's voice score must reach the meter, and neither may allocate on
/// the calling thread.
#[test]
fn dsp_on_the_real_worker_matches_its_per_sample_form_and_allocates_nothing() {
    const PERIOD: usize = 112;
    let mut target = rc2fx_core::Target::new("mic", "Mic", rc2fx_core::TargetKind::Mic, 1);
    target.suppression.enabled = true;
    target.suppression.wet = 0.79;
    target.suppression.gate = rc2fx_core::VoiceGate::with_strength(0.61);
    let p = rc2fx_dsp::Prepared::new(&target, 48_000).unwrap();
    let endpoints = [0, 1].map(|_| Endpoint::new(0, WorkerOptions::default()).unwrap());
    let size = endpoints[0].geometry().frame_samples;
    let telemetry = endpoints.each_ref().map(|endpoint| endpoint.telemetry());
    let [by_block, by_sample] = endpoints;
    let mut dsps = [
        rc2fx_dsp::Dsp::new(&p, [Some(Box::new(by_block)), None]).unwrap(),
        rc2fx_dsp::Dsp::new(&p, [Some(Box::new(PerSample(by_sample))), None]).unwrap(),
    ];
    let input: Vec<f32> = (0..9)
        .flat_map(|frame| signal(frame, size))
        .map(|x| x * 0.5)
        .collect();
    let mut scored = 0;
    for (block, chunk) in input.chunks(PERIOD).enumerate() {
        let mut outputs = [vec![0f32; chunk.len()], vec![0f32; chunk.len()]];
        let mut meters = Vec::new();
        for (dsp, output) in dsps.iter_mut().zip(&mut outputs) {
            ALLOCS.with(|c| c.set(Some(0)));
            let meter = dsp.process(&p, [chunk, &[]], [output, &mut []]).unwrap();
            let allocations = ALLOCS.with(|c| c.replace(None));
            // As above: only a thread's first call may set up thread-local
            // state.
            assert!(block == 0 || allocations == Some(0), "block {block}");
            meters.push(meter);
        }
        assert_eq!(bits(&outputs[0]), bits(&outputs[1]), "block {block}");
        assert_eq!(meters[0], meters[1], "block {block}");
        scored += usize::from(meters[0].voice > 0.0);
        let frames = ((block * PERIOD + chunk.len()) / size) as u64;
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while telemetry
            .iter()
            .any(|t| t.snapshot().completed_frames < frames)
        {
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(scored > 5, "blocks with a voice score: {scored}");
    for (dsp, telemetry) in dsps.iter_mut().zip(&telemetry) {
        dsp.drain_suppression(Duration::from_secs(5)).unwrap();
        let done = telemetry.terminal().unwrap();
        assert!(done.joined && done.drained);
        assert_eq!(done.snapshot.completed_frames, 9);
        assert_eq!(
            (done.snapshot.missed_deadlines, done.snapshot.late_results),
            (0, 0)
        );
    }
}
