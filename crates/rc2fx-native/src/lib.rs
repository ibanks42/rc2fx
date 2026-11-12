//! Private ELF-audit/DSP feasibility probe; never a device installation.
#![cfg(all(target_os = "linux", target_pointer_width = "64"))]

mod chat_mix;
mod client_timing;
mod client_timing_events;
mod gauges;
mod ipc;
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

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    ffi::{CStr, OsStr, c_char, c_int, c_uint, c_void},
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
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

/// One wrapped callback registration. Its callback thread reads the head and
/// writes the timing and the stamps every cycle, so each context starts on a
/// cache line and shares none with anything else.
#[repr(align(64))]
struct Context {
    claimed: AtomicBool,
    installed: AtomicBool,
    pending: AtomicBool,
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
            claimed: AtomicBool::new(false),
            installed: AtomicBool::new(false),
            pending: AtomicBool::new(false),
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
static INPUT_BLOCKS: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BLOCKS: AtomicU64 = AtomicU64::new(0);
static DSP_FAULTS: AtomicU64 = AtomicU64::new(0);
static SKIPPED_OUTPUTS: AtomicU64 = AtomicU64::new(0);
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
            frame: Cell::new(CallbackFrame { owner: 0, processing: false, pipeline: 0, started: 0 }),
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

fn inside() -> bool {
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
    if std::env::var_os("RC2FX_FIRMWARE_PROFILE").is_some()
        || std::env::var_os("RC2FX_NATIVE_PROBE_CALLBACK_OFFSET").is_some()
        || enabled(c"RC2FX_NATIVE_PROBE_VENDOR_JOB_LIFETIME")
    {
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

/// Resolve all addresses from one hash-selected, ABI-validated profile. A probe
/// offset can confirm that profile, but cannot bypass its identity checks.
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
    if let Ok(requested) = std::env::var("RC2FX_NATIVE_PROBE_CALLBACK_OFFSET") {
        let requested = usize::from_str_radix(requested.trim_start_matches("0x"), 16)
            .map_err(|e| e.to_string())?;
        if requested != offset {
            return Err("probe callback offset does not match the firmware profile".into());
        }
    }
    vendor_job::install(MAIN_BASE.load(Ordering::Acquire), profile)?;
    chat_mix::install(MAIN_BASE.load(Ordering::Acquire), profile)?;
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
            if context.claimed.load(Ordering::Acquire)
                && context.client.load(Ordering::Relaxed) == client as usize
            {
                context.installed.store(
                    installed.is_some_and(|selected| std::ptr::eq(context, selected)),
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
    let _ = probe::resolve_xrun_delayed();
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
        .find(|context| !context.claimed.load(Ordering::Acquire))
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
    context.pending.store(pending, Ordering::Release);
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
    context.claimed.store(true, Ordering::Release);
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
        start_control_server();
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
    let outer = enter_frame(CallbackFrame {
        owner: opaque as usize,
        processing,
        pipeline: engine as usize,
        started,
    });
    CYCLE.callbacks.fetch_add(1, Ordering::Relaxed);
    if !engine.is_null() {
        let getter = READ_MOSTLY.get_buffer.load(Ordering::Acquire);
        if getter != 0 {
            let get: GetBuffer = unsafe { std::mem::transmute(getter) };
            // This short exclusive borrow ends before the original callback.
            // Reentrant buffer hooks access the same owner only after it ends.
            unsafe { (*engine).begin_at(frames as usize, get, cycle_start, started) };
        }
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
    leave_frame(outer);
    // The exit stamp of the callback timing is also the callback's end: one
    // clock read serves both.
    let ended = context.timing.finish(frame, before_native, after_native);
    context.ended.store(ended, Ordering::Relaxed);
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
unsafe extern "C" fn get_buffer(port: *mut c_void, frames: u32) -> *mut c_void {
    let pointer = READ_MOSTLY.get_buffer.load(Ordering::Acquire);
    if pointer == 0 {
        return std::ptr::null_mut();
    }
    let in_frame = INSIDE.with(|inside| {
        let in_frame = inside.get().owner != 0;
        if in_frame {
            inside.buffer_calls.set(inside.buffer_calls.get() + 1);
        }
        in_frame
    });
    if !in_frame && READ_MOSTLY.gauges.load(Ordering::Relaxed) {
        BUFFER_CALLS_OUTSIDE.fetch_add(1, Ordering::Relaxed);
    }
    let original: GetBuffer = unsafe { std::mem::transmute(pointer) };
    let native = unsafe { original(port, frames) };
    let frame = INSIDE.with(|value| value.get());
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
        start_control_server();
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
        if !context.claimed.load(Ordering::Acquire)
            || !context.installed.load(Ordering::Acquire)
            || context.client.load(Ordering::Relaxed) != client
            || !context.pending.load(Ordering::Acquire)
        {
            continue;
        }
        context.pending.store(false, Ordering::Release);
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
            if context.claimed.load(Ordering::Relaxed)
                && context.client.load(Ordering::Relaxed) == client as usize
            {
                let engine = context
                    .pipeline
                    .swap(std::ptr::null_mut(), Ordering::AcqRel);
                if !engine.is_null() {
                    // Native close has quiesced the client. Destruction and
                    // telemetry collection stay on this control caller.
                    let engine = unsafe { Box::from_raw(engine) };
                    let counters = engine.counters();
                    INPUT_BLOCKS.fetch_add(counters.input_blocks, Ordering::Relaxed);
                    OUTPUT_BLOCKS.fetch_add(counters.output_blocks, Ordering::Relaxed);
                    DSP_FAULTS.fetch_add(counters.faults, Ordering::Relaxed);
                    SKIPPED_OUTPUTS.fetch_add(counters.skipped_outputs, Ordering::Relaxed);
                    drop(engine);
                    PIPELINES_RETIRED.fetch_add(1, Ordering::Relaxed);
                }
                // The native callback and its parameter readers have stopped.
                // Reclaim every remaining snapshot on this control caller.
                if let Ok(mut controls) = context.parameters.lock() {
                    controls.clear();
                }
                context.installed.store(false, Ordering::Release);
                context.timing.retire_into(&RETIRED_TIMING);
                context.pending.store(false, Ordering::Release);
                context.token.store(0, Ordering::Relaxed);
                context.callback.store(0, Ordering::Relaxed);
                context.argument.store(0, Ordering::Relaxed);
                context.client.store(0, Ordering::Relaxed);
                context.claimed.store(false, Ordering::Release);
                RECLAIMED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    result
}

fn parameter_error(error: parameters::Error) -> rc2fx_native_control::ErrorCode {
    match error {
        parameters::Error::WrongTarget => rc2fx_native_control::ErrorCode::Unavailable,
        parameters::Error::Busy => rc2fx_native_control::ErrorCode::Busy,
        parameters::Error::SuppressionInUse => rc2fx_native_control::ErrorCode::SuppressionInUse,
        parameters::Error::RequiresReset => rc2fx_native_control::ErrorCode::RequiresReset,
        parameters::Error::StaleRevision => rc2fx_native_control::ErrorCode::StaleRevision,
        parameters::Error::Invalid => rc2fx_native_control::ErrorCode::InvalidParameters,
        parameters::Error::Poisoned | parameters::Error::Runtime => {
            rc2fx_native_control::ErrorCode::Internal
        }
    }
}
fn lock_error<T>(error: std::sync::TryLockError<T>) -> rc2fx_native_control::ErrorCode {
    match error {
        std::sync::TryLockError::WouldBlock => rc2fx_native_control::ErrorCode::Busy,
        std::sync::TryLockError::Poisoned(_) => rc2fx_native_control::ErrorCode::Internal,
    }
}
fn target_status(
    control: &mut parameters::Control,
) -> Result<rc2fx_native_control::TargetStatus, rc2fx_native_control::ErrorCode> {
    let status = control.status().map_err(parameter_error)?;
    let worker = control.worker_status();
    let (misses, lates) = match control.worker_histories() {
        Some((misses, lates)) => (Some(misses), Some(lates)),
        None => (None, None),
    };
    Ok(rc2fx_native_control::TargetStatus {
        id: control.id().into(),
        published: status.published,
        seen: status.seen,
        applied: status.applied,
        fingerprint: status.fingerprint,
        mode: status.mode,
        runtime_epoch: status.runtime_epoch,
        meter: status.meter,
        worker,
        misses,
        lates,
        retired_workers: control.retired_workers(),
        retired_worker_count: control.retired_worker_count(),
    })
}
fn with_parameter_control<T>(
    token: u64,
    id: &str,
    apply: impl FnOnce(&mut parameters::Control) -> Result<T, rc2fx_native_control::ErrorCode>,
) -> Result<T, rc2fx_native_control::ErrorCode> {
    // Return backpressure instead of waiting behind a native lifecycle call.
    let _registry = CONTROL.try_lock().map_err(lock_error)?;
    let Some(context) = CONTEXTS.iter().find(|context| {
        context.claimed.load(Ordering::Acquire)
            && context.installed.load(Ordering::Acquire)
            && context.token.load(Ordering::Relaxed) == token
    }) else {
        return Err(rc2fx_native_control::ErrorCode::Unavailable);
    };
    let mut controls = context.parameters.try_lock().map_err(lock_error)?;
    let Some(control) = controls.iter_mut().find(|control| control.id() == id) else {
        return Err(rc2fx_native_control::ErrorCode::Unavailable);
    };
    apply(control)
}
#[derive(Default)]
struct NativeBackend {
    transactions: transactions::Manager,
}
struct LiveEndpoints;
impl transactions::Endpoints for LiveEndpoints {
    fn prepare(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        target: &rc2fx_core::Target,
        mode: rc2fx_native_control::PathMode,
    ) -> Result<parameters::Staged, rc2fx_native_control::ErrorCode> {
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            control
                .prepare_mode(target, transaction.revision, mode)
                .map_err(parameter_error)
        })
    }
    fn commit(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        staged: &mut Option<parameters::Staged>,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            let staged = staged
                .take()
                .ok_or(rc2fx_native_control::ErrorCode::Unavailable)?;
            control.reserve(staged).map_err(parameter_error)?.commit();
            Ok(())
        })
    }
    fn status(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<rc2fx_native_control::TargetStatus, rc2fx_native_control::ErrorCode> {
        with_parameter_control(transaction.context, &transaction.target_id, |control| {
            target_status(control)
        })
    }
}
impl ipc::Backend for NativeBackend {
    fn inventory(
        &mut self,
    ) -> Result<Vec<rc2fx_native_control::ContextStatus>, rc2fx_native_control::ErrorCode> {
        self.transactions.reap(std::time::Instant::now());
        let _registry = CONTROL.try_lock().map_err(lock_error)?;
        let mut inventory = Vec::new();
        for context in &CONTEXTS {
            if !context.claimed.load(Ordering::Acquire)
                || !context.installed.load(Ordering::Acquire)
            {
                continue;
            }
            let mut controls = context.parameters.try_lock().map_err(lock_error)?;
            let targets = controls
                .iter_mut()
                .map(target_status)
                .collect::<Result<Vec<_>, _>>()?;
            inventory.push(rc2fx_native_control::ContextStatus {
                context: context.token.load(Ordering::Relaxed),
                targets,
                // Live context plus what already retired: the app's view stays
                // monotonic across rebuilds, which is what its fault counters
                // need to see.
                timing: Some(context.timing.merged_with(RETIRED_TIMING.snapshot())),
                counters: engine_counters(),
                events: engine_events(),
            });
        }
        Ok(inventory)
    }
    fn status(
        &mut self,
        context: u64,
        id: &str,
    ) -> Result<rc2fx_native_control::TargetStatus, rc2fx_native_control::ErrorCode> {
        self.transactions.reap(std::time::Instant::now());
        with_parameter_control(context, id, target_status)
    }
    fn prepare(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
        target: &rc2fx_core::Target,
        mode: rc2fx_native_control::PathMode,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions.prepare(
            transaction,
            target,
            mode,
            std::time::Instant::now(),
            &mut LiveEndpoints,
        )
    }
    fn commit(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions
            .commit(transaction, std::time::Instant::now(), &mut LiveEndpoints)
    }
    fn cancel(
        &mut self,
        transaction: &rc2fx_native_control::Transaction,
    ) -> Result<(), rc2fx_native_control::ErrorCode> {
        self.transactions
            .cancel(transaction, std::time::Instant::now())
    }
}
fn start_control_server() {
    start_report_refresh();
    let Some(path) = CONTROL_SOCKET.get() else {
        return;
    };
    SERVER.get_or_init(|| {
        Mutex::new(
            match ipc::Server::start(path, NativeBackend::default(), Some(&CONTROL_ADMISSION)) {
                Ok(server) => Some(server),
                Err(_) => {
                    SERVER_ERRORS.fetch_add(1, Ordering::Relaxed);
                    None
                }
            },
        )
    });
}
fn stop_control_server() -> bool {
    let Some(server) = SERVER.get() else {
        return true;
    };
    let Ok(mut server) = server.lock() else {
        return false;
    };
    server.as_mut().is_none_or(|server| server.shutdown())
}

/// # Safety
/// The loader owns this cookie and invokes main-object finalization on exit.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn la_objclose(cookie: *mut usize) -> c_uint {
    if unsafe { *cookie } == MAIN {
        let stopped = stop_control_server();
        write_report();
        // Research-only finalization gate: returning while receiver code is
        // still executing would permit unsafe library teardown.
        if !stopped {
            std::process::abort();
        }
    }
    0
}

fn write_report() {
    let held = publishing();
    if REPORT_SELECTED_ONLY.load(Ordering::Relaxed) && !SELECTOR_SEEN.load(Ordering::Relaxed) {
        return;
    }
    let Some(path) = REPORT.get() else {
        return;
    };
    let report = build_report();
    let _ = publish_report(&held, path, &report, |partial| {
        std::fs::File::create(partial)
    });
}
/// The audit report. Report thread and finalization only.
fn build_report() -> serde_json::Value {
    let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    // Bench-only measurements are null without the gauges, never a zero that
    // could be read as a measured value.
    let gauged = gauges::enabled();
    let retained = CONTEXTS
        .iter()
        .filter(|context| context.claimed.load(Ordering::Acquire))
        .count();
    let mut report = serde_json::json!({"probeOnly":true,"dspEnabled":read(&INPUT_BLOCKS)+read(&OUTPUT_BLOCKS)>0,
        "registrations":read(&REGISTRATIONS),"callbacks":read(&CYCLE.callbacks),"buffersInside":read(&CYCLE.buffer_calls_inside),
        "buffersOutside":gauged.then(|| read(&BUFFER_CALLS_OUTSIDE)),"directBindings":read(&DIRECT_BINDINGS),"dlsymBindings":read(&DLSYM_BINDINGS),
        "bindingsDuringCallback":read(&CALLBACK_BINDINGS),"conflicts":read(&CONFLICTS),"capacityFailures":read(&CAPACITY_FAILURES),
        "teardownForwards":read(&TEARDOWN_FORWARDS),
        "reclaimedContexts":read(&RECLAIMED),"retainedContexts":retained,"callbackAllocations":read(&CALLBACK_ALLOCATIONS),
        "callbackFrees":read(&CALLBACK_FREES),"callbackReallocations":read(&CALLBACK_REALLOCATIONS),
        "planErrors":read(&PLAN_ERRORS),"pipelinesCreated":read(&PIPELINES_CREATED),"pipelinesRetired":read(&PIPELINES_RETIRED),
        "inputBlocks":read(&INPUT_BLOCKS),"outputBlocks":read(&OUTPUT_BLOCKS),"dspFaults":read(&DSP_FAULTS),
        "skippedOutputs":read(&SKIPPED_OUTPUTS),"retainedPorts":CONTROL.lock().map(|registry|registry.ports.len()).unwrap_or(usize::MAX)});
    report["vendorJobRepair"] = vendor_job::status();
    report["firmwareProfile"] = FIRMWARE_PROFILE.get().cloned().into();
    report["controlServerErrors"] = read(&SERVER_ERRORS).into();
    report["controlRequired"] = CONTROL_ADMISSION.required().into();
    report["processPid"] = std::process::id().into();
    report["clientTiming"] = client_timing::snapshot();
    report["srcFast"] = src_fast::snapshot();
    if gauged {
        report["gauges"] = gauges::snapshot();
    }
    report["mainCallbackTail"] = MAIN_TAIL.snapshot();
    report["retiredCallbackTiming"] =
        serde_json::to_value(RETIRED_TIMING.snapshot()).unwrap_or(serde_json::Value::Null);
    report["callbackTiming"] = CONTEXTS
        .iter()
        .filter(|context| context.installed.load(Ordering::Acquire))
        .map(|context| serde_json::to_value(context.timing.snapshot()).unwrap_or_default())
        .collect();
    report["helper"] = serde_json::json!({
        "frames": read(&pipeline::HELPER_TIME.frames),
        "totalNs": read(&pipeline::HELPER_TIME.total_ns),
        "maxNs": read(&pipeline::HELPER_TIME.max_ns),
        "stalls": read(&pipeline::HELPER_STALLS),
        "late": read(&pipeline::HELPER_LATE),
        "minMarginNs": read(&pipeline::HELPER_MIN_MARGIN_NS),
    });
    report["sources"] = pipeline::sources();
    // Live processing counters (mirrors; the fields above stay
    // retirement-only). Read without touching a pipeline.
    report["processingCounters"] = pipeline::processing_counters();
    report["pathCostNs"] = gauged
        .then(|| pipeline::PATH_COST_NS.iter().map(read).collect::<Vec<_>>())
        .into();
    report["resolveNs"] = gauged.then(|| read(&pipeline::RESOLVE_NS)).into();
    report["callbackGaps"] = CYCLE.gaps.iter().map(read).collect();
    report["callbackGapsOutsideBand"] = serde_json::json!({
        "short": read(&CYCLE.gaps_short),
        "long": read(&CYCLE.gaps_long),
        "band": "0.9-1.1 periods",
        "ring": "gap events >= 1.5 periods",
    });
    report["maxCallbackGapNs"] = read(&CYCLE.max_gap).into();
    report["monotonicNs"] = timing::monotonic().into();
    let mut preparation_workers = Vec::new();
    let mut unavailable = 0usize;
    for (context_slot, context) in CONTEXTS.iter().enumerate() {
        if !context.installed.load(Ordering::Acquire) {
            continue;
        }
        let Ok(controls) = context.parameters.try_lock() else {
            unavailable += 1;
            continue;
        };
        for (target_slot, control) in controls.iter().enumerate() {
            match control.worker_telemetry() {
                Ok(Some((runtime, telemetry))) => {
                    preparation_workers.push(serde_json::json!({
                        "id": control.id(), "contextSlot": context_slot,
                        "targetSlot": target_slot, "runtime": runtime,
                        "preparation": suppression::preparation_report(telemetry.preparation()),
                    }));
                }
                Ok(None) => {}
                Err(_) => unavailable += 1,
            }
        }
    }
    report["workerPreparation"] = preparation_workers.into();
    // Failed context/path reads are explicit: an empty array with failed
    // reads is not an idle worker.
    report["workerPreparationUnavailableReads"] = unavailable.into();
    report["workers"] = CONTEXTS
        .iter()
        .filter(|context| context.installed.load(Ordering::Acquire))
        .filter_map(|context| context.parameters.try_lock().ok())
        .flat_map(|controls| {
            controls
                .iter()
                .filter_map(|control| {
                    control
                        .worker_status()
                        .map(|worker| serde_json::json!({"id": control.id(), "worker": worker}))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    report["workerOutliers"] = CONTEXTS
        .iter()
        .filter(|context| context.installed.load(Ordering::Acquire))
        .filter_map(|context| context.parameters.try_lock().ok())
        .flat_map(|controls| {
            controls
                .iter()
                .filter_map(|control| {
                    control.worker_outliers().map(
                        |outliers| serde_json::json!({"id": control.id(), "outliers": outliers}),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    report["controlSkippedCallbacks"] = CONTROL_ADMISSION.skipped().into();
    if let serde_json::Value::Object(probe) = probe::snapshot() {
        for (key, value) in probe {
            report[key] = value;
        }
    }
    report["chatVoice"] = chat_mix::status();
    report["placement"] = pipeline::placement();
    report["suppression"] =
        serde_json::to_value(suppression::stats()).unwrap_or(serde_json::Value::Null);
    if let Some(server) = SERVER.get()
        && let Ok(server) = server.lock()
        && let Some(server) = server.as_ref()
        && let Ok(status) = serde_json::to_value(server.status())
    {
        report["controlServer"] = status;
    }
    report
}
/// Report publication, by the refresh thread and at finalization (never a
/// real-time thread). Held from before a report is built until it is
/// published, so each published report was built after the previous one was
/// published, and the partial file is always the holder's own.
static PUBLISHING: Mutex<()> = Mutex::new(());
/// Proof that PUBLISHING is held.
struct Publishing {
    _held: MutexGuard<'static, ()>,
}
fn publishing() -> Publishing {
    Publishing {
        _held: PUBLISHING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}
/// Room for a whole report, so it reaches the file in one write instead of
/// one per JSON token.
const REPORT_BUFFER: usize = 64 * 1024;
/// The report as one line of JSON, buffered; a failed flush is returned, not
/// dropped.
fn write_json<W: Write>(writer: W, report: &serde_json::Value) -> io::Result<W> {
    let mut buffered = io::BufWriter::with_capacity(REPORT_BUFFER, writer);
    serde_json::to_writer(&mut buffered, report)?;
    writeln!(buffered)?;
    buffered
        .into_inner()
        .map_err(io::IntoInnerError::into_error)
}
/// Replaces the report whole, so a reader never sees a partial one: written
/// and flushed to a partial file, renamed over the previous report only if
/// all of that succeeded. On failure the previous report stays.
fn publish_report<W: Write>(
    _held: &Publishing,
    path: &Path,
    report: &serde_json::Value,
    open: impl FnOnce(&Path) -> io::Result<W>,
) -> io::Result<()> {
    let partial = path.with_extension("partial");
    let result = open(&partial)
        .and_then(|file| write_json(file, report))
        .and_then(|_| std::fs::rename(&partial, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}
/// Seconds after start in which the report stays at one per second: the
/// install commit check and the GUI's readiness wait watch `callbacks` advance.
const REPORT_STARTUP_SECONDS: u64 = 180;
/// Afterwards, without a diagnostic marker. Must stay well inside the bench's
/// five-second telemetry limit, which a run without a marker also applies.
const REPORT_EVERY_SECONDS: u64 = 3;
fn report_due(elapsed_seconds: u64, diagnostic: bool) -> bool {
    diagnostic
        || elapsed_seconds < REPORT_STARTUP_SECONDS
        || elapsed_seconds.is_multiple_of(REPORT_EVERY_SECONDS)
}

/// Refresh the report while the mixer runs, so it can be read without stopping
/// audio. Events still reach their log every second.
fn start_report_refresh() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if REPORT.get().is_none() || STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("rc2fx-report".into())
        .spawn(|| {
            let diagnostic = client_timing::enabled() || gauges::enabled();
            let mut elapsed_seconds = 0u64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                elapsed_seconds += 1;
                if report_due(elapsed_seconds, diagnostic) {
                    write_report();
                }
                probe::flush_log();
            }
        });
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    /// The tests here drive the process wrapper and the buffer hook, which
    /// count into process-wide statics: one at a time.
    static HOOKS: Mutex<()> = Mutex::new(());
    /// What the stand-in vendor callback saw of the published count while it
    /// was still inside its frame.
    static SEEN_INSIDE: AtomicU64 = AtomicU64::new(0);

    fn serial() -> (MutexGuard<'static, ()>, MutexGuard<'static, ()>) {
        let hooks = HOOKS.lock().unwrap_or_else(|poison| poison.into_inner());
        // Callbacks a few microseconds apart count as short gaps, which the
        // gap test reads.
        let events = probe::EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        READ_MOSTLY
            .get_buffer
            .store(real_get_buffer as *const () as usize, Ordering::Release);
        (hooks, events)
    }
    /// The real getter: a pointer that names its port and frame count.
    unsafe extern "C" fn real_get_buffer(port: *mut c_void, frames: u32) -> *mut c_void {
        (port as usize * 1024 + frames as usize) as *mut c_void
    }
    /// The vendor callback: fetches `argument` buffers through the hook.
    unsafe extern "C" fn vendor(frames: u32, argument: *mut c_void) -> c_int {
        for port in 1..=argument as usize {
            let got = unsafe { get_buffer(port as *mut c_void, frames) };
            assert_eq!(got as usize, port * 1024 + frames as usize);
        }
        SEEN_INSIDE.store(
            CYCLE.buffer_calls_inside.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        0
    }
    fn context(calls: usize) -> &'static Context {
        let context = Box::leak(Box::new(Context::new()));
        context
            .callback
            .store(vendor as *const () as usize, Ordering::Release);
        context.argument.store(calls, Ordering::Relaxed);
        context
    }
    fn run(context: &'static Context) -> c_int {
        unsafe { process(112, (context as *const Context).cast_mut().cast()) }
    }
    fn clock_reads() -> [u64; 2] {
        timing::CLOCK_READS.with(Cell::get)
    }

    /// N callbacks of K hooked buffer calls each publish N x K, once per
    /// callback at its end, with no pipeline attached.
    #[test]
    fn buffer_calls_inside_a_callback_are_published_once_when_it_ends() {
        let _serial = serial();
        const CALLS: usize = 68;
        const CALLBACKS: u64 = 25;
        let context = context(CALLS);
        assert!(context.pipeline.load(Ordering::Relaxed).is_null());
        let inside = || CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let (before, callbacks) = (inside(), CYCLE.callbacks.load(Ordering::Relaxed));
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        for done in 0..CALLBACKS {
            assert_eq!(run(context), 0);
            // Nothing was published while the callback was inside its frame.
            assert_eq!(
                SEEN_INSIDE.load(Ordering::Relaxed),
                before + done * CALLS as u64
            );
            assert_eq!(inside(), before + (done + 1) * CALLS as u64);
        }
        assert_eq!(
            CYCLE.callbacks.load(Ordering::Relaxed),
            callbacks + CALLBACKS
        );
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
        // The thread is outside any frame again, with nothing left to publish.
        assert!(!super::inside());
        assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
        assert_eq!(
            context.timing.snapshot().callback_wall.calls,
            CALLBACKS,
            "every callback was timed"
        );
    }

    /// A call from a thread that is in no callback frame (a USB client's
    /// thread) returns exactly what the real getter returns and writes no
    /// counter, unless the bench gauges are on.
    #[test]
    fn a_buffer_call_outside_a_callback_is_forwarded_and_counted_only_with_the_gauges() {
        let _serial = serial();
        let inside = CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        std::thread::spawn(move || {
            for port in 1..=40usize {
                let got = unsafe { get_buffer(port as *mut c_void, 112) };
                assert_eq!(got as usize, port * 1024 + 112);
            }
            assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
        })
        .join()
        .unwrap();
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
        assert_eq!(CYCLE.buffer_calls_inside.load(Ordering::Relaxed), inside);
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        for port in 1..=40usize {
            let got = unsafe { get_buffer(port as *mut c_void, 112) };
            assert_eq!(got as usize, port * 1024 + 112);
        }
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside + 40);
        assert_eq!(CYCLE.buffer_calls_inside.load(Ordering::Relaxed), inside);
    }

    /// A frame inside another publishes its own calls when it ends; the
    /// enclosing frame keeps its own count and publishes it at its own end.
    #[test]
    fn a_nested_frame_publishes_only_its_own_buffer_calls() {
        let _serial = serial();
        let published = || CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let call = |times: usize| {
            for port in 1..=times {
                unsafe { get_buffer(port as *mut c_void, 112) };
            }
        };
        let frame = |owner| CallbackFrame {
            owner,
            processing: false,
            pipeline: 0,
            started: 0,
        };
        let before = published();
        let outer = enter_frame(frame(0x10));
        call(3);
        let inner = enter_frame(frame(0x20));
        call(2);
        assert_eq!(published(), before);
        leave_frame(inner);
        assert_eq!(published(), before + 2, "the inner frame's two");
        assert_eq!(INSIDE.with(|inside| inside.get().owner), 0x10);
        call(1);
        leave_frame(outer);
        assert_eq!(published(), before + 2 + 4, "the outer frame's four");
        assert!(!super::inside());
        // A frame with no buffer call publishes nothing.
        leave_frame(enter_frame(frame(0x30)));
        assert_eq!(published(), before + 6);
    }

    /// Several threads in callback frames at once, each with its own number
    /// of buffer calls, while other threads fetch buffers outside any frame
    /// (as the USB clients do). Each callback publishes exactly its own
    /// calls, no thread's count reaches another, and a call from outside a
    /// frame is counted nowhere while the gauges are off.
    #[test]
    fn concurrent_callbacks_and_outside_callers_keep_their_own_counts() {
        let _serial = serial();
        const THREADS: usize = 6;
        const CALLBACKS: u64 = 300;
        let inside = CYCLE.buffer_calls_inside.load(Ordering::Relaxed);
        let callbacks = CYCLE.callbacks.load(Ordering::Relaxed);
        let outside = BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed);
        let stop = AtomicBool::new(false);
        let outside_calls = AtomicU64::new(0);
        let expected: u64 = std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    let mut port = 1usize;
                    let mut calls = 0;
                    while !stop.load(Ordering::Relaxed) {
                        let got = unsafe { get_buffer(port as *mut c_void, 112) };
                        assert_eq!(got as usize, port * 1024 + 112);
                        port = port % 40 + 1;
                        calls += 1;
                    }
                    assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
                    outside_calls.fetch_add(calls, Ordering::Relaxed);
                });
            }
            let workers: Vec<_> = (0..THREADS)
                .map(|thread| {
                    scope.spawn(move || {
                        let calls = 3 + 13 * thread;
                        let context = context(calls);
                        for _ in 0..CALLBACKS {
                            assert_eq!(run(context), 0);
                            assert_eq!(INSIDE.with(|inside| inside.buffer_calls.get()), 0);
                            assert!(!super::inside());
                        }
                        assert_eq!(context.timing.snapshot().callback_wall.calls, CALLBACKS);
                        calls as u64 * CALLBACKS
                    })
                })
                .collect();
            let expected = workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum();
            stop.store(true, Ordering::Relaxed);
            expected
        });
        assert!(outside_calls.load(Ordering::Relaxed) > 0);
        assert_eq!(
            CYCLE.buffer_calls_inside.load(Ordering::Relaxed),
            inside + expected
        );
        assert_eq!(
            CYCLE.callbacks.load(Ordering::Relaxed),
            callbacks + THREADS as u64 * CALLBACKS
        );
        assert_eq!(BUFFER_CALLS_OUTSIDE.load(Ordering::Relaxed), outside);
    }

    unsafe extern "C" fn ten_frames_in(_: *const c_void) -> u32 {
        10
    }

    /// The wrapper's own clock reads: five per callback without a cycle clock
    /// and six with one, and never the thread CPU clock or the second
    /// cycle-start estimate unless the bench gauges are on.
    #[test]
    fn the_wrapper_reads_no_diagnostic_clock_without_the_gauges() {
        let _serial = serial();
        const CALLBACKS: u64 = 40;
        let context = context(4);
        // A callback the host deschedules for milliseconds records a gap or
        // an overrun event, which stamps itself: those reads are not the
        // wrapper's, and no other test records events while this one runs.
        let reads = |context: &'static Context| {
            let (before, events) = (clock_reads(), probe::events_recorded());
            for _ in 0..CALLBACKS {
                assert_eq!(run(context), 0);
            }
            let after = clock_reads();
            let events = probe::events_recorded() - events;
            [after[0] - before[0] - events, after[1] - before[1]]
        };
        // No client, so no cycle clock: start, timing entry, before and after
        // the original callback, exit.
        assert_eq!(reads(context), [5 * CALLBACKS, 0]);
        // With a cycle clock: one more read, for the cycle start.
        let clock = probe::CYCLE_CLOCK.swap(ten_frames_in as *const () as usize, Ordering::AcqRel);
        context.client.store(0x1234, Ordering::Relaxed);
        let drift = probe::ESTIMATE_DRIFT_MAX_NS.load(Ordering::Relaxed);
        assert_eq!(reads(context), [6 * CALLBACKS, 0]);
        assert_eq!(probe::ESTIMATE_DRIFT_MAX_NS.load(Ordering::Relaxed), drift);
        assert_eq!(context.timing.snapshot().callback_cpu.calls, 0);
        // Gauges on: the second estimate (one read per callback that ends in
        // the first half of its cycle, as all of these do), and four CPU
        // clock reads on the one sampled callback.
        let fresh = self::context(4);
        fresh.client.store(0x1234, Ordering::Relaxed);
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        let gauged = reads(fresh);
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        probe::CYCLE_CLOCK.store(clock, Ordering::Release);
        // A callback descheduled past half a period skips the second
        // estimate, so on a loaded host the count can fall short of seven.
        assert!(
            gauged[0] > 6 * CALLBACKS && gauged[0] <= 7 * CALLBACKS,
            "{gauged:?}"
        );
        assert_eq!(gauged[1], 4);
        assert_eq!(fresh.timing.snapshot().callback_cpu.calls, 1);
        assert_eq!(fresh.timing.snapshot().owned_cpu.calls, 1);
    }

    /// Every counter this change moved still reaches the report under its own
    /// key, and what only a bench run measures is null without the gauges.
    #[test]
    fn the_report_keeps_every_key_and_nulls_bench_only_fields_without_the_gauges() {
        let _serial = serial();
        let number = |value: &serde_json::Value| value.as_u64().expect("a number");
        // Marks: each counter gets its own amount, far above anything other
        // tests add meanwhile, so a swapped key shows as the wrong multiple.
        const MARK: u64 = 1 << 40;
        let marked = |after: &serde_json::Value, before: &serde_json::Value| {
            (number(after) - number(before)) / MARK
        };
        let before = build_report();
        CYCLE.callbacks.fetch_add(MARK, Ordering::Relaxed);
        CYCLE
            .buffer_calls_inside
            .fetch_add(2 * MARK, Ordering::Relaxed);
        CYCLE.gaps_short.fetch_add(3 * MARK, Ordering::Relaxed);
        CYCLE.gaps_long.fetch_add(4 * MARK, Ordering::Relaxed);
        for (bin, gap) in CYCLE.gaps.iter().enumerate() {
            gap.fetch_add((5 + bin as u64) * MARK, Ordering::Relaxed);
        }
        pipeline::HELPER_TIME
            .frames
            .fetch_add(12 * MARK, Ordering::Relaxed);
        pipeline::HELPER_TIME
            .total_ns
            .fetch_add(13 * MARK, Ordering::Relaxed);
        BUFFER_CALLS_OUTSIDE.fetch_add(14 * MARK, Ordering::Relaxed);
        pipeline::RESOLVE_NS.fetch_add(15 * MARK, Ordering::Relaxed);
        pipeline::PATH_COST_NS[3].fetch_add(16 * MARK, Ordering::Relaxed);
        let after = build_report();
        assert_eq!(marked(&after["callbacks"], &before["callbacks"]), 1);
        assert_eq!(marked(&after["buffersInside"], &before["buffersInside"]), 2);
        let (band, band_before) = (
            &after["callbackGapsOutsideBand"],
            &before["callbackGapsOutsideBand"],
        );
        assert_eq!(marked(&band["short"], &band_before["short"]), 3);
        assert_eq!(marked(&band["long"], &band_before["long"]), 4);
        let gaps = after["callbackGaps"].as_array().unwrap();
        assert_eq!(gaps.len(), 7);
        for (bin, gap) in gaps.iter().enumerate() {
            assert_eq!(marked(gap, &before["callbackGaps"][bin]), 5 + bin as u64);
        }
        assert_eq!(
            marked(&after["helper"]["frames"], &before["helper"]["frames"]),
            12
        );
        assert_eq!(
            marked(&after["helper"]["totalNs"], &before["helper"]["totalNs"]),
            13
        );
        for key in ["maxNs", "stalls", "late", "minMarginNs"] {
            assert!(after["helper"][key].is_u64(), "helper.{key}");
        }
        assert!(after["maxCallbackGapNs"].is_u64());
        for key in [
            "processed",
            "processedUnacknowledged",
            "rawHelperUnready",
            "rawNoHandoff",
            "rawNotDone",
            "rawInvalid",
            "unobserved",
            "rawReturns",
        ] {
            assert!(after["sources"][key].is_u64(), "sources.{key}");
        }
        for key in ["startPos", "endPos", "helperPos"] {
            assert_eq!(
                after["cycle"][key]["histogram"].as_array().unwrap().len(),
                8
            );
        }
        // Bench-only: present as keys, null without the gauges, and no gauges
        // section.
        let object = after.as_object().unwrap();
        for key in ["buffersOutside", "pathCostNs", "resolveNs"] {
            assert!(object.contains_key(key), "{key}");
            assert!(after[key].is_null(), "{key} without the gauges");
        }
        let cycle = after["cycle"].as_object().unwrap();
        assert!(cycle.contains_key("estimateDriftMaxNs"));
        assert!(cycle["estimateDriftMaxNs"].is_null());
        assert!(!object.contains_key("gauges"));
        assert_eq!(after["retiredCallbackTiming"]["cpuSampleEvery"], 0);
        assert_eq!(
            after["retiredCallbackTiming"]["callbackCpu"]["calls"], 0,
            "the wire format keeps its CPU fields"
        );
        // The launcher greps the first `"callbacks":` of the file for the
        // callback count: it must still be the top-level one.
        let text = serde_json::to_string(&after).unwrap();
        let first = text.find("\"callbacks\":").unwrap();
        let count: String = text[first + 12..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        assert_eq!(count.parse::<u64>().unwrap(), number(&after["callbacks"]));
        // With the gauges, the same keys carry the measurements.
        READ_MOSTLY.gauges.store(true, Ordering::Relaxed);
        let gauged = build_report();
        READ_MOSTLY.gauges.store(false, Ordering::Relaxed);
        assert_eq!(number(&gauged["buffersOutside"]) / MARK, 14);
        assert_eq!(number(&gauged["resolveNs"]) / MARK, 15);
        let costs = gauged["pathCostNs"].as_array().unwrap();
        assert_eq!(costs.len(), rc2fx_native_control::MAX_PATHS);
        assert_eq!(number(&costs[3]) / MARK, 16);
        assert_eq!(number(&costs[2]) / MARK, 0);
        assert!(gauged["cycle"]["estimateDriftMaxNs"].is_u64());
        assert_eq!(
            gauged["retiredCallbackTiming"]["cpuSampleEvery"],
            timing::CPU_SAMPLE_EVERY
        );
        let wake = &gauged["gauges"]["helperWake"];
        assert_eq!(wake["boundsUs"].as_array().unwrap().len(), 7);
        for key in ["wakeCall", "wakeLatency", "end", "place"] {
            assert_eq!(wake[key]["histogram"].as_array().unwrap().len(), 8, "{key}");
            for field in ["calls", "totalNs", "maxNs"] {
                assert!(wake[key][field].is_u64(), "{key}.{field}");
            }
        }
    }

    /// The statics the callback, the helper and every hooked call use are on
    /// lines of their own.
    #[test]
    fn hot_statics_do_not_share_cache_lines() {
        let line = |address: usize| address / 64;
        let read_mostly = &raw const READ_MOSTLY as usize;
        let cycle = &raw const CYCLE as usize;
        let outside = &raw const BUFFER_CALLS_OUTSIDE as usize;
        let admission = &raw const CONTROL_ADMISSION as usize;
        let helper = &raw const pipeline::HELPER_TIME as usize;
        let positions = [
            &raw const probe::START_POS as usize,
            &raw const probe::END_POS as usize,
            &raw const probe::HELPER_POS as usize,
        ];
        let mut lines = vec![
            line(read_mostly),
            line(cycle),
            line(cycle + 64),
            line(outside),
            line(admission),
            line(helper),
        ];
        for start in positions {
            assert_eq!(start % 64, 0);
            assert_eq!(std::mem::size_of::<probe::Positions>(), 128);
            lines.extend([line(start), line(start + 64)]);
        }
        for address in [read_mostly, cycle, outside, admission, helper] {
            assert_eq!(address % 64, 0);
        }
        let distinct: std::collections::HashSet<_> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len());
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn the_report_slows_only_after_startup_and_without_a_marker() {
        let writes = |range: std::ops::Range<u64>, diagnostic| {
            range.filter(|tick| report_due(*tick, diagnostic)).count()
        };
        assert_eq!(writes(1..REPORT_STARTUP_SECONDS, false), 179);
        assert_eq!(writes(180..300, true), 120);
        assert_eq!(writes(180..300, false), 40);
        // No gap comes near the bench's five-second telemetry limit.
        let due: Vec<_> = (170..300).filter(|tick| report_due(*tick, false)).collect();
        assert!(due.windows(2).all(|pair| pair[1] - pair[0] <= 3));
    }

    /// Counts the writes and bytes that reach it; with `fail_after`, writes
    /// at most that many bytes in total (a short write at the limit) and then
    /// fails.
    struct Counting<W> {
        inner: W,
        writes: usize,
        bytes: usize,
        fail_after: Option<usize>,
    }
    impl<W: Write> Counting<W> {
        fn new(inner: W, fail_after: Option<usize>) -> Self {
            Self {
                inner,
                writes: 0,
                bytes: 0,
                fail_after,
            }
        }
    }
    impl<W: Write> Write for Counting<W> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            let data = match self.fail_after {
                Some(limit) if self.bytes >= limit => {
                    return Err(io::Error::other("injected write failure"));
                }
                Some(limit) => &data[..data.len().min(limit - self.bytes)],
                None => data,
            };
            let written = self.inner.write(data)?;
            self.writes += 1;
            self.bytes += written;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    /// A report shaped like the mixer's (about 4.3 KB of nested objects,
    /// arrays, strings and numbers; the mixer's is larger), told apart by
    /// `writer` and `sequence`.
    fn report(writer: u64, sequence: u64) -> serde_json::Value {
        let events: Vec<_> = (0..64)
            .map(|index| {
                serde_json::json!({"kind": "helperBusy", "sequence": index, "atNs": 1_000_000 + index, "valueNs": 2 * index})
            })
            .collect();
        serde_json::json!({
            "writer": writer, "sequence": sequence, "probeOnly": true, "callbacks": 123_456_789,
            "workers": [{"id": "mic-1", "worker": {"completedFrames": 99, "cpu": {"histogram": [0, 0, 16_684, 594, 0]}}}],
            "events": events,
            "placement": {"samples": 3, "deferred": 0, "history": [{"cycle": 64, "callbackCpus": 1}]},
        })
    }
    /// What the unbuffered writer produced, byte for byte.
    fn old_bytes(report: &serde_json::Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(report).unwrap();
        bytes.push(b'\n');
        bytes
    }
    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rc2fx-report-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn the_report_is_byte_identical_in_a_few_writes() {
        let report = report(0, 0);
        let mut unbuffered = Counting::new(Vec::new(), None);
        serde_json::to_writer(&mut unbuffered, &report).unwrap();
        writeln!(unbuffered).unwrap();
        let buffered = write_json(Counting::new(Vec::new(), None), &report).unwrap();
        assert_eq!(buffered.inner, old_bytes(&report));
        assert_eq!(buffered.inner, unbuffered.inner);
        assert!(
            unbuffered.writes > 1000,
            "{} unbuffered writes",
            unbuffered.writes
        );
        assert!(buffered.writes <= 2, "{} buffered writes", buffered.writes);
    }

    /// Gaps outside 0.9 to 1.1 periods are counted in both directions; the
    /// event ring keeps only 1.5 periods and more, with the gap as its size.
    #[test]
    fn gaps_outside_the_period_band_are_counted_and_only_long_ones_keep_an_event() {
        let _events = probe::EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let period = timing::BUDGET_NS;
        let counts = |read: &AtomicU64| read.load(Ordering::Relaxed);
        let events =
            |report: &serde_json::Value| report["events"]["total"].as_u64().unwrap_or_default();
        let total = || events(&probe::snapshot());
        let before = (
            counts(&CYCLE.gaps_short),
            counts(&CYCLE.gaps_long),
            counts(&CYCLE.gaps[2]),
            total(),
        );
        // Inside the band: neither direction, and no event (the size bins
        // count every gap, so this one lands in the 0.9-1.1 bin).
        record_gap(period * 19 / 20);
        assert_eq!(
            (counts(&CYCLE.gaps_short), counts(&CYCLE.gaps_long), total()),
            (before.0, before.1, before.3)
        );
        assert_eq!(counts(&CYCLE.gaps[2]) - before.2, 1);
        // Early (0.5 period) and late (1.3 periods): counted, no event.
        record_gap(period / 2);
        record_gap(period * 13 / 10);
        assert_eq!(
            (
                counts(&CYCLE.gaps_short) - before.0,
                counts(&CYCLE.gaps_long) - before.1,
                total() - before.3
            ),
            (1, 1, 0)
        );
        // Two periods: counted and kept, with its size.
        record_gap(period * 2);
        assert_eq!(counts(&CYCLE.gaps_long) - before.1, 2);
        assert_eq!(total() - before.3, 1);
        let report = probe::snapshot();
        let recent = report["events"]["recent"].as_array().unwrap();
        let last = recent.last().unwrap();
        assert_eq!(last["kind"], "gap");
        assert_eq!(last["valueNs"], period * 2);
    }

    #[test]
    fn a_failed_flush_is_reported() {
        let report = report(0, 0);
        // Everything is buffered, so the only write is the final flush.
        assert!(write_json(Counting::new(Vec::new(), Some(0)), &report).is_err());
    }

    /// Publishes through the real seam with a writer that fails after
    /// `fail_after` bytes; the previous report must survive.
    fn publish_failing(name: &str, fail_after: usize) {
        let directory = scratch(name);
        let path = directory.join("audit.json");
        let previous = report(1, 1);
        publish_report(&publishing(), &path, &previous, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&previous));
        // A second handle on the partial file shows what reached it, even
        // after it is removed.
        let mut partial_file = None;
        let failed = publish_report(&publishing(), &path, &report(1, 2), |partial| {
            let file = std::fs::File::create(partial)?;
            partial_file = Some(file.try_clone()?);
            Ok(Counting::new(file, Some(fail_after)))
        });
        assert!(failed.is_err());
        let reached = partial_file.unwrap().metadata().unwrap().len();
        assert_eq!(reached, fail_after as u64, "bytes in the failed partial");
        assert!(old_bytes(&report(1, 2)).len() > fail_after);
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&previous));
        assert!(
            !path.with_extension("partial").exists(),
            "partial left behind"
        );
        let next = report(1, 3);
        publish_report(&publishing(), &path, &next, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), old_bytes(&next));
        assert!(!path.with_extension("partial").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_failure_while_flushing_keeps_the_previous_report() {
        publish_failing("flush", 0);
    }

    #[test]
    fn a_failure_part_way_keeps_the_previous_report() {
        publish_failing("partway", 4096);
    }

    /// As write_report: hold the publishing lock from before the report is
    /// built until it is published.
    fn write_like_write_report(path: &Path, writer: u64, sequence: u64) {
        let held = publishing();
        let report = report(writer, sequence);
        publish_report(&held, path, &report, |partial| {
            std::fs::File::create(partial)
        })
        .unwrap();
    }

    #[test]
    fn concurrent_publishers_only_ever_publish_whole_reports_in_order() {
        let directory = scratch("concurrent");
        let path = directory.join("audit.json");
        write_like_write_report(&path, 0, 0);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let (mut reads, mut published) = (0, 0);
                let mut latest = [0; 3];
                while !done.load(Ordering::Acquire) {
                    let bytes = std::fs::read(&path).unwrap();
                    let report: serde_json::Value = serde_json::from_slice(&bytes)
                        .unwrap_or_else(|error| panic!("torn report after {reads} reads: {error}"));
                    let expected = super::report_tests::report(
                        report["writer"].as_u64().unwrap(),
                        report["sequence"].as_u64().unwrap(),
                    );
                    assert_eq!(bytes, old_bytes(&expected));
                    // Never an older report of a writer after a newer one.
                    let (writer, sequence) = (
                        report["writer"].as_u64().unwrap() as usize,
                        report["sequence"].as_u64().unwrap(),
                    );
                    assert!(sequence >= latest[writer], "writer {writer} went back");
                    latest[writer] = sequence;
                    reads += 1;
                    published += usize::from(writer != 0);
                }
                (reads, published)
            });
            let publishers: Vec<_> = (1..=2)
                .map(|writer| {
                    let path = &path;
                    scope.spawn(move || {
                        for sequence in 1..=150 {
                            write_like_write_report(path, writer, sequence);
                        }
                    })
                })
                .collect();
            for publisher in publishers {
                publisher.join().unwrap();
            }
            done.store(true, Ordering::Release);
            let (reads, published) = reader.join().unwrap();
            assert!(
                reads > 0 && published > 0,
                "{reads} reads, {published} published"
            );
        });
        // The last report published is whole: one writer's final sequence.
        let last: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(last["sequence"], 150);
        assert!(matches!(last["writer"].as_u64(), Some(1 | 2)));
        assert!(
            !path.with_extension("partial").exists(),
            "partial left behind"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
