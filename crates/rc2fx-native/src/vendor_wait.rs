//! Optional sampled timing of the two Mixer::Process worker-event wait sites.
//! The original wait and its return value are preserved. Not a replacement
//! scheduler, and not enabled during ordinary performance comparisons.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use rc2fx_firmware::Profile;
use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
type Wait = unsafe extern "C-unwind" fn(*mut c_void, i32) -> u32;
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static FIRST: AtomicUsize = AtomicUsize::new(0);
static SECOND: AtomicUsize = AtomicUsize::new(0);
const SAMPLE_EVERY: u64 = 64;
#[repr(align(64))]
struct Stats {
    calls: AtomicU64,
    samples: AtomicU64,
    wall: AtomicU64,
    max: AtomicU64,
    bad_clock: AtomicU64,
}
impl Stats {
    const fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            wall: AtomicU64::new(0),
            max: AtomicU64::new(0),
            bad_clock: AtomicU64::new(0),
        }
    }
    fn status(&self) -> serde_json::Value {
        serde_json::json!({"calls":self.calls.load(Ordering::Relaxed),"sampledCalls":self.samples.load(Ordering::Relaxed),"sampledWallNs":self.wall.load(Ordering::Relaxed),"sampledMaxNs":self.max.load(Ordering::Relaxed),"clockErrors":self.bad_clock.load(Ordering::Relaxed)})
    }
}
static STATS: [Stats; 2] = [Stats::new(), Stats::new()];
unsafe extern "C-unwind" fn wait(event: *mut c_void, timeout: i32, caller: usize) -> u32 {
    let original: Wait = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let index = if caller == FIRST.load(Ordering::Relaxed) {
        Some(0)
    } else if caller == SECOND.load(Ordering::Relaxed) {
        Some(1)
    } else {
        None
    };
    let Some(index) = index else {
        return unsafe { original(event, timeout) };
    };
    let stats = &STATS[index];
    let sequence = stats.calls.fetch_add(1, Ordering::Relaxed);
    if !sequence.is_multiple_of(SAMPLE_EVERY) {
        return unsafe { original(event, timeout) };
    }
    let start = crate::timing::monotonic();
    let result = unsafe { original(event, timeout) };
    let end = crate::timing::monotonic();
    if start != 0 && end >= start {
        stats.wall.fetch_add(end - start, Ordering::Relaxed);
        stats.max.fetch_max(end - start, Ordering::Relaxed);
        stats.samples.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.bad_clock.fetch_add(1, Ordering::Relaxed);
    }
    result
}
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C-unwind" fn entry(_: *mut c_void, _: i32) -> u32 {
    std::arch::naked_asm!("mov x2, x30","b {target}", target=sym wait);
}
#[cfg(target_arch = "aarch64")]
pub fn install(base: usize, profile: &Profile) -> Result<(), String> {
    if !std::env::var_os("RC2FX_VENDOR_WAIT_PROFILE").is_some_and(|v| v == "1")
        && !std::path::Path::new("/run/rc2fx-vendor-wait-profile").is_file()
    {
        return Ok(());
    }
    if profile.mixer_sha256 != crate::vendor_wake::QUALIFIED_SHA {
        return Err("worker wait probe requires pinned mixer 1.7.7".into());
    }
    let site = (base + 0x5db660) as *mut u8;
    let expected = [
        0x3f, 0x23, 0x03, 0xd5, 0xfd, 0x7b, 0xb9, 0xa9, 0xfd, 0x03, 0x00, 0x91, 0xf3, 0x53, 0x01,
        0xa9,
    ];
    if unsafe { std::slice::from_raw_parts(site, 16) } != expected {
        return Err("worker wait prologue differs".into());
    }
    FIRST.store(base + 0x6584b8, Ordering::Release);
    SECOND.store(base + 0x6586f0, Ordering::Release);
    unsafe { crate::vendor_job::install_detour(site, entry as *const () as usize, &ORIGINAL) }
}
#[cfg(not(target_arch = "aarch64"))]
pub fn install(_: usize, _: &Profile) -> Result<(), String> {
    Ok(())
}
pub fn status() -> serde_json::Value {
    serde_json::json!({"installed":ORIGINAL.load(Ordering::Acquire)!=0,"sampleEvery":SAMPLE_EVERY,"firstJoin":STATS[0].status(),"secondJoin":STATS[1].status(),"scope":"wait helper at the two qualified callback call sites; excludes completed-job scans and queue locks"})
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Observed {
        calls: u32,
        timeout: i32,
    }
    unsafe extern "C-unwind" fn original(event: *mut c_void, timeout: i32) -> u32 {
        let observed = unsafe { &mut *event.cast::<Observed>() };
        observed.calls += 1;
        observed.timeout = timeout;
        timeout.wrapping_add(42) as u32
    }
    #[test]
    fn sampling_preserves_arguments_returns_and_unrelated_waits() {
        ORIGINAL.store(original as *const () as usize, Ordering::Release);
        FIRST.store(0x10, Ordering::Release);
        SECOND.store(0x20, Ordering::Release);
        let mut observed = Observed {
            calls: 0,
            timeout: 0,
        };
        let event = std::ptr::addr_of_mut!(observed).cast();
        assert_eq!(unsafe { wait(event, -100, 0x30) }, (-58i32) as u32);
        assert_eq!(STATS[0].calls.load(Ordering::Relaxed), 0);
        for _ in 0..65 {
            assert_eq!(unsafe { wait(event, 2, 0x10) }, 44)
        }
        assert_eq!(observed.calls, 66);
        assert_eq!(observed.timeout, 2);
        assert_eq!(STATS[0].calls.load(Ordering::Relaxed), 65);
        assert_eq!(STATS[0].samples.load(Ordering::Relaxed), 2);
        assert_eq!(STATS[0].bad_clock.load(Ordering::Relaxed), 0);
        assert_eq!(STATS[1].calls.load(Ordering::Relaxed), 0);
        ORIGINAL.store(0, Ordering::Release);
        FIRST.store(0, Ordering::Release);
        SECOND.store(0, Ordering::Release);
    }
}
