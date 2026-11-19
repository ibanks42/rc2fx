//! Where the callback and the helper threads run: the callback samples its
//! own placement, and the helper takes one derived from it.
use super::helper::Shared;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// The mixer's audio cores (docs/reference/hardware.md). The helper runs on those the
/// callback is not pinned to, one real-time priority below it: RØDE's own
/// real-time threads there are short, so the helper is rarely kept waiting,
/// and it never competes with the callback for its core.
pub(super) const AUDIO_CPUS: u64 = 0b1110;
/// Callbacks between re-reads of the callback thread's placement. RØDE pins
/// and reprioritizes that thread after its first callbacks have run.
pub(super) const PLACE_EVERY: u32 = 2048;
/// Until the callback is real-time on its own cores and the helper has taken
/// the placement derived from it, re-read every this many callbacks instead
/// (150 ms), for at most PLACE_FAST_UNTIL callbacks (19 s).
pub(super) const PLACE_FAST_EVERY: u32 = 64;
pub(super) const PLACE_FAST_UNTIL: u32 = 8192;
/// When the callback samples its placement (see Helper::place).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Sampler {
    pub(super) settled: bool,
    /// The first boundary not yet sampled: its callback had no handoff.
    pub(super) pending: Option<u32>,
    /// Samples taken after their boundary's callback.
    pub(super) deferred: u64,
}
impl Sampler {
    /// Every callback: note a boundary, with or without a handoff. Later
    /// boundaries before the sample do not replace the first.
    pub(super) fn mark(&mut self, cycle: u32) {
        if self.pending.is_none() && self.due(cycle) {
            self.pending = Some(cycle);
        }
    }
    /// With a handoff: whether to sample now (the boundary's own callback
    /// or the first one after it that hands off), and if so whether late.
    pub(super) fn take(&mut self, cycle: u32) -> Option<bool> {
        let deferred = self.pending.take()? != cycle;
        if deferred {
            self.deferred += 1;
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
    /// After a sample: settled once the callback is real-time, leaves the
    /// helper some audio core of its own, and the helper has applied the
    /// placement of the latest generation.
    pub(super) fn observe(&mut self, policy: i32, cpus: u64, generation: u64, applied: u64) {
        let realtime = matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR);
        let separate = AUDIO_CPUS & !cpus != 0;
        if realtime && separate && generation != 0 && applied == generation {
            self.settled = true;
        }
    }
}
/// The first placement changes since load, across pipelines, for the report:
/// what the callback sampled, and what the helper then requested and got.
/// Each record is claimed once and never reused.
pub(super) const PLACEMENT_HISTORY: usize = 16;
pub(super) static PLACEMENT_NEXT: AtomicUsize = AtomicUsize::new(0);
/// Pipelines created, to tell their records apart.
pub(super) static PIPELINE_IDS: AtomicU64 = AtomicU64::new(0);
pub(super) struct PlacementRecord {
    /// Written last by the callback: the generation, 0 for an empty slot.
    pub(super) generation: AtomicU64,
    pub(super) pipeline: AtomicU64,
    pub(super) cycle: AtomicU64,
    /// Sampled after its boundary, whose callback had no handoff.
    pub(super) deferred: AtomicBool,
    pub(super) at_ns: AtomicU64,
    pub(super) cpus: AtomicU64,
    pub(super) policy: AtomicI64,
    pub(super) priority: AtomicI64,
    /// Written by the helper when it applies this generation: what it
    /// requested, then its actual mask, policy and priority read back.
    pub(super) applied_at_ns: AtomicU64,
    pub(super) requested_cpus: AtomicU64,
    pub(super) applied_cpus: AtomicU64,
    pub(super) applied_policy: AtomicI64,
    pub(super) applied_priority: AtomicI64,
    /// sched_setaffinity and pthread_setschedparam results (0 is success).
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
/// Placement samples taken, and the cycle at which sampling settled (0: not).
pub(super) static PLACEMENT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static PLACEMENT_DEFERRED: AtomicU64 = AtomicU64::new(0);
pub(super) static PLACEMENT_SETTLED_CYCLE: AtomicU64 = AtomicU64::new(0);
/// Callback and helper thread placement, for the report: [tid, cpu, policy,
/// priority] each, from the pipeline that last sampled them.
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
/// Best effort: unprivileged hosts (tests) keep ordinary scheduling.
/// The helper's cores for a callback on `callback`: the audio cores the
/// callback is not on, else (callback not pinned yet, or on every audio core,
/// or its mask unknown) all of them. Never CPU 0, the suppression worker's.
pub(super) fn helper_mask(callback: u64) -> u64 {
    let mask = AUDIO_CPUS & !callback;
    if mask == 0 { AUDIO_CPUS } else { mask }
}
/// What the helper requested and then actually had, and the two calls'
/// results (0 is success).
pub(super) struct Taken {
    pub(super) requested_cpus: u64,
    pub(super) cpus: u64,
    pub(super) policy: i32,
    pub(super) priority: i32,
    pub(super) affinity: i32,
    pub(super) sched: i32,
}
/// This thread's affinity mask, policy and priority (0, -1, -1 if unreadable).
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
