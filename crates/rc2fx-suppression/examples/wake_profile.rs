//! Standalone wake-path comparison. No JACK, audio routes, model or saved state.
//! Run in release mode; numbers describe this workload, not DSP qualification.
#[cfg(target_os = "linux")]
#[path = "../src/wake.rs"]
mod wake;

#[cfg(target_os = "linux")]
fn main() {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicU64, Ordering},
        },
        time::Duration,
    };
    const COUNT: u32 = 100;
    fn clock(id: libc::clockid_t) -> u64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: initialized writable timespec and a Linux clock identifier.
        assert_eq!(unsafe { libc::clock_gettime(id, &mut ts) }, 0);
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    for polling in [true, false] {
        let wake = Arc::new(wake::Wake::default());
        let published = Arc::new(AtomicU32::new(0));
        let stamp = Arc::new(AtomicU64::new(0));
        let (ack, received) = std::sync::mpsc::sync_channel(1);
        let (worker_wake, worker_published, worker_stamp) =
            (wake.clone(), published.clone(), stamp.clone());
        let worker = std::thread::spawn(move || {
            let mut samples = [0_u64; COUNT as usize];
            let start = clock(libc::CLOCK_MONOTONIC);
            let cpu_start = clock(libc::CLOCK_THREAD_CPUTIME_ID);
            let mut waits = 0_u64;
            ack.send(0).unwrap();
            for expected in 1..=COUNT {
                loop {
                    let epoch = worker_wake.observe();
                    if worker_published.load(Ordering::Acquire) == expected {
                        break;
                    }
                    waits += 1;
                    if polling {
                        std::thread::sleep(Duration::from_micros(100));
                    } else {
                        assert!(worker_wake.wait(epoch));
                    }
                }
                samples[expected as usize - 1] =
                    clock(libc::CLOCK_MONOTONIC) - worker_stamp.load(Ordering::Relaxed);
                ack.send(expected).unwrap();
            }
            let cpu = clock(libc::CLOCK_THREAD_CPUTIME_ID) - cpu_start;
            let elapsed = clock(libc::CLOCK_MONOTONIC) - start;
            (samples, cpu, elapsed, waits)
        });
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        let mut notify_ns = [0_u64; COUNT as usize];
        for sequence in 1..=COUNT {
            std::thread::sleep(Duration::from_millis(10));
            let now = clock(libc::CLOCK_MONOTONIC);
            stamp.store(now, Ordering::Relaxed);
            published.store(sequence, Ordering::Release);
            if !polling {
                assert!(wake.notify());
            }
            notify_ns[sequence as usize - 1] = clock(libc::CLOCK_MONOTONIC) - now;
            assert_eq!(
                received.recv_timeout(Duration::from_secs(2)).unwrap(),
                sequence
            );
        }
        let (mut samples, cpu, elapsed, waits) = worker.join().unwrap();
        samples.sort_unstable();
        notify_ns.sort_unstable();
        println!(
            "mode={} samples={} elapsed_ns={} worker_cpu_ns={} waits={} ready_p50_ns={} ready_p99_ns={} ready_max_ns={} publish_p99_ns={}",
            if polling { "poll-100us" } else { "futex" },
            COUNT,
            elapsed,
            cpu,
            waits,
            samples[49],
            samples[98],
            samples[99],
            notify_ns[98]
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This comparison requires Linux clocks and futexes.");
}
