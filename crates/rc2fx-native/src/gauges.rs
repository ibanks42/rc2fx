//! Bench-only gauges, enabled by the RAM marker `/run/rc2fx-gauges` at mixer
//! start. Without it libasound stays unaudited, and libsamplerate too unless
//! `src_fast` audits it, so the loader never asks about their symbols, nothing
//! here is bound and they resolve exactly as stock.
//!
//! The same switch turns on the measurements that cost the callback time and
//! that nothing but a bench run reads: thread CPU clocks (timing.rs), per-path
//! and buffer-resolve costs and the helper wake gauge (pipeline.rs), the
//! cycle-start drift check and the count of buffer lookups made outside the
//! callback (lib.rs).
//!
//! On the 1.7.7 image the USB bridges at 0x6b8900 (out) and 0x6b90d0 (in) are
//! the only callers of `snd_pcm_avail`, `jack_frames_since_cycle_start` and
//! `src_process`. Each calls the first two back to back on its client thread,
//! then runs `src_process` once per channel. The vendor's fill is
//! `ring - avail - frames` for playback and `avail - frames` for capture.
//!
//! A wrapper forwards the call unchanged and adds no allocation, lock or
//! syscall beyond a clock read. Client names are read at registration.
use super::{probe, timing};
use std::{
    cell::Cell,
    ffi::{c_int, c_long, c_void},
    sync::{
        OnceLock,
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
    },
};

/// Loader cookies of the objects audited only while gauges are on.
pub const ASOUND: usize = 4;
pub const SAMPLERATE: usize = 5;
const MAX_HANDLES: usize = 8;
const MAX_CLIENTS: usize = 16;
const STREAM_PLAYBACK: u8 = 0;
const STREAM_CAPTURE: u8 = 1;
const STREAM_UNKNOWN: u8 = 2;
const EPOCH_FILE: &str = "/run/rc2fx-gauge-epoch";

type Avail = unsafe extern "C" fn(*mut c_void) -> c_long;
type FramesSince = unsafe extern "C" fn(*const c_void) -> u32;
type SrcProcess = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type Stream = unsafe extern "C" fn(*mut c_void) -> c_int;

static AVAIL: AtomicUsize = AtomicUsize::new(0);
static FRAMES_SINCE: AtomicUsize = AtomicUsize::new(0);
static SRC_PROCESS: AtomicUsize = AtomicUsize::new(0);
static ASOUND_MAP: AtomicUsize = AtomicUsize::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static OVERFLOW: AtomicU64 = AtomicU64::new(0);
static UNATTRIBUTED_SRC: AtomicU64 = AtomicU64::new(0);

/// One ALSA handle. Only its client thread writes after claiming it, so the
/// plain load-then-store updates below cannot race.
struct Handle {
    pcm: AtomicUsize,
    client: AtomicUsize,
    stream: AtomicUsize,
    epoch: AtomicU64,
    cycles: AtomicU64,
    errors: AtomicU64,
    /// `avail + frames` for playback, `avail - frames` for capture.
    lowest: AtomicI64,
    highest: AtomicI64,
    src_calls: AtomicU64,
    src_total_ns: AtomicU64,
    src_cycle_ns: AtomicU64,
    src_max_cycle_ns: AtomicU64,
}
impl Handle {
    const fn new() -> Self {
        Self {
            pcm: AtomicUsize::new(0),
            client: AtomicUsize::new(0),
            stream: AtomicUsize::new(STREAM_UNKNOWN as usize),
            epoch: AtomicU64::new(0),
            cycles: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            lowest: AtomicI64::new(i64::MAX),
            highest: AtomicI64::new(i64::MIN),
            src_calls: AtomicU64::new(0),
            src_total_ns: AtomicU64::new(0),
            src_cycle_ns: AtomicU64::new(0),
            src_max_cycle_ns: AtomicU64::new(0),
        }
    }
    /// Starts a cycle: closes the previous one's resampler total, and restarts
    /// the extremes when the bench has moved the epoch.
    fn begin_cycle(&self, epoch: u64) {
        let finished = self.src_cycle_ns.swap(0, Ordering::Relaxed);
        if self.epoch.load(Ordering::Relaxed) != epoch {
            self.epoch.store(epoch, Ordering::Relaxed);
            self.lowest.store(i64::MAX, Ordering::Relaxed);
            self.highest.store(i64::MIN, Ordering::Relaxed);
            self.src_max_cycle_ns.store(0, Ordering::Relaxed);
        } else if finished > self.src_max_cycle_ns.load(Ordering::Relaxed) {
            self.src_max_cycle_ns.store(finished, Ordering::Relaxed);
        }
        self.cycles.fetch_add(1, Ordering::Relaxed);
    }
    fn record_position(&self, avail: i64, frames: u32) {
        let frames = i64::from(frames);
        let value = if self.stream.load(Ordering::Relaxed) == STREAM_CAPTURE as usize {
            avail - frames
        } else {
            avail + frames
        };
        if value < self.lowest.load(Ordering::Relaxed) {
            self.lowest.store(value, Ordering::Relaxed);
        }
        if value > self.highest.load(Ordering::Relaxed) {
            self.highest.store(value, Ordering::Relaxed);
        }
    }
    fn record_src(&self, elapsed: u64) {
        self.src_calls.fetch_add(1, Ordering::Relaxed);
        self.src_total_ns.fetch_add(elapsed, Ordering::Relaxed);
        self.src_cycle_ns.fetch_add(elapsed, Ordering::Relaxed);
    }
}
static HANDLES: [Handle; MAX_HANDLES] = [const { Handle::new() }; MAX_HANDLES];

struct Client {
    client: AtomicUsize,
    name: OnceLock<String>,
}
static CLIENTS: [Client; MAX_CLIENTS] = [const {
    Client {
        client: AtomicUsize::new(0),
        name: OnceLock::new(),
    }
}; MAX_CLIENTS];

/// This thread's handle since its last `snd_pcm_avail`, and that call's value
/// until `jack_frames_since_cycle_start` pairs with it.
#[derive(Clone, Copy)]
struct Current {
    handle: Option<&'static Handle>,
    pending: Option<i64>,
}
thread_local! {
    static CURRENT: Cell<Current> = const { Cell::new(Current { handle: None, pending: None }) };
}

/// The flag lives with what every hooked call reads (`READ_MOSTLY`), on a
/// line no thread writes after load.
pub fn enabled() -> bool {
    super::READ_MOSTLY.gauges.load(Ordering::Relaxed)
}

/// Read once, before the first object is opened.
pub fn initialize() {
    let value = unsafe { libc::getenv(c"RC2FX_GAUGES".as_ptr()) };
    let on = !value.is_null() && unsafe { std::ffi::CStr::from_ptr(value) }.to_bytes() == b"1";
    super::READ_MOSTLY.gauges.store(on, Ordering::Relaxed);
}

/// The cookie for an object audited only while gauges are on.
pub fn object(enabled: bool, basename: &[u8]) -> Option<usize> {
    if !enabled {
        None
    } else if basename.starts_with(b"libasound.so") {
        Some(ASOUND)
    } else if basename.starts_with(b"libsamplerate.so") {
        Some(SAMPLERATE)
    } else {
        None
    }
}

pub fn remember_asound(map: usize) {
    ASOUND_MAP.store(map, Ordering::Release);
}

/// Which storage and wrapper serve a symbol the mixer itself binds. Everything
/// else, including any call from a library, stays with the real definition.
fn selected(
    enabled: bool,
    reference: usize,
    definition: usize,
    name: &[u8],
) -> Option<(&'static AtomicUsize, usize)> {
    if !enabled || reference != super::MAIN {
        return None;
    }
    match (definition, name) {
        (ASOUND, b"snd_pcm_avail") => Some((&AVAIL, avail as *const () as usize)),
        (SAMPLERATE, b"src_process") => Some((&SRC_PROCESS, src_process as *const () as usize)),
        (super::JACK, b"jack_frames_since_cycle_start") => {
            Some((&FRAMES_SINCE, frames_since as *const () as usize))
        }
        _ => None,
    }
}

/// The wrapper to bind in place of `address`, if this symbol has one.
pub fn replacement(
    reference: usize,
    definition: usize,
    name: &[u8],
    address: usize,
) -> Option<usize> {
    let (storage, wrapper) = selected(enabled(), reference, definition, name)?;
    super::remember(storage, address).then_some(wrapper)
}

/// Control path: keeps the name so that reports never read it from a client
/// that may have closed since.
pub fn register_client(client: usize) {
    let Some(slot) = CLIENTS.iter().find(|slot| {
        let current = slot.client.load(Ordering::Acquire);
        current == client
            || (current == 0
                && slot
                    .client
                    .compare_exchange(0, client, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok())
    }) else {
        return;
    };
    if let Some(name) = probe::client_name(client) {
        let _ = slot.name.set(name);
    }
}

fn stream_of(pcm: *mut c_void) -> u8 {
    let map = ASOUND_MAP.load(Ordering::Acquire);
    if map == 0 {
        return STREAM_UNKNOWN;
    }
    let address = unsafe { probe::dynamic_symbol(map as *const _, c"snd_pcm_stream") };
    if address == 0 {
        return STREAM_UNKNOWN;
    }
    let stream: Stream = unsafe { std::mem::transmute(address) };
    match unsafe { stream(pcm) } {
        0 => STREAM_PLAYBACK,
        1 => STREAM_CAPTURE,
        _ => STREAM_UNKNOWN,
    }
}

fn handle_for(
    handles: &'static [Handle],
    pcm: usize,
    stream: impl Fn() -> u8,
) -> Option<&'static Handle> {
    for handle in handles {
        match handle.pcm.load(Ordering::Acquire) {
            current if current == pcm => return Some(handle),
            0 => {
                if handle
                    .pcm
                    .compare_exchange(0, pcm, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    handle.stream.store(stream() as usize, Ordering::Relaxed);
                    return Some(handle);
                }
                if handle.pcm.load(Ordering::Acquire) == pcm {
                    return Some(handle);
                }
            }
            _ => {}
        }
    }
    None
}

unsafe extern "C" fn avail(pcm: *mut c_void) -> c_long {
    let real: Avail = unsafe { std::mem::transmute(AVAIL.load(Ordering::Acquire)) };
    let value = unsafe { real(pcm) };
    let handle = handle_for(&HANDLES, pcm as usize, || stream_of(pcm));
    if handle.is_none() {
        OVERFLOW.fetch_add(1, Ordering::Relaxed);
    }
    observe_avail(handle, value as i64);
    value
}

fn observe_avail(handle: Option<&'static Handle>, value: i64) {
    let mut pending = None;
    if let Some(handle) = handle {
        handle.begin_cycle(EPOCH.load(Ordering::Relaxed));
        if value < 0 {
            handle.errors.fetch_add(1, Ordering::Relaxed);
        } else {
            pending = Some(value);
        }
    }
    let _ = CURRENT.try_with(|current| current.set(Current { handle, pending }));
}

unsafe extern "C" fn frames_since(client: *const c_void) -> u32 {
    let real: FramesSince = unsafe { std::mem::transmute(FRAMES_SINCE.load(Ordering::Acquire)) };
    let value = unsafe { real(client) };
    observe_frames(client as usize, value);
    value
}

fn observe_frames(client: usize, frames: u32) {
    let _ = CURRENT.try_with(|current| {
        let mut state = current.get();
        if let (Some(handle), Some(avail)) = (state.handle, state.pending.take()) {
            handle.client.store(client, Ordering::Relaxed);
            handle.record_position(avail, frames);
            current.set(state);
        }
    });
}

unsafe extern "C" fn src_process(state: *mut c_void, data: *mut c_void) -> c_int {
    let real: SrcProcess = unsafe { std::mem::transmute(SRC_PROCESS.load(Ordering::Acquire)) };
    let started = timing::monotonic();
    let result = unsafe { real(state, data) };
    observe_src(timing::monotonic().saturating_sub(started));
    result
}

fn observe_src(elapsed: u64) {
    match CURRENT
        .try_with(Cell::get)
        .ok()
        .and_then(|state| state.handle)
    {
        Some(handle) => handle.record_src(elapsed),
        None => {
            UNATTRIBUTED_SRC.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn client_name(client: usize) -> Option<&'static str> {
    CLIENTS
        .iter()
        .find(|slot| client != 0 && slot.client.load(Ordering::Acquire) == client)
        .and_then(|slot| slot.name.get())
        .map(String::as_str)
}

fn parse_epoch(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}

fn handle_report(handle: &Handle) -> serde_json::Value {
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    let extreme = |value: &AtomicI64, empty: i64| {
        let value = value.load(Ordering::Relaxed);
        (value != empty).then_some(value)
    };
    let stream = handle.stream.load(Ordering::Relaxed) as u8;
    serde_json::json!({
        "client": client_name(handle.client.load(Ordering::Relaxed)),
        "direction": match stream {
            STREAM_PLAYBACK => "playback",
            STREAM_CAPTURE => "capture",
            _ => "unknown",
        },
        "epoch": read(&handle.epoch),
        "cycles": read(&handle.cycles),
        "availErrors": read(&handle.errors),
        "lowestPosition": extreme(&handle.lowest, i64::MAX),
        "highestPosition": extreme(&handle.highest, i64::MIN),
        "srcCalls": read(&handle.src_calls),
        "srcTotalNs": read(&handle.src_total_ns),
        "srcMaxCycleNs": read(&handle.src_max_cycle_ns),
    })
}

/// Report thread only. The marker file's epoch restarts the extremes, so that
/// a settled run does not report the startup transient.
pub fn snapshot() -> serde_json::Value {
    let epoch = std::fs::read_to_string(EPOCH_FILE).map_or(0, |text| parse_epoch(&text));
    EPOCH.store(epoch, Ordering::Relaxed);
    serde_json::json!({
        "epoch": epoch,
        "positionMeaning": "avail + frames for playback, avail - frames for capture, as the vendor bridge computes them; playback fill is the ring size minus highestPosition, capture fill is lowestPosition; extremes restart at each epoch",
        "handleOverflow": OVERFLOW.load(Ordering::Relaxed),
        "unattributedSrcCalls": UNATTRIBUTED_SRC.load(Ordering::Relaxed),
        "handles": HANDLES
            .iter()
            .filter(|handle| handle.pcm.load(Ordering::Acquire) != 0)
            .map(handle_report)
            .collect::<Vec<_>>(),
        "vendorJob": super::vendor_job::gauge_status(),
        "helperWake": super::pipeline::wake_gauges(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaked<const N: usize>() -> &'static [Handle; N] {
        Box::leak(Box::new([const { Handle::new() }; N]))
    }

    #[test]
    fn without_the_marker_no_object_is_audited_and_no_symbol_is_wrapped() {
        for name in [
            b"libasound.so.2".as_slice(),
            b"libsamplerate.so.0",
            b"libjack.so.0",
        ] {
            assert_eq!(object(false, name), None);
        }
        for (definition, name) in [
            (ASOUND, b"snd_pcm_avail".as_slice()),
            (SAMPLERATE, b"src_process"),
            (crate::JACK, b"jack_frames_since_cycle_start"),
        ] {
            assert!(selected(false, crate::MAIN, definition, name).is_none());
            assert!(selected(true, crate::MAIN, definition, name).is_some());
        }
    }

    #[test]
    fn only_the_mixers_own_calls_to_the_three_symbols_are_wrapped() {
        assert_eq!(object(true, b"libasound.so.2"), Some(ASOUND));
        assert_eq!(object(true, b"libsamplerate.so.0.1.8"), Some(SAMPLERATE));
        assert_eq!(object(true, b"libc.so.6"), None);
        assert!(selected(true, 2, ASOUND, b"snd_pcm_avail").is_none());
        assert!(selected(true, crate::MAIN, ASOUND, b"snd_pcm_writei").is_none());
        assert!(selected(true, crate::MAIN, 2, b"snd_pcm_avail").is_none());
    }

    #[test]
    fn playback_and_capture_keep_the_vendors_arithmetic() {
        let handles = leaked::<1>();
        let handle = &handles[0];
        handle.record_position(500, 100);
        handle.record_position(530, 110);
        assert_eq!(handle.lowest.load(Ordering::Relaxed), 600);
        assert_eq!(handle.highest.load(Ordering::Relaxed), 640);
        let capture = &leaked::<1>()[0];
        capture
            .stream
            .store(STREAM_CAPTURE as usize, Ordering::Relaxed);
        capture.record_position(146, 100);
        assert_eq!(capture.lowest.load(Ordering::Relaxed), 46);
    }

    #[test]
    fn avail_pairs_once_with_the_next_frame_count_on_its_own_thread() {
        let handle = &leaked::<1>()[0];
        std::thread::scope(|scope| {
            scope.spawn(|| {
                observe_avail(Some(handle), 500);
                observe_frames(42, 100);
                observe_frames(42, 300); // unpaired: no new position
                observe_avail(Some(handle), -32); // -EPIPE
                observe_frames(42, 100);
            });
        });
        assert_eq!(handle.cycles.load(Ordering::Relaxed), 2);
        assert_eq!(handle.errors.load(Ordering::Relaxed), 1);
        assert_eq!(handle.lowest.load(Ordering::Relaxed), 600);
        assert_eq!(handle.highest.load(Ordering::Relaxed), 600);
        assert_eq!(handle.client.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn resampler_time_sums_per_cycle_and_the_epoch_restarts_extremes() {
        let handle = &leaked::<1>()[0];
        handle.begin_cycle(0);
        handle.record_src(30);
        handle.record_src(40);
        handle.begin_cycle(0);
        handle.record_src(10);
        handle.begin_cycle(0);
        assert_eq!(handle.src_calls.load(Ordering::Relaxed), 3);
        assert_eq!(handle.src_total_ns.load(Ordering::Relaxed), 80);
        assert_eq!(handle.src_max_cycle_ns.load(Ordering::Relaxed), 70);
        handle.record_position(10, 1);
        handle.begin_cycle(9);
        assert_eq!(handle.lowest.load(Ordering::Relaxed), i64::MAX);
        assert_eq!(handle.src_max_cycle_ns.load(Ordering::Relaxed), 0);
        assert_eq!(handle.src_total_ns.load(Ordering::Relaxed), 80);
    }

    #[test]
    fn a_full_table_forwards_without_recording() {
        let handles = leaked::<2>();
        assert!(handle_for(handles, 1, || STREAM_PLAYBACK).is_some());
        assert!(handle_for(handles, 2, || STREAM_CAPTURE).is_some());
        assert!(handle_for(handles, 3, || STREAM_CAPTURE).is_none());
        let again = handle_for(handles, 2, || unreachable!()).unwrap();
        assert_eq!(
            again.stream.load(Ordering::Relaxed),
            STREAM_CAPTURE as usize
        );
    }

    #[test]
    fn a_bad_epoch_file_reads_as_no_epoch() {
        assert_eq!(parse_epoch("17\n"), 17);
        assert_eq!(parse_epoch("x"), 0);
    }

    #[test]
    fn recording_allocates_nothing() {
        use crate::{
            CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
        };
        let handle = &leaked::<1>()[0];
        // First touch of the thread's state is outside the measured region.
        observe_avail(Some(handle), 1);
        let counts = || {
            [
                CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
                CALLBACK_FREES.load(Ordering::Relaxed),
                CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
            ]
        };
        let before = counts();
        let previous = INSIDE.with(|value| {
            value.replace(CallbackFrame {
                owner: 1,
                processing: true,
                pipeline: 0,
                started: 0,
            })
        });
        for _ in 0..1024 {
            observe_avail(Some(handle), 500);
            observe_frames(7, 100);
            observe_src(10);
        }
        INSIDE.with(|value| value.set(previous));
        assert_eq!(counts(), before);
    }
}
