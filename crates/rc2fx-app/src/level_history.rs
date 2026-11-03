//! Recent output peaks per processed path, for the level indicator.
//!
//! Each reading is one meter event's output peak (measured after the path's
//! processing), at the time this app received it. Meter events carry no
//! timestamp of their own, so times are approximate: they include transport
//! delay, and readings the device delayed or reordered can't be detected.
//! What redraws an idle Home when readings stop is the app's poll (main.rs).

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// How long a reading can still raise the displayed level: a full-scale
/// reading falls to the bottom of the 60 dB scale in 3 s (see `display`).
const KEEP: Duration = Duration::from_secs(3);
/// A bound on readings per path, whatever the event rate.
const CAPACITY: usize = 300;
/// How fast the displayed level falls after a reading, in fractions of the
/// display's 60 dB meter scale (`desk::level`) per second: linear in dB, so
/// 1/3 is 20 dB/s.
const FALL_PER_SECOND: f32 = 1. / 3.;
/// Readings further apart than this belong to different segments: the
/// displayed level uses only the latest segment, so a reading from before a
/// gap (telemetry stale, then back) never shows after it. The same 1 s after
/// which a path stops counting as fresh (telemetry.rs).
const GAP: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct LevelHistory {
    points: VecDeque<(Instant, f32)>,
}

impl LevelHistory {
    pub(super) fn push(&mut self, at: Instant, peak: f64) {
        // Out-of-order or invalid readings are dropped, never reordered.
        if !peak.is_finite() || self.points.back().is_some_and(|(last, _)| at < *last) {
            return;
        }
        self.points.push_back((at, peak.max(0.) as f32));
        while self.points.len() > CAPACITY
            || self
                .points
                .front()
                .is_some_and(|(first, _)| at.saturating_duration_since(*first) > KEEP)
        {
            self.points.pop_front();
        }
    }

    #[cfg(test)]
    fn last_at(&self) -> Option<Instant> {
        self.points.back().map(|(at, _)| *at)
    }

    /// The displayed level at `now`, 0 to 1 on the display's meter scale
    /// (`scale`, `desk::level`): it rises at once to each reading and then
    /// falls `FALL_PER_SECOND` (linear in dB). Display smoothing, computed
    /// from the readings' times so it doesn't depend on how often it's drawn.
    /// Only the latest segment counts: walking back from the newest reading,
    /// it stops at the first gap longer than `GAP`, and nothing counts if the
    /// newest reading is itself older than `GAP`.
    pub(super) fn display(&self, now: Instant, scale: impl Fn(f64) -> f32) -> f32 {
        let mut level = 0f32;
        let mut later = now;
        for (at, peak) in self.points.iter().rev() {
            if *at > now {
                continue;
            }
            if later.saturating_duration_since(*at) > GAP {
                break;
            }
            let age = now.saturating_duration_since(*at).as_secs_f32();
            level = level.max(scale(f64::from(*peak)) - age * FALL_PER_SECOND);
            later = *at;
        }
        level.max(0.)
    }
}

/// Every reported path's readings, keyed by target id.
#[derive(Default)]
pub(super) struct LevelHistories {
    paths: HashMap<String, LevelHistory>,
}

impl LevelHistories {
    /// One accepted meter event. Paths the event no longer reports are
    /// dropped, so a path that returns starts over.
    pub(super) fn receive<'a>(
        &mut self,
        at: Instant,
        paths: impl IntoIterator<Item = (&'a str, f64)>,
    ) {
        let mut seen = Vec::new();
        for (id, peak) in paths {
            self.paths.entry(id.to_owned()).or_default().push(at, peak);
            seen.push(id);
        }
        self.paths.retain(|id, _| seen.contains(&id.as_str()));
    }

    /// Forget everything: a new connection, engine or source.
    pub(super) fn clear(&mut self) {
        self.paths.clear();
    }

    pub(super) fn get(&self, id: &str) -> Option<&LevelHistory> {
        self.paths.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::desk::level;

    #[test]
    fn rises_at_once_and_falls_20_db_a_second() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(start, 0.5); // about -6 dBFS
        assert_eq!(h.display(start, level), level(0.5));
        let later = start + Duration::from_millis(300);
        // 300 ms at 20 dB/s is 6 dB: 6/60 of the meter scale.
        assert!((h.display(later, level) - (level(0.5) - 0.1)).abs() < 1e-5);
        // The same instant gives the same level however often it's asked.
        assert_eq!(h.display(later, level), h.display(later, level));
        // A louder reading takes over at once.
        h.push(later, 1.0);
        assert_eq!(h.display(later, level), level(1.0));
        assert_eq!(h.display(start + Duration::from_secs(10), level), 0.);
    }

    #[test]
    fn a_reading_from_before_a_gap_never_shows_after_it() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(start, 1.0);
        // Telemetry stops for 1.5 s, then returns quietly.
        let back = start + Duration::from_millis(1500);
        h.push(back, 0.01);
        assert_eq!(h.display(back, level), level(0.01));
        // And with no reading for longer than the gap, nothing is shown.
        assert_eq!(h.display(back + Duration::from_millis(1100), level), 0.);
    }

    #[test]
    fn history_is_bounded_and_ordered() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        for i in 0..2000u64 {
            h.push(start + Duration::from_millis(i), 0.1);
        }
        assert!(h.points.len() <= CAPACITY);
        h.push(start, 0.9);
        assert_ne!(
            h.points.back().map(|p| p.1),
            Some(0.9),
            "older readings are dropped"
        );
        h.push(start + Duration::from_secs(60), f64::NAN);
        assert_eq!(h.last_at(), Some(start + Duration::from_millis(1999)));
    }

    #[test]
    fn paths_that_disappear_start_over() {
        let start = Instant::now();
        let mut all = LevelHistories::default();
        all.receive(start, [("mic-1", 0.5), ("usb-main", 0.2)]);
        all.receive(start + Duration::from_millis(50), [("mic-1", 0.4)]);
        assert!(all.get("usb-main").is_none());
        assert_eq!(all.get("mic-1").unwrap().points.len(), 2);
        all.clear();
        assert!(all.get("mic-1").is_none());
    }
}
