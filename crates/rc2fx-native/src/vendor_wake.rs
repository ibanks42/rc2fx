//! Wakes a single idle worker per queued job instead of broadcasting to all of
//! them on the qualified 1.7.7 mixer image.
//!
//! Stock `0x5c8fa0` appends the job under the pool mutex and then calls
//! `0x5dba10(worker+0xc0)` for every worker (9 futex broadcasts per callback,
//! waking mostly idle threads with nothing to do).
//!
//! We patch the loop at `0x5c9028` to scan for the first worker that is neither
//! running a job (`+0x190 == 0`) nor already signaled (`+0x128 == 0`), then
//! jump into the original loop body at `0x5c9040` for that single slot (or
//! straight to the epilogue at `0x5c9054` if none is idle).
//!
//! Correctness (no lost wakeups):
//! - The job is already visible in the queue before the scan runs (`0x5c9018`/
//!   `0x5c901c` store count/pointer and `0x5c9024` unlocks the pool mutex).
//! - Running workers (`+0x190 != 0`) clear `+0x190` at `0x5c02a4` and loop
//!   back to `0x5c0190` to re-check the queue under the mutex.
//! - Signaled workers (`+0x128 != 0`) have a pending wake published under the
//!   event mutex (`0x5dba2c`) and checked under the same mutex in wait (`0x5db660`).
//! - Wait sites also retain their 500 ms fallback timeout.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use rc2fx_firmware::Profile;
use std::{
    cell::Cell,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

/// Reason the patch was skipped, if any (mismatch falls back to stock wake loop).
static SKIPPED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static BODY: AtomicUsize = AtomicUsize::new(0);
static EPILOGUE: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static SIGNALED: AtomicU64 = AtomicU64::new(0);
static NOBODY: AtomicU64 = AtomicU64::new(0);

/// Thread-local submit counters. Flushed once per callback on the audio thread,
/// or immediately when `select` is called from another thread.
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
    static COUNTERS: Cell<Counters> = const { Cell::new(Counters::ZERO) };
}

pub fn publish() {
    let counters =
        COUNTERS.with(|cell| std::mem::replace(unsafe { &mut *cell.as_ptr() }, Counters::ZERO));
    if counters.calls != 0 {
        CALLS.fetch_add(counters.calls, Ordering::Relaxed);
    }
    if counters.signals != 0 {
        SIGNALED.fetch_add(counters.signals, Ordering::Relaxed);
    }
    if counters.nobody != 0 {
        NOBODY.fetch_add(counters.nobody, Ordering::Relaxed);
    }
}

/// SHA-256 of the qualified Pro II 1.7.7 `rc_audio_mixer` binary.
pub(crate) const QUALIFIED_SHA: &str =
    "5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd";

// Worker field offsets: running job pointer (+0x190) and event pending byte (+0xc0 + 0x68).
const RUNNING: usize = 0x190;
const PENDING: usize = 0x128;
// Pool worker-pointer array (+0x10) and count (+0x1c).
const WORKERS: usize = 0x10;
const COUNT: usize = 0x1c;

/// Returns the pool array slot address of the first idle, unsignaled worker, or 0.
fn select(pool: usize) -> usize {
    let chosen = choose(pool);
    COUNTERS.with(|cell| {
        // SAFETY: thread-local cell; no other reference is alive in this closure.
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
        // Published as a full 64-bit pointer (`stlr x20` at 0x5c028c, `stlr xzr` at 0x5c02a4).
        let running =
            unsafe { (*((worker + RUNNING) as *const AtomicUsize)).load(Ordering::Acquire) };
        if running != 0 {
            continue;
        }
        // Single-byte flag (`stlrb` at 0x5dba3c, `ldarb` at 0x5db69c); a wider
        // load would pick up adjacent non-zero bytes and hide idle workers.
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
    // At 0x5c9028, x20 holds `pool`. The loop body at 0x5c9040 only consumes
    // x19 (current slot) and x20 (end), calls 0x5dba10, and falls through to
    // the epilogue at 0x5c9054 which restores x19..x24. Caller-saved regs and
    // x30 are safe to clobber across `bl select` once x29/x30 are spilled.
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
    // `install_detour` rolls back on error, leaving the stock loop intact.
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
        "signals": SIGNALED.load(Ordering::Relaxed),
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
        let mut neighbors = worker(0, 0);
        neighbors[PENDING / 8] = 0xffff_ff00;
        // A running job whose pointer has zero low 32 bits is still running.
        let high = worker(0x1_0000_0000, 0);
        let block = pool(&[
            Some(high.as_ptr() as usize),
            Some(neighbors.as_ptr() as usize),
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
    fn the_first_idle_unsignaled_worker_is_chosen() {
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
    fn nothing_is_signaled_when_every_worker_is_busy_or_flagged() {
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
            SIGNALED.load(Ordering::Relaxed),
            NOBODY.load(Ordering::Relaxed),
        );
        let busy = worker(1, 0);
        let block = pool(&[Some(busy.as_ptr() as usize)]);
        assert_eq!(select(block.as_ptr() as usize), 0);
        assert_eq!(select(0), 0);
        // Calls outside an audio callback flush immediately.
        assert_eq!(CALLS.load(Ordering::Relaxed), before.0 + 2);
        assert_eq!(NOBODY.load(Ordering::Relaxed), before.2 + 2);
        assert_eq!(SIGNALED.load(Ordering::Relaxed), before.1);
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
