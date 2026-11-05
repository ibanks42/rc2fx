//! Optional non-main callback measurement. Slots are never reused in a process:
//! failed/replaced registrations may still have an in-flight callback. Exhaustion
//! forwards stock unchanged. This deliberately trades finite coverage for lifetime
//! safety without introducing callback locks or reclamation machinery.
use super::{Process, timing};
use std::{
    ffi::{c_int, c_void},
    sync::{
        OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
const CAPACITY: usize = 128;
struct Slot {
    callback: AtomicUsize,
    argument: AtomicUsize,
    client: AtomicUsize,
    tid: AtomicU64,
    name: OnceLock<String>,
    timing: timing::Shared,
    events: super::client_timing_events::Events,
}
impl Slot {
    const fn new() -> Self {
        Self {
            callback: AtomicUsize::new(0),
            argument: AtomicUsize::new(0),
            client: AtomicUsize::new(0),
            tid: AtomicU64::new(0),
            name: OnceLock::new(),
            timing: timing::Shared::new(),
            events: super::client_timing_events::Events::new(),
        }
    }
}
static SLOTS: [Slot; CAPACITY] = [const { Slot::new() }; CAPACITY];
static EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static ENABLED: OnceLock<bool> = OnceLock::new();
pub fn active() -> bool {
    ENABLED.get().copied().unwrap_or(false)
}
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("RC2FX_CLIENT_TIMING").is_some_and(|v| v == "1"))
}
// Registration is serialized by the caller's CONTROL lock. Callback data is
// published before JACK receives the wrapper, and never mutated afterwards.
fn reserve(
    slots: &'static [Slot],
    client: usize,
    callback: Process,
    argument: usize,
    name: Option<String>,
) -> Option<(Process, *mut c_void)> {
    let slot = slots
        .iter()
        .find(|s| s.callback.load(Ordering::Acquire) == 0)?;
    if let Some(name) = name {
        let _ = slot.name.set(name);
    }
    slot.client.store(client, Ordering::Relaxed);
    slot.argument.store(argument, Ordering::Relaxed);
    slot.callback.store(callback as usize, Ordering::Release);
    Some((process, (slot as *const Slot).cast_mut().cast()))
}
pub fn wrap(client: usize, callback: Process, argument: *mut c_void) -> (Process, *mut c_void) {
    reserve(
        &SLOTS,
        client,
        callback,
        argument as usize,
        super::probe::client_name(client),
    )
    .unwrap_or_else(|| {
        EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        (callback, argument)
    })
}
unsafe extern "C" fn process(frames: u32, opaque: *mut c_void) -> c_int {
    let slot = unsafe { &*opaque.cast::<Slot>() };
    let callback: Process = unsafe { std::mem::transmute(slot.callback.load(Ordering::Acquire)) };
    if slot.tid.load(Ordering::Relaxed) == 0 {
        slot.tid.store(
            unsafe { libc::syscall(libc::SYS_gettid) } as u64,
            Ordering::Relaxed,
        );
    }
    let started = timing::monotonic();
    let cycle = super::probe::cycle_start(slot.client.load(Ordering::Relaxed));
    let frame = slot.timing.begin(frames);
    let before = timing::Frame::before_native(&frame);
    let result = unsafe { callback(frames, slot.argument.load(Ordering::Relaxed) as *mut c_void) };
    let after = timing::Frame::after_native(&frame);
    let ended = timing::monotonic();
    slot.timing.finish(frame, before, after);
    slot.events.record(
        started,
        ended,
        cycle,
        u64::from(frames) * 1_000_000_000 / 48_000,
    );
    result
}
pub fn snapshot() -> serde_json::Value {
    serde_json::json!({
        "enabled": ENABLED.get().copied().unwrap_or(false),
        "capacity": CAPACITY, "exhausted": EXHAUSTED.load(Ordering::Relaxed),
        "scope": "registration lifetime; includes replaced, failed and closed registrations; slots never reused; not a list of live clients",
        "registrations": SLOTS.iter().enumerate().filter_map(|(index, slot)| {
            let callback = slot.callback.load(Ordering::Acquire);
            (callback != 0).then(|| serde_json::json!({
                "registration": index, "name": slot.name.get(), "client": slot.client.load(Ordering::Relaxed),
                "callback": callback, "tid": slot.tid.load(Ordering::Relaxed),
                "timing": slot.timing.snapshot(), "lateCallbacks": slot.events.snapshot()
            }))
        }).collect::<Vec<_>>()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn vendor(frames: u32, argument: *mut c_void) -> c_int {
        assert_eq!(frames, 112);
        assert_eq!(argument as usize, 123);
        -7
    }
    unsafe extern "C" fn registration(
        client: *mut c_void,
        callback: Option<Process>,
        argument: *mut c_void,
    ) -> c_int {
        assert_eq!(client as usize, 42);
        match callback {
            Some(callback) => unsafe { callback(112, argument) },
            None => 9,
        }
    }
    #[test]
    fn native_registration_preserves_failure_and_null_callback_results() {
        let _guard = super::super::CONTROL.lock().unwrap();
        let slots = Box::leak(Box::new([Slot::new()]));
        let (callback, argument) = reserve(slots, 42, vendor, 123, Some("usb1out".into())).unwrap();
        let result = unsafe {
            super::super::native_registration(
                registration,
                42usize as *mut c_void,
                Some(callback),
                argument,
                None,
            )
        };
        assert_eq!(result, -7);
        assert_eq!(unsafe { callback(112, argument) }, -7);
        let result = unsafe {
            super::super::native_registration(
                registration,
                42usize as *mut c_void,
                None,
                std::ptr::null_mut(),
                None,
            )
        };
        assert_eq!(result, 9);
    }
    #[test]
    #[ignore = "explicit host callback probe overhead benchmark"]
    fn callback_probe_overhead() {
        use std::{hint::black_box, time::Instant};
        let slots = Box::leak(Box::new([Slot::new()]));
        let (wrapped, argument) = reserve(slots, 42, vendor, 123, Some("usb1out".into())).unwrap();
        let measure = |callback: Process, argument| {
            let start = Instant::now();
            for _ in 0..100_000 {
                black_box(unsafe { black_box(callback)(112, black_box(argument)) });
            }
            start.elapsed().as_nanos() as f64 / 100_000.0
        };
        let direct = measure(vendor, 123usize as *mut c_void);
        let measured = measure(wrapped, argument);
        eprintln!(
            "host ns/callback: direct={direct:.1}, instrumented={measured:.1}, added={:.1}; cycle getter unavailable in this fixture",
            measured - direct
        );
        assert_eq!(slots[0].timing.snapshot().attempts, 100_000);
    }
    #[test]
    fn forwarding_and_retained_registration_lifetimes() {
        let slots = Box::leak(Box::new([Slot::new(), Slot::new()]));
        let (first, arg1) = reserve(slots, 42, vendor, 123, Some("usb1out".into())).unwrap();
        let (second, arg2) = reserve(slots, 42, vendor, 123, Some("usb1out".into())).unwrap();
        assert_ne!(arg1, arg2, "client-pointer reuse gets a distinct slot");
        assert!(reserve(slots, 43, vendor, 123, None).is_none());
        // Both old and new callback arguments remain valid even if the caller
        // rejected or replaced a registration, or closed and reused its client.
        assert_eq!(unsafe { first(112, arg1) }, -7);
        assert_eq!(unsafe { second(112, arg2) }, -7);
        assert_eq!(slots[0].name.get().map(String::as_str), Some("usb1out"));
        assert_eq!(slots[0].timing.snapshot().attempts, 1);
        assert_eq!(slots[1].timing.snapshot().attempts, 1);
    }
}
