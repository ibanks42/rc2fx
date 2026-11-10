//! `src_process` for the vendor's USB bridges, through the bit-exact mono path
//! in `src_fast_core`. Anything else runs the real library.
//!
//! `RC2FX_SRC_MODE` picks the mode at audio start. `fast` is what the launcher
//! passes by default. `verify` keeps the real library's audio and compares the
//! fast path with it on every call, for bench runs. `stock`, an unset value or
//! a library whose code differs from the verified build leave the symbol
//! unbound, so the mixer calls libsamplerate directly.
#[path = "src_fast_core.rs"]
pub mod core;

use self::core::{Data, MAX_FRAMES, Plan, State};
use std::{
    cell::{Cell, UnsafeCell},
    ffi::{c_int, c_void},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

const STOCK: u8 = 0;
const FAST: u8 = 1;
const VERIFY: u8 = 2;
/// A thread holds a slot for life, and a slot is never handed to a second
/// thread: two threads writing one plan would produce wrong samples. A USB
/// mode change starts new client threads, so the table is sized for many of
/// them, as `client_timing`'s is. A slot's pages are touched only once a
/// thread claims it. A thread that finds the table full runs the real library
/// from then on, which is stock behaviour, and is counted once in `noSlot`.
const SLOTS: usize = 128;
/// This thread has not asked for a slot yet.
const UNCLAIMED: usize = usize::MAX;
/// The table was full when this thread asked. It does not ask again, so it
/// touches no shared counter on any later call.
const REFUSED: usize = usize::MAX - 1;

type SrcProcess = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;

static MODE: AtomicU8 = AtomicU8::new(STOCK);
static VERIFIED: AtomicBool = AtomicBool::new(false);
static OPENED: AtomicBool = AtomicBool::new(false);
static REAL: AtomicUsize = AtomicUsize::new(0);
static LINEAR_VT: AtomicUsize = AtomicUsize::new(0);
static LINEAR_VARI: AtomicUsize = AtomicUsize::new(0);
/// Slots claimed, at most `SLOTS`, and the threads that found none left. Both
/// are written only when a thread makes its first call.
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);
static NO_SLOT: AtomicU64 = AtomicU64::new(0);

/// One thread's plan and counters. Only that thread writes them; reports read
/// the counters. Aligned to a cache line, so that the counters one thread
/// writes on every call share no line with the next thread's plan.
#[repr(align(64))]
struct Slot {
    plan: UnsafeCell<Plan>,
    scratch: UnsafeCell<[f32; MAX_FRAMES]>,
    calls: AtomicU64,
    builds: AtomicU64,
    declined: AtomicU64,
    compared: AtomicU64,
    mismatches: AtomicU64,
}
// Each slot is claimed by one thread; see `slot`.
unsafe impl Sync for Slot {}

static SLOT_TABLE: [Slot; SLOTS] = [const {
    Slot {
        plan: UnsafeCell::new(Plan::new()),
        scratch: UnsafeCell::new([0.0; MAX_FRAMES]),
        calls: AtomicU64::new(0),
        builds: AtomicU64::new(0),
        declined: AtomicU64::new(0),
        compared: AtomicU64::new(0),
        mismatches: AtomicU64::new(0),
    }
}; SLOTS];

thread_local! {
    static CURRENT: Cell<usize> = const { Cell::new(UNCLAIMED) };
}

fn bump(counter: &AtomicU64) {
    counter.store(counter.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

fn parse_mode(value: &[u8]) -> u8 {
    match value {
        b"fast" => FAST,
        b"verify" => VERIFY,
        _ => STOCK,
    }
}

/// Read once, before the first object is opened.
pub fn initialize() {
    let value = unsafe { libc::getenv(c"RC2FX_SRC_MODE".as_ptr()) };
    let mode = if value.is_null() {
        STOCK
    } else {
        parse_mode(unsafe { std::ffi::CStr::from_ptr(value) }.to_bytes())
    };
    MODE.store(mode, Ordering::Relaxed);
}

/// Whether this object must be audited for the resampler.
pub fn object(basename: &[u8]) -> bool {
    MODE.load(Ordering::Relaxed) != STOCK && basename.starts_with(b"libsamplerate.so")
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// Whether the file at `path` is the verified library. Reads the file, not the
/// mapping: another library may be too small to read at these offsets.
fn verified_file(path: &std::ffi::CStr) -> bool {
    let mut text = [0u8; core::TEXT_SIZE];
    unsafe {
        let file = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if file < 0 {
            return false;
        }
        let mut status = std::mem::zeroed::<libc::stat>();
        let read = libc::fstat(file, &mut status) == 0
            && status.st_size == core::FILE_SIZE
            && libc::pread(
                file,
                text.as_mut_ptr().cast(),
                text.len(),
                core::TEXT_OFFSET as libc::off_t,
            ) == text.len() as isize;
        libc::close(file);
        read && fnv(&text) == core::TEXT_FNV
    }
}

/// Checks the library's code, which fixes every layout `core` relies on.
pub fn opened(base: usize, path: &std::ffi::CStr) {
    OPENED.store(true, Ordering::Relaxed);
    if verified_file(path) {
        LINEAR_VT.store(base + core::LINEAR_VT_OFFSET, Ordering::Release);
        LINEAR_VARI.store(base + core::LINEAR_VARI_OFFSET, Ordering::Release);
        VERIFIED.store(true, Ordering::Release);
    }
}

/// The wrapper to bind in place of the real `src_process`, once verified.
pub fn replacement(address: usize) -> Option<usize> {
    if !VERIFIED.load(Ordering::Acquire) || !super::remember(&REAL, address) {
        return None;
    }
    Some(src_process as *const () as usize)
}

/// Claims the next of `slots` slots, or counts a refusal: called once per
/// thread. `next` never passes `slots`, so it is also the number claimed.
fn claim(next: &AtomicUsize, refused: &AtomicU64, slots: usize) -> usize {
    let mut index = next.load(Ordering::Relaxed);
    loop {
        if index >= slots {
            refused.fetch_add(1, Ordering::Relaxed);
            return REFUSED;
        }
        match next.compare_exchange_weak(index, index + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return index,
            Err(actual) => index = actual,
        }
    }
}

/// What `current` (a thread's own cell) holds after its first use: its slot
/// index, or `REFUSED`. Only the first use touches `next` or `refused`.
fn slot_index(
    current: &Cell<usize>,
    next: &AtomicUsize,
    refused: &AtomicU64,
    slots: usize,
) -> usize {
    if current.get() == UNCLAIMED {
        current.set(claim(next, refused, slots));
    }
    current.get()
}

/// This thread's slot, claiming one on first use. None once refused.
fn slot() -> Option<&'static Slot> {
    let index = CURRENT
        .try_with(|current| slot_index(current, &NEXT_SLOT, &NO_SLOT, SLOTS))
        .ok()?;
    SLOT_TABLE.get(index)
}

unsafe extern "C" fn src_process(state: *mut c_void, data: *mut c_void) -> c_int {
    let real: SrcProcess = unsafe { std::mem::transmute(REAL.load(Ordering::Acquire)) };
    let Some(slot) = slot() else {
        return unsafe { real(state, data) };
    };
    let plan = unsafe { &mut *slot.plan.get() };
    let (fast_state, fast_data) = (state as *mut State, data as *mut Data);
    bump(&slot.calls);
    let current = unsafe {
        core::prepare(
            plan,
            fast_state,
            fast_data,
            LINEAR_VT.load(Ordering::Relaxed),
            LINEAR_VARI.load(Ordering::Relaxed),
        )
    };
    slot.builds.store(plan.builds(), Ordering::Relaxed);
    if !current {
        bump(&slot.declined);
        return unsafe { real(state, data) };
    }
    if MODE.load(Ordering::Relaxed) == VERIFY {
        return unsafe { verify(slot, plan, real, fast_state, fast_data) };
    }
    unsafe {
        core::interpolate(
            plan,
            fast_state,
            (*fast_data).data_in,
            (*fast_data).data_out,
        );
        core::commit(plan, fast_state, fast_data);
    }
    0
}

/// Runs the real library, whose audio the mixer keeps, and compares.
unsafe fn verify(
    slot: &Slot,
    plan: &Plan,
    real: SrcProcess,
    state: *mut State,
    data: *mut Data,
) -> c_int {
    let scratch = unsafe { &mut *slot.scratch.get() };
    unsafe {
        let last_value = *(*(*state).private_data).last_value;
        core::interpolate(plan, state, (*data).data_in, scratch.as_mut_ptr());
        let status = real(state as *mut c_void, data as *mut c_void);
        let expected_last = if plan.in_used() > 0 {
            *(*data).data_in.add(plan.in_used() as usize - 1)
        } else {
            last_value
        };
        let frames = plan.count();
        let same = status == 0
            && (*data).output_frames_gen == frames as std::ffi::c_long
            && (*data).input_frames_used == plan.in_used()
            && (*state).last_position.to_bits() == plan.last_position().to_bits()
            && (*state).last_ratio.to_bits() == plan.last_ratio().to_bits()
            && (*(*(*state).private_data).last_value).to_bits() == expected_last.to_bits()
            && (0..frames).all(|i| (*(*data).data_out.add(i)).to_bits() == scratch[i].to_bits());
        bump(&slot.compared);
        if !same {
            bump(&slot.mismatches);
        }
        status
    }
}

pub fn snapshot() -> serde_json::Value {
    // Claimed slots only: an unclaimed slot's pages stay untouched.
    let threads = NEXT_SLOT.load(Ordering::Relaxed).min(SLOTS);
    let sum = |field: fn(&Slot) -> &AtomicU64| -> u64 {
        SLOT_TABLE[..threads]
            .iter()
            .map(|slot| field(slot).load(Ordering::Relaxed))
            .sum()
    };
    let mode = match MODE.load(Ordering::Relaxed) {
        FAST => "fast",
        VERIFY => "verify",
        _ => "stock",
    };
    serde_json::json!({
        "mode": mode,
        "libraryOpened": OPENED.load(Ordering::Relaxed),
        "libraryVerified": VERIFIED.load(Ordering::Relaxed),
        "bound": REAL.load(Ordering::Relaxed) != 0,
        "slots": SLOTS,
        "threads": threads,
        "noSlot": NO_SLOT.load(Ordering::Relaxed),
        "noSlotMeaning": "threads that found the table full and run the real library; counted once per thread, not per call",
        "calls": sum(|slot| &slot.calls),
        "planBuilds": sum(|slot| &slot.builds),
        "declined": sum(|slot| &slot.declined),
        "compared": sum(|slot| &slot.compared),
        "mismatches": sum(|slot| &slot.mismatches),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_mode_is_stock() {
        assert_eq!(parse_mode(b"fast"), FAST);
        assert_eq!(parse_mode(b"verify"), VERIFY);
        for value in [&b""[..], b"stock", b"FAST", b"fast ", b"1"] {
            assert_eq!(parse_mode(value), STOCK);
        }
    }

    /// More threads than slots: every slot goes to exactly one thread, the
    /// rest are refused once each, and a refused thread writes no shared
    /// counter on its later calls.
    #[test]
    fn a_full_table_refuses_each_extra_thread_once_and_is_then_left_alone() {
        const TABLE: usize = 32;
        const THREADS: usize = 45;
        let next = AtomicUsize::new(0);
        let refused = AtomicU64::new(0);
        let claimed: Vec<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        let current = Cell::new(UNCLAIMED);
                        let first = slot_index(&current, &next, &refused, TABLE);
                        for _ in 0..1000 {
                            assert_eq!(slot_index(&current, &next, &refused, TABLE), first);
                        }
                        first
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let mut slots: Vec<usize> = claimed
            .iter()
            .copied()
            .filter(|index| *index != REFUSED)
            .collect();
        slots.sort_unstable();
        assert_eq!(
            slots,
            (0..TABLE).collect::<Vec<_>>(),
            "each slot has one owner"
        );
        assert_eq!(next.load(Ordering::Relaxed), TABLE, "never past the table");
        assert_eq!(refused.load(Ordering::Relaxed), (THREADS - TABLE) as u64);
        // A refused thread's later calls touch neither counter: with both set
        // to values a write would disturb, they stay as set.
        let current = Cell::new(REFUSED);
        next.store(7, Ordering::Relaxed);
        refused.store(11, Ordering::Relaxed);
        for _ in 0..1000 {
            assert_eq!(slot_index(&current, &next, &refused, TABLE), REFUSED);
        }
        assert_eq!(next.load(Ordering::Relaxed), 7);
        assert_eq!(refused.load(Ordering::Relaxed), 11);
        // Neither sentinel is a table index.
        assert!(SLOT_TABLE.get(REFUSED).is_none() && SLOT_TABLE.get(UNCLAIMED).is_none());
    }

    unsafe extern "C" fn stub_library(_: *mut c_void, _: *mut c_void) -> c_int {
        7
    }

    /// Through the wrapper itself, with more short-lived threads than slots:
    /// every call reaches the library when the fast path declines, threads
    /// beyond the table are counted once each, and their calls are not
    /// counted against any slot.
    #[test]
    fn more_threads_than_slots_all_reach_the_library() {
        const EXTRA: usize = 5;
        const CALLS: u64 = 3;
        REAL.store(stub_library as *const () as usize, Ordering::Release);
        let before = snapshot();
        assert_eq!(before["threads"], 0, "the only test that claims slots");
        for _ in 0..SLOTS + EXTRA {
            std::thread::spawn(|| {
                for _ in 0..CALLS {
                    // Null arguments: declined, so the stub library answers.
                    let status = unsafe { src_process(std::ptr::null_mut(), std::ptr::null_mut()) };
                    assert_eq!(status, 7);
                }
            })
            .join()
            .unwrap();
        }
        let after = snapshot();
        assert_eq!(after["slots"], SLOTS);
        assert_eq!(after["threads"], SLOTS);
        assert_eq!(after["noSlot"], EXTRA);
        assert_eq!(after["calls"], SLOTS as u64 * CALLS);
        assert_eq!(after["declined"], SLOTS as u64 * CALLS);
        assert_eq!(after["mismatches"], 0);
    }

    #[test]
    fn slots_start_on_their_own_cache_lines() {
        assert_eq!(std::mem::align_of::<Slot>(), 64);
        assert_eq!(std::mem::size_of::<Slot>() % 64, 0);
    }

    #[test]
    fn the_fingerprint_is_fnv1a() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    fn converter(last_value: &mut f32, linear: &mut core::Linear, vt: &[usize; 2]) -> State {
        linear.dirty = 1;
        linear.last_value = last_value;
        State {
            vt: vt.as_ptr(),
            last_ratio: 1.0,
            last_position: 0.25,
            error: 0,
            channels: 1,
            mode: 0,
            callback: 0,
            user_data: 0,
            saved_frames: 0,
            saved_data: 0,
            private_data: linear,
        }
    }

    fn blank() -> core::Linear {
        core::Linear {
            magic: 0,
            dirty: 0,
            in_count: 0,
            in_used: 0,
            out_count: 0,
            out_gen: 0,
            last_value: std::ptr::null_mut(),
        }
    }

    #[test]
    fn anything_but_the_plain_mono_case_is_declined() {
        let vt = [0x5700usize, 0];
        let input = [0.5f32; 16];
        let mut output = [0f32; 16];
        let (mut value, mut linear) = (0f32, blank());
        let mut state = converter(&mut value, &mut linear, &vt);
        let mut data = Data {
            data_in: input.as_ptr(),
            data_out: output.as_mut_ptr(),
            input_frames: 16,
            output_frames: 16,
            input_frames_used: 0,
            output_frames_gen: 0,
            end_of_input: 0,
            src_ratio: 1.0,
        };
        let address = vt.as_ptr() as usize;
        let mut plan = Box::new(Plan::new());
        let check = |plan: &mut Plan, state: &State, data: &Data| unsafe {
            core::prepare(plan, state, data, address, 0x5700)
        };
        assert!(check(&mut plan, &state, &data));
        state.channels = 2;
        assert!(!check(&mut plan, &state, &data));
        state.channels = 1;
        state.mode = 1;
        assert!(!check(&mut plan, &state, &data));
        state.mode = 0;
        state.last_ratio = 0.0;
        assert!(!check(&mut plan, &state, &data));
        state.last_ratio = 1.0;
        state.last_position = f64::NAN;
        assert!(!check(&mut plan, &state, &data));
        state.last_position = 0.25;
        data.src_ratio = 300.0;
        assert!(!check(&mut plan, &state, &data));
        data.src_ratio = 1.0;
        data.input_frames = 0;
        assert!(!check(&mut plan, &state, &data));
        data.input_frames = MAX_FRAMES as std::ffi::c_long + 1;
        assert!(!check(&mut plan, &state, &data));
        data.input_frames = 16;
        data.data_out = input.as_ptr().wrapping_add(8) as *mut f32;
        assert!(!check(&mut plan, &state, &data));
        data.data_out = output.as_mut_ptr();
        unsafe { (*state.private_data).dirty = 0 };
        assert!(!check(&mut plan, &state, &data));
        unsafe { (*state.private_data).dirty = 1 };
        assert!(!unsafe { core::prepare(&mut plan, &state, &data, address + 8, 0x5700) });
        assert!(!unsafe { core::prepare(&mut plan, &state, &data, address, 0x5704) });
    }

    #[test]
    fn converters_in_the_same_state_share_a_plan() {
        let vt = [0x5700usize, 0];
        let input = [0.5f32; 16];
        let mut output = [0f32; 16];
        let (mut first_value, mut first) = (0f32, blank());
        let (mut second_value, mut second) = (0f32, blank());
        let one = converter(&mut first_value, &mut first, &vt);
        let mut two = converter(&mut second_value, &mut second, &vt);
        let data = Data {
            data_in: input.as_ptr(),
            data_out: output.as_mut_ptr(),
            input_frames: 16,
            output_frames: 16,
            input_frames_used: 0,
            output_frames_gen: 0,
            end_of_input: 0,
            src_ratio: 1.0,
        };
        let address = vt.as_ptr() as usize;
        let mut plan = Box::new(Plan::new());
        unsafe {
            assert!(core::prepare(&mut plan, &one, &data, address, 0x5700));
            assert!(core::prepare(&mut plan, &two, &data, address, 0x5700));
            assert_eq!(plan.builds(), 1);
            two.last_position = 0.5;
            assert!(core::prepare(&mut plan, &two, &data, address, 0x5700));
            assert_eq!(plan.builds(), 2);
        }
    }
}
