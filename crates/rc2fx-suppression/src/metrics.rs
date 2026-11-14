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
    /// Worst measurement since the last control read; only the control status
    /// takes it. The cumulative `max` stays in `snapshot`.
    recent: AtomicU64,
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
        if elapsed > self.recent.load(Relaxed) {
            self.recent.store(elapsed, Relaxed);
        }
        if elapsed > budget {
            self.overruns.fetch_add(1, Relaxed);
        }
        self.calls.fetch_add(1, Relaxed);
    }
    /// The worst measurement since the last call, and zero from then on.
    pub fn take_recent(&self) -> u64 {
        self.recent.swap(0, Relaxed)
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
// Linux on the mixer; other Unix hosts (macOS, for the tests) have both clocks.
#[cfg(unix)]
pub(crate) fn now(cpu: bool) -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec, clock IDs supported on Linux and macOS.
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
#[cfg(not(unix))]
pub(crate) fn now(_cpu: bool) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recent peak is taken once, and a smaller measurement never wins.
    /// `snapshot` reads the cumulative maximum without consuming the peak.
    #[test]
    fn the_recent_peak_is_taken_once_and_keeps_the_largest() {
        let counters = Counters::default();
        counters.record(Some(0), Some(500), 1_000);
        counters.record(Some(0), Some(300), 1_000);
        assert_eq!(counters.snapshot().max_ns, 500);
        assert_eq!(counters.take_recent(), 500);
        assert_eq!(counters.take_recent(), 0);
        counters.record(Some(0), Some(700), 1_000);
        counters.record(Some(0), Some(400), 1_000);
        assert_eq!(counters.snapshot().max_ns, 700);
        assert_eq!(counters.take_recent(), 700);
        // A failed clock read is not a peak.
        counters.record(None, Some(900), 1_000);
        assert_eq!(counters.take_recent(), 0);
    }
}
