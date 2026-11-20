//! Pause denoiser weight streaming while the audio callback is running.
//!
//! On the mixer, all four cores share one L2 cache and memory bus. The
//! suppression worker (CPU 0) streams ~590 KB of weights every 5 ms hop, which
//! stalls the audio callback (CPU 2) on cache misses. The audio callback sets
//! [`CALLBACK_BUSY`] while it runs, and the worker checks it before each large
//! matvec and sleeps in [`NAP`] increments until it clears.
//!
//! To keep waiting from blowing a frame's deadline, [`grant`] sets two limits
//! per frame: a total wait budget and a cutoff window measured from frame
//! submission. Late-starting frames wait less (or not at all) so slow frames
//! don't cascade. If the callback never clears the flag (e.g. engine stopped),
//! each frame burns at most its budget; callers that never call `grant`
//! (benchmarks, tests) never wait.
//!
//! Only one denoiser runs per process, so the state lives in statics.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Set by the audio callback while it is active.
pub static CALLBACK_BUSY: AtomicBool = AtomicBool::new(false);
/// Maximum sleep slice while holding. Callbacks run ~800 us on the mixer, so
/// waiting takes a handful of sleeps. Measured on-device (2026-11-11): sleeping
/// uses ~77 us of worker CPU per 5 ms frame vs ~300 us for spinning, with the
/// same callback benefit.
pub const NAP: Duration = Duration::from_micros(100);
/// Cutoff window after frame submission (ns); overridden in tests.
pub static WINDOW_NS: AtomicU64 = AtomicU64::new(DEFAULT_WINDOW_NS);
/// Measured on-device (2026-11-12, 10-minute blocks): at <=1.5 ms, frames
/// rarely start before the window closes (<100 us wait, no callback win); at
/// 2.5 ms and above they wait 600-750 us and RØDE's callback time drops ~14 us
/// (flat at 4 ms and 6 ms). Worst-case frame time is bounded by the 1 ms wait
/// budget (~3.9 ms + 1 ms, comfortably under the 6.67 ms allowance).
pub const DEFAULT_WINDOW_NS: u64 = 2_500_000;
/// Remaining wait budget for the current frame (ns).
static LEFT_NS: AtomicU64 = AtomicU64::new(0);
/// Deadline (ns since [`epoch`]) when the current frame's wait window closes.
static UNTIL_NS: AtomicU64 = AtomicU64::new(0);
/// Diagnostic counters: total holds, total ns held, holds that hit the budget,
/// and holds skipped or cut short by the submission window.
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

/// Allow the next frame to wait up to `budget_ns` total, within the window
/// opened `since_submitted_ns` ago (`None` grants nothing). Call before each
/// frame and reset with `grant(0, None)` afterward.
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

/// Checked before each large weight matvec.
#[inline(always)]
pub(crate) fn gate() {
    // Only let the hold tests' own threads wait so parallel model tests don't
    // stomp on the shared budget statics.
    #[cfg(test)]
    if !tests::GATED.with(std::cell::Cell::get) {
        return;
    }
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
    // The flag's cache line stays local in L1 until the callback writes it, so
    // polling it here adds no bus traffic.
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

    // Serialize tests that touch the global statics.
    static TURN: Mutex<()> = Mutex::new(());

    thread_local! {
        pub(super) static GATED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        GATED.with(|gated| gated.set(true));
        TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn timed(f: impl FnOnce()) -> Duration {
        let t = Instant::now();
        f();
        t.elapsed()
    }

    #[test]
    fn nothing_waits_without_a_grant_or_without_the_mark() {
        let _turn = turn();
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
        let _turn = turn();
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
        let _turn = turn();
        WINDOW_NS.store(3_000_000, Ordering::Relaxed);
        CALLBACK_BUSY.store(true, Ordering::Relaxed);
        // Submitted 1 ms ago with a 3 ms window -> ~2 ms left despite a 1 s budget.
        grant(1_000_000_000, Some(1_000_000));
        let spent = timed(|| (0..10).for_each(|_| gate()));
        assert!(
            spent.as_micros() >= 1_500 && spent.as_micros() < 20_000,
            "{spent:?}"
        );
        // Window already closed -> shouldn't wait.
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
        let _turn = turn();
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
