//! Marks the audio callback's span for the denoiser worker (rc2fx-dfn
//! `hold`): while the mark is set, the worker's model waits before each large
//! weight product instead of streaming weights through the shared L2 and
//! memory bus, within a budget and a window per frame.
//!
//! The hold is always on. The report's `denoiserHold` says how often the
//! worker waited and how much of its budget and window it used.
use rc2fx_suppression::hold;
use std::sync::atomic::Ordering;

/// The callback has started.
#[inline(always)]
pub fn enter() {
    hold::CALLBACK_BUSY.store(true, Ordering::Relaxed);
}

/// The callback is done.
#[inline(always)]
pub fn leave() {
    hold::CALLBACK_BUSY.store(false, Ordering::Release);
}

pub fn report() -> serde_json::Value {
    serde_json::json!({
        "holds": hold::HOLDS.load(Ordering::Relaxed),
        "heldNs": hold::HELD_NS.load(Ordering::Relaxed),
        "exhausted": hold::EXHAUSTED.load(Ordering::Relaxed),
        "pastWindow": hold::PAST_WINDOW.load(Ordering::Relaxed),
    })
}
