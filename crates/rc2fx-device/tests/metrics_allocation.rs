use rc2fx_device::metrics::{Clock, Elapsed, Slot};
use rc2fx_dsp::Meter;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

struct Counting;
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOC: Cell<u64> = const { Cell::new(0) };
    static FREE: Cell<u64> = const { Cell::new(0) };
}
fn increment(counter: &'static std::thread::LocalKey<Cell<u64>>) {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let _ = counter.try_with(|count| count.set(count.get() + 1));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        increment(&ALLOC);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        increment(&FREE);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        increment(&ALLOC);
        increment(&FREE);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn callback_recording_clocks_and_spsc_copies_never_allocate_or_free() {
    let mut slot = Slot::new(2_333_333, true).unwrap();
    let clock = Clock::probe(true);
    let (mut producer, mut consumer) = slot.split();
    // Initialize test TLS before counting; production endpoints contain no TLS.
    ALLOC.set(0);
    FREE.set(0);
    ACTIVE.set(true);
    for index in 0..20_000 {
        let started = clock.start();
        let measured = clock.finish(started);
        let timing = if index % 4 < 2 {
            measured
        } else {
            Elapsed::from_durations(Some(100), Some(50))
        };
        producer.record(
            Some(Meter {
                input_peak: 0.2,
                output_peak: 0.2,
                sum_squares: 4.48,
                gain_reduction: 0.0,
                voice: 0.0,
                gate_open: 0.0,
                samples: 112,
                clipped: 0,
            }),
            1,
            112,
            Some(timing),
        );
        if index % 37 == 0 {
            std::hint::black_box(consumer.drain_latest(1));
        }
    }
    ACTIVE.set(false);
    assert_eq!(ALLOC.get(), 0);
    assert_eq!(FREE.get(), 0);
    let totals = slot.final_snapshot().totals;
    assert_eq!(totals.callbacks, 20_000);
    assert_eq!(totals.cadence.attempts, 20_000);
    assert_eq!(totals.cadence.unmeasured, 10_000);
    #[cfg(target_os = "linux")]
    {
        assert!(totals.cadence.intervals > 0);
        assert_eq!(totals.cadence.unavailable, 0);
        assert_eq!(
            totals.cadence.intervals + totals.cadence.baselines + totals.cadence.clock_errors,
            10_000
        );
    }
    #[cfg(not(target_os = "linux"))]
    assert_eq!(totals.cadence.unavailable, 10_000);
}
