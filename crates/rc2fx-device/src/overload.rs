//! Control-thread circuit breaker for sustained unusable audio. A single
//! startup burst is not a trip, and historical counters establish a baseline.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
pub(crate) struct Counts {
    /// JACK reports the same server event to multiple clients: take their max.
    pub xruns: u64,
    /// Sum of late suppression frames across independent workers.
    pub late: u64,
}

#[derive(Default)]
pub(crate) struct Overload {
    baseline: Option<(Instant, Counts)>,
    bad_windows: u8,
}

impl Overload {
    pub fn observe(&mut self, now: Instant, counts: Counts) -> Option<String> {
        let Some((start, old)) = self.baseline else {
            self.baseline = Some((now, counts));
            return None;
        };
        let elapsed = now.saturating_duration_since(start);
        if counts.xruns < old.xruns || counts.late < old.late {
            self.baseline = Some((now, counts));
            self.bad_windows = 0;
            return None;
        }
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        self.baseline = Some((now, counts));
        let xruns = counts.xruns - old.xruns;
        let late = counts.late - old.late;
        // At least ten new faults/second for three consecutive observation
        // windows. Scale by actual time so a delayed poll does not inflate rate.
        let bad = u128::from(xruns.max(late)) * 1_000_000_000 >= elapsed.as_nanos() * 10;
        self.bad_windows = if bad { self.bad_windows + 1 } else { 0 };
        (self.bad_windows >= 3).then(|| {
            format!(
                "Sustained audio overload: {xruns} JACK underruns and {late} late suppression frames in {} ms after three consecutive overloaded windows; restoring stock audio. Saved effects are unchanged.",
                elapsed.as_millis()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_counts_and_one_startup_burst_do_not_trip() {
        let mut guard = Overload::default();
        let start = Instant::now();
        for (second, xruns) in [(0, 8000), (1, 9000), (2, 9000), (3, 9010), (4, 9010)] {
            assert!(
                guard
                    .observe(
                        start + Duration::from_secs(second),
                        Counts { xruns, late: 0 }
                    )
                    .is_none()
            );
        }
    }

    #[test]
    fn sustained_xruns_or_worker_misses_trip_after_three_windows() {
        for worker in [false, true] {
            let mut guard = Overload::default();
            let start = Instant::now();
            for second in 0..=3 {
                let count = second * 112;
                let counts = if worker {
                    Counts {
                        late: count,
                        xruns: 0,
                    }
                } else {
                    Counts {
                        xruns: count,
                        late: 0,
                    }
                };
                assert_eq!(
                    guard
                        .observe(start + Duration::from_secs(second), counts)
                        .is_some(),
                    second == 3
                );
            }
        }
    }

    #[test]
    fn delayed_poll_and_counter_reset_do_not_inherit_a_trip() {
        let mut guard = Overload::default();
        let start = Instant::now();
        for (second, xruns) in [
            (0, 0),
            (1, 20),
            (2, 40),
            (12, 60),
            (13, 80),
            (14, 100),
            (15, 0),
            (16, 20),
        ] {
            assert!(
                guard
                    .observe(
                        start + Duration::from_secs(second),
                        Counts { xruns, late: 0 }
                    )
                    .is_none()
            );
        }
    }

    #[test]
    fn repeated_reads_do_not_turn_one_window_into_three() {
        let mut guard = Overload::default();
        let start = Instant::now();
        guard.observe(start, Counts::default());
        for ms in 1..=2999 {
            assert!(
                guard
                    .observe(
                        start + Duration::from_millis(ms),
                        Counts {
                            xruns: 500,
                            late: 0
                        }
                    )
                    .is_none()
            );
        }
    }
}
