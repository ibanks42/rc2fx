//! Mixer-side audio library injected into `rc_audio_mixer` via `LD_AUDIT`.
#![cfg(target_pointer_width = "64")]

#[cfg(not(target_os = "linux"))]
compile_error!("rc2fx's mixer crates build for Linux only");

mod audit_report;
mod backend;
mod chat_mix;
mod client_timing;
mod client_timing_events;
mod denoiser_hold;
mod gauges;
mod ipc;
mod jack_cache;
mod parameters;
mod pipeline;
mod prefetch;
mod probe;
mod src_fast;
mod suppression;
mod telemetry;
mod timing;
mod transactions;
mod usb_control;
mod vendor_job;
mod vendor_wake;

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    ffi::{CStr, OsStr, c_char, c_int, c_uint, c_void},
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
};

const MAIN: usize = 1;
const JACK: usize = 3;
const MAX_CONTEXTS: usize = 32;
type Process = unsafe extern "C" fn(u32, *mut c_void) -> c_int;
type SetProcess = unsafe extern "C" fn(*mut c_void, Option<Process>, *mut c_void) -> c_int;
type SetXrun = unsafe extern "C" fn(*mut c_void, Option<probe::Xrun>, *mut c_void) -> c_int;
type GetBuffer = unsafe extern "C" fn(*mut c_void, u32) -> *mut c_void;
type Close = unsafe extern "C" fn(*mut c_void) -> c_int;
type Register =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, usize, usize) -> *mut c_void;
type Unregister = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;

#[repr(C)]
pub struct LinkMap {
    address: usize,
    name: *const c_char,
    dynamic: *mut c_void,
    next: *mut LinkMap,
    previous: *mut LinkMap,
}
#[repr(C)]
pub struct Symbol {
    name: u32,
    info: u8,
    other: u8,
    section: u16,
    value: u64,
    size: u64,
}

/// A value on cache lines of its own (64 bytes on the Cortex-A53). Used for
/// what one thread writes every cycle and for what several threads only read,
/// so that one thread's writes never invalidate a line another thread uses.
#[repr(align(64))]
pub(crate) struct Line<T>(pub(crate) T);
impl<T> std::ops::Deref for Line<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
const _: () = assert!(std::mem::align_of::<Line<AtomicU64>>() == 64);
const _: () = assert!(std::mem::size_of::<Line<CycleCounters>>() == 128);
const _: () = assert!(std::mem::size_of::<Line<ReadMostly>>() == 64);

/// What every hooked `jack_port_get_buffer` call and every callback reads.
/// Written only at load and while symbols bind, and kept apart from every
/// counter, so that reading it never waits for another core's write.
struct ReadMostly {
    /// The real `jack_port_get_buffer`.
    get_buffer: AtomicUsize,
    /// The bench gauges are on (see gauges.rs).
    gauges: AtomicBool,
}
static READ_MOSTLY: Line<ReadMostly> = Line(ReadMostly {
    get_buffer: AtomicUsize::new(0),
    gauges: AtomicBool::new(false),
});

/// What the wrapped callback counts every cycle. Only the callback thread
/// writes these, so they sit together and away from other threads' counters.
struct CycleCounters {
    callbacks: AtomicU64,
    /// `jack_port_get_buffer` calls made inside the wrapped callback, added
    /// once per callback from the thread's own count.
    buffer_calls_inside: AtomicU64,
    max_gap: AtomicU64,
    /// Gaps outside 0.9 to 1.1 periods, counted separately: a callback that
    /// woke early or late. All are counted here; the event ring keeps only
    /// those of 1.5 periods and more, so a persistent offset cannot flood it.
    gaps_short: AtomicU64,
    gaps_long: AtomicU64,
    /// Time between consecutive callback starts, against the 112-frame
    /// period: [<0.5, <0.9, <1.1, <1.5, <2, <3, >=3 periods].
    gaps: [AtomicU64; 7],
}
static CYCLE: Line<CycleCounters> = Line(CycleCounters {
    callbacks: AtomicU64::new(0),
    buffer_calls_inside: AtomicU64::new(0),
    max_gap: AtomicU64::new(0),
    gaps_short: AtomicU64::new(0),
    gaps_long: AtomicU64::new(0),
    gaps: [const { AtomicU64::new(0) }; 7],
});

/// Whether a context's pipeline still waits for its client's ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attach {
    /// Built once the client's planned ports are registered.
    Pending,
    /// Not waiting: built at registration or after the ports, refused, or
    /// never planned.
    Settled,
}

/// Where a context slot is in its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// Unclaimed; a new registration may take it.
    Free,
    /// Claimed and the client's installed callback: control requests and
    /// reports reach it.
    Live(Attach),
    /// Claimed but not the installed callback: native registration has not
    /// succeeded yet, failed, been replaced by a newer one, or the client is
    /// being closed. Kept until native close quiesces its callbacks.
    Retained(Attach),
}
impl Slot {
    const FREE: u8 = 0;
    const RETAINED_PENDING: u8 = 1;
    const RETAINED_SETTLED: u8 = 2;
    const LIVE_PENDING: u8 = 3;
    const LIVE_SETTLED: u8 = 4;
    const fn to_u8(self) -> u8 {
        match self {
            Self::Free => Self::FREE,
            Self::Retained(Attach::Pending) => Self::RETAINED_PENDING,
            Self::Retained(Attach::Settled) => Self::RETAINED_SETTLED,
            Self::Live(Attach::Pending) => Self::LIVE_PENDING,
            Self::Live(Attach::Settled) => Self::LIVE_SETTLED,
        }
    }
    /// Total: only `to_u8` writes the atomic, so other values never occur.
    const fn from_u8(value: u8) -> Self {
        match value {
            Self::RETAINED_PENDING => Self::Retained(Attach::Pending),
            Self::RETAINED_SETTLED => Self::Retained(Attach::Settled),
            Self::LIVE_PENDING => Self::Live(Attach::Pending),
            Self::LIVE_SETTLED => Self::Live(Attach::Settled),
            _ => Self::Free,
        }
    }
    const fn is_claimed(self) -> bool {
        !matches!(self, Self::Free)
    }
    const fn is_live(self) -> bool {
        matches!(self, Self::Live(_))
    }
    /// The same slot, installed or not, keeping its attach state.
    const fn installed(self, installed: bool) -> Self {
        match self {
            Self::Free => Self::Free,
            Self::Live(attach) | Self::Retained(attach) if installed => Self::Live(attach),
            Self::Live(attach) | Self::Retained(attach) => Self::Retained(attach),
        }
    }
}

/// A context's `Slot`, in one atomic so that a reader without CONTROL sees a
/// consistent lifetime. Only CONTROL holders change it; the audio callback
/// never reads it.
struct Lifecycle(AtomicU8);
impl Lifecycle {
    const fn new() -> Self {
        Self(AtomicU8::new(Slot::FREE))
    }
    fn load(&self, order: Ordering) -> Slot {
        Slot::from_u8(self.0.load(order))
    }
    fn store(&self, slot: Slot, order: Ordering) {
        self.0.store(slot.to_u8(), order);
    }
}

/// One wrapped callback registration. Its callback thread reads the head and
/// writes the timing and the stamps every cycle, so each context starts on a
/// cache line and shares none with anything else.
#[repr(align(64))]
struct Context {
    /// Published last on claim and cleared last on close, with Release, so a
    /// reader that loads it with Acquire sees the fields it guards.
    slot: Lifecycle,
    token: AtomicU64,
    client: AtomicUsize,
    callback: AtomicUsize,
    argument: AtomicUsize,
    pipeline: AtomicPtr<pipeline::Pipeline>,
    parameters: Mutex<Vec<parameters::Control>>,
    timing: timing::Shared,
    /// CLOCK_MONOTONIC when this context's latest callback started.
    started: AtomicU64,
    /// CLOCK_MONOTONIC when this context's latest callback returned.
    ended: AtomicU64,
    /// CLOCK_MONOTONIC when the jackd cycle of the latest callback started.
    cycle_start: AtomicU64,
}
impl Context {
    const fn new() -> Self {
        Self {
            slot: Lifecycle::new(),
            token: AtomicU64::new(0),
            client: AtomicUsize::new(0),
            callback: AtomicUsize::new(0),
            argument: AtomicUsize::new(0),
            pipeline: AtomicPtr::new(std::ptr::null_mut()),
            parameters: Mutex::new(Vec::new()),
            timing: timing::Shared::new(),
            started: AtomicU64::new(0),
            ended: AtomicU64::new(0),
            cycle_start: AtomicU64::new(0),
        }
    }
}
static CONTEXTS: [Context; MAX_CONTEXTS] = [const { Context::new() }; MAX_CONTEXTS];
const _: () = assert!(std::mem::align_of::<Context>() == 64);
// Only callback/port registration, retirement and close acquire this mutex.
// The audio callback never does.
static CONTROL: Mutex<pipeline::Registry> = Mutex::new(pipeline::Registry { ports: Vec::new() });
static REPORT: OnceLock<PathBuf> = OnceLock::new();
static REPORT_SELECTED_ONLY: AtomicBool = AtomicBool::new(false);
static SELECTOR_SEEN: AtomicBool = AtomicBool::new(false);
// Callback wall timing is always collected: the app's telemetry and the bench
// read it. It costs four CLOCK_MONOTONIC reads per callback (entry, around the
// original callback, exit). Thread CPU clocks are system calls on the mixer,
// so they are read only while the bench gauges are on (see timing.rs).
static RETIRED_TIMING: timing::Shared = timing::Shared::new();
static MAIN_TAIL: client_timing_events::MainEvents = client_timing_events::MainEvents::new();
static PLAN: OnceLock<pipeline::Plan> = OnceLock::new();
static CONTROL_SOCKET: OnceLock<PathBuf> = OnceLock::new();
// The callback reads this every cycle; no counter shares its line.
static CONTROL_ADMISSION: Line<ipc::Admission> = Line(ipc::Admission::new());
static SERVER: OnceLock<Mutex<Option<ipc::Server>>> = OnceLock::new();
static SERVER_ERRORS: AtomicU64 = AtomicU64::new(0);
static MAIN_BASE: AtomicUsize = AtomicUsize::new(0);
static CALLBACK_OFFSET: AtomicUsize = AtomicUsize::new(0);
static FIRMWARE_PROFILE: OnceLock<String> = OnceLock::new();
static SET_PROCESS: AtomicUsize = AtomicUsize::new(0);
static SET_XRUN: AtomicUsize = AtomicUsize::new(0);
static CLOSE: AtomicUsize = AtomicUsize::new(0);
static REGISTER: AtomicUsize = AtomicUsize::new(0);
static UNREGISTER: AtomicUsize = AtomicUsize::new(0);
static PLAN_ERRORS: AtomicU64 = AtomicU64::new(0);
static PIPELINES_CREATED: AtomicU64 = AtomicU64::new(0);
static PIPELINES_RETIRED: AtomicU64 = AtomicU64::new(0);
static REGISTRATIONS: AtomicU64 = AtomicU64::new(0);
/// `jack_port_get_buffer` calls from outside the wrapped callback (the USB
/// clients, on their own threads). Counted only while the bench gauges are on.
static BUFFER_CALLS_OUTSIDE: Line<AtomicU64> = Line(AtomicU64::new(0));
static DIRECT_BINDINGS: AtomicU64 = AtomicU64::new(0);
static DLSYM_BINDINGS: AtomicU64 = AtomicU64::new(0);
static CALLBACK_BINDINGS: AtomicU64 = AtomicU64::new(0);
static CONFLICTS: AtomicU64 = AtomicU64::new(0);
static CAPACITY_FAILURES: AtomicU64 = AtomicU64::new(0);
static TEARDOWN_FORWARDS: AtomicU64 = AtomicU64::new(0);
static NEXT_CONTEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
static RECLAIMED: AtomicU64 = AtomicU64::new(0);
static CALLBACK_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static CALLBACK_FREES: AtomicU64 = AtomicU64::new(0);
static CALLBACK_REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy)]
struct CallbackFrame {
    owner: usize,
    processing: bool,
    pipeline: usize,
    started: u64,
    /// The graph generation this callback's buffer lookups run under, taken
    /// once at frame start (see jack_cache::begin).
    jack: jack_cache::Snapshot,
}
/// This thread's callback frame, and the `jack_port_get_buffer` calls made
/// inside it. The count is a plain thread-owned number, published once when
/// the frame ends, so the buffer hook writes no shared counter.
struct Inside {
    frame: Cell<CallbackFrame>,
    buffer_calls: Cell<u64>,
}
impl std::ops::Deref for Inside {
    type Target = Cell<CallbackFrame>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
// No destructor, so the first use on a thread registers nothing. The library's
// whole thread-local block (about 200 bytes) must stay within glibc's optional
// static TLS (512 bytes by default): beyond that the loader gives an audit
// library dynamic TLS, which allocates on a thread's first access.
thread_local! {
    static INSIDE: Inside = const {
        Inside {
            frame: Cell::new(CallbackFrame {
                owner: 0,
                processing: false,
                pipeline: 0,
                started: 0,
                jack: jack_cache::Snapshot::INACTIVE,
            }),
            buffer_calls: Cell::new(0),
        }
    };
}
/// Enters a callback frame on this thread. Returns what `leave_frame` restores:
/// the frame that was current, and its buffer-call count so far.
fn enter_frame(frame: CallbackFrame) -> (CallbackFrame, u64) {
    INSIDE.with(|inside| (inside.replace(frame), inside.buffer_calls.replace(0)))
}
/// Leaves the frame entered with `enter_frame` and publishes its buffer calls.
/// An enclosing frame keeps its own count.
fn leave_frame(outer: (CallbackFrame, u64)) {
    let calls = INSIDE.with(|inside| {
        inside.set(outer.0);
        inside.buffer_calls.replace(outer.1)
    });
    if calls != 0 {
        CYCLE
            .buffer_calls_inside
            .fetch_add(calls, Ordering::Relaxed);
    }
    // Counters kept in plain thread-owned numbers are published once per
    // callback.
    jack_cache::publish();
    vendor_wake::publish();
}

/// Inside a JACK callback: its context and CLOCK_MONOTONIC start time.
pub(crate) fn callback_started() -> Option<(usize, u64)> {
    INSIDE
        .try_with(|inside| inside.get())
        .ok()
        .filter(|frame| frame.owner != 0)
        .map(|frame| (frame.owner, frame.started))
}
/// When the latest callback of a context from `callback_started` started.
pub(crate) fn context_started(owner: usize) -> u64 {
    // SAFETY: owners are addresses of entries in the static CONTEXTS array.
    unsafe { &*(owner as *const Context) }
        .started
        .load(Ordering::Relaxed)
}

/// The estimated cycle start of a context's latest callback.
pub(crate) fn context_cycle_start(owner: usize) -> u64 {
    // SAFETY: owners are addresses of entries in the static CONTEXTS array.
    unsafe { &*(owner as *const Context) }
        .cycle_start
        .load(Ordering::Relaxed)
}

/// The engine-wide numbers every control reply carries: JACK xrun
/// notifications and the helper's late finishes and busy callbacks. All are
/// atomics the audio path already writes; none is sampled here.
/// The engine event ring's newest entries, mapped to the control types.
fn engine_events() -> Vec<rc2fx_native_control::NativeEvent> {
    probe::recent_events()
        .into_iter()
        .map(|event| rc2fx_native_control::NativeEvent {
            seq: event.seq,
            at_ns: event.at,
            kind: match event.kind {
                probe::Kind::LateStart => rc2fx_native_control::NativeEventKind::LateStart,
                probe::Kind::LateEnd => rc2fx_native_control::NativeEventKind::LateEnd,
                probe::Kind::Overrun => rc2fx_native_control::NativeEventKind::Overrun,
                probe::Kind::Gap => rc2fx_native_control::NativeEventKind::Gap,
                probe::Kind::HelperLateFinish => {
                    rc2fx_native_control::NativeEventKind::HelperLateFinish
                }
                probe::Kind::HelperStall => rc2fx_native_control::NativeEventKind::HelperStall,
                probe::Kind::HelperBusy => rc2fx_native_control::NativeEventKind::HelperBusy,
                probe::Kind::Xrun => rc2fx_native_control::NativeEventKind::Xrun,
                probe::Kind::VendorJobLate => rc2fx_native_control::NativeEventKind::VendorJobLate,
                probe::Kind::UsbGadget => rc2fx_native_control::NativeEventKind::UsbGadget,
                probe::Kind::CycleSkip => rc2fx_native_control::NativeEventKind::CycleSkip,
            },
            value_ns: (event.value != u64::MAX).then_some(event.value),
            delayed_us: (event.delayed_us != u64::MAX).then_some(event.delayed_us),
        })
        .collect()
}

fn engine_counters() -> rc2fx_native_control::EngineCounters {
    rc2fx_native_control::EngineCounters {
        xruns: probe::xrun_notifications(),
        helper_late: probe::helper_late(),
        helper_stalls: probe::helper_stalls(),
    }
}

pub(crate) fn inside() -> bool {
    INSIDE
        .try_with(|value| value.get().owner != 0)
        .unwrap_or(false)
}
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if inside() {
            CALLBACK_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, value: *mut u8, layout: Layout) {
        if inside() {
            CALLBACK_FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(value, layout) }
    }
    unsafe fn realloc(&self, value: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if inside() {
            CALLBACK_REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(value, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// # Safety
/// Called only by the GNU ELF64 loader with its documented audit ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_version(version: c_uint) -> c_uint {
    gauges::initialize();
    src_fast::initialize();
    if version >= 2 { 2 } else { 0 }
}

/// # Safety
/// `map` and `cookie` must be the loader-owned pointers supplied to la_objopen.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_objopen(
    map: *mut LinkMap,
    namespace: isize,
    cookie: *mut usize,
) -> c_uint {
    if namespace != 0 {
        return 0;
    }
    let name = unsafe { CStr::from_ptr((*map).name) }.to_bytes();
    if name.is_empty() {
        MAIN_BASE.store(unsafe { (*map).address }, Ordering::Release);
    }
    let basename = name.rsplit(|byte| *byte == b'/').next().unwrap_or(name);
    let jack = matches!(
        basename,
        b"libjack.so" | b"libjack.so.0" | b"libjack.so.0.1.0"
    );
    if jack {
        probe::JACK_MAP.store(map as usize, Ordering::Release);
    }
    let gauged = gauges::object(gauges::enabled(), basename);
    if gauged == Some(gauges::ASOUND) {
        gauges::remember_asound(map as usize);
    }
    let resampled = src_fast::object(basename);
    if resampled {
        src_fast::opened(unsafe { (*map).address }, unsafe {
            CStr::from_ptr((*map).name)
        });
    }
    let gauged = gauged.or(resampled.then_some(gauges::SAMPLERATE));
    unsafe {
        *cookie = if jack {
            JACK
        } else if name.is_empty() {
            MAIN
        } else {
            gauged.unwrap_or(2)
        };
    }
    if jack {
        1
    } else if basename == b"libc.so.6" || gauged.is_some() {
        3
    } else {
        2
    }
}

/// # Safety
/// The loader owns the cookie and the process environment during this callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_preinit(_: *mut usize) {
    let enabled = |name: &std::ffi::CStr| {
        let value = unsafe { libc::getenv(name.as_ptr()) };
        !value.is_null() && unsafe { CStr::from_ptr(value) }.to_bytes() == b"1"
    };
    REPORT_SELECTED_ONLY.store(
        enabled(c"RC2FX_NATIVE_PROBE_REPORT_SELECTED_ONLY"),
        Ordering::Relaxed,
    );
    let socket = unsafe { libc::getenv(c"RC2FX_NATIVE_PROBE_CONTROL_SOCKET".as_ptr()) };
    if !socket.is_null() {
        // An invalid/failed requested receiver must leave native audio intact.
        CONTROL_ADMISSION.require();
        let bytes = unsafe { CStr::from_ptr(socket) }.to_bytes();
        if bytes.len() <= 103 && bytes.starts_with(b"/") {
            let _ = CONTROL_SOCKET.set(PathBuf::from(OsStr::from_bytes(bytes)));
        } else {
            SERVER_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
    }
    let plan = unsafe { libc::getenv(c"RC2FX_NATIVE_PROBE_PLAN".as_ptr()) };
    if !plan.is_null() {
        let path = unsafe { CStr::from_ptr(plan) }.to_bytes();
        let parsed = (|| {
            use std::io::Read;
            if path.len() > 4096 || !path.starts_with(b"/") {
                return Err("invalid plan path".to_string());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(OsStr::from_bytes(path))
                .map_err(|error| error.to_string())?
                .take(65_537)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > 65_536 {
                return Err("oversized plan".to_string());
            }
            pipeline::Plan::parse(&bytes)
        })();
        match parsed {
            Ok(plan) => {
                let _ = PLAN.set(plan);
            }
            Err(error) => {
                PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
                eprintln!("RC2FX native plan refused: {error}");
            }
        }
    }
    if std::env::var_os("RC2FX_FIRMWARE_PROFILE").is_some() {
        // The mixer passes its environment to ordinary vendor exec helpers.
        // Those helpers must not receive a repair or new stderr diagnostics.
        let vendor_process = std::fs::read_link("/proc/self/exe")
            .is_ok_and(|path| path == std::path::Path::new("/usr/bin/rc_audio_mixer"));
        let repair = if vendor_process {
            select_firmware()
        } else {
            Ok(0)
        };
        match repair {
            Ok(offset) => CALLBACK_OFFSET.store(offset, Ordering::Release),
            Err(error) => {
                // A refused repair must leave the stock callback intact. Never
                // attach custom DSP to the known dangling-capture integration.
                CALLBACK_OFFSET.store(0, Ordering::Release);
                PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
                eprintln!("RC2FX native firmware profile refused: {error}");
            }
        }
    }
    usb_control::initialize();
    let path = unsafe { libc::getenv(c"RC2FX_NATIVE_PROBE_REPORT".as_ptr()) };
    if !path.is_null() {
        let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
        if bytes.len() <= 4096 && bytes.starts_with(b"/") {
            let _ = REPORT.set(PathBuf::from(OsStr::from_bytes(bytes)));
        }
    }
    let path = unsafe { libc::getenv(c"RC2FX_NATIVE_PROBE_EVENTS".as_ptr()) };
    if !path.is_null() {
        let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
        if bytes.len() <= 4096 && bytes.starts_with(b"/") {
            probe::set_log(PathBuf::from(OsStr::from_bytes(bytes)));
        }
    }
}

/// Resolve all addresses from one hash-selected, ABI-validated profile.
fn select_firmware() -> Result<usize, String> {
    use rc2fx_firmware::{Catalog, DEFAULT_CATALOG};
    let catalog = match std::env::var_os("RC2FX_FIRMWARE_PROFILES") {
        Some(path) => Catalog::load(std::path::Path::new(&path))?,
        None => Catalog::parse(DEFAULT_CATALOG)?,
    };
    let image = std::fs::read("/proc/self/exe").map_err(|e| e.to_string())?;
    let model = rc2fx_firmware::Model::read_device()?;
    let profile = catalog.select_for_model_with(
        model,
        &image,
        rc2fx_firmware::experimental_duo_from_env(),
    )?;
    if let Ok(requested) = std::env::var("RC2FX_FIRMWARE_PROFILE")
        && requested != profile.id
    {
        return Err("requested firmware profile does not match this executable".into());
    }
    let offset = profile
        .runtime
        .as_ref()
        .ok_or("profile has no qualified runtime")?
        .callback
        .offset()?;
    vendor_job::install(MAIN_BASE.load(Ordering::Acquire), profile)?;
    chat_mix::install(MAIN_BASE.load(Ordering::Acquire), profile)?;
    vendor_wake::install(MAIN_BASE.load(Ordering::Acquire), profile)?;
    jack_cache::install();
    let _ = FIRMWARE_PROFILE.set(profile.id.clone());
    Ok(offset)
}

fn remember(original: &AtomicUsize, address: usize) -> bool {
    match original.compare_exchange(0, address, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => true,
        Err(previous) if previous == address => true,
        Err(_) => {
            CONFLICTS.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

/// # Safety
/// Every pointer is supplied by the ELF64 loader and follows the audit ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_symbind64(
    symbol: *mut Symbol,
    _: c_uint,
    reference: *mut usize,
    definition: *mut usize,
    flags: *mut c_uint,
    name: *const c_char,
) -> usize {
    let address = unsafe { (*symbol).value as usize };
    let symbol_name = unsafe { CStr::from_ptr(name) }.to_bytes();
    if unsafe { *reference } == MAIN
        && symbol_name == b"system"
        && remember(&usb_control::SYSTEM, address)
    {
        unsafe {
            *flags |= 1 | 2;
        }
        return usb_control::system as *const () as usize;
    }
    if unsafe { *reference } == MAIN
        && symbol_name == b"read"
        && remember(&usb_control::READ, address)
    {
        unsafe {
            *flags |= 1 | 2;
        }
        return usb_control::read as *const () as usize;
    }
    if unsafe { *reference } == MAIN
        && symbol_name == b"open"
        && remember(&usb_control::OPEN, address)
    {
        unsafe {
            *flags |= 1 | 2;
        }
        return usb_control::open as *const () as usize;
    }
    // The gauges, when on, time whatever the mixer's call reaches.
    let resampler = (unsafe { *reference } == MAIN
        && unsafe { *definition } == gauges::SAMPLERATE
        && symbol_name == b"src_process")
        .then(|| src_fast::replacement(address))
        .flatten();
    let address = resampler.unwrap_or(address);
    if let Some(wrapper) = gauges::replacement(
        unsafe { *reference },
        unsafe { *definition },
        symbol_name,
        address,
    ) {
        if symbol_name == b"jack_frames_since_cycle_start" {
            // RC2FX's own cycle clock stays the real function.
            let _ = probe::CYCLE_CLOCK.compare_exchange(
                0,
                address,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        unsafe {
            *flags |= 1 | 2;
        }
        return wrapper;
    }
    if resampler.is_some() {
        unsafe {
            *flags |= 1 | 2;
        }
        return address;
    }
    if unsafe { *definition } != JACK || unsafe { *reference } == JACK {
        return address;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    if name == b"jack_frames_since_cycle_start" {
        // Only observed: the cycle clock for probe positions.
        let _ =
            probe::CYCLE_CLOCK.compare_exchange(0, address, Ordering::AcqRel, Ordering::Acquire);
        return address;
    }
    let (storage, replacement) = match name {
        b"jack_set_process_callback" => (&SET_PROCESS, set_process as *const () as usize),
        b"jack_set_xrun_callback" => (&SET_XRUN, set_xrun as *const () as usize),
        b"jack_port_get_buffer" => (&READ_MOSTLY.get_buffer, get_buffer as *const () as usize),
        b"jack_client_close" => (&CLOSE, close as *const () as usize),
        b"jack_port_register" => (&REGISTER, register as *const () as usize),
        b"jack_port_unregister" => (&UNREGISTER, unregister as *const () as usize),
        _ => return address,
    };
    if !remember(storage, address) {
        return address;
    }
    let observed = unsafe { *flags };
    if observed & 8 != 0 {
        DLSYM_BINDINGS.fetch_add(1, Ordering::Relaxed);
    } else {
        DIRECT_BINDINGS.fetch_add(1, Ordering::Relaxed);
    }
    if inside() {
        CALLBACK_BINDINGS.fetch_add(1, Ordering::Relaxed);
    }
    unsafe {
        *flags |= 1 | 2;
    } // no per-PLT-entry enter/exit instrumentation
    replacement
}

// Caller holds CONTROL. Failed native registration leaves the previous active
// context installed; successful native forwarding retires it for control writes.
unsafe fn native_registration(
    original: SetProcess,
    client: *mut c_void,
    callback: Option<Process>,
    argument: *mut c_void,
    installed: Option<&Context>,
) -> c_int {
    let result = unsafe { original(client, callback, argument) };
    if result == 0 {
        for context in &CONTEXTS {
            let slot = context.slot.load(Ordering::Acquire);
            if slot.is_claimed() && context.client.load(Ordering::Relaxed) == client as usize {
                context.slot.store(
                    slot.installed(
                        installed.is_some_and(|selected| std::ptr::eq(context, selected)),
                    ),
                    Ordering::Release,
                );
            }
        }
    }
    result
}

/// JACK reports graph deadline failures and backend xruns through this same
/// notification. It does not identify the culprit or prove an audible dropout.
/// Preserve the vendor callback, recording the notification before forwarding.
unsafe extern "C" fn set_xrun(
    client: *mut c_void,
    callback: Option<probe::Xrun>,
    argument: *mut c_void,
) -> c_int {
    let pointer = SET_XRUN.load(Ordering::Acquire);
    if pointer == 0 || inside() {
        return -1;
    }
    let original: SetXrun = unsafe { std::mem::transmute(pointer) };
    // libjack is loaded by now, so its xrun delay getter can be found here
    // once, on the control path.
    probe::resolve_xrun_delayed();
    let Some(callback) = callback else {
        probe::register_vendor_xrun(client as usize, 0, 0);
        return unsafe { original(client, None, argument) };
    };
    if !probe::register_vendor_xrun(client as usize, callback as usize, argument as usize) {
        CAPACITY_FAILURES.fetch_add(1, Ordering::Relaxed);
        return unsafe { original(client, Some(callback), argument) };
    }
    unsafe { original(client, Some(probe::xrun), client) }
}

unsafe extern "C" fn set_process(
    client: *mut c_void,
    callback: Option<Process>,
    argument: *mut c_void,
) -> c_int {
    let pointer = SET_PROCESS.load(Ordering::Acquire);
    if pointer == 0 || inside() {
        return -1;
    }
    let original: SetProcess = unsafe { std::mem::transmute(pointer) };
    let Ok(registry) = CONTROL.lock() else {
        PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
        return unsafe { original(client, callback, argument) };
    };
    let Some(callback) = callback else {
        return unsafe { native_registration(original, client, None, argument, None) };
    };
    let offset = CALLBACK_OFFSET.load(Ordering::Acquire);
    if offset == 0
        || MAIN_BASE.load(Ordering::Acquire).checked_add(offset) != Some(callback as usize)
    {
        if gauges::enabled() {
            gauges::register_client(client as usize);
        }
        let (callback, argument) = if client_timing::enabled() {
            client_timing::wrap(client as usize, callback, argument)
        } else {
            (callback, argument)
        };
        return unsafe { native_registration(original, client, Some(callback), argument, None) };
    }
    // The retained stock JUCE teardown registers the same callback with a null
    // argument. It must remain the native no-op, without allocating a pipeline
    // or consuming a callback context during shutdown/reconfiguration.
    if argument.is_null() {
        TEARDOWN_FORWARDS.fetch_add(1, Ordering::Relaxed);
        return unsafe { native_registration(original, client, Some(callback), argument, None) };
    }
    SELECTOR_SEEN.store(true, Ordering::Relaxed);
    // Registration and reclamation both hold CONTROL, so a free slot remains
    // free while the optional pipeline is constructed. Check the bound first
    // instead of allocating and immediately destroying a DSP on exhaustion.
    let Some(context) = CONTEXTS
        .iter()
        .find(|context| !context.slot.load(Ordering::Acquire).is_claimed())
    else {
        CAPACITY_FAILURES.fetch_add(1, Ordering::Relaxed);
        // Retained contexts cannot be reclaimed until native close quiesces
        // callbacks. Exhausting this optional extension must not refuse the
        // native client's new callback or leave an older callback installed.
        return unsafe { native_registration(original, client, Some(callback), argument, None) };
    };
    let Ok(mut controls) = context.parameters.lock() else {
        PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
        return unsafe { native_registration(original, client, Some(callback), argument, None) };
    };
    let token = NEXT_CONTEXT_TOKEN.load(Ordering::Relaxed);
    if token == u64::MAX {
        CAPACITY_FAILURES.fetch_add(1, Ordering::Relaxed);
        return unsafe { native_registration(original, client, Some(callback), argument, None) };
    }
    let pending = PLAN
        .get()
        .is_some_and(|plan| !pipeline::Pipeline::ports_ready(plan, &registry, client as usize));
    let engine = if let Some(plan) = PLAN.get().filter(|_| !pending) {
        match pipeline::Pipeline::new(plan, &registry, client as usize) {
            Ok((engine, parameters)) => {
                *controls = parameters;
                Some(Box::new(engine))
            }
            Err(error) => {
                PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
                eprintln!("RC2FX native pipeline refused: {error}");
                return unsafe {
                    native_registration(original, client, Some(callback), argument, None)
                };
            }
        }
    } else {
        None
    };
    context.client.store(client as usize, Ordering::Relaxed);
    // CONTROL serializes token assignment. A reused client pointer or static
    // context slot cannot accept a request for its previous installed lifetime.
    NEXT_CONTEXT_TOKEN.store(token + 1, Ordering::Relaxed);
    context.token.store(token, Ordering::Relaxed);
    context.argument.store(argument as usize, Ordering::Relaxed);
    context.callback.store(callback as usize, Ordering::Release);
    if let Some(engine) = engine {
        context
            .pipeline
            .store(Box::into_raw(engine), Ordering::Release);
        PIPELINES_CREATED.fetch_add(1, Ordering::Relaxed);
    }
    // Not yet installed: native registration below makes it live.
    context.slot.store(
        Slot::Retained(if pending {
            Attach::Pending
        } else {
            Attach::Settled
        }),
        Ordering::Release,
    );
    // Retain old/failed registrations until successful native close. Context
    // addresses are static; no racing callback can observe a freed allocation.
    let result = unsafe {
        native_registration(
            original,
            client,
            Some(process),
            (context as *const Context).cast_mut().cast(),
            Some(context),
        )
    };
    if result == 0 {
        REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
    }
    // Socket/thread setup stays outside loader hooks and registry ownership.
    drop(controls);
    drop(registry);
    if result == 0 && !pending {
        backend::start_control_server();
    }
    result
}

unsafe extern "C" fn process(frames: u32, opaque: *mut c_void) -> c_int {
    let context = unsafe { &*opaque.cast::<Context>() };
    let pointer = context.callback.load(Ordering::Acquire);
    if pointer == 0 {
        return 1;
    }
    let original: Process = unsafe { std::mem::transmute(pointer) };
    denoiser_hold::enter();
    let started = timing::monotonic();
    let flight_token = client_timing::main_begin(started);
    let previous_start = context.started.swap(started, Ordering::Relaxed);
    if previous_start != 0 && started > previous_start {
        record_gap(started - previous_start);
    }
    let cycle_start = probe::cycle_start(context.client.load(Ordering::Relaxed));
    if cycle_start != 0 {
        let position = started.saturating_sub(cycle_start);
        probe::START_POS.record(position);
        if position > probe::LATE_START_NS {
            probe::record(probe::Kind::LateStart, position);
        }
        let previous_cycle = context.cycle_start.swap(cycle_start, Ordering::Relaxed);
        if previous_cycle != 0
            && cycle_start.saturating_sub(previous_cycle) > probe::PERIOD_NS * 3 / 2
        {
            probe::CYCLE_SKIPS.fetch_add(1, Ordering::Relaxed);
            probe::record(probe::Kind::CycleSkip, cycle_start - previous_cycle);
        }
    }
    // One read of a line no thread writes after load.
    let gauged = gauges::enabled();
    let frame = context.timing.begin(frames, gauged);
    let processing = CONTROL_ADMISSION.admit();
    let engine = if processing {
        context.pipeline.load(Ordering::Acquire)
    } else {
        std::ptr::null_mut()
    };
    let getter = READ_MOSTLY.get_buffer.load(Ordering::Acquire);
    let outer = enter_frame(CallbackFrame {
        owner: opaque as usize,
        processing,
        pipeline: engine as usize,
        started,
        // One accessor call and one generation read per callback; every buffer
        // lookup in this frame reuses them.
        jack: jack_cache::begin(getter),
    });
    CYCLE.callbacks.fetch_add(1, Ordering::Relaxed);
    if !engine.is_null() && getter != 0 {
        let get: GetBuffer = unsafe { std::mem::transmute(getter) };
        // This short exclusive borrow ends before the original callback.
        // Reentrant buffer hooks access the same owner only after it ends.
        unsafe { (*engine).begin_at(frames as usize, get, cycle_start, started) };
    }
    let before_native = timing::Frame::before_native(&frame);
    let result = unsafe {
        original(
            frames,
            context.argument.load(Ordering::Relaxed) as *mut c_void,
        )
    };
    let after_native = timing::Frame::after_native(&frame);
    if !engine.is_null() {
        unsafe { (*engine).end(frames as usize, result == 0) };
    }
    denoiser_hold::leave();
    leave_frame(outer);
    // The exit stamp of the callback timing is also the callback's end: one
    // clock read serves both.
    let ended = context.timing.finish(frame, before_native, after_native);
    context.ended.store(ended, Ordering::Relaxed);
    if !engine.is_null() {
        // For the helper's own wake-up (pipeline::Forecast).
        unsafe { (*engine).ended(ended) };
    }
    let native_span = before_native
        .wall_ns()
        .zip(after_native.wall_ns())
        .map(|(before, after)| [before, after]);
    client_timing::main_end(
        [started, ended, cycle_start, probe::PERIOD_NS, flight_token],
        native_span,
    );
    if client_timing::active() {
        // Keep evidence before USB's deadline: the main callback must leave
        // time for downstream work. This threshold is NOT a JACK deadline.
        MAIN_TAIL.record_partitioned(
            [
                started,
                ended,
                cycle_start,
                probe::PERIOD_NS * 3 / 4,
                context.token.load(Ordering::Relaxed),
            ],
            native_span,
        );
    }
    let wall = ended.saturating_sub(started);
    if wall > probe::PERIOD_NS {
        probe::record(probe::Kind::Overrun, wall);
    }
    if cycle_start != 0 {
        let position = ended.saturating_sub(cycle_start);
        probe::END_POS.record(position);
        if position > probe::PERIOD_NS {
            probe::LATE_ENDS.fetch_add(1, Ordering::Relaxed);
        }
        if position > probe::LATE_END_NS {
            probe::record(probe::Kind::LateEnd, position);
        }
        // Still in the same cycle: both estimates should agree to the clock
        // resolution of jack_frames_since_cycle_start (one frame, 21 us).
        // A second libjack call and clock read, after the callback's end
        // stamp: a consistency check for bench runs only.
        if gauged && position < probe::PERIOD_NS / 2 {
            let again = probe::cycle_start(context.client.load(Ordering::Relaxed));
            let drift = again.abs_diff(cycle_start);
            if drift < probe::PERIOD_NS / 2 {
                probe::ESTIMATE_DRIFT_MAX_NS.fetch_max(drift, Ordering::Relaxed);
            }
        }
    }
    result
}

/// Counts the time between two callback starts (see `CycleCounters`).
fn record_gap(gap: u64) {
    let period = timing::BUDGET_NS;
    let bounds = [
        period / 2,
        period * 9 / 10,
        period * 11 / 10,
        period * 3 / 2,
        period * 2,
        period * 3,
    ];
    let bin = bounds.iter().position(|bound| gap < *bound).unwrap_or(6);
    CYCLE.gaps[bin].fetch_add(1, Ordering::Relaxed);
    if gap < period * 9 / 10 {
        CYCLE.gaps_short.fetch_add(1, Ordering::Relaxed);
    } else if gap > period * 11 / 10 {
        CYCLE.gaps_long.fetch_add(1, Ordering::Relaxed);
    }
    CYCLE.max_gap.fetch_max(gap, Ordering::Relaxed);
    // A gap of 1.5 periods or more, with its size. Smaller out-of-band gaps
    // are in the counters above; they would otherwise fill the ring.
    if gap >= period * 3 / 2 {
        probe::record(probe::Kind::Gap, gap);
    }
}
/// The mixer's `jack_port_get_buffer`, on every thread that fetches a buffer:
/// about 68 times a cycle in the wrapped callback and 40 on the USB client
/// threads. It writes nothing shared. Inside a callback frame it adds to the
/// thread's own count; outside one it counts only while the bench gauges are
/// on.
/// One buffer lookup inside a callback frame: the cache may answer without
/// calling libjack, and a miss records what the real function returned.
///
/// # Safety
/// `port` and `frames` are the caller's `jack_port_get_buffer` arguments and
/// `get` is that function.
pub(crate) unsafe fn port_buffer(
    jack: jack_cache::Snapshot,
    get: GetBuffer,
    port: *mut c_void,
    frames: u32,
) -> *mut c_void {
    match unsafe { jack_cache::hit(jack, port) } {
        Some(cached) => cached,
        None => {
            let value = unsafe { get(port, frames) };
            unsafe { jack_cache::record(jack, port, value) };
            value
        }
    }
}

/// The same lookup for a caller that is inside a callback frame but did not
/// receive its snapshot, such as the pipeline's own port resolution.
///
/// # Safety
/// As [`port_buffer`].
pub(crate) unsafe fn port_buffer_in_frame(
    get: GetBuffer,
    port: *mut c_void,
    frames: u32,
) -> *mut c_void {
    let jack = INSIDE.with(|inside| inside.get().jack);
    unsafe { port_buffer(jack, get, port, frames) }
}

unsafe extern "C" fn get_buffer(port: *mut c_void, frames: u32) -> *mut c_void {
    let pointer = READ_MOSTLY.get_buffer.load(Ordering::Acquire);
    if pointer == 0 {
        return std::ptr::null_mut();
    }
    let (frame, in_frame) = INSIDE.with(|inside| {
        let frame = inside.get();
        let in_frame = frame.owner != 0;
        if in_frame {
            inside.buffer_calls.set(inside.buffer_calls.get() + 1);
        }
        (frame, in_frame)
    });
    if !in_frame && READ_MOSTLY.gauges.load(Ordering::Relaxed) {
        BUFFER_CALLS_OUTSIDE.fetch_add(1, Ordering::Relaxed);
    }
    let original: GetBuffer = unsafe { std::mem::transmute(pointer) };
    let native = if in_frame {
        unsafe { port_buffer(frame.jack, original, port, frames) }
    } else {
        unsafe { original(port, frames) }
    };
    if frame.owner != 0 && frame.processing {
        let engine = frame.pipeline as *mut pipeline::Pipeline;
        if !engine.is_null() {
            return unsafe { (*engine).buffer(port as usize, frames as usize, native) };
        }
    }
    native
}

unsafe extern "C" fn register(
    client: *mut c_void,
    name: *const c_char,
    kind: *const c_char,
    flags: usize,
    size: usize,
) -> *mut c_void {
    let pointer = REGISTER.load(Ordering::Acquire);
    if pointer == 0 || inside() {
        return std::ptr::null_mut();
    }
    let original: Register = unsafe { std::mem::transmute(pointer) };
    let Ok(mut registry) = CONTROL.lock() else {
        return std::ptr::null_mut();
    };
    let port = unsafe { original(client, name, kind, flags, size) };
    if !port.is_null()
        && unsafe { CStr::from_ptr(kind) }.to_bytes() == b"32 bit float mono audio"
        && !registry.register(
            client as usize,
            port as usize,
            unsafe { CStr::from_ptr(name) }.to_bytes(),
            flags,
        )
    {
        PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    let attached = attach_pending(&registry, client as usize);
    drop(registry);
    if attached {
        backend::start_control_server();
    }
    port
}

// CONTROL is held by the native port-registration caller. Publish a fully
// constructed engine once; callbacks capture its pointer only at frame entry.
fn attach_pending(registry: &pipeline::Registry, client: usize) -> bool {
    let Some(plan) = PLAN.get() else {
        return false;
    };
    if !pipeline::Pipeline::ports_ready(plan, registry, client) {
        return false;
    }
    let mut attached = false;
    for context in &CONTEXTS {
        if context.slot.load(Ordering::Acquire) != Slot::Live(Attach::Pending)
            || context.client.load(Ordering::Relaxed) != client
        {
            continue;
        }
        context
            .slot
            .store(Slot::Live(Attach::Settled), Ordering::Release);
        let Ok(mut controls) = context.parameters.lock() else {
            continue;
        };
        match pipeline::Pipeline::new(plan, registry, client) {
            Ok((engine, parameters)) => {
                *controls = parameters;
                context
                    .pipeline
                    .store(Box::into_raw(Box::new(engine)), Ordering::Release);
                PIPELINES_CREATED.fetch_add(1, Ordering::Relaxed);
                attached = true;
                eprintln!("RC2FX native processing attached after port registration");
            }
            Err(error) => {
                PLAN_ERRORS.fetch_add(1, Ordering::Relaxed);
                eprintln!("RC2FX deferred native pipeline refused: {error}");
            }
        }
    }
    attached
}

unsafe extern "C" fn unregister(client: *mut c_void, port: *mut c_void) -> c_int {
    let pointer = UNREGISTER.load(Ordering::Acquire);
    if pointer == 0 || inside() {
        return -1;
    }
    let original: Unregister = unsafe { std::mem::transmute(pointer) };
    let Ok(mut registry) = CONTROL.lock() else {
        return -1;
    };
    registry.retire(client as usize, port as usize);
    unsafe { original(client, port) }
}

unsafe extern "C" fn close(client: *mut c_void) -> c_int {
    let pointer = CLOSE.load(Ordering::Acquire);
    if pointer == 0 || inside() {
        return -1;
    }
    let original: Close = unsafe { std::mem::transmute(pointer) };
    let Ok(mut registry) = CONTROL.lock() else {
        return -1;
    };
    let result = unsafe { original(client) };
    if result == 0 {
        registry.close(client as usize);
        for context in &CONTEXTS {
            if context.slot.load(Ordering::Relaxed).is_claimed()
                && context.client.load(Ordering::Relaxed) == client as usize
            {
                let engine = context
                    .pipeline
                    .swap(std::ptr::null_mut(), Ordering::AcqRel);
                if !engine.is_null() {
                    // Native close has quiesced the client. Destruction stays
                    // on this control caller.
                    drop(unsafe { Box::from_raw(engine) });
                    PIPELINES_RETIRED.fetch_add(1, Ordering::Relaxed);
                }
                // The native callback and its parameter readers have stopped.
                // Reclaim every remaining snapshot on this control caller.
                if let Ok(mut controls) = context.parameters.lock() {
                    controls.clear();
                }
                // No longer reported as live, and no longer waiting for ports.
                context
                    .slot
                    .store(Slot::Retained(Attach::Settled), Ordering::Release);
                context.timing.retire_into(&RETIRED_TIMING);
                context.token.store(0, Ordering::Relaxed);
                context.callback.store(0, Ordering::Relaxed);
                context.argument.store(0, Ordering::Relaxed);
                context.client.store(0, Ordering::Relaxed);
                context.slot.store(Slot::Free, Ordering::Release);
                RECLAIMED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    result
}

/// # Safety
/// The loader owns this cookie and invokes main-object finalization on exit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_objclose(cookie: *mut usize) -> c_uint {
    if unsafe { *cookie } == MAIN {
        let stopped = backend::stop_control_server();
        audit_report::write_report();
        // Finalization gate: returning while receiver code is
        // still executing would permit unsafe library teardown.
        if !stopped {
            std::process::abort();
        }
    }
    0
}

#[cfg(test)]
use audit_report::build_report;
#[cfg(test)]
use backend::{parameter_error, target_status};
#[cfg(test)]
mod lib_tests;
