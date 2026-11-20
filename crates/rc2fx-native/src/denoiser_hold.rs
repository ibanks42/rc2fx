//! Signals audio callback activity to the denoiser worker (`rc2fx_suppression::hold`).
//! While set, the worker pauses before large weight products so it doesn't
//! thrash the shared L2 cache and memory bus during a callback.
use rc2fx_suppression::hold;
use std::sync::atomic::Ordering;

#[inline(always)]
pub fn enter() {
    hold::CALLBACK_BUSY.store(true, Ordering::Relaxed);
}

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
