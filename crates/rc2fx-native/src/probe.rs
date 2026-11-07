//! Where audio work lands in jackd's cycle, and a timestamped record of every
//! observed anomaly, to correlate symptoms with timing evidence. JACK's xrun
//! notifications alone identify neither the late client nor a hardware direction.
//!
//! jackd (JACK2, async) writes a client's output port buffers and reads its
//! capture buffers at the start of the next cycle, before it triggers the
//! graph. Positions are estimates in nanoseconds after the cycle start from
//! `jack_frames_since_cycle_start`; compare them with server evidence rather
//! than treating them as JACK's actual deadline checks. Recording is allocation
//! and lock free on every thread.
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

/// `jack_frames_since_cycle_start`: bound by the mixer, or resolved from
/// libjack's dynamic symbols. 1 means unavailable.
pub static CYCLE_CLOCK: AtomicUsize = AtomicUsize::new(0);
/// libjack's link map, from `la_objopen`.
pub static JACK_MAP: AtomicUsize = AtomicUsize::new(0);

/// Counts positions in the cycle, as fractions of the period:
/// [<=1/8, <=1/4, <=1/2, <=3/4, <=9/10, <=1, <=11/10, >11/10].
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
            PERIOD_NS / 2,
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
/// Callback start, callback end and helper finish, after the cycle start.
pub static START_POS: Positions = Positions::new();
pub static END_POS: Positions = Positions::new();
pub static HELPER_POS: Positions = Positions::new();
/// Helper frames finished at or past `HELPER_LATE_NS`: the next callback was
/// about to start, and may have had to pass its frame through.
pub static HELPER_LATE_FINISHES: AtomicU64 = AtomicU64::new(0);
/// Callbacks whose estimated cycle-relative end exceeded one period.
/// This is local timing evidence, not a read of JACK's actual deadline check.
pub static LATE_ENDS: AtomicU64 = AtomicU64::new(0);
/// Consecutive callbacks whose estimated cycle starts were more than 1.5
/// periods apart: evidence of a discontinuity, not proof of its cause.
pub static CYCLE_SKIPS: AtomicU64 = AtomicU64::new(0);
/// The largest disagreement between the cycle start estimated at callback
/// entry and at exit, among callbacks that stayed within their cycle. Checks
/// consistency of these projected timestamps, not equality of clock epochs.
pub static ESTIMATE_DRIFT_MAX_NS: AtomicU64 = AtomicU64::new(0);
/// Notifications delivered to instrumented clients, NOT unique failed periods
/// or proven audible dropouts. JACK can broadcast once for each late client.
pub static XRUNS: AtomicU64 = AtomicU64::new(0);
pub static USB_REPORTS: AtomicU64 = AtomicU64::new(0);
static LAST_USB_NS: AtomicU64 = AtomicU64::new(0);

/// Risk thresholds that make an event, not proof of a missed deadline. The mixer's
/// callback normally starts 300-600 us into the cycle, behind the clients
/// jackd runs before it.
pub const LATE_START_NS: u64 = PERIOD_NS / 2;
pub const LATE_END_NS: u64 = PERIOD_NS * 9 / 10;
/// The helper's results are consumed by the next callback, which starts a
/// little after the next cycle begins; this far into the frame's own cycle
/// they are at risk.
pub const HELPER_LATE_NS: u64 = PERIOD_NS * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// The callback started this late into its cycle.
    LateStart = 1,
    /// The callback returned this late into its cycle.
    LateEnd = 2,
    /// The callback took longer than a period.
    Overrun = 3,
    /// Consecutive callbacks started this far apart.
    Gap = 4,
    /// The helper finished this late into the frame's cycle.
    HelperLateFinish = 5,
    /// A callback waited for the helper this long and passed its frame
    /// through: no processed audio for that callback, nothing handed off.
    HelperStall = 6,
    /// A callback found the helper still busy, this long into its frame.
    HelperBusy = 7,
    /// A JACK xrun notification arrived. The value is the receiver's cycle
    /// position at notification time, NOT an xrun duration; MAX means unknown.
    Xrun = 8,
    /// A vendor job ended this long after the next callback had started.
    VendorJobLate = 9,
    /// The mixer rebuilt its USB gadget (a USB mode change).
    UsbGadget = 10,
    /// The estimated cycle start jumped by more than 1.5 periods.
    CycleSkip = 11,
}
impl Kind {
    /// The variant for a stored code; None for a code this build never wrote.
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
    /// Kind-specific extra in nanoseconds or microseconds; MAX means unknown.
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

/// From any thread. `value` is a duration in nanoseconds where the kind has one.
pub fn record(kind: Kind, value: u64) {
    record_extra(kind, value, u64::MAX);
}

/// `extra` is the kind's second value: for an xrun, jackd's
/// jack_get_xrun_delayed_usecs() in microseconds (MAX when unavailable).
pub fn record_extra(kind: Kind, value: u64, extra: u64) {
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
            // Keep valueNs for existing consumers, but explicitly name its
            // meaning. Neither the elapsed notification delay nor the failed
            // cycle's duration is available from frames_since_cycle_start.
            result["notificationCyclePositionNs"] = serde_json::json!(value);
            result["valueMeaning"] = "notificationCyclePositionNotXrunDuration".into();
            // jackd's own backend delay value for the last backend xrun, in
            // microseconds: a client-lateness notification does not update it,
            // so a stale value is possible.
            result["xrunDelayedUs"] =
                serde_json::json!((self.extra != u64::MAX).then_some(self.extra));
            result["xrunDelayedMeaning"] = "lastBackendXrunDelayNotThisNotificationDuration".into();
        }
        result
    }
}
/// One event as the control API carries it.
pub struct EngineEvent {
    pub seq: u64,
    pub at: u64,
    pub value: u64,
    /// jackd's xrun delay for an xrun; u64::MAX when unknown.
    pub delayed_us: u64,
    pub kind: Kind,
}

/// The newest entries of the ring (the same tail audit.json shows), oldest
/// first. `seq` is monotonic across the process, so a reader dedupes by it.
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

/// Events with a sequence number above `after`, oldest first. Slots being
/// written concurrently read inconsistently; a slot is accepted only when its
/// sequence number is unchanged across the read.
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
            "estimateDriftMaxNs": read(&ESTIMATE_DRIFT_MAX_NS),
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
/// At most this many lines per flush, so a storm cannot wear the flash.
const LOG_BURST: usize = 200;
/// The log lives on the mixer's flash and rotates only at a mixer start, so a
/// fault that lasts for weeks must not fill it. Past this size events stay in
/// the ring and in `snapshot`, but are no longer written.
const LOG_CAP: u64 = 4 << 20;
pub fn set_log(path: std::path::PathBuf) {
    let _ = LOG.set(path);
}
/// Append the events since the last flush, with wall-clock and monotonic
/// times. Called from the report thread once a second.
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
/// Append `events`, unless the file has already reached [`LOG_CAP`]. The last
/// flush to reach it says so, and a flush writes at most [`LOG_BURST`] lines.
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
        let _ = writeln!(file, "{} log full, later events not written", stamp(now_wall));
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

/// CLOCK_MONOTONIC at the start of `client`'s current jackd cycle, or 0
/// without a cycle clock. JACK supplies an elapsed frame count, which is
/// subtracted from our CLOCK_MONOTONIC time. JACK's absolute timestamps may
/// use CLOCK_MONOTONIC_RAW instead; their epochs must not be equated.
/// Read the JACK-owned name during registration, never from an audio callback.
/// Use libjack's observed map directly rather than the audit namespace.
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

/// Look `name` up in a loaded object's dynamic symbol table, through its GNU
/// or SysV hash table. 0 when absent. Pointer-valued dynamic entries are
/// used as the loader left them: relocated once the object is relocated,
/// otherwise still relative to its load address.
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

/// Reads of counters the audio path already writes: JACK xrun notifications,
/// and the helper's late finishes and stalls (pipeline's own statics).
pub fn xrun_notifications() -> u64 {
    XRUNS.load(Ordering::Relaxed)
}
pub fn helper_late() -> u64 {
    super::pipeline::HELPER_LATE.load(Ordering::Relaxed)
}
pub fn helper_stalls() -> u64 {
    super::pipeline::HELPER_STALLS.load(Ordering::Relaxed)
}

/// jackd's jack_get_xrun_delayed_usecs(), when libjack exports it: a plain
/// getter of the client's last backend xrun delay (a float, microseconds).
/// Resolved once at registration, from the control path, never the callback.
static XRUN_DELAYED: AtomicU64 = AtomicU64::new(0);
/// What the getter said at each notification: 0, non-zero, or unavailable.
static XRUN_DELAYED_ZERO: AtomicU64 = AtomicU64::new(0);
static XRUN_DELAYED_VALUE: AtomicU64 = AtomicU64::new(0);
static XRUN_DELAYED_MISSING: AtomicU64 = AtomicU64::new(0);

/// Called once, when the mixer registers its xrun callback: `libjack.so` is
/// loaded by then. Returns whether the getter was found.
/// Where the getter was found; in audit.json so a device run says which way
/// the lookup had to go. Empty until a lookup happens.
static XRUN_DELAYED_ROUTE: AtomicU8 = AtomicU8::new(0);
const ROUTE_MAIN: u8 = 1;
const ROUTE_DEFAULT: u8 = 2;
const ROUTE_MISSING: u8 = 3;

/// Finds a symbol in the MIXER's namespace. This library is injected with
/// LD_AUDIT, which glibc loads into its own link map namespace: `dlsym` with
/// RTLD_DEFAULT then searches only that namespace's global scope, where the
/// mixer's libjack is not, so `jack_get_xrun_delayed_usecs` was never found on
/// the device (`xrunDelayed.getter:false`). The main program's namespace is
/// reached with dlmopen(LM_ID_BASE, NULL, RTLD_NOLOAD); RTLD_DEFAULT stays as
/// a fallback for an injection that does not use LD_AUDIT.
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

pub fn resolve_xrun_delayed() -> bool {
    if XRUN_DELAYED.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some(address) = find_in_mixer(c"jack_get_xrun_delayed_usecs") else {
        return false;
    };
    XRUN_DELAYED
        .compare_exchange(0, address as u64, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        || XRUN_DELAYED.load(Ordering::Acquire) != 0
}

/// The mixer's own xrun callbacks, by client, kept behind this library's.
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
/// Record the mixer's registration; the library registers `xrun` instead,
/// with the client as its argument. Returns false when no slot is free.
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
/// jackd's xrun notification, on the client's notification thread.
pub unsafe extern "C" fn xrun(client: *mut c_void) -> c_int {
    super::client_timing::freeze(timing::monotonic());
    XRUNS.fetch_add(1, Ordering::Relaxed);
    // jackd's own view, when it exports one: the delay of its last backend
    // xrun in microseconds. A client-lateness notification does not update
    // it, so a non-zero value proves only that a backend xrun happened at
    // some point; zero does not prove that this notification is a client
    // timeout (JACK2 1.9.20's JackEngine sets it only for backend xruns).
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

/// Thread placement, for the report: this thread's id, CPU, policy and
/// priority packed into one word each.
pub fn placement() -> [i64; 4] {
    unsafe {
        let mut policy = 0;
        let mut param: libc::sched_param = std::mem::zeroed();
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) != 0 {
            policy = -1;
            param.sched_priority = -1;
        }
        [
            i64::from(libc::gettid()),
            i64::from(libc::sched_getcpu()),
            i64::from(policy),
            i64::from(param.sched_priority),
        ]
    }
}
pub fn placement_json(values: [i64; 4]) -> serde_json::Value {
    serde_json::json!({"tid": values[0], "cpu": values[1], "policy": values[2], "priority": values[3]})
}

/// Held by tests that hold a pipeline's helper busy (each such callback
/// records probe events) and by the one that reads the event ring expecting
/// only its own: they would otherwise interleave across test threads.
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

    /// The lookup goes to the main program's namespace, not the audit one:
    /// an LD_AUDIT module cannot see the mixer's libjack through RTLD_DEFAULT,
    /// which is what left jack_get_xrun_delayed_usecs unresolved on the device.
    #[test]
    fn the_xrun_delay_getter_is_looked_up_in_the_mixers_namespace() {
        // libc is in both namespaces; the route must be the main one.
        let found = find_in_mixer(c"getpid");
        assert!(found.is_some(), "getpid not found in the main namespace");
        assert_eq!(XRUN_DELAYED_ROUTE.load(Ordering::Relaxed), ROUTE_MAIN);
        // And a name that exists nowhere is reported as missing, not guessed.
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
