//! Keeps the denoiser's weight streaming out of the audio callback's time.
//!
//! The mixer's four cores share one L2 cache and one memory bus. The
//! suppression worker (CPU 0) reads about 590 KB of weights every 5 ms hop,
//! and while it does, the audio callback (CPU 2) waits longer for each of its
//! own cache misses. The library marks the callback's span in
//! [`CALLBACK_BUSY`]; before each large weight product the worker's model
//! checks it, and while it is set, sleeps in [`NAP`] slices until it clears.
//!
//! Two limits keep the waiting from costing the frame its deadline, both set
//! per frame by [`grant`]: a budget for the frame's total wait, and a window
//! measured from the frame's submission, after which the frame never waits.
//! A frame that starts late (its predecessor ran long) therefore waits less or
//! not at all, so one slow frame cannot push the next ones later. A callback
//! that never clears the mark (a stopped engine) costs each frame at most the
//! budget, and a caller that granted nothing (the inline endpoint, tests)
//! never waits.
//!
//! One denoiser runs per process (the service allows suppression on one
//! target), so the limits are statics.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Set by the audio callback while it runs.
pub static CALLBACK_BUSY: AtomicBool = AtomicBool::new(false);
/// The longest one sleep while held. A callback takes about 800 us on the
/// mixer, so a wait is a handful of sleeps, and the frame resumes at most this
/// much (plus the wake-up) after the callback ends. Measured on the mixer
/// (2026-11-11): sleeping costs the worker about 77 us of CPU per 5 ms frame,
/// spinning about 300, for the same callback gain.
pub const NAP: Duration = Duration::from_micros(100);
/// How long after its submission a frame may still wait, in nanoseconds.
/// Production uses the default; tests vary it.
pub static WINDOW_NS: AtomicU64 = AtomicU64::new(DEFAULT_WINDOW_NS);
/// Measured on the mixer (2026-11-12, 10-minute blocks): with a window of
/// 1.5 ms or less a frame has rarely started before the window closes, and
/// waits under 100 us per frame, which gains the callback nothing; from 2.5 ms
/// on it waits 600 to 750 us and RØDE's share of the callback falls by about
/// 14 us, the same at 4 and 6 ms. Frames then take up to 3.75 ms from
/// submission (2.5 without the hold). The worst case is bounded by the
/// frame's budget, not the window: the slowest frames take about 3.9 ms
/// without the hold, plus at most the 1 ms budget, inside the 6.67 ms
/// allowance.
pub const DEFAULT_WINDOW_NS: u64 = 2_500_000;
/// Nanoseconds the current frame may still spend waiting.
static LEFT_NS: AtomicU64 = AtomicU64::new(0);
/// When the current frame's window closes, in nanoseconds since [`epoch`].
static UNTIL_NS: AtomicU64 = AtomicU64::new(0);
/// Totals, for reports: waits, nanoseconds waited, waits that used up the
/// frame's budget with the callback still running, and waits cut short or
/// skipped because the frame's window had closed.
pub static HOLDS: AtomicU64 = AtomicU64::new(0);
pub static HELD_NS: AtomicU64 = AtomicU64::new(0);
pub static EXHAUSTED: AtomicU64 = AtomicU64::new(0);
pub static PAST_WINDOW: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// Lets the next frame wait up to `budget_ns` in total, and only within the
/// window that opened `since_submitted_ns` ago (None, an unreadable clock,
/// grants nothing). The worker calls this before each frame and
/// `grant(0, None)` after it.
pub fn grant(budget_ns: u64, since_submitted_ns: Option<u64>) {
    let open = since_submitted_ns
        .map(|since| WINDOW_NS.load(Ordering::Relaxed).saturating_sub(since))
        .unwrap_or(0);
    if budget_ns != 0 && open == 0 {
        PAST_WINDOW.fetch_add(1, Ordering::Relaxed);
    }
    UNTIL_NS.store(now_ns().saturating_add(open), Ordering::Relaxed);
    LEFT_NS.store(budget_ns.min(open), Ordering::Relaxed);
}

/// Called before each large weight product.
#[inline(always)]
pub(crate) fn gate() {
    if CALLBACK_BUSY.load(Ordering::Relaxed) && LEFT_NS.load(Ordering::Relaxed) != 0 {
        wait();
    }
}

#[cold]
#[inline(never)]
fn wait() {
    let left = LEFT_NS.load(Ordering::Relaxed);
    let start = now_ns();
    let open = UNTIL_NS.load(Ordering::Relaxed).saturating_sub(start);
    let limit = left.min(open);
    let mut waited = 0;
    // The line holding the flag stays in this core's L1 until the callback
    // writes it, so looking at it adds no memory traffic.
    while CALLBACK_BUSY.load(Ordering::Acquire) {
        if waited >= limit {
            if open < left {
                PAST_WINDOW.fetch_add(1, Ordering::Relaxed);
            } else {
                EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            }
            break;
        }
        std::thread::sleep(NAP.min(Duration::from_nanos(limit - waited)));
        waited = now_ns().saturating_sub(start);
    }
    // A window that has closed ends this frame's waiting.
    let left = if open <= waited {
        0
    } else {
        left.saturating_sub(waited)
    };
    LEFT_NS.store(left, Ordering::Relaxed);
    HOLDS.fetch_add(1, Ordering::Relaxed);
    HELD_NS.fetch_add(waited, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The statics are shared; these tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn timed(f: impl FnOnce()) -> Duration {
        let t = Instant::now();
        f();
        t.elapsed()
    }

    #[test]
    fn nothing_waits_without_a_grant_or_without_the_mark() {
        let _turn = TURN.lock().unwrap();
        WINDOW_NS.store(DEFAULT_WINDOW_NS, Ordering::Relaxed);
        let spent = timed(|| {
            grant(0, Some(0));
            CALLBACK_BUSY.store(true, Ordering::Relaxed);
            gate();
            CALLBACK_BUSY.store(false, Ordering::Relaxed);
            grant(5_000_000, Some(0));
            gate();
            grant(0, None);
        });
        assert!(spent.as_millis() < 5, "{spent:?}");
    }

    #[test]
    fn a_frame_waits_at_most_its_budget_in_total() {
        let _turn = TURN.lock().unwrap();
        WINDOW_NS.store(1_000_000_000, Ordering::Relaxed);
        CALLBACK_BUSY.store(true, Ordering::Relaxed);
        grant(2_000_000, Some(0));
        let spent = timed(|| (0..10).for_each(|_| gate()));
        CALLBACK_BUSY.store(false, Ordering::Relaxed);
        grant(0, None);
        WINDOW_NS.store(DEFAULT_WINDOW_NS, Ordering::Relaxed);
        assert!(
            spent.as_micros() >= 2_000 && spent.as_micros() < 20_000,
            "{spent:?}"
        );
    }

    #[test]
    fn a_frame_never_waits_past_its_window() {
        let _turn = TURN.lock().unwrap();
        WINDOW_NS.store(3_000_000, Ordering::Relaxed);
        CALLBACK_BUSY.store(true, Ordering::Relaxed);
        // Submitted 1 ms ago with a 3 ms window: at most about 2 ms of waiting
        // left, though the budget allows a second.
        grant(1_000_000_000, Some(1_000_000));
        let spent = timed(|| (0..10).for_each(|_| gate()));
        assert!(
            spent.as_micros() >= 1_500 && spent.as_micros() < 20_000,
            "{spent:?}"
        );
        // Submitted after its window closed: no waiting at all.
        let past = PAST_WINDOW.load(Ordering::Relaxed);
        grant(1_000_000_000, Some(3_000_000));
        let spent = timed(gate);
        CALLBACK_BUSY.store(false, Ordering::Relaxed);
        grant(0, None);
        WINDOW_NS.store(DEFAULT_WINDOW_NS, Ordering::Relaxed);
        assert!(spent.as_micros() < 1_000, "{spent:?}");
        assert!(PAST_WINDOW.load(Ordering::Relaxed) > past);
    }

    #[test]
    fn a_wait_ends_when_the_callback_clears_the_mark() {
        let _turn = TURN.lock().unwrap();
        WINDOW_NS.store(1_000_000_000, Ordering::Relaxed);
        CALLBACK_BUSY.store(true, Ordering::Relaxed);
        grant(1_000_000_000, Some(0));
        let clearer = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(5));
            CALLBACK_BUSY.store(false, Ordering::Release);
        });
        let spent = timed(gate);
        clearer.join().unwrap();
        grant(0, None);
        WINDOW_NS.store(DEFAULT_WINDOW_NS, Ordering::Relaxed);
        assert!(
            spent.as_millis() >= 4 && spent.as_millis() < 500,
            "{spent:?}"
        );
    }
}
