//! The helper thread that processes sources from the callback's copies: its
//! handoff with the callback, its sleep, and its wake.
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
/// The longest a callback waits for a busy helper before passing its frame
/// through.
pub(super) const HELPER_WAIT_NS: u64 = 200_000;
/// One callback period.
pub(super) const PERIOD_NS: u64 = super::super::timing::BUDGET_NS;
/// `Shared::wake_at` while the helper runs, or is about to look at `state`.
pub(super) const AWAKE: u64 = 0;
/// `Shared::wake_at` while the helper is parked with no timer: only the
/// callback's wake starts it.
pub(super) const PARKED: u64 = u64::MAX;
/// How long after the forecast end of the next callback the helper's timer
/// wakes it, so that the frame is normally queued by then and the callback
/// made no system call for it (on the mixer the wake cost the callback 12 us
/// at its very end, every cycle).
///
/// Measured on the mixer (2026-11-12, 10-minute blocks with the denoiser
/// hold at 2.5 ms): 50, 100 and 200 us all saved the callback the same 10 to
/// 11 us against the callback's wake, and 50 us had the fewest callbacks that
/// started late; at 400 us the timer is set past LATEST_SELF_START_NS, so
/// the callback wakes the helper every time and nothing is saved. A timer
/// that fires before the handoff finds nothing and leaves the helper parked
/// for the callback to wake, as before. The helper starts about 50 us after
/// the handoff instead of the 27 us a wake took.
pub(crate) const WAKE_MARGIN_NS: u64 = 50_000;
/// The latest the helper may start a frame by itself, from the start of the
/// callback that hands the frame off. The callback wakes a helper whose timer
/// is set later than this, as it wakes a parked one.
///
/// Callbacks start a period apart, and never less than half a period apart
/// in the 67 million of one 43-hour run (13,366 between 0.5 and 0.9 of a
/// period, against 14,174 between 1.1 and 1.5: mostly a late callback and
/// the short gap after it, and a late callback starts the forecast over).
/// A helper that starts by this bound and takes its usual 240 us (213 us for
/// the frame, 27 us to get its core) is done 990 us before a callback one
/// period later, and 173 us after the start of one half a period later: less
/// than that callback waits (HELPER_WAIT_NS), so its frame is not passed
/// through unprocessed. It is also no later than the wake from a callback
/// that ends 1100 us after its start, which is an ordinary callback today.
///
/// That holds for the usual frame only. A frame longer than 240 us, met by a
/// callback half a period early, is passed through where a woken helper would
/// have had it done (it started some 200 us sooner): the price of the timer,
/// to be judged on the mixer by `helper.stalls` and `helperSlack`.
pub(super) const LATEST_SELF_START_NS: u64 = PERIOD_NS / 2 + HELPER_WAIT_NS - 266_666;
/// A callback that ends further than this from one period after the last
/// counts as irregular (a late start, a skipped or doubled cycle, a restart):
/// the forecast starts over from it. A tenth of a period, the band the
/// callback gap counters use.
pub(super) const IRREGULAR_NS: u64 = PERIOD_NS / 10;
/// Regular callbacks in a row before the helper sleeps on a forecast (37 ms).
/// Until then it parks, and every callback wakes it.
pub(super) const FORECAST_WARMUP: u32 = 16;

/// Where the next callback will end, from where the last ones did.
///
/// The callbacks' ends lie one period apart, give or take the jitter of the
/// cycle's start and of the callback's length; the anchor follows them with
/// a weight of an eighth for each new one. Anything irregular starts it over,
/// and the helper then parks as it always did.
#[derive(Default, Debug)]
pub(super) struct Forecast {
    /// The smoothed end of the latest callback, 0 with no history.
    pub(super) anchor: u64,
    /// Regular callbacks in a row.
    pub(super) streak: u32,
    /// When the callback of the frame before started.
    previous: u64,
    /// The latest callback's end after (positive) or before the forecast, for
    /// the bench gauges; None for a frame that had no forecast to compare.
    pub(super) deviation: Option<i64>,
}
impl Forecast {
    /// After a frame: `begun` is when its callback started, `ended` the
    /// latest end stamp published (`Shared::ended`). Returns when the helper
    /// should wake by itself for the next frame, or None to park.
    pub(super) fn observe(&mut self, begun: u64, ended: u64) -> Option<u64> {
        // A callback publishes its end after it has handed its frame off, so
        // a short frame can be over first, and the stamp still that of the
        // callback before: this one is then taken to be as long as that one.
        // A stamp is a callback's own between its start and one period on;
        // anything else (no stamp, a callback that found the helper busy in
        // between) is no sample.
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
            // The first callback, or an irregular one.
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
        // Not when the next callback would have to wake the helper anyway.
        (wake_at <= begun + PERIOD_NS + LATEST_SELF_START_NS).then_some(wake_at)
    }
}

/// Whether a callback that started at `begun` must wake a helper that
/// published `wake_at`: when it is parked, or sleeps towards a wake-up later
/// than it may start by itself. A helper that is awake finds the frame.
pub(super) fn must_wake(wake_at: u64, begun: u64) -> bool {
    wake_at == PARKED || (wake_at != AWAKE && wake_at > begun.saturating_add(LATEST_SELF_START_NS))
}
/// What `Helper::queue` did about the helper's sleep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Woke {
    /// Nothing: no system call.
    No,
    /// Woke it out of its park.
    Parked,
    /// Woke it out of a timed sleep that would have ended too late.
    Late,
}
/// How the helper came by a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Start {
    /// It was queued when the helper looked, before any sleep.
    Queued,
    /// During the timed sleep towards this wake-up: at its end, or earlier if
    /// the callback woke the helper.
    Slept(u64),
    /// While parked; `expired`: after a timed sleep that ended with nothing
    /// queued.
    Parked { expired: bool },
}
/// The helper's clock and sleeps, which the tests replace.
pub(super) trait Pace {
    /// Once, on the helper thread, before its first sleep.
    fn prepare(&self) {}
    /// CLOCK_MONOTONIC in nanoseconds.
    fn now(&self) -> u64;
    /// Sleeps until `wake_at` at the latest. `Thread::unpark` ends it, and it
    /// may end early for no reason.
    fn sleep_until(&self, wake_at: u64);
    /// Sleeps until `Thread::unpark`; it may end for no reason.
    fn park(&self);
    /// After each frame, once it is handed back and before the helper turns
    /// to its sleep. Nothing here; a test takes its time, so that the next
    /// handoff meets every step of the way to sleep.
    fn rest(&self) {}
    /// Whether the timed sleep is on time only for a real-time thread: the
    /// helper then parks, as it always did, until it has been made one.
    fn needs_realtime(&self) -> bool {
        false
    }
    /// Whether a thread that has `policy` after taking its placement is a
    /// real-time one. The kernel's rule; a test, which cannot make its
    /// threads real-time, says which of its placements count.
    fn realtime(&self, policy: i32) -> bool {
        matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR)
    }
}
/// The mixer's: a futex wait, with the kernel's timer for the timed one. For
/// a real-time thread on PREEMPT_RT that timer is a hard interrupt on the
/// core the helper went to sleep on.
pub(super) struct Kernel;
impl Pace for Kernel {
    fn prepare(&self) {
        // A futex wait's timeout is rounded by the thread's timer slack,
        // 50 us by default on this kernel even for a real-time thread.
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
        // An ordinary thread's timer waits for the kernel's timer thread,
        // which every real-time thread on that core holds up: so not before
        // the helper has taken its placement (the first 64 callbacks or
        // more). The hosts the tests run on have no such kernel, and give
        // the helper no real-time placement.
        !cfg!(test)
    }
}
pub(super) struct Shared {
    pub(super) state: AtomicU8,
    handoff: AtomicPtr<Handoff>,
    pub(super) stop: AtomicBool,
    /// CLOCK_MONOTONIC when the helper began and finished its latest frame.
    pub(super) started: AtomicU64,
    pub(super) finished: AtomicU64,
    /// Bench gauges only: CLOCK_MONOTONIC just before the callback handed
    /// off the frame now queued. 0 while the gauges are off.
    pub(super) queued_at: AtomicU64,
    /// What the helper is doing about its next frame: AWAKE, PARKED, or the
    /// CLOCK_MONOTONIC time its timer ends its sleep (see `await_frame`).
    pub(super) wake_at: AtomicU64,
    /// CLOCK_MONOTONIC at the end of the latest callback (`Pipeline::ended`),
    /// 0 before the first: what the helper forecasts the next handoff from.
    pub(super) ended: AtomicU64,
    /// Every frame the helper ran, in order: its sequence number, how it
    /// came by it and when it started.
    #[cfg(test)]
    pub(super) ran: std::sync::Mutex<Vec<(u64, Start, u64)>>,
    /// Set when the helper's thread has returned from its loop.
    #[cfg(test)]
    pub(super) gone: AtomicBool,
    /// The callback thread's CPUs, policy and priority; `generation` counts
    /// changes, and the helper derives its placement from each one.
    pub(super) generation: AtomicU64,
    /// The generation whose placement the helper last applied.
    pub(super) applied: AtomicU64,
    /// PLACEMENTS index of the latest generation's record, or usize::MAX.
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
    /// Spin until the helper finishes, at most `limit` ns. The helper runs on
    /// another core, so spinning here does not hold it up.
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
    /// From the callback thread, when `sampler` says so (every
    /// PLACE_FAST_EVERY callbacks until settled, then every PLACE_EVERY; a
    /// boundary without a handoff at the next callback with one, since the
    /// helper reads the placement only when it starts a queued frame):
    /// record its placement for the helper, which follows any change.
    pub(super) fn place(&self, cycle: u32, sampler: &mut Sampler) {
        let Some(deferred) = sampler.take(cycle) else {
            return;
        };
        PLACEMENT_SAMPLES.fetch_add(1, Ordering::Relaxed);
        store_placement(&CALLBACK_PLACEMENT);
        {
            let (mask, policy, priority) = current_placement();
            // As before: an unreadable policy counts as ordinary.
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
                // Claim a record only while any are left.
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
    /// Hands the frame to the helper, and wakes it only if it would not start
    /// in time by itself (`must_wake`): normally it sleeps towards a timer set
    /// just after this call, and the callback makes no system call. `begun`:
    /// when this callback started. `queued_at`: with the bench gauges on, the
    /// time of this call, for the helper's wake latency; 0 otherwise, and then
    /// nothing is stored.
    ///
    /// QUEUED is stored before `wake_at` is read, both SeqCst: `await_frame`
    /// has the other half and the reason.
    pub(super) fn queue(&self, handoff: *mut Handoff, begun: u64, queued_at: u64) -> Woke {
        let shared = &*self.shared;
        shared.handoff.store(handoff, Ordering::Relaxed);
        if queued_at != 0 {
            // Before QUEUED, so that the helper never reads an older stamp.
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
    /// Tells the helper to stop and waits until its thread has returned: from
    /// then on it touches no frame. Control only, after callbacks have stopped.
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
/// Waits for the next frame: asleep until `wake_at` if there is a forecast,
/// then parked. None when the helper is to stop: it then starts no frame,
/// queued or not, since the pipeline that owns the frames is going.
///
/// No wake is lost. Before every sleep the helper stores what it is about to
/// do in `wake_at` and then reads `state`; the callback stores QUEUED in
/// `state` and then reads `wake_at` (`Helper::queue`). All four are SeqCst,
/// so they have one order, and in it either the helper's read comes after the
/// callback's store, and the helper sees QUEUED and does not sleep; or it
/// comes before, and then the helper's store, which is earlier still, comes
/// before the callback's read. The callback then sees exactly the sleep the
/// helper is entering: PARKED, and it unparks; or a timer, and it unparks
/// unless that timer is soon enough (`must_wake`), in which case the timer
/// itself ends the sleep. An unpark that comes before the sleep begins is
/// kept by the thread's token and ends it at once. With a weaker ordering
/// both sides could read the other's old value: the helper parks, the
/// callback does not wake it, and the frame waits for a wake that never comes.
///
/// No frame runs twice: the helper runs one only on reading QUEUED, stores
/// IDLE when it is done, and only the callback stores QUEUED, after reading
/// IDLE. A wake with nothing queued sends the helper back to sleep.
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
            // The timer has ended the sleep with nothing queued (the callback
            // is late, or there is none): park. Also if the clock makes no
            // sense: a forecast is never two periods ahead.
            let now = pace.now();
            if now >= wake_at || wake_at - now > 2 * PERIOD_NS {
                break;
            }
            // Also ended by the callback's wake, by an older wake's token, or
            // for no reason: each time round looks again.
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
    // Whether the placement the helper last took left it real-time.
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
        // Sources first: the next callback reads them.
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
        // While the frame is still the helper's: when to wake for the next.
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
