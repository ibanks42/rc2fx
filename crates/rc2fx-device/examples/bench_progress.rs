//! Isolate the four per-path progress atomics; no device or JACK access.
use std::{
    hint::black_box,
    sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

#[derive(Default)]
struct Packed {
    seen: AtomicU64,
    applied: AtomicU64,
    frames: AtomicU64,
    calls: AtomicU64,
}
#[repr(align(64))]
#[derive(Default)]
struct Padded(Packed);

fn run(slots: &[&Packed], stores: bool) -> (f64, f64) {
    const CALLS: u64 = 1_000_000;
    let barrier = Barrier::new(slots.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = slots
            .iter()
            .map(|slot| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let start = Instant::now();
                    for _ in 0..CALLS {
                        slot.seen.store(black_box(1), Ordering::Release);
                        if stores {
                            slot.frames.store(
                                slot.frames.load(Ordering::Relaxed).wrapping_add(112),
                                Ordering::Relaxed,
                            );
                            slot.calls.store(
                                slot.calls.load(Ordering::Relaxed).wrapping_add(1),
                                Ordering::Relaxed,
                            );
                        } else {
                            slot.frames.fetch_add(112, Ordering::Relaxed);
                            slot.calls.fetch_add(1, Ordering::Relaxed);
                        }
                        slot.applied.store(1, Ordering::Release);
                    }
                    let ns = start.elapsed().as_nanos() as f64 / CALLS as f64;
                    assert_eq!(slot.frames.load(Ordering::Relaxed), CALLS * 112);
                    assert_eq!(slot.calls.load(Ordering::Relaxed), CALLS);
                    ns
                })
            })
            .collect();
        let elapsed: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        (
            elapsed.iter().sum::<f64>() / elapsed.len() as f64,
            elapsed.iter().copied().fold(0.0, f64::max),
        )
    })
}

fn main() {
    for trial in 0..6 {
        for threads in [1, 4] {
            for offset in 0..4 {
                let mode = (trial + offset) % 4;
                let packed: Vec<_> = (0..threads).map(|_| Packed::default()).collect();
                let padded: Vec<_> = (0..threads).map(|_| Padded::default()).collect();
                let refs: Vec<_> = if mode < 2 {
                    packed.iter().collect()
                } else {
                    padded.iter().map(|p| &p.0).collect()
                };
                let (mean, max) = run(&refs, mode % 2 == 1);
                println!(
                    "trial={trial} threads={threads} mode={} mean_wall_ns={mean:.2} max_wall_ns={max:.2}",
                    ["packed-rmw", "packed-store", "padded-rmw", "padded-store"][mode]
                );
            }
        }
    }
}
