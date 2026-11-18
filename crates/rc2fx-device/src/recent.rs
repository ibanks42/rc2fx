//! Sliding worst-case timing over the engine's recent telemetry windows.
//!
//! An engine window carries a peak, not every sample. Only control threads
//! call this module: it keeps the peaks young enough to describe the last
//! [`WINDOW`] and lets the rest fall out, so an engine-startup spike cannot
//! outlive its ten seconds. An empty ring means nothing was timed there, never
//! that the cost was zero.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How far back a recent maximum looks.
pub const WINDOW: Duration = Duration::from_secs(10);
/// Peaks kept. The controller reads at most about nine windows a second, so
/// ten seconds is under a hundred; this only bounds a caller that reads
/// faster than that.
const MAX_PEAKS: usize = 256;

/// The largest peak still inside the window, with the peaks that could still
/// become it. Values fall from the front to the back, so the front is the
/// answer and every peak behind it is older and smaller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentMax {
    peaks: VecDeque<(Instant, u64)>,
}

impl RecentMax {
    /// Adds one window's peak. `None` is a window in which nothing was timed;
    /// it still ages the others out. A peak no larger than a later one can
    /// never be the maximum again while that one is inside the window, so it
    /// is dropped at once. A zero peak is a failed measurement, not a free
    /// window, and is treated as `None`.
    pub fn observe(&mut self, peak: Option<u64>, now: Instant) {
        while self
            .peaks
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= WINDOW)
        {
            self.peaks.pop_front();
        }
        let Some(peak) = peak.filter(|peak| *peak > 0) else {
            return;
        };
        while self.peaks.back().is_some_and(|(_, value)| *value <= peak) {
            self.peaks.pop_back();
        }
        self.peaks.push_back((now, peak));
        if self.peaks.len() > MAX_PEAKS {
            // Values fall towards the back, so this is never the maximum.
            self.peaks.pop_back();
        }
    }

    /// The largest peak inside the window, or `None` with nothing timed there.
    pub fn value(&self, now: Instant) -> Option<u64> {
        self.peaks
            .iter()
            .find(|(at, _)| now.duration_since(*at) < WINDOW)
            .map(|(_, value)| *value)
    }

    /// Forgets everything: a new engine generation or a diagnostics reset has
    /// its own maxima, which must not inherit an older engine's.
    pub fn clear(&mut self) {
        self.peaks.clear();
    }

    /// Peaks held, for the boundedness test.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.peaks.len()
    }
}

/// One path's callback and worker maxima over the recent window. The two are
/// read from different producers, but are always filed and reported together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathRecent {
    callback: RecentMax,
    worker: RecentMax,
}

impl PathRecent {
    /// Files one read's callback and worker peaks. `None` is a read that timed
    /// nothing there; it still ages the older peaks out.
    pub fn observe(&mut self, callback: Option<u64>, worker: Option<u64>, now: Instant) {
        self.callback.observe(callback, now);
        self.worker.observe(worker, now);
    }

    /// The callback and worker maxima still inside the window.
    pub fn value(&self, now: Instant) -> (Option<u64>, Option<u64>) {
        (self.callback.value(now), self.worker.value(now))
    }

    /// A new engine generation or a diagnostics reset starts over.
    pub fn clear(&mut self) {
        self.callback.clear();
        self.worker.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_timed_and_a_failed_clock_are_not_a_maximum() {
        let mut recent = RecentMax::default();
        let now = Instant::now();
        assert_eq!(recent.value(now), None);
        recent.observe(None, now);
        recent.observe(Some(0), now);
        assert_eq!(recent.value(now), None);
        recent.observe(Some(700), now);
        assert_eq!(recent.value(now), Some(700));
    }

    #[test]
    fn the_worst_inside_the_window_wins_and_ages_out() {
        let mut recent = RecentMax::default();
        let start = Instant::now();
        recent.observe(Some(900), start);
        recent.observe(Some(300), start + Duration::from_secs(1));
        recent.observe(Some(400), start + Duration::from_secs(9));
        assert_eq!(recent.value(start + Duration::from_secs(9)), Some(900));
        // The 900 is now ten seconds old; the 400 inside the window wins.
        assert_eq!(recent.value(start + Duration::from_secs(10)), Some(400));
        recent.observe(None, start + Duration::from_secs(10));
        assert_eq!(recent.value(start + Duration::from_secs(10)), Some(400));
        assert_eq!(recent.value(start + Duration::from_secs(18)), Some(400));
        assert_eq!(recent.value(start + Duration::from_secs(19)), None);
    }

    #[test]
    fn a_later_peak_retires_only_the_ones_it_beats() {
        let mut recent = RecentMax::default();
        let start = Instant::now();
        recent.observe(Some(300), start);
        recent.observe(Some(900), start + Duration::from_secs(1));
        recent.observe(Some(100), start + Duration::from_secs(2));
        assert_eq!(recent.value(start + Duration::from_secs(2)), Some(900));
        // With the 900 expired, the 100 that was still inside is the answer.
        assert_eq!(
            recent.value(start + Duration::from_secs(11) + Duration::from_millis(500)),
            Some(100)
        );
        assert_eq!(recent.value(start + Duration::from_secs(12)), None);
    }

    #[test]
    fn equal_peaks_keep_the_newer_of_the_two() {
        let mut recent = RecentMax::default();
        let start = Instant::now();
        recent.observe(Some(500), start);
        recent.observe(Some(500), start + Duration::from_secs(5));
        assert_eq!(recent.len(), 1);
        // The newer one is the one that carries the value forward.
        assert_eq!(recent.value(start + Duration::from_secs(14)), Some(500));
        assert_eq!(recent.value(start + Duration::from_secs(15)), None);
    }

    #[test]
    fn clearing_starts_over() {
        let mut recent = RecentMax::default();
        let start = Instant::now();
        recent.observe(Some(900), start);
        recent.observe(Some(800), start + Duration::from_secs(1));
        recent.clear();
        assert_eq!(recent.value(start + Duration::from_secs(1)), None);
        assert_eq!(recent.len(), 0);
    }

    #[test]
    fn a_paths_peaks_age_out_together_and_clearing_starts_over() {
        let mut recent = PathRecent::default();
        let start = Instant::now();
        recent.observe(Some(900), Some(7_000), start);
        assert_eq!(recent.value(start), (Some(900), Some(7_000)));
        // A read with nothing timed there does not keep the startup spike.
        recent.observe(None, None, start + Duration::from_secs(5));
        assert_eq!(
            recent.value(start + WINDOW),
            (None, None),
            "the startup spike ages out"
        );
        recent.observe(Some(400), Some(1_000), start + WINDOW);
        assert_eq!(recent.value(start + WINDOW), (Some(400), Some(1_000)));
        // A new generation or a diagnostics reset starts over.
        recent.clear();
        assert_eq!(recent.value(start + WINDOW), (None, None));
    }

    #[test]
    fn a_fast_reader_stays_bounded_and_keeps_the_maximum() {
        let mut recent = RecentMax::default();
        let start = Instant::now();
        // Falling peaks, one millisecond apart: each is inside the window and
        // cannot retire the one before it.
        for step in 0..(2 * MAX_PEAKS as u64) {
            recent.observe(Some(1_000_000 - step), start + Duration::from_millis(step));
        }
        assert_eq!(recent.len(), MAX_PEAKS);
        assert_eq!(
            recent.value(start + Duration::from_secs(1)),
            Some(1_000_000)
        );
        assert_eq!(recent.value(start + Duration::from_secs(11)), None);
    }
}
