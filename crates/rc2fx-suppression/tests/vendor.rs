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
    // The worker's clocks are Linux-only; elsewhere frames go untimed.
    let timed = if cfg!(target_os = "linux") { 4 } else { 0 };
    assert_eq!(done.snapshot.wall.calls, timed);
    assert_eq!(done.snapshot.cpu.calls, timed);
    // Deliberately paced functional waits, never a host deadline admission.
}
