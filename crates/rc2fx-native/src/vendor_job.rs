//! Profile-validated capture-values-v1 repair for Mixer::Process's queued lambda.
//!
//! That lambda captures three returning-stack locals by reference. Copy their
//! values at enqueue, using the vendor's existing 32-byte lambda allocations.
//! The invoker reconstructs references only for its synchronous body call. No
//! Rust allocation, extra worker, audio queue, routing or scheduling is added.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use rc2fx_firmware::Profile;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct AnyData {
    words: [usize; 2],
}
type Manager = unsafe extern "C-unwind" fn(*mut AnyData, *const AnyData, i32) -> bool;
#[repr(C)]
struct Function {
    data: AnyData,
    manager: usize,
    invoker: usize,
}
type Enqueue = unsafe extern "C-unwind" fn(*mut c_void, *const Function);
type Body = unsafe extern "C-unwind" fn(*const Capture);
#[repr(C)]
#[derive(Clone, Copy)]
struct Capture {
    mixer: usize,
    outputs: usize,
    frames: usize,
    flag: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct ValueCapture {
    mixer: usize,
    output_table: usize,
    frame_count: u32,
    flag_value: u8,
    /// Bench gauges only, in the padding before `queued_in`: microseconds from
    /// the cycle start to the queuing callback's start. 0 means unknown.
    start_position_us: u16,
    /// CLOCK_MONOTONIC start of the callback that queued this job.
    queued_in: u64,
}
// The vendor's job allocations hold exactly 32 bytes.
const _: () = assert!(std::mem::size_of::<ValueCapture>() == 32);
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static MANAGER: AtomicUsize = AtomicUsize::new(0);
static INVOKER: AtomicUsize = AtomicUsize::new(0);
static BODY: AtomicUsize = AtomicUsize::new(0);
static FIXED: AtomicU64 = AtomicU64::new(0);
static FORWARDED: AtomicU64 = AtomicU64::new(0);
static CLONED: AtomicU64 = AtomicU64::new(0);
static INVOKED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static MAX_LIVE: AtomicU64 = AtomicU64::new(0);

/// The callback context that queues jobs (RØDE's mixer client).
static OWNER: AtomicUsize = AtomicUsize::new(0);
/// Job timing against the 112-frame period: time from the queuing callback's
/// start to the job's end, and jobs still running when that context's next
/// callback had started.
struct JobTiming {
    timed: AtomicU64,
    bins: [AtomicU64; 8],
    max_end: AtomicU64,
    max_run: AtomicU64,
    late: AtomicU64,
    max_late: AtomicU64,
}
static TIMING: JobTiming = JobTiming {
    timed: AtomicU64::new(0),
    bins: [const { AtomicU64::new(0) }; 8],
    max_end: AtomicU64::new(0),
    max_run: AtomicU64::new(0),
    late: AtomicU64::new(0),
    max_late: AtomicU64::new(0),
};
const PERIOD_NS: u64 = 112 * 1_000_000_000 / 48_000;
impl JobTiming {
    fn record(&self, queued_in: u64, started: u64, ended: u64, next: u64) {
        if queued_in == 0 || started < queued_in || ended < started {
            return;
        }
        let end = ended - queued_in;
        let bounds = [
            PERIOD_NS / 8,
            PERIOD_NS / 4,
            PERIOD_NS / 2,
            PERIOD_NS * 3 / 4,
            PERIOD_NS,
            PERIOD_NS * 3 / 2,
            PERIOD_NS * 2,
        ];
        let bin = bounds.iter().position(|bound| end <= *bound).unwrap_or(7);
        self.bins[bin].fetch_add(1, Ordering::Relaxed);
        self.max_end.fetch_max(end, Ordering::Relaxed);
        self.max_run.fetch_max(ended - started, Ordering::Relaxed);
        if next > queued_in && ended > next {
            self.late.fetch_add(1, Ordering::Relaxed);
            self.max_late.fetch_max(ended - next, Ordering::Relaxed);
            crate::probe::record(crate::probe::Kind::VendorJobLate, ended - next);
        }
        self.timed.fetch_add(1, Ordering::Relaxed);
    }
    fn status(&self) -> serde_json::Value {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        serde_json::json!({
            "periodNs": PERIOD_NS,
            "timed": read(&self.timed),
            "endHistogram": self.bins.iter().map(read).collect::<Vec<_>>(),
            "maxEndNs": read(&self.max_end),
            "maxRunNs": read(&self.max_run),
            "lateJobs": read(&self.late),
            "maxLateNs": read(&self.max_late),
        })
    }
}

/// Bench gauge: where the job ended in the cycle that queued it, and how many
/// ended after jackd's read of that cycle's output.
static END_POSITION: crate::probe::Positions = crate::probe::Positions::new();
static PAST_READ: AtomicU64 = AtomicU64::new(0);
/// jackd wakes about this long before the next estimated cycle start.
const JACKD_READ_LEAD_NS: u64 = 110_000;

fn start_position_us(started: u64, cycle_start: u64) -> u16 {
    if cycle_start == 0 || started < cycle_start {
        return 0;
    }
    ((started - cycle_start) / 1_000).clamp(1, u64::from(u16::MAX)) as u16
}
/// Where the job ended relative to its own cycle's start, when that is known.
fn end_position(queued_in: u64, start_position_us: u16, ended: u64) -> Option<u64> {
    let cycle_start = queued_in.checked_sub(u64::from(start_position_us) * 1_000)?;
    (start_position_us != 0 && queued_in != 0).then(|| ended.saturating_sub(cycle_start))
}
fn record_end_position(position: u64, positions: &crate::probe::Positions, past_read: &AtomicU64) {
    positions.record(position);
    if position > PERIOD_NS - JACKD_READ_LEAD_NS {
        past_read.fetch_add(1, Ordering::Relaxed);
    }
}
pub fn gauge_status() -> serde_json::Value {
    serde_json::json!({
        "endPosition": END_POSITION.snapshot(),
        "pastJackdRead": PAST_READ.load(Ordering::Relaxed),
        "jackdReadLeadNs": JACKD_READ_LEAD_NS,
    })
}

impl ValueCapture {
    /// The original references are live here, before Mixer::Process returns.
    unsafe fn copy(original: Capture) -> Self {
        let (queued_in, start_position_us) =
            super::callback_started().map_or((0, 0), |(owner, started)| {
                OWNER.store(owner, Ordering::Relaxed);
                let position = if crate::gauges::enabled() {
                    start_position_us(started, super::context_cycle_start(owner))
                } else {
                    0
                };
                (started, position)
            });
        Self {
            mixer: original.mixer,
            output_table: unsafe { *(original.outputs as *const usize) },
            frame_count: unsafe { *(original.frames as *const u32) },
            flag_value: unsafe { *(original.flag as *const u8) },
            start_position_us,
            queued_in,
        }
    }
    fn borrowed(&self) -> Capture {
        Capture {
            mixer: self.mixer,
            outputs: std::ptr::addr_of!(self.output_table) as usize,
            frames: std::ptr::addr_of!(self.frame_count) as usize,
            flag: std::ptr::addr_of!(self.flag_value) as usize,
        }
    }
}

// The pinned libstdc++ manager has a 32-bit operation enum; op2 copies exactly
// 32 raw bytes and op3 deletes that allocation. Only the FunctionJob's private
// copy/invoke/destroy path receives this replacement; target<T> is not used.
unsafe fn managed(
    original: Manager,
    dst: *mut AnyData,
    src: *const AnyData,
    operation: i32,
) -> bool {
    let result = unsafe { original(dst, src, operation) };
    match operation {
        2 => {
            CLONED.fetch_add(1, Ordering::Relaxed);
            let live = LIVE.fetch_add(1, Ordering::Relaxed) + 1;
            MAX_LIVE.fetch_max(live, Ordering::Relaxed);
        }
        3 => {
            LIVE.fetch_sub(1, Ordering::Relaxed);
        }
        _ => {}
    }
    result
}
unsafe extern "C-unwind" fn manage(dst: *mut AnyData, src: *const AnyData, operation: i32) -> bool {
    let original: Manager = unsafe { std::mem::transmute(MANAGER.load(Ordering::Acquire)) };
    unsafe { managed(original, dst, src, operation) }
}
unsafe fn invoke_body(data: *const AnyData, body: Body) {
    let values = unsafe { &*((*data).words[0] as *const ValueCapture) };
    let capture = values.borrowed();
    let started = crate::timing::monotonic();
    // These reconstructed references point into the job's owned allocation,
    // which remains alive through this complete synchronous body invocation.
    unsafe {
        body(&capture);
    }
    let ended = crate::timing::monotonic();
    let owner = OWNER.load(Ordering::Relaxed);
    let next = if owner == 0 {
        0
    } else {
        super::context_started(owner)
    };
    TIMING.record(values.queued_in, started, ended, next);
    if let Some(position) = end_position(values.queued_in, values.start_position_us, ended) {
        record_end_position(position, &END_POSITION, &PAST_READ);
    }
}
unsafe extern "C-unwind" fn invoke(data: *const AnyData) {
    let body: Body = unsafe { std::mem::transmute(BODY.load(Ordering::Acquire)) };
    INVOKED.fetch_add(1, Ordering::Relaxed);
    unsafe {
        invoke_body(data, body);
    }
}
unsafe extern "C-unwind" fn enqueue(pool: *mut c_void, function: *const Function) {
    let original: Enqueue = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let borrowed = unsafe { &*function };
    if borrowed.manager != MANAGER.load(Ordering::Acquire)
        || borrowed.invoker != INVOKER.load(Ordering::Acquire)
    {
        FORWARDED.fetch_add(1, Ordering::Relaxed);
        unsafe {
            original(pool, function);
        }
        return;
    }
    let captured = unsafe { *(borrowed.data.words[0] as *const Capture) };
    let values = unsafe { ValueCapture::copy(captured) };
    let replacement = Function {
        data: AnyData {
            words: [std::ptr::addr_of!(values) as usize, 0],
        },
        manager: manage as *const () as usize,
        invoker: invoke as *const () as usize,
    };
    FIXED.fetch_add(1, Ordering::Relaxed);
    // Enqueue copies its source, then owns/destroys only those heap copies. This
    // borrowed stack Function has no Drop and must never run manager op3 itself.
    unsafe {
        original(pool, &replacement);
    }
}

pub fn status() -> serde_json::Value {
    serde_json::json!({
        "installed": ORIGINAL.load(Ordering::Acquire) != 0,
        "fixedJobs": FIXED.load(Ordering::Relaxed),
        "forwardedJobs": FORWARDED.load(Ordering::Relaxed),
        "captureClones": CLONED.load(Ordering::Relaxed),
        "invokedJobs": INVOKED.load(Ordering::Relaxed),
        "liveCaptures": LIVE.load(Ordering::Relaxed),
        "maxLiveCaptures": MAX_LIVE.load(Ordering::Relaxed),
        "timing": TIMING.status(),
    })
}
fn branch(destination: usize) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..4].copy_from_slice(&0x58000050u32.to_le_bytes()); // ldr x16, PC+8
    bytes[4..8].copy_from_slice(&0xd61f0200u32.to_le_bytes()); // br x16
    bytes[8..].copy_from_slice(&(destination as u64).to_le_bytes());
    bytes
}
fn pristine(bytes: &[u8], expected: &[u8; 16]) -> bool {
    bytes == expected
}

#[cfg(target_arch = "aarch64")]
unsafe fn clear_cache(start: *mut u8, length: usize) {
    use std::arch::asm;
    let ctr: usize;
    unsafe {
        asm!("mrs {value}, ctr_el0", value = out(reg) ctr, options(nostack, preserves_flags));
    }
    let data_line = 4usize << ((ctr >> 16) & 15);
    let instruction_line = 4usize << (ctr & 15);
    let end = start as usize + length;
    let mut address = start as usize & !(data_line - 1);
    while address < end {
        unsafe {
            asm!("dc cvau, {address}", address = in(reg) address, options(nostack, preserves_flags));
        }
        address += data_line;
    }
    unsafe {
        asm!("dsb ish", options(nostack, preserves_flags));
    }
    address = start as usize & !(instruction_line - 1);
    while address < end {
        unsafe {
            asm!("ic ivau, {address}", address = in(reg) address, options(nostack, preserves_flags));
        }
        address += instruction_line;
    }
    unsafe {
        asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

/// Only called from la_preinit after the opt-in and exact callback selector.
/// All validation precedes mutation. No file-backed executable bytes are written
/// to disk; MAP_PRIVATE text is changed in this one process before threads exist.
#[cfg(target_arch = "aarch64")]
pub fn install(base: usize, profile: &Profile) -> Result<(), String> {
    if ORIGINAL.load(Ordering::Acquire) != 0 {
        return Err("job lifetime repair already installed".into());
    }
    let bytes = std::fs::read("/proc/self/exe").map_err(|e| e.to_string())?;
    profile.verify_image(&bytes)?;
    let runtime = profile
        .runtime
        .as_ref()
        .ok_or("unqualified firmware profile")?;
    let tasks = std::fs::read_dir("/proc/self/task")
        .map_err(|e| e.to_string())?
        .count();
    if tasks != 1 {
        return Err("job lifetime repair must be installed before native threads".into());
    }
    for site in runtime.sites() {
        let address = base
            .checked_add(site.offset()?)
            .ok_or("job repair address overflow")?;
        let original = unsafe { std::slice::from_raw_parts(address as *const u8, 16) };
        if !pristine(original, &site.bytes()?) {
            return Err(format!(
                "in-memory firmware instructions differ at {}",
                site.offset
            ));
        }
    }
    // These extra addresses identify the one closure's manager, invoker, body,
    // and RTTI in the pinned image. The SHA guards their complete definitions.
    MANAGER.store(
        base + runtime.job_lifetime.manager.offset()?,
        Ordering::Release,
    );
    INVOKER.store(
        base + runtime.job_lifetime.invoker.offset()?,
        Ordering::Release,
    );
    BODY.store(
        base + runtime.job_lifetime.body.offset()?,
        Ordering::Release,
    );
    let site = (base + runtime.job_lifetime.enqueue.offset()?) as *mut u8;
    unsafe {
        install_detour(site, enqueue as *const () as usize, &ORIGINAL)?;
    }
    Ok(())
}
#[cfg(not(target_arch = "aarch64"))]
pub fn install(_: usize, _: &Profile) -> Result<(), String> {
    Err("job lifetime repair is supported only on the verified ARM64 image".into())
}

#[cfg(target_arch = "aarch64")]
pub(crate) unsafe fn install_detour(
    site: *mut u8,
    destination: usize,
    original: &AtomicUsize,
) -> Result<(), String> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 || !(page_size as usize).is_power_of_two() {
        return Err("invalid page size".into());
    }
    let page_size = page_size as usize;
    let page = (site as usize & !(page_size - 1)) as *mut c_void;
    if (site as usize & (page_size - 1)) + 16 > page_size {
        return Err("job repair spans code pages".into());
    }
    let trampoline = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if trampoline == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let trampoline = trampoline.cast::<u8>();
    unsafe {
        std::ptr::copy_nonoverlapping(site, trampoline, 16);
        std::ptr::copy_nonoverlapping(branch(site as usize + 16).as_ptr(), trampoline.add(16), 16);
        clear_cache(trampoline, 32);
    }
    if unsafe {
        libc::mprotect(
            trampoline.cast(),
            page_size,
            libc::PROT_READ | libc::PROT_EXEC,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error().to_string();
        unsafe {
            libc::munmap(trampoline.cast(), page_size);
        }
        return Err(error);
    }
    if unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_WRITE) } != 0 {
        let error = std::io::Error::last_os_error().to_string();
        unsafe {
            libc::munmap(trampoline.cast(), page_size);
        }
        return Err(error);
    }
    original.store(trampoline as usize, Ordering::Release);
    unsafe {
        std::ptr::copy_nonoverlapping(branch(destination).as_ptr(), site, 16);
        clear_cache(site, 16);
    }
    if unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
        // A failed final protection must not leave a half-installed detour.
        unsafe {
            std::ptr::copy_nonoverlapping(trampoline, site, 16);
            clear_cache(site, 16);
        }
        if unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC) } != 0 {
            std::process::abort();
        }
        original.store(0, Ordering::Release);
        unsafe {
            libc::munmap(trampoline.cast(), page_size);
        }
        return Err("job repair final protection failed; original restored".into());
    }
    // This mapping intentionally lives until process exit. A queued vendor job
    // may retain our invoker/manager; it can never outlive the loaded audit owner.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prologue() -> [u32; 4] {
        let catalog = rc2fx_firmware::Catalog::parse(rc2fx_firmware::DEFAULT_CATALOG).unwrap();
        let bytes = catalog.profiles[0]
            .runtime
            .as_ref()
            .unwrap()
            .job_lifetime
            .enqueue
            .bytes()
            .unwrap();
        std::array::from_fn(|index| {
            u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
        })
    }
    unsafe extern "C-unwind" fn manager_fixture(
        dst: *mut AnyData,
        src: *const AnyData,
        op: i32,
    ) -> bool {
        match op {
            2 => {
                let value = unsafe { *((*src).words[0] as *const ValueCapture) };
                unsafe {
                    (*dst).words[0] = Box::into_raw(Box::new(value)) as usize;
                }
            }
            3 => unsafe {
                drop(Box::from_raw((*dst).words[0] as *mut ValueCapture));
            },
            _ => panic!("fixture manager operation"),
        }
        false
    }
    #[derive(Default)]
    struct Observed {
        output_table: usize,
        frames: i32,
        flag: u8,
    }
    unsafe extern "C-unwind" fn body_fixture(capture: *const Capture) {
        let capture = unsafe { &*capture };
        let observed = unsafe { &mut *(capture.mixer as *mut Observed) };
        observed.output_table = unsafe { *(capture.outputs as *const usize) };
        observed.frames = unsafe { *(capture.frames as *const i32) };
        observed.flag = unsafe { *(capture.flag as *const u8) };
    }
    #[test]
    fn vendor_copy_manager_retains_values_after_returning_stack_is_overwritten() {
        let mut observed = Observed::default();
        let mut output_table = 0x1234usize;
        let mut frames = 112i32;
        let mut flag = 1u8;
        let capture = Capture {
            mixer: std::ptr::addr_of_mut!(observed) as usize,
            outputs: std::ptr::addr_of!(output_table) as usize,
            frames: std::ptr::addr_of!(frames) as usize,
            flag: std::ptr::addr_of!(flag) as usize,
        };
        let mut first = AnyData::default();
        let mut job = AnyData::default();
        {
            let values = unsafe { ValueCapture::copy(capture) };
            let source = AnyData {
                words: [std::ptr::addr_of!(values) as usize, 0],
            };
            // Pinned enqueuer performs these exact two manager clones, followed
            // by destroying its first copy. The final job owns the second.
            unsafe {
                managed(manager_fixture, &mut first, &source, 2);
                managed(manager_fixture, &mut job, &first, 2);
                managed(manager_fixture, &mut first, std::ptr::null(), 3);
            }
        }
        output_table = 0xffff88010560usize;
        frames = -1_073_748_465;
        flag = 0;
        std::hint::black_box((output_table, frames, flag));
        unsafe {
            invoke_body(&job, body_fixture);
            managed(manager_fixture, &mut job, std::ptr::null(), 3);
        }
        assert_eq!(observed.output_table, 0x1234);
        assert_eq!(observed.frames, 112);
        assert_eq!(observed.flag, 1);
        assert_eq!(LIVE.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn the_gauge_position_fits_the_padding_and_finds_the_jobs_own_cycle() {
        assert_eq!(std::mem::offset_of!(ValueCapture, start_position_us), 22);
        assert_eq!(std::mem::offset_of!(ValueCapture, queued_in), 24);
        assert_eq!(start_position_us(1_500_000, 1_000_000), 500);
        assert_eq!(start_position_us(1_000_000, 1_000_000), 1);
        assert_eq!(start_position_us(1_000_000, 0), 0);
        assert_eq!(start_position_us(1_000_000, 2_000_000), 0);
        assert_eq!(start_position_us(u64::MAX, 1), u16::MAX);
        assert_eq!(end_position(1_500_000, 500, 3_000_000), Some(2_000_000));
        assert_eq!(end_position(1_500_000, 0, 3_000_000), None);
        assert_eq!(end_position(0, 500, 3_000_000), None);
    }
    #[test]
    fn only_a_job_past_jackds_read_counts_as_past_it() {
        let positions = crate::probe::Positions::new();
        let past = AtomicU64::new(0);
        record_end_position(PERIOD_NS - JACKD_READ_LEAD_NS, &positions, &past);
        assert_eq!(past.load(Ordering::Relaxed), 0);
        record_end_position(PERIOD_NS - JACKD_READ_LEAD_NS + 1, &positions, &past);
        assert_eq!(past.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn absolute_branch_and_pristine_prologue_are_exact() {
        let mut expected = Vec::new();
        for instruction in prologue() {
            expected.extend(instruction.to_le_bytes());
        }
        let original: [u8; 16] = expected.as_slice().try_into().unwrap();
        assert!(pristine(&expected, &original));
        expected[3] ^= 1;
        assert!(!pristine(&expected, &original));
        let branch = branch(0x123456789abcdef0);
        assert_eq!(&branch[..8], &[0x50, 0, 0, 0x58, 0, 2, 0x1f, 0xd6]);
        assert_eq!(
            u64::from_le_bytes(branch[8..].try_into().unwrap()),
            0x123456789abcdef0
        );
        assert_eq!(std::mem::size_of::<Function>(), 32);
        assert_eq!(std::mem::size_of::<Capture>(), 32);
        assert_eq!(std::mem::size_of::<ValueCapture>(), 32);
        assert_eq!(std::mem::offset_of!(Function, manager), 16);
        assert_eq!(std::mem::offset_of!(Function, invoker), 24);
    }
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn arm_detour_replays_exact_vendor_prologue_and_forwards_unrelated_job() {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(page, libc::MAP_FAILED);
        let code = page.cast::<u32>();
        let prologue = prologue();
        let instructions = [
            prologue[0],
            prologue[1],
            prologue[2],
            prologue[3],
            0x528009a2, // mov w2,#77
            0xb9000002, // str w2,[x0]
            0xa9425bf5, // ldp x21,x22,[sp,#32]
            0xa8c87bfd, // ldp x29,x30,[sp],#128
            0xd50323bf, // autiasp
            0xd65f03c0, // ret
        ];
        unsafe {
            std::ptr::copy_nonoverlapping(instructions.as_ptr(), code, instructions.len());
            clear_cache(code.cast(), instructions.len() * 4);
        }
        assert_eq!(
            unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        let original_function: Enqueue = unsafe { std::mem::transmute(page) };
        // Empty manager/invoker are valid foreign std::function representations.
        // Use a distinct nonmatching integer to exercise unrelated forwarding.
        let previous_manager = MANAGER.swap(usize::MAX, Ordering::AcqRel);
        let empty = Function {
            data: AnyData::default(),
            manager: 0,
            invoker: 0,
        };
        let mut result = 0u32;
        unsafe {
            original_function(std::ptr::addr_of_mut!(result).cast(), &empty);
        }
        assert_eq!(result, 77);
        unsafe {
            install_detour(page.cast(), enqueue as *const () as usize, &ORIGINAL).unwrap();
        }
        result = 0;
        unsafe {
            original_function(std::ptr::addr_of_mut!(result).cast(), &empty);
        }
        assert_eq!(
            result, 77,
            "detour, Rust forwarding and replay trampoline execute correctly"
        );
        let trampoline = ORIGINAL.swap(0, Ordering::AcqRel) as *mut c_void;
        assert_eq!(
            unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_WRITE) },
            0
        );
        unsafe {
            std::ptr::copy_nonoverlapping(instructions.as_ptr(), code, 4);
            clear_cache(code.cast(), 16);
        }
        assert_eq!(
            unsafe { libc::mprotect(page, page_size, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        result = 0;
        unsafe {
            original_function(std::ptr::addr_of_mut!(result).cast(), &empty);
        }
        assert_eq!(result, 77);
        assert_eq!(unsafe { libc::munmap(trampoline, page_size) }, 0);
        assert_eq!(unsafe { libc::munmap(page, page_size) }, 0);
        MANAGER.store(previous_manager, Ordering::Release);
    }
    #[test]
    fn pinned_image_sha_and_prologue_match_retained_vendor() {
        let Ok(path) = std::env::var("RC2FX_VENDOR_JOB_IMAGE") else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let catalog = rc2fx_firmware::Catalog::parse(rc2fx_firmware::DEFAULT_CATALOG).unwrap();
        catalog
            .select_for_model(rc2fx_firmware::Model::ProIi, &bytes)
            .unwrap();
    }
}
