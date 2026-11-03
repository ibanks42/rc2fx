use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub overruns: u64,
    pub clock_errors: u64,
    pub bins: [u64; 8],
}
#[derive(Default)]
pub(crate) struct Counters {
    calls: AtomicU64,
    total: AtomicU64,
    max: AtomicU64,
    overruns: AtomicU64,
    errors: AtomicU64,
    bins: [AtomicU64; 8],
}
impl Counters {
    pub fn record(&self, start: Option<u64>, end: Option<u64>, budget: u64) {
        let Some(elapsed) = start.zip(end).and_then(|(s, e)| e.checked_sub(s)) else {
            self.errors.fetch_add(1, Relaxed);
            return;
        };
        let bounds = [
            budget / 16,
            budget / 8,
            budget / 4,
            budget / 2,
            budget * 3 / 4,
            budget,
            budget * 2,
        ];
        let bin = bounds.iter().position(|&b| elapsed <= b).unwrap_or(7);
        self.bins[bin].fetch_add(1, Relaxed);
        self.total.fetch_add(elapsed, Relaxed);
        // Exactly one worker writes each counter set.
        if elapsed > self.max.load(Relaxed) {
            self.max.store(elapsed, Relaxed);
        }
        if elapsed > budget {
            self.overruns.fetch_add(1, Relaxed);
        }
        self.calls.fetch_add(1, Relaxed);
    }
    pub fn snapshot(&self) -> Timing {
        Timing {
            calls: self.calls.load(Relaxed),
            total_ns: self.total.load(Relaxed),
            max_ns: self.max.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            clock_errors: self.errors.load(Relaxed),
            bins: std::array::from_fn(|i| self.bins[i].load(Relaxed)),
        }
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn now(cpu: bool) -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec, supported Linux clock IDs.
    let rc = unsafe {
        libc::clock_gettime(
            if cpu {
                libc::CLOCK_THREAD_CPUTIME_ID
            } else {
                libc::CLOCK_MONOTONIC
            },
            &mut ts,
        )
    };
    if rc != 0 || ts.tv_sec < 0 || ts.tv_nsec < 0 {
        None
    } else {
        Some(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    }
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn now(_cpu: bool) -> Option<u64> {
    None
}
