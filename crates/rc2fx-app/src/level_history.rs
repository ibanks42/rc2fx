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

/// How long a reading can still affect the displayed level: by then a reading
/// has been replaced, whichever way the level was moving.
const KEEP: Duration = Duration::from_secs(1);
/// A bound on readings per path, whatever the event rate.
const CAPACITY: usize = 300;
/// How quickly the displayed level closes on the latest reading, as the time
/// to cover about two thirds of the distance. Readings arrive a few times a
/// second, so the level eases between them instead of stepping. A peak still
/// shows almost at once; a level that stops is gone in about half a second.
const RISE: Duration = Duration::from_millis(40);
const FALL: Duration = Duration::from_millis(120);
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
    /// (`scale`, `desk::level`): it closes on each reading in turn (`RISE`,
    /// `FALL`), starting from nothing. Display smoothing, computed from the
    /// readings' times so it doesn't depend on how often it's drawn.
    /// Only the latest segment counts: walking back from the newest reading,
    /// it stops at the first gap longer than `GAP`, and nothing counts if the
    /// newest reading is itself older than `GAP`.
    pub(super) fn display(&self, now: Instant, scale: impl Fn(f64) -> f32) -> f32 {
        let mut first = self.points.len();
        let mut later = now;
        for (index, (at, _)) in self.points.iter().enumerate().rev() {
            if *at > now {
                continue;
            }
            if later.saturating_duration_since(*at) > GAP {
                break;
            }
            first = index;
            later = *at;
        }
        let mut level = 0f32;
        let mut latest = None;
        for (at, peak) in self.points.range(first..).filter(|(at, _)| *at <= now) {
            if let Some((since, target)) = latest {
                level = approach(level, target, at.saturating_duration_since(since));
            }
            latest = Some((*at, scale(f64::from(*peak))));
        }
        latest.map_or(0., |(since, target)| {
            approach(level, target, now.saturating_duration_since(since))
        })
    }
}

/// Where `level` is after closing on `target` for `elapsed`.
fn approach(level: f32, target: f32, elapsed: Duration) -> f32 {
    let time = if target > level { RISE } else { FALL };
    target + (level - target) * (-elapsed.as_secs_f32() / time.as_secs_f32()).exp()
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

    /// `count` readings of `peak`, `step` apart. Returns the last one's time.
    fn steady(
        h: &mut LevelHistory,
        from: Instant,
        step: Duration,
        count: u32,
        peak: f64,
    ) -> Instant {
        for i in 0..count {
            h.push(from + step * i, peak);
        }
        from + step * count.saturating_sub(1)
    }

    #[test]
    fn a_rise_reaches_the_reading_quickly_without_jumping() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(start, 0.5); // about -6 dBFS
        assert_eq!(h.display(start, level), 0., "starts from nothing");
        let soon = h.display(start + Duration::from_millis(16), level);
        assert!(soon > 0. && soon < level(0.5) * 0.5, "{soon}");
        let settled = h.display(start + Duration::from_millis(200), level);
        assert!((settled - level(0.5)).abs() < 0.01, "{settled}");
    }

    #[test]
    fn the_level_drops_soon_after_sound_stops() {
        let start = Instant::now();
        let step = Duration::from_millis(150);
        let mut h = LevelHistory::default();
        let last = steady(&mut h, start, step, 8, 0.5);
        let stop = last + step;
        h.push(stop, 0.);
        // Still on its way down a frame later, not snapped to the bottom.
        let frame = h.display(stop + Duration::from_millis(16), level);
        assert!(frame > level(0.5) * 0.7, "{frame}");
        // Nearly gone within half a second.
        let later = stop + Duration::from_millis(500);
        h.push(stop + step, 0.);
        h.push(stop + step * 2, 0.);
        h.push(stop + step * 3, 0.);
        let gone = h.display(later, level);
        assert!(gone < 0.02, "{gone}");
    }

    #[test]
    fn a_new_reading_changes_direction_without_moving_the_level() {
        let start = Instant::now();
        let step = Duration::from_millis(150);
        let mut h = LevelHistory::default();
        let last = steady(&mut h, start, step, 8, 0.5);
        for (peak, at) in [(1.0, last + step), (0.01, last + step * 2)] {
            let before = h.display(at, level);
            h.push(at, peak);
            assert!((h.display(at, level) - before).abs() < 1e-5);
        }
    }

    #[test]
    fn sparse_readings_move_in_small_steps_each_frame() {
        let start = Instant::now();
        let step = Duration::from_millis(150);
        let frame = Duration::from_micros(16_667);
        let mut h = LevelHistory::default();
        let (mut last, mut next) = (0., 0u32);
        for i in 0..180 {
            let now = start + frame * i;
            while start + step * next <= now {
                let peak = if next % 2 == 0 { 0.5 } else { 0.05 };
                h.push(start + step * next, peak);
                next += 1;
            }
            let shown = h.display(now, level);
            // After the first rise from nothing.
            if i > 30 {
                assert!((shown - last).abs() < 0.1, "frame {i}: {last} to {shown}");
            }
            last = shown;
        }
    }

    #[test]
    fn the_level_does_not_depend_on_how_often_it_is_drawn() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        h.push(start, 0.5);
        let at = start + Duration::from_millis(70);
        assert_eq!(h.display(at, level), h.display(at, level));
    }

    #[test]
    fn a_reading_from_before_a_gap_never_shows_after_it() {
        let start = Instant::now();
        let mut h = LevelHistory::default();
        steady(&mut h, start, Duration::from_millis(100), 5, 1.0);
        // Telemetry stops for 1.5 s, then returns quietly.
        let back = start + Duration::from_millis(1900);
        h.push(back, 0.01);
        assert_eq!(h.display(back, level), 0.);
        let settled = h.display(back + Duration::from_millis(300), level);
        assert!((settled - level(0.01)).abs() < 0.01, "{settled}");
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
