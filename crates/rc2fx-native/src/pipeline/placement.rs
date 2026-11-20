//! CPU affinity and RT scheduling placement for the callback and helper threads.
use super::helper::Shared;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// Audio CPU mask (CPUs 1–3; CPU 0 is reserved for the suppression worker).
/// The helper runs on whichever audio cores the callback isn't pinned to, at
/// one RT priority below the callback.
pub(super) const AUDIO_CPUS: u64 = 0b1110;
/// Steady-state interval (in callbacks) between re-reading callback placement.
pub(super) const PLACE_EVERY: u32 = 2048;
/// Fast polling interval (~150 ms) during startup while RØDE finishes pinning
/// and reprioritizing the callback thread, capped at `PLACE_FAST_UNTIL` (~19 s).
pub(super) const PLACE_FAST_EVERY: u32 = 64;
pub(super) const PLACE_FAST_UNTIL: u32 = 8192;
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Sampler {
    pub(super) settled: bool,
    /// Pending sample cycle if the boundary callback had no handoff.
    pub(super) pending: Option<u32>,
}
impl Sampler {
    /// Marks a sample boundary on any callback without overwriting an earlier pending one.
    pub(super) fn mark(&mut self, cycle: u32) {
        if self.pending.is_none() && self.due(cycle) {
            self.pending = Some(cycle);
        }
    }
    /// Consumes a pending sample on a handoff callback, returning whether it was deferred.
    pub(super) fn take(&mut self, cycle: u32) -> Option<bool> {
        let deferred = self.pending.take()? != cycle;
        if deferred {
            PLACEMENT_DEFERRED.fetch_add(1, Ordering::Relaxed);
        }
        Some(deferred)
    }
    pub(super) fn due(&self, cycle: u32) -> bool {
        let every = if !self.settled && cycle < PLACE_FAST_UNTIL {
            PLACE_FAST_EVERY
        } else {
            PLACE_EVERY
        };
        cycle.is_multiple_of(every)
    }
    /// Marks placement settled once the callback is RT, leaves at least one
    /// audio core for the helper, and the helper has applied that generation.
    pub(super) fn observe(&mut self, policy: i32, cpus: u64, generation: u64, applied: u64) {
        let realtime = matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR);
        let separate = AUDIO_CPUS & !cpus != 0;
        if realtime && separate && generation != 0 && applied == generation {
            self.settled = true;
        }
    }
}
/// First `PLACEMENT_HISTORY` placement transitions recorded for diagnostics (write-once slots).
pub(super) const PLACEMENT_HISTORY: usize = 16;
pub(super) static PLACEMENT_NEXT: AtomicUsize = AtomicUsize::new(0);
pub(super) static PIPELINE_IDS: AtomicU64 = AtomicU64::new(0);
pub(super) struct PlacementRecord {
    /// Published last by the callback (0 = empty slot).
    pub(super) generation: AtomicU64,
    pub(super) pipeline: AtomicU64,
    pub(super) cycle: AtomicU64,
    pub(super) deferred: AtomicBool,
    pub(super) at_ns: AtomicU64,
    pub(super) cpus: AtomicU64,
    pub(super) policy: AtomicI64,
    pub(super) priority: AtomicI64,
    pub(super) applied_at_ns: AtomicU64,
    pub(super) requested_cpus: AtomicU64,
    pub(super) applied_cpus: AtomicU64,
    pub(super) applied_policy: AtomicI64,
    pub(super) applied_priority: AtomicI64,
    /// Return values of `sched_setaffinity` / `pthread_setschedparam` (0 = ok).
    pub(super) affinity_result: AtomicI64,
    pub(super) sched_result: AtomicI64,
}
pub(super) static PLACEMENTS: [PlacementRecord; PLACEMENT_HISTORY] = [const {
    PlacementRecord {
        generation: AtomicU64::new(0),
        pipeline: AtomicU64::new(0),
        cycle: AtomicU64::new(0),
        deferred: AtomicBool::new(false),
        at_ns: AtomicU64::new(0),
        cpus: AtomicU64::new(0),
        policy: AtomicI64::new(-1),
        priority: AtomicI64::new(-1),
        applied_at_ns: AtomicU64::new(0),
        requested_cpus: AtomicU64::new(0),
        applied_cpus: AtomicU64::new(0),
        applied_policy: AtomicI64::new(-1),
        applied_priority: AtomicI64::new(-1),
        affinity_result: AtomicI64::new(i64::MIN),
        sched_result: AtomicI64::new(i64::MIN),
    }
}; PLACEMENT_HISTORY];
pub(super) static PLACEMENT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static PLACEMENT_DEFERRED: AtomicU64 = AtomicU64::new(0);
pub(super) static PLACEMENT_SETTLED_CYCLE: AtomicU64 = AtomicU64::new(0);
/// Latest `[tid, cpu, policy, priority]` sampled for callback and helper threads.
pub(super) static CALLBACK_PLACEMENT: [AtomicI64; 4] = [const { AtomicI64::new(-1) }; 4];
pub(super) static HELPER_PLACEMENT: [AtomicI64; 4] = [const { AtomicI64::new(-1) }; 4];
pub fn placement() -> serde_json::Value {
    let read = |values: &[AtomicI64; 4]| {
        super::super::probe::placement_json(std::array::from_fn(|i| {
            values[i].load(Ordering::Relaxed)
        }))
    };
    let history: Vec<_> = PLACEMENTS
        .iter()
        .filter(|record| record.generation.load(Ordering::Acquire) != 0)
        .map(|record| {
            let get = |value: &AtomicU64| value.load(Ordering::Relaxed);
            let geti = |value: &AtomicI64| value.load(Ordering::Relaxed);
            serde_json::json!({
                "pipeline": get(&record.pipeline),
                "generation": get(&record.generation),
                "cycle": get(&record.cycle),
                "deferred": record.deferred.load(Ordering::Relaxed),
                "atNs": get(&record.at_ns),
                "callbackCpus": get(&record.cpus),
                "callbackPolicy": geti(&record.policy),
                "callbackPriority": geti(&record.priority),
                "appliedAtNs": get(&record.applied_at_ns),
                "helperRequestedCpus": get(&record.requested_cpus),
                "helperCpus": get(&record.applied_cpus),
                "helperPolicy": geti(&record.applied_policy),
                "helperPriority": geti(&record.applied_priority),
                "affinityResult": geti(&record.affinity_result),
                "schedResult": geti(&record.sched_result),
            })
        })
        .collect();
    serde_json::json!({
        "callback": read(&CALLBACK_PLACEMENT),
        "helper": read(&HELPER_PLACEMENT),
        "samples": PLACEMENT_SAMPLES.load(Ordering::Relaxed),
        "deferred": PLACEMENT_DEFERRED.load(Ordering::Relaxed),
        "settledCycle": PLACEMENT_SETTLED_CYCLE.load(Ordering::Relaxed),
        "history": history,
    })
}
pub(super) fn store_placement(target: &[AtomicI64; 4]) {
    for (slot, value) in target.iter().zip(super::super::probe::placement()) {
        slot.store(value, Ordering::Relaxed);
    }
}
/// Picks the audio cores not used by `callback` (falling back to all audio
/// cores, never CPU 0 which belongs to the suppression worker).
pub(super) fn helper_mask(callback: u64) -> u64 {
    let mask = AUDIO_CPUS & !callback;
    if mask == 0 { AUDIO_CPUS } else { mask }
}
pub(super) struct Taken {
    pub(super) requested_cpus: u64,
    pub(super) cpus: u64,
    pub(super) policy: i32,
    pub(super) priority: i32,
    pub(super) affinity: i32,
    pub(super) sched: i32,
}
/// Returns `(cpu_mask, policy, priority)` for the calling thread, or `(mask, -1, -1)` on error.
pub(super) fn current_placement() -> (u64, i32, i32) {
    unsafe {
        let mut cpus: libc::cpu_set_t = std::mem::zeroed();
        let mut mask = 0u64;
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpus) == 0 {
            for cpu in 0..64 {
                if libc::CPU_ISSET(cpu, &cpus) {
                    mask |= 1 << cpu;
                }
            }
        }
        let mut policy = -1;
        let mut param: libc::sched_param = std::mem::zeroed();
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) != 0 {
            return (mask, -1, -1);
        }
        (mask, policy, param.sched_priority)
    }
}
pub(super) fn take_placement(shared: &Shared) -> Taken {
    unsafe {
        let mask = helper_mask(shared.cpus.load(Ordering::Relaxed));
        let mut cpus: libc::cpu_set_t = std::mem::zeroed();
        for cpu in (0..64).filter(|cpu| mask & (1 << cpu) != 0) {
            libc::CPU_SET(cpu, &mut cpus);
        }
        let affinity = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpus);
        let policy = shared.policy.load(Ordering::Relaxed);
        let mut priority = shared.priority.load(Ordering::Relaxed);
        if matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR) && priority > 1 {
            priority -= 1;
        }
        let param = libc::sched_param {
            sched_priority: priority,
        };
        let sched = libc::pthread_setschedparam(libc::pthread_self(), policy, &param);
        let (actual, policy, priority) = current_placement();
        Taken {
            requested_cpus: mask,
            cpus: actual,
            policy,
            priority,
            affinity,
            sched,
        }
    }
}
