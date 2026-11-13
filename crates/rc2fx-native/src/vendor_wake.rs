//! Wake one idle worker per job instead of all of them, for the qualified
//! 1.7.7 mixer; any other image keeps the stock wake loop.
//!
//! `0x5c8fa0` appends the job to the shared queue under the pool mutex, then
//! calls `0x5dba10(worker+0xc0)` for every worker: lock the event, set its
//! pending byte, `notify_all`, unlock. That is nine broadcasts per callback and
//! most of them wake a thread that has nothing to do.
//!
//! This entry replaces the loop at `0x5c9028` with a scan for the first worker
//! that is neither running a job (`worker+0x190 == 0`) nor already signalled
//! (`worker+0x128 == 0`), then re-enters the original loop body at `0x5c9040`
//! with the array cursor on that worker, so the signal itself is the vendor's
//! own call. With no such worker it jumps straight to the vendor epilogue.
//!
//! ## No job is lost
//!
//! - The job is in the queue before the wake loop runs: `0x5c9018` stores the
//!   count, `0x5c901c` the pointer, and only `0x5c9024` unlocks before the
//!   scan.
//! - A worker that is running (`+0x190 != 0`) re-scans after the job returns:
//!   it clears `+0x190` at `0x5c02a4`, loops to `0x5c0190`, and takes the queue
//!   mutex at `0x5c01ac` before consulting the queue head.
//! - A worker with `+0x128 != 0` has a wake published by `0x5dba10` under the
//!   event mutex (`0x5dba2c`) and checks that byte under the same mutex in its
//!   wait (`0x5db660`), so it cannot sleep past the flag.
//! - A worker with both words zero is exactly a worker that is neither running
//!   nor signalled, so signalling it is sufficient.
//!
//! The visible difference is which worker takes the job and how many futex
//! wakeups happen. The wait sites keep their 500 ms guard, so even a missed
//! wake would be bounded; this entry argues it cannot be missed.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use rc2fx_firmware::Profile;
use std::{
    cell::Cell,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

/// Why the patch is not installed, when it is not. A firmware or code mismatch
/// only leaves the stock wake loop in place; it never refuses RC2FX itself.
static SKIPPED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static BODY: AtomicUsize = AtomicUsize::new(0);
static EPILOGUE: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static SIGNALLED: AtomicU64 = AtomicU64::new(0);
static NOBODY: AtomicU64 = AtomicU64::new(0);

/// This thread's submit counters. `select` runs on whichever thread submits a
/// job; the callback's frame publishes them once per callback, and a submit
/// from any other thread publishes on the spot, so no count waits for a
/// callback that may never come.
#[derive(Clone, Copy)]
struct Counters {
    calls: u64,
    signals: u64,
    nobody: u64,
}
impl Counters {
    const ZERO: Self = Self {
        calls: 0,
        signals: 0,
        nobody: 0,
    };
}
thread_local! {
    /// `UnsafeCell` semantics through `Cell::as_ptr`: an update touches one
    /// field, not the whole struct.
    static COUNTERS: Cell<Counters> = const { Cell::new(Counters::ZERO) };
}

/// Publish this thread's submit counters. Called once per callback, and by
/// `select` itself when it runs outside a callback.
pub fn publish() {
    let counters =
        COUNTERS.with(|cell| std::mem::replace(unsafe { &mut *cell.as_ptr() }, Counters::ZERO));
    if counters.calls != 0 {
        CALLS.fetch_add(counters.calls, Ordering::Relaxed);
    }
    if counters.signals != 0 {
        SIGNALLED.fetch_add(counters.signals, Ordering::Relaxed);
    }
    if counters.nobody != 0 {
        NOBODY.fetch_add(counters.nobody, Ordering::Relaxed);
    }
}

/// Slot offset of the running job (`worker+0x190`) and of the pending-wake
/// byte (`worker+0x128`, that is the event at `worker+0xc0` plus its `+0x68`).
/// The pinned Pro II 1.7.7 `rc_audio_mixer` every vendor patch is checked
/// against (see config/firmware-profiles.json).
pub(crate) const QUALIFIED_SHA: &str =
    "5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd";

const RUNNING: usize = 0x190;
const PENDING: usize = 0x128;
/// The pool's worker-pointer array and its count.
const WORKERS: usize = 0x10;
const COUNT: usize = 0x1c;

/// Pick the worker slot to signal: the first with neither a running job nor a
/// pending wake. Returns the address of its entry in the pool's array, or 0.
fn select(pool: usize) -> usize {
    let chosen = choose(pool);
    COUNTERS.with(|cell| {
        // SAFETY: this thread's own cell; no reference outlives the closure.
        let counters = unsafe { &mut *cell.as_ptr() };
        counters.calls += 1;
        if chosen == 0 {
            counters.nobody += 1;
        } else {
            counters.signals += 1;
        }
    });
    if !crate::inside() {
        publish();
    }
    chosen
}

/// The scan itself, without counters, so tests can drive it directly.
fn choose(pool: usize) -> usize {
    if pool == 0 {
        return 0;
    }
    let workers = unsafe { std::ptr::read_volatile((pool + WORKERS) as *const usize) };
    let count = unsafe { std::ptr::read_volatile((pool + COUNT) as *const i32) };
    if workers == 0 || count <= 0 {
        return 0;
    }
    for index in 0..count as usize {
        let worker = unsafe { std::ptr::read_volatile((workers + index * 8) as *const usize) };
        if worker == 0 {
            continue;
        }
        // The worker publishes its running job as a full pointer (`stlr x20`
        // at 0x5c028c, `stlr xzr` at 0x5c02a4): read all 64 bits.
        let running =
            unsafe { (*((worker + RUNNING) as *const AtomicUsize)).load(Ordering::Acquire) };
        if running != 0 {
            continue;
        }
        // The wake flag is one byte (`stlrb` at 0x5dba3c, `ldarb` at 0x5db69c).
        // Reading a wider word would mix in the neighbouring bytes, and a
        // nonzero neighbour would hide a sleeping worker until its 500 ms
        // timeout.
        let pending = unsafe { (*((worker + PENDING) as *const AtomicU8)).load(Ordering::Acquire) };
        if pending != 0 {
            continue;
        }
        return workers + index * 8;
    }
    0
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C-unwind" fn entry() {
    // On arrival x20 is the pool. Registers checked: the body at 0x5c9040 reads
    // only x19 (the slot cursor) and x20 (the end), calls 0x5dba10, and on
    // return compares x20 with x19 before falling into the epilogue at
    // 0x5c9054, which reloads x19..x24 from the frame `0x5c8fac` saved. Nothing
    // else that this entry or `select` touches is live: x0-x5 and x30 are
    // caller-saved here, x30 is saved around the call, and sp stays 16-byte
    // aligned while it is adjusted.
    std::arch::naked_asm!(
        "bti c",
        "stp x29, x30, [sp, #-16]!",
        "mov x0, x20",
        "bl {select}",
        "ldp x29, x30, [sp], #16",
        "cbz x0, 2f",
        "mov x19, x0",
        "add x20, x19, #8",
        "adrp x16, {body}",
        "ldr x16, [x16, :lo12:{body}]",
        "br x16",
        "2:",
        "adrp x16, {epilogue}",
        "ldr x16, [x16, :lo12:{epilogue}]",
        "br x16",
        select = sym select,
        body = sym BODY,
        epilogue = sym EPILOGUE,
    );
}

#[cfg(target_arch = "aarch64")]
pub fn install(base: usize, profile: &Profile) -> Result<(), String> {
    if profile.mixer_sha256 != QUALIFIED_SHA {
        let _ = SKIPPED.set("mixer is not the qualified 1.7.7 image".into());
        return Ok(());
    }
    let site = (base + 0x5c9028) as *mut u8;
    let expected = [
        0x93, 0x0a, 0x40, 0xf9, 0x80, 0x1e, 0x80, 0xb9, 0x74, 0x0e, 0x00, 0x8b, 0x9f, 0x02, 0x13,
        0xeb,
    ];
    if unsafe { std::slice::from_raw_parts(site, 16) } != expected {
        let _ = SKIPPED.set("submit wake loop instructions differ".into());
        return Ok(());
    }
    BODY.store(base + 0x5c9040, Ordering::Release);
    EPILOGUE.store(base + 0x5c9054, Ordering::Release);
    // install_detour changes nothing when it fails, so a failure only keeps
    // the stock loop.
    if let Err(error) =
        unsafe { crate::vendor_job::install_detour(site, entry as *const () as usize, &ORIGINAL) }
    {
        let _ = SKIPPED.set(error);
    }
    Ok(())
}

#[cfg(not(target_arch = "aarch64"))]
pub fn install(_: usize, _: &Profile) -> Result<(), String> {
    Ok(())
}

pub fn status() -> serde_json::Value {
    serde_json::json!({
        "installed": ORIGINAL.load(Ordering::Acquire) != 0,
        "skipped": SKIPPED.get(),
        "calls": CALLS.load(Ordering::Relaxed),
        "signals": SIGNALLED.load(Ordering::Relaxed),
        "nobodyIdle": NOBODY.load(Ordering::Relaxed),
        "rule": "first worker with running job 0 and pending wake 0",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `select` updates process-wide counters; one test at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn pool(workers: &[Option<usize>]) -> Vec<usize> {
        // [0x00..0x10) unused, 0x10 = array, 0x1c = count
        let mut block = vec![0usize; 8 + workers.len() + 1];
        let array = block.as_mut_ptr() as usize + 0x40;
        let slot = array as *mut usize;
        for (index, worker) in workers.iter().enumerate() {
            unsafe { slot.add(index).write(worker.unwrap_or(0)) };
        }
        unsafe {
            (block.as_mut_ptr() as *mut u8)
                .add(0x10)
                .cast::<usize>()
                .write(array);
            (block.as_mut_ptr() as *mut u8)
                .add(0x1c)
                .cast::<i32>()
                .write(workers.len() as i32);
        }
        block
    }

    fn worker(running: u64, pending: u8) -> Vec<u64> {
        let mut block = vec![0u64; 0x400 / 8];
        block[RUNNING / 8] = running;
        block[PENDING / 8] = u64::from(pending);
        block
    }

    #[test]
    fn only_the_flag_byte_and_the_whole_job_pointer_count() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        // Nonzero bytes beside the flag must not hide an idle worker.
        let mut neighbours = worker(0, 0);
        neighbours[PENDING / 8] = 0xffff_ff00;
        // A running job whose pointer has zero low 32 bits is still running.
        let high = worker(0x1_0000_0000, 0);
        let block = pool(&[
            Some(high.as_ptr() as usize),
            Some(neighbours.as_ptr() as usize),
        ]);
        let array = unsafe {
            (block.as_ptr() as *const u8)
                .add(0x10)
                .cast::<usize>()
                .read()
        };
        assert_eq!(select(block.as_ptr() as usize), array + 8);
    }

    #[test]
    fn the_first_idle_unsignalled_worker_is_chosen() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        let busy = worker(1, 0);
        let flagged = worker(0, 1);
        let idle = worker(0, 0);
        let block = pool(&[
            Some(busy.as_ptr() as usize),
            Some(flagged.as_ptr() as usize),
            Some(idle.as_ptr() as usize),
        ]);
        let array = unsafe {
            (block.as_ptr() as *const u8)
                .add(0x10)
                .cast::<usize>()
                .read()
        };
        assert_eq!(select(block.as_ptr() as usize), array + 16);
    }

    #[test]
    fn nothing_is_signalled_when_every_worker_is_busy_or_flagged() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        let busy = worker(1, 0);
        let flagged = worker(0, 1);
        let block = pool(&[
            Some(busy.as_ptr() as usize),
            Some(flagged.as_ptr() as usize),
        ]);
        let before = NOBODY.load(Ordering::Relaxed);
        assert_eq!(select(block.as_ptr() as usize), 0);
        assert_eq!(NOBODY.load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn submit_counters_publish_once_per_frame() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        COUNTERS.with(|cell| cell.set(Counters::ZERO));
        let before = (
            CALLS.load(Ordering::Relaxed),
            SIGNALLED.load(Ordering::Relaxed),
            NOBODY.load(Ordering::Relaxed),
        );
        let busy = worker(1, 0);
        let block = pool(&[Some(busy.as_ptr() as usize)]);
        // No idle worker: one call, one nobody.
        assert_eq!(select(block.as_ptr() as usize), 0);
        assert_eq!(select(0), 0);
        // `select` outside a callback publishes on the spot, so the shared
        // counters have moved before any frame ends.
        assert_eq!(CALLS.load(Ordering::Relaxed), before.0 + 2);
        assert_eq!(NOBODY.load(Ordering::Relaxed), before.2 + 2);
        assert_eq!(SIGNALLED.load(Ordering::Relaxed), before.1);
        // Publishing again adds nothing.
        publish();
        assert_eq!(CALLS.load(Ordering::Relaxed), before.0 + 2);
    }

    #[test]
    fn an_empty_pool_signals_nobody() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        assert_eq!(select(0), 0);
        let block = pool(&[]);
        assert_eq!(select(block.as_ptr() as usize), 0);
    }
}
