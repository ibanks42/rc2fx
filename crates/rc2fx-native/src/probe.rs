//! Cycle-position histograms and lock-free anomaly event ring for correlating
//! JACK xruns with callback/helper timing.
//!
//! Positions are estimated in ns from cycle start using
//! `jack_frames_since_cycle_start`; treat them as local timing evidence rather
//! than jackd's internal deadline check. All recording is lock- and alloc-free.
use super::{LinkMap, Symbol, timing};
use std::{
    ffi::{CStr, c_char, c_int, c_void},
    io::Write,
    sync::{
        OnceLock,
        atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
};

pub const PERIOD_NS: u64 = timing::BUDGET_NS;
type FramesSinceCycleStart = unsafe extern "C" fn(*const c_void) -> u32;
pub type Xrun = unsafe extern "C" fn(*mut c_void) -> c_int;

/// Resolved `jack_frames_since_cycle_start` pointer (1 = unavailable).
pub static CYCLE_CLOCK: AtomicUsize = AtomicUsize::new(0);
/// `libjack`'s `LinkMap` pointer from `la_objopen`.
pub static JACK_MAP: AtomicUsize = AtomicUsize::new(0);

/// Cycle-position histogram with edges at `[1/8, 1/4, 3/5, 3/4, 9/10, 1, 11/10]`
/// of `PERIOD_NS` (3/5 = `LATE_START_NS`). Cache-line aligned so each writer
/// thread (callback, helper, vendor job) avoids false sharing.
#[repr(align(64))]
pub struct Positions {
    bins: [AtomicU64; 8],
    max: AtomicU64,
    samples: AtomicU64,
}
impl Positions {
    pub(crate) const fn new() -> Self {
        Self {
            bins: [const { AtomicU64::new(0) }; 8],
            max: AtomicU64::new(0),
            samples: AtomicU64::new(0),
        }
    }
    pub fn record(&self, position: u64) {
        let bounds = [
            PERIOD_NS / 8,
            PERIOD_NS / 4,
            LATE_START_NS,
            PERIOD_NS * 3 / 4,
            PERIOD_NS * 9 / 10,
            PERIOD_NS,
            PERIOD_NS * 11 / 10,
        ];
        let bin = bounds
            .iter()
            .position(|bound| position <= *bound)
            .unwrap_or(7);
        self.bins[bin].fetch_add(1, Ordering::Relaxed);
        self.max.fetch_max(position, Ordering::Relaxed);
        self.samples.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        serde_json::json!({
            "histogram": self.bins.iter().map(read).collect::<Vec<_>>(),
            "maxNs": read(&self.max),
            "samples": read(&self.samples),
        })
    }
}
pub static START_POS: Positions = Positions::new();
pub static END_POS: Positions = Positions::new();
pub static HELPER_POS: Positions = Positions::new();
/// Helper frames finishing at or past `HELPER_LATE_NS`.
pub static HELPER_LATE_FINISHES: AtomicU64 = AtomicU64::new(0);
/// Callbacks whose estimated cycle-relative end exceeded one period.
pub static LATE_ENDS: AtomicU64 = AtomicU64::new(0);
/// Consecutive callbacks whose estimated cycle starts jumped by > 1.5 periods.
pub static CYCLE_SKIPS: AtomicU64 = AtomicU64::new(0);
/// Max drift between entry and exit cycle-start estimates (bench gauges only).
pub static ESTIMATE_DRIFT_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// Total JACK xrun client notifications (not unique dropouts; JACK can notify multiple clients).
pub static XRUNS: AtomicU64 = AtomicU64::new(0);
pub static USB_REPORTS: AtomicU64 = AtomicU64::new(0);
static LAST_USB_NS: AtomicU64 = AtomicU64::new(0);

/// Warning thresholds for event logging. The mixer callback normally starts
/// 300–600 us into the cycle behind upstream JACK clients; starting past 3/5
/// of a period (~1400 us at 112 frames) cuts into margin before `LATE_END_NS`.
pub const LATE_START_NS: u64 = PERIOD_NS * 3 / 5;
pub const LATE_END_NS: u64 = PERIOD_NS * 9 / 10;
/// Helper results are consumed by the next callback; finishing past 2 periods is at risk.
pub const HELPER_LATE_NS: u64 = PERIOD_NS * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    LateStart = 1,
    LateEnd = 2,
    Overrun = 3,
    Gap = 4,
    HelperLateFinish = 5,
    HelperStall = 6,
    HelperBusy = 7,
    /// JACK xrun notification (`value` is cycle position at notification time, not duration; `u64::MAX` = unknown).
    Xrun = 8,
    VendorJobLate = 9,
    UsbGadget = 10,
    CycleSkip = 11,
}
impl Kind {
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::LateStart,
            2 => Self::LateEnd,
            3 => Self::Overrun,
            4 => Self::Gap,
            5 => Self::HelperLateFinish,
            6 => Self::HelperStall,
            7 => Self::HelperBusy,
            8 => Self::Xrun,
            9 => Self::VendorJobLate,
            10 => Self::UsbGadget,
            11 => Self::CycleSkip,
            _ => return None,
        })
    }
    fn name(code: u8) -> &'static str {
        match code {
            1 => "lateStart",
            2 => "lateEnd",
            3 => "overrun",
            4 => "gap",
            5 => "helperLateFinish",
            6 => "helperStall",
            7 => "helperBusy",
            8 => "xrun",
            9 => "vendorJobLate",
            10 => "usbGadget",
            11 => "cycleSkip",
            _ => "unknown",
        }
    }
}

const EVENTS: usize = 1024;
struct Event {
    seq: AtomicU64,
    at: AtomicU64,
    value: AtomicU64,
    since_usb: AtomicU64,
    /// Kind-specific extra field (`u64::MAX` = unavailable).
    extra: AtomicU64,
    kind: AtomicU8,
}
static RING: [Event; EVENTS] = [const {
    Event {
        seq: AtomicU64::new(0),
        at: AtomicU64::new(0),
        value: AtomicU64::new(0),
        since_usb: AtomicU64::new(0),
        extra: AtomicU64::new(0),
        kind: AtomicU8::new(0),
    }
}; EVENTS];
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn record(kind: Kind, value: u64) {
    record_extra(kind, value, u64::MAX);
}

/// Records an event with a kind-specific `extra` (for `Xrun`, `jack_get_xrun_delayed_usecs()`).
fn record_extra(kind: Kind, value: u64, extra: u64) {
    let now = timing::monotonic();
    let index = NEXT.fetch_add(1, Ordering::Relaxed);
    let slot = &RING[(index % EVENTS as u64) as usize];
    let usb = LAST_USB_NS.load(Ordering::Relaxed);
    slot.at.store(now, Ordering::Relaxed);
    slot.value.store(value, Ordering::Relaxed);
    slot.extra.store(extra, Ordering::Relaxed);
    slot.since_usb.store(
        if usb == 0 {
            u64::MAX
        } else {
            now.saturating_sub(usb)
        },
        Ordering::Relaxed,
    );
    slot.kind.store(kind as u8, Ordering::Relaxed);
    slot.seq.store(index + 1, Ordering::Release);
}
/// Events recorded so far. Each one read the clock once on its thread.
#[cfg(test)]
pub(crate) fn events_recorded() -> u64 {
    NEXT.load(Ordering::Relaxed)
}
pub fn usb_report() {
    USB_REPORTS.fetch_add(1, Ordering::Relaxed);
    LAST_USB_NS.store(timing::monotonic(), Ordering::Relaxed);
}

struct Snapshot {
    seq: u64,
    at: u64,
    value: u64,
    since_usb: u64,
    extra: u64,
    kind: u8,
}
impl Snapshot {
    fn json(&self) -> serde_json::Value {
        let value = (self.value != u64::MAX).then_some(self.value);
        let mut result = serde_json::json!({
            "seq": self.seq,
            "atNs": self.at,
            "kind": Kind::name(self.kind),
            "valueNs": value,
            "sinceUsbNs": (self.since_usb != u64::MAX).then_some(self.since_usb),
        });
        if self.kind == Kind::Xrun as u8 {
            // Keep `valueNs` for existing consumers, and annotate what it actually is.
            result["notificationCyclePositionNs"] = serde_json::json!(value);
            result["valueMeaning"] = "notificationCyclePositionNotXrunDuration".into();
            // `jack_get_xrun_delayed_usecs()` only updates on backend xruns, so
            // a client-lateness notification can carry a stale value.
            result["xrunDelayedUs"] =
                serde_json::json!((self.extra != u64::MAX).then_some(self.extra));
            result["xrunDelayedMeaning"] = "lastBackendXrunDelayNotThisNotificationDuration".into();
        }
        result
    }
}
pub struct EngineEvent {
    pub seq: u64,
    pub at: u64,
    pub value: u64,
    /// `jack_get_xrun_delayed_usecs()` for xruns (`u64::MAX` when unknown).
    pub delayed_us: u64,
    pub kind: Kind,
}

/// Returns up to the 64 newest ring entries, oldest first.
pub fn recent_events() -> Vec<EngineEvent> {
    let total = NEXT.load(Ordering::Relaxed);
    events_after(total.saturating_sub(64))
        .into_iter()
        .filter_map(|event| {
            let kind = Kind::from_code(event.kind)?;
            Some(EngineEvent {
                seq: event.seq,
                at: event.at,
                value: event.value,
                delayed_us: event.extra,
                kind,
            })
        })
        .collect()
}

/// Reads all ring entries with `seq > after` (oldest first), skipping any slot
/// whose sequence number changes mid-read.
fn events_after(after: u64) -> Vec<Snapshot> {
    let mut events: Vec<Snapshot> = RING
        .iter()
        .filter_map(|slot| {
            let seq = slot.seq.load(Ordering::Acquire);
            if seq <= after {
                return None;
            }
            let event = Snapshot {
                seq,
                at: slot.at.load(Ordering::Relaxed),
                value: slot.value.load(Ordering::Relaxed),
                since_usb: slot.since_usb.load(Ordering::Relaxed),
                extra: slot.extra.load(Ordering::Relaxed),
                kind: slot.kind.load(Ordering::Relaxed),
            };
            (slot.seq.load(Ordering::Acquire) == seq).then_some(event)
        })
        .collect();
    events.sort_by_key(|event| event.seq);
    events
}

pub fn snapshot() -> serde_json::Value {
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    let total = NEXT.load(Ordering::Relaxed);
    let recent: Vec<_> = events_after(total.saturating_sub(64))
        .into_iter()
        .map(|event| event.json())
        .collect();
    serde_json::json!({
        "cycle": {
            "clock": CYCLE_CLOCK.load(Ordering::Relaxed) > 1,
            "periodNs": PERIOD_NS,
            "startPos": START_POS.snapshot(),
            "endPos": END_POS.snapshot(),
            "helperPos": HELPER_POS.snapshot(),
            "lateEnds": read(&LATE_ENDS),
            "helperLateFinishes": read(&HELPER_LATE_FINISHES),
            "cycleSkips": read(&CYCLE_SKIPS),
            // Checked only while the bench gauges are on; null otherwise.
            "estimateDriftMaxNs": super::gauges::enabled().then(|| read(&ESTIMATE_DRIFT_MAX_NS)),
        },
        "xruns": read(&XRUNS),
        "xrunCountMeaning": "clientNotificationsNotUniqueDropouts",
        "xrunDelayed": {
            "getter": XRUN_DELAYED.load(Ordering::Relaxed) != 0,
            "lookup": match XRUN_DELAYED_ROUTE.load(Ordering::Relaxed) {
                ROUTE_MAIN => "main-namespace",
                ROUTE_DEFAULT => "rtld-default",
                ROUTE_MISSING => "not-found",
                _ => "not-tried",
            },
            "zero": read(&XRUN_DELAYED_ZERO),
            "nonZero": read(&XRUN_DELAYED_VALUE),
            "unavailable": read(&XRUN_DELAYED_MISSING),
        },
        "usbReports": read(&USB_REPORTS),
        "events": {"total": total, "recent": recent},
    })
}

static LOG: OnceLock<std::path::PathBuf> = OnceLock::new();
static LOGGED: AtomicU64 = AtomicU64::new(0);
/// Cap lines per flush and total file size so an event storm doesn't wear flash.
const LOG_BURST: usize = 200;
const LOG_CAP: u64 = 4 << 20;
pub fn set_log(path: std::path::PathBuf) {
    let _ = LOG.set(path);
}
/// Flushes new ring events to the log file (called at ~1 Hz from the report thread).
pub fn flush_log() {
    let Some(path) = LOG.get() else {
        return;
    };
    let events = events_after(LOGGED.load(Ordering::Relaxed));
    let Some(last) = events.last() else {
        return;
    };
    let last_seq = last.seq;
    append_log(path, &events);
    LOGGED.store(last_seq, Ordering::Relaxed);
}
fn append_log(path: &std::path::Path, events: &[Snapshot]) {
    let Ok(file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    else {
        return;
    };
    if file.metadata().map_or(true, |info| info.len() >= LOG_CAP) {
        return;
    }
    let mut file = std::io::BufWriter::new(file);
    let now_mono = timing::monotonic();
    let now_wall = std::time::SystemTime::now();
    let skipped = events.len().saturating_sub(LOG_BURST);
    for event in events.iter().skip(skipped) {
        let wall = now_wall
            .checked_sub(std::time::Duration::from_nanos(
                now_mono.saturating_sub(event.at),
            ))
            .unwrap_or(now_wall);
        let _ = writeln!(
            file,
            "{} mono={}.{:06} {} value={}{}{}",
            stamp(wall),
            event.at / 1_000_000_000,
            event.at % 1_000_000_000 / 1000,
            Kind::name(event.kind),
            if event.value == u64::MAX {
                "unknown".into()
            } else {
                format!("{}us", event.value / 1000)
            },
            if event.kind == Kind::Xrun as u8 {
                " valueMeaning=notificationCyclePositionNotXrunDuration"
            } else {
                ""
            },
            if event.since_usb == u64::MAX {
                String::new()
            } else {
                format!(" usbReportBefore={}ms", event.since_usb / 1_000_000)
            }
        );
    }
    if skipped != 0 {
        let _ = writeln!(
            file,
            "{} {skipped} earlier events not written",
            stamp(now_wall)
        );
    }
    let _ = file.flush();
    if file
        .get_ref()
        .metadata()
        .is_ok_and(|info| info.len() >= LOG_CAP)
    {
        let _ = writeln!(
            file,
            "{} log full, later events not written",
            stamp(now_wall)
        );
        let _ = file.flush();
    }
}
fn stamp(time: std::time::SystemTime) -> String {
    let since = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let (secs, millis) = (since.as_secs(), since.subsec_millis());
    let days = secs / 86_400;
    let (hours, minutes, seconds) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Reads a JACK client's name during registration via libjack's `LinkMap`
/// (control path only, never from an audio callback).
pub fn client_name(client: usize) -> Option<String> {
    let map = JACK_MAP.load(Ordering::Acquire);
    if map == 0 || client == 0 {
        return None;
    }
    let address = unsafe { dynamic_symbol(map as *const LinkMap, c"jack_get_client_name") };
    if address == 0 {
        return None;
    }
    let get: unsafe extern "C" fn(*mut c_void) -> *const c_char =
        unsafe { std::mem::transmute(address) };
    let name = unsafe { get(client as *mut c_void) };
    if name.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(name) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Estimates `CLOCK_MONOTONIC` at the start of `client`'s current JACK cycle
/// by subtracting elapsed frames from `monotonic()`. Returns 0 if unavailable.
pub fn cycle_start(client: usize) -> u64 {
    let clock = cycle_clock();
    if clock <= 1 || client == 0 {
        return 0;
    }
    let frames_since: FramesSinceCycleStart = unsafe { std::mem::transmute(clock) };
    let frames = u64::from(unsafe { frames_since(client as *const c_void) });
    timing::monotonic().saturating_sub(frames * 1_000_000_000 / 48_000)
}
fn cycle_clock() -> usize {
    let known = CYCLE_CLOCK.load(Ordering::Acquire);
    if known != 0 {
        return known;
    }
    let map = JACK_MAP.load(Ordering::Acquire);
    let resolved = if map == 0 {
        0
    } else {
        unsafe { dynamic_symbol(map as *const LinkMap, c"jack_frames_since_cycle_start") }
    };
    let value = if resolved == 0 { 1 } else { resolved };
    match CYCLE_CLOCK.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => value,
        Err(previous) => previous,
    }
}

/// Resolves `name` from a loaded ELF's `DT_GNU_HASH` or `DT_HASH` table, returning 0 if absent.
pub(crate) unsafe fn dynamic_symbol(map: *const LinkMap, name: &CStr) -> usize {
    #[repr(C)]
    struct Dyn {
        tag: i64,
        value: u64,
    }
    const DT_NULL: i64 = 0;
    const DT_HASH: i64 = 4;
    const DT_STRTAB: i64 = 5;
    const DT_SYMTAB: i64 = 6;
    const DT_GNU_HASH: i64 = 0x6fff_fef5;
    let base = unsafe { (*map).address };
    let mut entry = unsafe { (*map).dynamic }.cast::<Dyn>();
    if entry.is_null() {
        return 0;
    }
    let (mut strtab, mut symtab, mut hash, mut gnu_hash) = (0usize, 0usize, 0usize, 0usize);
    let pointer = |value: u64| {
        let value = value as usize;
        if value < base { value + base } else { value }
    };
    loop {
        let dynamic = unsafe { &*entry };
        match dynamic.tag {
            DT_NULL => break,
            DT_STRTAB => strtab = pointer(dynamic.value),
            DT_SYMTAB => symtab = pointer(dynamic.value),
            DT_HASH => hash = pointer(dynamic.value),
            DT_GNU_HASH => gnu_hash = pointer(dynamic.value),
            _ => {}
        }
        entry = unsafe { entry.add(1) };
    }
    if strtab == 0 || symtab == 0 {
        return 0;
    }
    let symbols = symtab as *const Symbol;
    let matches = |index: usize| {
        let symbol = unsafe { &*symbols.add(index) };
        symbol.section != 0
            && symbol.value != 0
            && unsafe { CStr::from_ptr((strtab + symbol.name as usize) as *const c_char) } == name
    };
    let wanted = name.to_bytes();
    if gnu_hash != 0 {
        let table = gnu_hash as *const u32;
        let [buckets, offset, bloom_size, _] = unsafe { *table.cast::<[u32; 4]>() };
        if buckets == 0 {
            return 0;
        }
        let bucket_table =
            unsafe { table.add(4).cast::<u64>().add(bloom_size as usize) }.cast::<u32>();
        let chains = unsafe { bucket_table.add(buckets as usize) };
        let hash = wanted.iter().fold(5381u32, |h, byte| {
            h.wrapping_mul(33).wrapping_add(u32::from(*byte))
        });
        let mut index = unsafe { *bucket_table.add((hash % buckets) as usize) } as usize;
        if index < offset as usize {
            return 0;
        }
        loop {
            let chain = unsafe { *chains.add(index - offset as usize) };
            if (chain | 1) == (hash | 1) && matches(index) {
                return base + unsafe { (*symbols.add(index)).value } as usize;
            }
            if chain & 1 != 0 {
                return 0;
            }
            index += 1;
        }
    }
    if hash != 0 {
        let table = hash as *const u32;
        let [buckets, chains] = unsafe { *table.cast::<[u32; 2]>() };
        if buckets == 0 {
            return 0;
        }
        let bucket_table = unsafe { table.add(2) };
        let chain_table = unsafe { bucket_table.add(buckets as usize) };
        let hash = wanted.iter().fold(0u32, |h, byte| {
            let h = (h << 4).wrapping_add(u32::from(*byte));
            let g = h & 0xf000_0000;
            (if g != 0 { h ^ (g >> 24) } else { h }) & !g
        });
        let mut index = unsafe { *bucket_table.add((hash % buckets) as usize) } as usize;
        while index != 0 && index < chains as usize {
            if matches(index) {
                return base + unsafe { (*symbols.add(index)).value } as usize;
            }
            index = unsafe { *chain_table.add(index) } as usize;
        }
    }
    0
}

pub fn xrun_notifications() -> u64 {
    XRUNS.load(Ordering::Relaxed)
}
pub fn helper_late() -> u64 {
    super::pipeline::HELPER_LATE.load(Ordering::Relaxed)
}
pub fn helper_stalls() -> u64 {
    super::pipeline::HELPER_STALLS.load(Ordering::Relaxed)
}

/// Resolved `jack_get_xrun_delayed_usecs` pointer (populated once on registration).
static XRUN_DELAYED: AtomicU64 = AtomicU64::new(0);
static XRUN_DELAYED_ZERO: AtomicU64 = AtomicU64::new(0);
static XRUN_DELAYED_VALUE: AtomicU64 = AtomicU64::new(0);
static XRUN_DELAYED_MISSING: AtomicU64 = AtomicU64::new(0);

static XRUN_DELAYED_ROUTE: AtomicU8 = AtomicU8::new(0);
const ROUTE_MAIN: u8 = 1;
const ROUTE_DEFAULT: u8 = 2;
const ROUTE_MISSING: u8 = 3;

/// Looks up `name` in the main program's link-map namespace. Because `LD_AUDIT`
/// loads us into a separate namespace, `dlsym(RTLD_DEFAULT, ...)` can't see
/// the mixer's `libjack`; we open `LM_ID_BASE` first and fall back to
/// `RTLD_DEFAULT` for non-audit tests.
fn find_in_mixer(name: &CStr) -> Option<usize> {
    let base = unsafe {
        libc::dlmopen(
            libc::LM_ID_BASE,
            std::ptr::null(),
            libc::RTLD_LAZY | libc::RTLD_NOLOAD,
        )
    };
    if !base.is_null() {
        let address = unsafe { libc::dlsym(base, name.as_ptr()) };
        unsafe { libc::dlclose(base) };
        if !address.is_null() {
            XRUN_DELAYED_ROUTE.store(ROUTE_MAIN, Ordering::Relaxed);
            return Some(address as usize);
        }
    }
    let address = unsafe { libc::dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if !address.is_null() {
        XRUN_DELAYED_ROUTE.store(ROUTE_DEFAULT, Ordering::Relaxed);
        return Some(address as usize);
    }
    XRUN_DELAYED_ROUTE.store(ROUTE_MISSING, Ordering::Relaxed);
    None
}

pub fn resolve_xrun_delayed() {
    if XRUN_DELAYED.load(Ordering::Acquire) != 0 {
        return;
    }
    let Some(address) = find_in_mixer(c"jack_get_xrun_delayed_usecs") else {
        return;
    };
    let _ = XRUN_DELAYED.compare_exchange(0, address as u64, Ordering::AcqRel, Ordering::Acquire);
}

/// Vendor xrun callbacks chained behind our wrapper, keyed by client pointer.
struct Vendor {
    client: AtomicUsize,
    callback: AtomicUsize,
    argument: AtomicUsize,
}
static VENDOR_XRUN: [Vendor; 8] = [const {
    Vendor {
        client: AtomicUsize::new(0),
        callback: AtomicUsize::new(0),
        argument: AtomicUsize::new(0),
    }
}; 8];
pub fn register_vendor_xrun(client: usize, callback: usize, argument: usize) -> bool {
    let slot = VENDOR_XRUN
        .iter()
        .find(|slot| slot.client.load(Ordering::Acquire) == client)
        .or_else(|| {
            VENDOR_XRUN.iter().find(|slot| {
                slot.client
                    .compare_exchange(0, client, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        });
    let Some(slot) = slot else {
        return false;
    };
    slot.argument.store(argument, Ordering::Relaxed);
    slot.callback.store(callback, Ordering::Release);
    true
}
pub unsafe extern "C" fn xrun(client: *mut c_void) -> c_int {
    super::client_timing::freeze(timing::monotonic());
    XRUNS.fetch_add(1, Ordering::Relaxed);
    // JACK2 only updates `jack_get_xrun_delayed_usecs` on backend xruns, not
    // client timeouts, so 0 or a stale non-zero value is possible here.
    let delayed = XRUN_DELAYED.load(Ordering::Acquire);
    let delayed_us = if delayed == 0 {
        XRUN_DELAYED_MISSING.fetch_add(1, Ordering::Relaxed);
        u64::MAX
    } else {
        let getter: unsafe extern "C" fn(*mut c_void) -> f32 =
            unsafe { std::mem::transmute(delayed) };
        let value = f64::from(unsafe { getter(client) });
        let us = if value.is_finite() && value >= 0. {
            value.round() as u64
        } else {
            u64::MAX
        };
        if us == 0 {
            XRUN_DELAYED_ZERO.fetch_add(1, Ordering::Relaxed);
        } else {
            XRUN_DELAYED_VALUE.fetch_add(1, Ordering::Relaxed);
        }
        us
    };
    let clock = CYCLE_CLOCK.load(Ordering::Acquire);
    let position = if clock > 1 {
        let frames_since: FramesSinceCycleStart = unsafe { std::mem::transmute(clock) };
        u64::from(unsafe { frames_since(client) }) * 1_000_000_000 / 48_000
    } else {
        u64::MAX
    };
    record_extra(Kind::Xrun, position, delayed_us);
    let Some(slot) = VENDOR_XRUN
        .iter()
        .find(|slot| slot.client.load(Ordering::Acquire) == client as usize)
    else {
        return 0;
    };
    let callback = slot.callback.load(Ordering::Acquire);
    if callback == 0 {
        return 0;
    }
    let callback: Xrun = unsafe { std::mem::transmute(callback) };
    unsafe { callback(slot.argument.load(Ordering::Relaxed) as *mut c_void) }
}

/// Returns `[tid, cpu, sched_policy, sched_priority]` for the calling thread.
pub fn placement() -> [i64; 4] {
    unsafe {
        let mut policy = 0;
        let mut param: libc::sched_param = std::mem::zeroed();
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) != 0 {
            policy = -1;
            param.sched_priority = -1;
        }
        [
            libc::syscall(libc::SYS_gettid),
            i64::from(libc::sched_getcpu()),
            i64::from(policy),
            i64::from(param.sched_priority),
        ]
    }
}
pub fn placement_json(values: [i64; 4]) -> serde_json::Value {
    serde_json::json!({"tid": values[0], "cpu": values[1], "policy": values[2], "priority": values[3]})
}

/// Serializes tests that inspect or emit into the global probe event ring.
#[cfg(test)]
pub(crate) static EVENT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_ordered_and_logged_once() {
        let _events = EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        record(Kind::Gap, 1);
        record(Kind::Xrun, 2);
        let events = events_after(0);
        assert!(events.len() >= 2);
        assert!(events.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        let last = events.last().unwrap().seq;
        assert!(events_after(last).is_empty());
    }

    #[test]
    fn the_event_log_stops_at_its_cap_and_says_so() {
        let path = std::env::temp_dir().join(format!("rc2fx-probe-{}.log", std::process::id()));
        let events: Vec<_> = (0..LOG_BURST as u64)
            .map(|seq| Snapshot {
                seq,
                at: 0,
                value: 0,
                since_usb: u64::MAX,
                extra: 0,
                kind: Kind::Gap as u8,
            })
            .collect();
        std::fs::write(&path, vec![b'x'; LOG_CAP as usize - 1]).unwrap();
        append_log(&path, &events);
        let full = std::fs::read(&path).unwrap();
        assert!(full.len() as u64 > LOG_CAP, "the last flush is written");
        assert!(String::from_utf8_lossy(&full).ends_with("log full, later events not written\n"));
        append_log(&path, &events);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), full.len() as u64);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_xrun_delay_getter_is_looked_up_in_the_mixers_namespace() {
        let found = find_in_mixer(c"getpid");
        assert!(found.is_some(), "getpid not found in the main namespace");
        assert_eq!(XRUN_DELAYED_ROUTE.load(Ordering::Relaxed), ROUTE_MAIN);
        assert!(find_in_mixer(c"rc2fx_no_such_symbol_exists").is_none());
        assert_eq!(XRUN_DELAYED_ROUTE.load(Ordering::Relaxed), ROUTE_MISSING);
    }

    #[test]
    fn xrun_position_is_not_reported_as_failure_duration() {
        let mut event = Snapshot {
            seq: 1,
            at: 10,
            value: 123,
            since_usb: u64::MAX,
            extra: u64::MAX,
            kind: Kind::Xrun as u8,
        };
        let json = event.json();
        assert_eq!(json["valueNs"], 123);
        assert_eq!(json["notificationCyclePositionNs"], 123);
        assert_eq!(
            json["valueMeaning"],
            "notificationCyclePositionNotXrunDuration"
        );
        assert!(json.get("xrunDurationNs").is_none());
        event.value = u64::MAX;
        let unknown = event.json();
        assert!(unknown["valueNs"].is_null());
        assert!(unknown["notificationCyclePositionNs"].is_null());
        // jackd's own delay value rides along when it was read.
        event.extra = 320;
        let with_delay = event.json();
        assert_eq!(with_delay["xrunDelayedUs"], 320);
        assert_eq!(
            with_delay["xrunDelayedMeaning"],
            "lastBackendXrunDelayNotThisNotificationDuration"
        );
        event.kind = Kind::Overrun as u8;
        event.value = PERIOD_NS + 1;
        let duration = event.json();
        assert_eq!(duration["valueNs"], PERIOD_NS + 1);
        assert!(duration.get("notificationCyclePositionNs").is_none());
        assert!(duration.get("xrunDelayedUs").is_none());
    }

    #[test]
    fn positions_bin_against_the_period() {
        let positions = Positions::new();
        positions.record(PERIOD_NS / 10);
        positions.record(PERIOD_NS);
        positions.record(PERIOD_NS * 2);
        let value = positions.snapshot();
        assert_eq!(value["histogram"][0], 1);
        assert_eq!(value["histogram"][5], 1);
        assert_eq!(value["histogram"][7], 1);
        assert_eq!(value["maxNs"], PERIOD_NS * 2);
    }

    #[test]
    fn the_late_start_line_is_a_bin_edge() {
        let positions = Positions::new();
        positions.record(LATE_START_NS);
        positions.record(LATE_START_NS + 1);
        let value = positions.snapshot();
        assert_eq!(value["histogram"][2], 1);
        assert_eq!(value["histogram"][3], 1);
        // 112 frames at 48 kHz: just under 1,400 us.
        assert_eq!(LATE_START_NS / 1_000, 1_399);
    }

    #[test]
    fn stamps_are_civil_utc() {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_777_000_000_123);
        assert_eq!(stamp(time), "2026-04-24T03:06:40.123Z");
    }

    #[test]
    fn libc_symbols_resolve_through_the_dynamic_table() {
        // Any loaded object with a hash table will do; glibc's link map for
        // libc is reachable through dlopen on the test host.
        unsafe {
            let handle = libc::dlopen(c"libc.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
            if handle.is_null() {
                return;
            }
            let mut map: *const LinkMap = std::ptr::null();
            if libc::dlinfo(
                handle,
                libc::RTLD_DI_LINKMAP,
                (&mut map as *mut *const LinkMap).cast(),
            ) != 0
            {
                return;
            }
            let expected = libc::dlsym(handle, c"clock_gettime".as_ptr()) as usize;
            assert_eq!(dynamic_symbol(map, c"clock_gettime"), expected);
            assert_eq!(dynamic_symbol(map, c"rc2fx_no_such_symbol"), 0);
        }
    }
}
