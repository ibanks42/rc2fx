//! Dedicated helper thread that processes source audio off the main callback,
//! plus its handoff protocol, forecast timer, and wake logic.
use super::placement::{
    CALLBACK_PLACEMENT, HELPER_PLACEMENT, PIPELINE_IDS, PLACEMENT_HISTORY, PLACEMENT_NEXT,
    PLACEMENT_SAMPLES, PLACEMENT_SETTLED_CYCLE, PLACEMENTS, Sampler, current_placement,
    store_placement, take_placement,
};
use super::stats::{
    HANDOFF_EARLY, HANDOFF_LATE, HELPER_TIME, SELF_LATENCY, STARTS, STARTS_PARKED_AFTER_TIMER,
    STARTS_PARKED_NO_FORECAST, STARTS_QUEUED, STARTS_TIMER, STARTS_WOKEN_EARLY, TIMER_LATE,
    WAKE_LATENCY, WOKEN_LATENCY,
};
use super::{Handoff, Job};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

pub(super) const IDLE: u8 = 0;
pub(super) const QUEUED: u8 = 1;
/// Max time the callback spins for a busy helper before falling back to passthrough.
pub(super) const HELPER_WAIT_NS: u64 = 200_000;
pub(super) const PERIOD_NS: u64 = super::super::timing::BUDGET_NS;
/// `Shared::wake_at` value while the helper is running or checking `state`.
pub(super) const AWAKE: u64 = 0;
/// `Shared::wake_at` value while parked indefinitely (requires an explicit unpark).
pub(super) const PARKED: u64 = u64::MAX;
/// Margin past the forecast callback end when the helper's self-wake timer
/// fires, avoiding a ~12 us futex wake syscall on the callback thread.
///
/// Tuned on hardware (2026-11-12, 10-min blocks, 2.5 ms denoiser hold): 50/100/
/// 200 us all saved ~10–11 us on the callback, with 50 us showing the fewest
/// late starts. Past 400 us the timer crosses `LATEST_SELF_START_NS` so the
/// callback has to unpark anyway. If the timer fires before handoff, the helper
/// parks for the callback to wake it.
pub(crate) const WAKE_MARGIN_NS: u64 = 50_000;
/// Latest offset from callback start that the helper is allowed to self-wake.
/// If its timer is later than this, the callback explicitly unparks it.
///
/// In a 43-hour run (~67M callbacks), inter-callback gaps were never shorter
/// than half a period. Starting by this bound with a typical ~240 us helper
/// run finishes ~990 us before a normal next callback and within
/// `HELPER_WAIT_NS` of a half-period early callback.
pub(super) const LATEST_SELF_START_NS: u64 = PERIOD_NS / 2 + HELPER_WAIT_NS - 266_666;
/// Callbacks ending more than 10% of a period off the expected cadence reset
/// the forecast (late start, skipped cycle, restart).
pub(super) const IRREGULAR_NS: u64 = PERIOD_NS / 10;
/// Consecutive regular callbacks required before switching from park to timed wake (~37 ms).
pub(super) const FORECAST_WARMUP: u32 = 16;

/// Exponentially smoothed (weight 1/8) predictor for when the next callback
/// will end. Resets on irregular timing so the helper falls back to parking.
#[derive(Default, Debug)]
pub(super) struct Forecast {
    pub(super) anchor: u64,
    pub(super) streak: u32,
    previous: u64,
    /// Deviation of the latest callback end from the forecast (for bench gauges).
    pub(super) deviation: Option<i64>,
}
impl Forecast {
    /// Updates the forecast after a frame and returns the next self-wake
    /// timestamp, or `None` if the helper should park instead.
    pub(super) fn observe(&mut self, begun: u64, ended: u64) -> Option<u64> {
        // The callback publishes `ended` after handoff, so a fast helper frame
        // can finish first and still see the previous callback's end stamp; in
        // that case estimate this callback's length from the previous one.
        let own = |start: u64| start != 0 && ended > start && ended - start < PERIOD_NS;
        let length = if own(begun) {
            Some(ended - begun)
        } else if begun != 0 && own(self.previous) {
            Some(ended - self.previous)
        } else {
            None
        };
        self.previous = begun;
        self.deviation = None;
        let expected = self.anchor.saturating_add(PERIOD_NS);
        match length.map(|length| begun + length) {
            Some(end) if self.anchor != 0 && end.abs_diff(expected) <= IRREGULAR_NS => {
                self.deviation = Some(if end >= expected {
                    (end - expected) as i64
                } else {
                    -((expected - end) as i64)
                });
                self.anchor = if end >= expected {
                    expected + (end - expected) / 8
                } else {
                    expected - (expected - end) / 8
                };
                self.streak = self.streak.saturating_add(1);
            }
            Some(end) => {
                self.anchor = end;
                self.streak = 0;
            }
            None => {
                self.anchor = 0;
                self.streak = 0;
            }
        }
        if self.streak < FORECAST_WARMUP {
            return None;
        }
        let wake_at = self.anchor + PERIOD_NS + WAKE_MARGIN_NS;
        // Skip setting a timer if the next callback would unpark us anyway.
        (wake_at <= begun + PERIOD_NS + LATEST_SELF_START_NS).then_some(wake_at)
    }
}

/// Returns true if a callback starting at `begun` must explicitly unpark a
/// helper whose published state is `wake_at`.
pub(super) fn must_wake(wake_at: u64, begun: u64) -> bool {
    wake_at == PARKED || (wake_at != AWAKE && wake_at > begun.saturating_add(LATEST_SELF_START_NS))
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Woke {
    No,
    Parked,
    Late,
}
/// How the helper woke up for a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Start {
    Queued,
    Slept(u64),
    Parked { expired: bool },
}
/// Clock and sleep abstraction for the helper loop (mocked in tests).
pub(super) trait Pace {
    fn prepare(&self) {}
    fn now(&self) -> u64;
    fn sleep_until(&self, wake_at: u64);
    fn park(&self);
    /// Test hook called after returning a frame, before going back to sleep.
    fn rest(&self) {}
    /// If true, timed wakes are disabled until the helper has RT scheduling.
    fn needs_realtime(&self) -> bool {
        false
    }
    fn realtime(&self, policy: i32) -> bool {
        matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR)
    }
}
/// Production `Pace` using `thread::park_timeout` (hard-IRQ hrtimer for RT
/// threads on PREEMPT_RT).
pub(super) struct Kernel;
impl Pace for Kernel {
    fn prepare(&self) {
        // Default 50 us timer slack applies to futex timeouts even on RT threads.
        unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong) };
    }
    fn now(&self) -> u64 {
        super::super::timing::monotonic()
    }
    fn sleep_until(&self, wake_at: u64) {
        let left = wake_at.saturating_sub(super::super::timing::monotonic());
        if left != 0 {
            thread::park_timeout(std::time::Duration::from_nanos(left));
        }
    }
    fn park(&self) {
        thread::park();
    }
    fn needs_realtime(&self) -> bool {
        // Non-RT timers run on ktimersoftd, which RT audio threads preempt, so
        // hold off on timed wakes until placement makes the helper RT.
        !cfg!(test)
    }
}
pub(super) struct Shared {
    pub(super) state: AtomicU8,
    handoff: AtomicPtr<Handoff>,
    pub(super) stop: AtomicBool,
    pub(super) started: AtomicU64,
    pub(super) finished: AtomicU64,
    /// Bench gauges only: `CLOCK_MONOTONIC` right before handoff (0 when off).
    pub(super) queued_at: AtomicU64,
    /// `AWAKE`, `PARKED`, or the `CLOCK_MONOTONIC` deadline of the current timed sleep.
    pub(super) wake_at: AtomicU64,
    /// `CLOCK_MONOTONIC` end of the latest callback (input to `Forecast`).
    pub(super) ended: AtomicU64,
    #[cfg(test)]
    pub(super) ran: std::sync::Mutex<Vec<(u64, Start, u64)>>,
    #[cfg(test)]
    pub(super) gone: AtomicBool,
    /// Callback thread placement snapshot; bumping `generation` tells the helper to re-apply.
    pub(super) generation: AtomicU64,
    pub(super) applied: AtomicU64,
    /// Index into `PLACEMENTS` for the latest generation, or `usize::MAX`.
    record: AtomicUsize,
    id: u64,
    pub(super) cpus: AtomicU64,
    pub(super) policy: AtomicI32,
    pub(super) priority: AtomicI32,
}
impl Shared {
    pub(super) fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            handoff: AtomicPtr::new(std::ptr::null_mut()),
            stop: AtomicBool::new(false),
            started: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            queued_at: AtomicU64::new(0),
            wake_at: AtomicU64::new(AWAKE),
            ended: AtomicU64::new(0),
            #[cfg(test)]
            ran: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            gone: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            record: AtomicUsize::new(usize::MAX),
            id: PIPELINE_IDS.fetch_add(1, Ordering::Relaxed) + 1,
            cpus: AtomicU64::new(0),
            policy: AtomicI32::new(0),
            priority: AtomicI32::new(0),
        }
    }
}
pub(super) struct Helper {
    pub(super) shared: Arc<Shared>,
    pub(super) thread: Option<thread::JoinHandle<()>>,
}
impl Helper {
    pub(super) fn start() -> Result<Self, String> {
        Self::start_with(Kernel)
    }
    pub(super) fn start_with<P: Pace + Send + 'static>(pace: P) -> Result<Self, String> {
        let shared = Arc::new(Shared::new());
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("rc2fx-dsp".into())
            .spawn(move || {
                pace.prepare();
                helper_loop(&worker, &pace);
                #[cfg(test)]
                worker.gone.store(true, Ordering::SeqCst);
            })
            .map_err(|error| format!("native helper thread: {error}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub(super) fn idle(&self) -> bool {
        self.shared.state.load(Ordering::Acquire) == IDLE
    }
    /// Spins up to `limit` ns for the helper (running on another core) to go idle.
    pub(super) fn wait(&self, limit: u64) -> bool {
        let deadline = super::super::timing::monotonic().saturating_add(limit);
        while !self.idle() {
            if super::super::timing::monotonic() >= deadline {
                return false;
            }
            std::hint::spin_loop();
        }
        true
    }
    /// Samples the callback thread's CPU affinity and RT priority when due,
    /// publishing changes for the helper to mirror on its next frame.
    pub(super) fn place(&self, cycle: u32, sampler: &mut Sampler) {
        let Some(deferred) = sampler.take(cycle) else {
            return;
        };
        PLACEMENT_SAMPLES.fetch_add(1, Ordering::Relaxed);
        store_placement(&CALLBACK_PLACEMENT);
        {
            let (mask, policy, priority) = current_placement();
            let (policy, priority) = if policy < 0 {
                (libc::SCHED_OTHER, 0)
            } else {
                (policy, priority)
            };
            let param = libc::sched_param {
                sched_priority: priority,
            };
            let shared = &self.shared;
            if shared.cpus.load(Ordering::Relaxed) != mask
                || shared.policy.load(Ordering::Relaxed) != policy
                || shared.priority.load(Ordering::Relaxed) != param.sched_priority
                || shared.generation.load(Ordering::Relaxed) == 0
            {
                shared.cpus.store(mask, Ordering::Relaxed);
                shared.policy.store(policy, Ordering::Relaxed);
                shared
                    .priority
                    .store(param.sched_priority, Ordering::Relaxed);
                let generation = shared.generation.fetch_add(1, Ordering::Release) + 1;
                let index = PLACEMENT_NEXT.load(Ordering::Relaxed);
                let claimed = index < PLACEMENT_HISTORY
                    && PLACEMENT_NEXT
                        .compare_exchange(index, index + 1, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok();
                shared
                    .record
                    .store(if claimed { index } else { usize::MAX }, Ordering::Release);
                if let Some(record) = claimed.then(|| &PLACEMENTS[index]) {
                    record.pipeline.store(shared.id, Ordering::Relaxed);
                    record.cycle.store(u64::from(cycle), Ordering::Relaxed);
                    record.deferred.store(deferred, Ordering::Relaxed);
                    record
                        .at_ns
                        .store(super::super::timing::monotonic(), Ordering::Relaxed);
                    record.cpus.store(mask, Ordering::Relaxed);
                    record.policy.store(i64::from(policy), Ordering::Relaxed);
                    record
                        .priority
                        .store(i64::from(param.sched_priority), Ordering::Relaxed);
                    record.generation.store(generation, Ordering::Release);
                }
            }
            let settled = sampler.settled;
            sampler.observe(
                policy,
                mask,
                shared.generation.load(Ordering::Relaxed),
                shared.applied.load(Ordering::Acquire),
            );
            if sampler.settled && !settled {
                PLACEMENT_SETTLED_CYCLE.store(u64::from(cycle), Ordering::Relaxed);
            }
        }
    }
    /// Hands off a frame to the helper, unparking it only if `must_wake` says
    /// its timer won't fire in time.
    ///
    /// Pairs with `await_frame` via a Dekker-style SeqCst store/load: we store
    /// `QUEUED` to `state` then load `wake_at`, while the helper stores
    /// `wake_at` then loads `state`.
    pub(super) fn queue(&self, handoff: *mut Handoff, begun: u64, queued_at: u64) -> Woke {
        let shared = &*self.shared;
        shared.handoff.store(handoff, Ordering::Relaxed);
        if queued_at != 0 {
            shared.queued_at.store(queued_at, Ordering::Relaxed);
        }
        shared.state.store(QUEUED, Ordering::SeqCst);
        let wake_at = shared.wake_at.load(Ordering::SeqCst);
        if !must_wake(wake_at, begun) {
            return Woke::No;
        }
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
        if wake_at == PARKED {
            Woke::Parked
        } else {
            Woke::Late
        }
    }
}
impl Helper {
    /// Signals the helper to exit and joins it. Control-only, after callbacks stop.
    pub(super) fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Waits for the next queued frame (timed sleep until `wake_at` if we have a
/// forecast, then park), returning `None` on shutdown.
///
/// Dekker SeqCst ordering with `Helper::queue` prevents lost wakeups: either
/// our `state` load observes `QUEUED` before sleeping, or the callback's
/// `wake_at` load observes our sleep state (`PARKED` or timer deadline) and
/// unparks us if needed.
pub(super) fn await_frame<P: Pace>(
    shared: &Shared,
    pace: &P,
    wake_at: Option<u64>,
) -> Option<Start> {
    let mut slept = false;
    if let Some(wake_at) = wake_at {
        loop {
            shared.wake_at.store(wake_at, Ordering::SeqCst);
            if shared.stop.load(Ordering::Acquire) {
                return None;
            }
            if shared.state.load(Ordering::SeqCst) == QUEUED {
                return Some(if slept {
                    Start::Slept(wake_at)
                } else {
                    Start::Queued
                });
            }
            // Fall back to parking if the timer expired with no frame (or the
            // clock jumped unreasonably far from the forecast).
            let now = pace.now();
            if now >= wake_at || wake_at - now > 2 * PERIOD_NS {
                break;
            }
            pace.sleep_until(wake_at);
            slept = true;
        }
    }
    let expired = slept;
    let mut parked = false;
    loop {
        shared.wake_at.store(PARKED, Ordering::SeqCst);
        if shared.stop.load(Ordering::Acquire) {
            return None;
        }
        if shared.state.load(Ordering::SeqCst) == QUEUED {
            return Some(if parked || expired {
                Start::Parked { expired }
            } else {
                Start::Queued
            });
        }
        pace.park();
        parked = true;
    }
}
fn helper_loop<P: Pace>(shared: &Shared, pace: &P) {
    let mut placed = 0;
    let mut realtime = false;
    let mut forecast = Forecast::default();
    let mut wake_at = None;
    loop {
        let Some(start) = await_frame(shared, pace, wake_at) else {
            return;
        };
        shared.wake_at.store(AWAKE, Ordering::SeqCst);
        let generation = shared.generation.load(Ordering::Acquire);
        if generation != placed {
            let taken = take_placement(shared);
            realtime = pace.realtime(taken.policy);
            if let Some(record) = PLACEMENTS.get(shared.record.load(Ordering::Acquire))
                && record.generation.load(Ordering::Acquire) == generation
                && record.pipeline.load(Ordering::Relaxed) == shared.id
            {
                record
                    .applied_at_ns
                    .store(super::super::timing::monotonic(), Ordering::Relaxed);
                record
                    .requested_cpus
                    .store(taken.requested_cpus, Ordering::Relaxed);
                record.applied_cpus.store(taken.cpus, Ordering::Relaxed);
                record
                    .applied_policy
                    .store(i64::from(taken.policy), Ordering::Relaxed);
                record
                    .applied_priority
                    .store(i64::from(taken.priority), Ordering::Relaxed);
                record
                    .affinity_result
                    .store(i64::from(taken.affinity), Ordering::Relaxed);
                record
                    .sched_result
                    .store(i64::from(taken.sched), Ordering::Relaxed);
            }
            shared.applied.store(generation, Ordering::Release);
            store_placement(&HELPER_PLACEMENT);
            placed = generation;
        }
        let started = pace.now();
        shared.started.store(started, Ordering::Relaxed);
        let queued_at = shared.queued_at.load(Ordering::Relaxed);
        if queued_at != 0 {
            let latency = started.saturating_sub(queued_at);
            WAKE_LATENCY.record(latency);
            let (kind, woken) = match start {
                Start::Queued => (STARTS_QUEUED, false),
                Start::Slept(wake_at) if started >= wake_at => {
                    TIMER_LATE.record(started - wake_at);
                    (STARTS_TIMER, false)
                }
                Start::Slept(_) => (STARTS_WOKEN_EARLY, true),
                Start::Parked { expired: true } => (STARTS_PARKED_AFTER_TIMER, true),
                Start::Parked { expired: false } => (STARTS_PARKED_NO_FORECAST, true),
            };
            STARTS[kind].fetch_add(1, Ordering::Relaxed);
            if woken {
                WOKEN_LATENCY.record(latency);
            } else {
                SELF_LATENCY.record(latency);
            }
        }
        // SAFETY: the callback handed the frame over with QUEUED and does not
        // touch it again until it observes IDLE.
        let handoff = unsafe { &mut *shared.handoff.load(Ordering::Relaxed) };
        #[cfg(test)]
        shared
            .ran
            .lock()
            .unwrap()
            .push((handoff.seq, start, started));
        let slot = (handoff.seq & 1) as usize;
        let left_alone = if handoff.native_success {
            Job::Count
        } else {
            Job::None
        };
        for job in handoff.jobs.iter_mut().filter(|job| **job == Job::Skipped) {
            *job = left_alone;
        }
        // Process source inputs first since the next callback reads them.
        for wanted in [Job::Input, Job::Meter, Job::Count, Job::None] {
            for (work, job) in handoff.work.iter_mut().zip(&handoff.jobs) {
                if let Some(work) = work
                    && *job == wanted
                {
                    work.run(
                        *job,
                        slot,
                        handoff.frames,
                        handoff.native_success,
                        &mut handoff.counters,
                    );
                }
            }
        }
        handoff.jobs.fill(Job::Skipped);
        handoff.done_seq = handoff.seq;
        let finished = pace.now();
        let spent = finished.saturating_sub(started);
        shared.finished.store(finished, Ordering::Relaxed);
        HELPER_TIME.frames.fetch_add(1, Ordering::Relaxed);
        HELPER_TIME.total_ns.fetch_add(spent, Ordering::Relaxed);
        HELPER_TIME.max_ns.fetch_max(spent, Ordering::Relaxed);
        if handoff.cycle_start != 0 {
            let position = finished.saturating_sub(handoff.cycle_start);
            super::super::probe::HELPER_POS.record(position);
            if position >= super::super::probe::HELPER_LATE_NS {
                super::super::probe::HELPER_LATE_FINISHES.fetch_add(1, Ordering::Relaxed);
                super::super::probe::record(super::super::probe::Kind::HelperLateFinish, position);
            }
        }
        // Compute next wake time while we still own `handoff`.
        wake_at = forecast.observe(handoff.started, shared.ended.load(Ordering::Relaxed));
        if !realtime && pace.needs_realtime() {
            wake_at = None;
        }
        if queued_at != 0
            && let Some(deviation) = forecast.deviation
        {
            let gauge = if deviation >= 0 {
                &HANDOFF_LATE
            } else {
                &HANDOFF_EARLY
            };
            gauge.record(deviation.unsigned_abs());
        }
        shared.state.store(IDLE, Ordering::Release);
        pace.rest();
    }
}
