//! Isolate callback instrumentation cost; no JACK connection or device access.
//! Run release mode on Linux. This does not measure scheduling or DSP.
#[cfg(target_os = "linux")]
fn main() {
    use rc2fx_device::metrics::{Clock, RecordResult, Slot};
    use rc2fx_dsp::Meter;
    use std::hint::black_box;

    fn cpu_ns() -> u128 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: initialized writable timespec, querying this thread only.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) },
            0
        );
        u128::try_from(ts.tv_sec).unwrap() * 1_000_000_000 + u128::try_from(ts.tv_nsec).unwrap()
    }
    const CALLS: usize = 200_000;
    let meter = Meter {
        samples: 224,
        input_peak: 0.1,
        output_peak: 0.1,
        sum_squares: 1.0,
        ..Meter::default()
    };
    for trial in 0..6 {
        // Rotate order to reduce systematic warmup/frequency bias.
        for offset in 0..3 {
            let mode = (trial + offset) % 3;
            let clock = (mode != 0).then(|| Clock::probe(mode == 2));
            let mut slot = Slot::new(2_333_333, mode == 2).unwrap();
            let (mut producer, mut consumer) = slot.split();
            let start = cpu_ns();
            for _ in 0..CALLS {
                let stamp = clock.map(Clock::start);
                let elapsed = clock.zip(stamp).map(|(c, s)| c.finish(s));
                let result = producer.record(Some(black_box(meter)), 1, 112, elapsed);
                if result == RecordResult::Published {
                    // Drain outside the callback in production. Including this
                    // copy here measures more than producer-only overhead.
                    black_box(consumer.pop());
                }
            }
            let elapsed = cpu_ns() - start;
            println!(
                "trial={trial} mode={} cpu_ns_per_callback={:.2}",
                ["record", "wall+record", "wall+cpu+record"][mode],
                elapsed as f64 / CALLS as f64
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Run on Linux: cargo run --release -p rc2fx-device --example bench_metrics");
    std::process::exit(2);
}
