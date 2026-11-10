//! Bounded callback endpoint and off-callback model worker.
//!
//! Construct and retire on the control thread. `process_sample` only moves
//! bounded frames through SPSC queues and performs aligned dry/wet blending.
//! Explicit `finish` is the observable shutdown path; timeout is unresolved
//! ownership and must prevent starting a replacement worker.

mod capture;
mod inline;
mod metrics;
mod model;
mod outlier;
mod queue;
mod scheduling;
mod wake;
pub use capture::{
    CAPTURE_FRAMES, CAPTURE_POST_SEQUENCES, CaptureFrame, CaptureMetadata, CaptureState,
    CaptureWindow,
};
pub use inline::Inline;
pub use metrics::Timing;
use model::Model;
pub use model::VendorModel;
pub use outlier::{EventHistory, EventRecord, FrameRecord, HISTORY, Outliers, Recorded};
use queue::{Consumer, Frame, Producer};
pub use rc2fx_dfn::LayerPause;
use rc2fx_dsp::{MAX_SUPPRESSION_DELAY, RetirementError, Suppression};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidAllowance,
    InvalidFrame,
    ModelGeometry,
    ModelCreation,
    ModelFailed,
    StartupFailed,
    Scheduling,
    ThreadCreation,
    StartupTimeout,
    StartupTerminationUnresolved,
    JoinTimeout,
    WorkerPanicked,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub sample_rate: u32,
    pub frame_samples: usize,
    pub intrinsic_delay_samples: usize,
    pub worker_lead_samples: usize,
}
impl Geometry {
    /// `lookahead` is DeepFilterNet3's (0 or 1); it adds one hop of intrinsic
    /// delay, and so of latency, for 1.
    pub fn for_lookahead(lookahead: u8) -> Result<Self, Error> {
        rc2fx_core::Suppression::admitted_geometry(lookahead)
            .map(Self::from)
            .ok_or(Error::ModelGeometry)
    }
    /// Actual worker geometries originate in admitted core metadata or a
    /// validated provisional allowance, so every field fits the core type.
    pub fn resolved(self) -> rc2fx_core::SuppressionGeometry {
        rc2fx_core::SuppressionGeometry {
            sample_rate: self.sample_rate,
            frame_samples: self
                .frame_samples
                .try_into()
                .expect("bounded frame geometry"),
            intrinsic_delay_samples: self
                .intrinsic_delay_samples
                .try_into()
                .expect("bounded intrinsic geometry"),
            worker_lead_samples: self
                .worker_lead_samples
                .try_into()
                .expect("bounded allowance geometry"),
        }
    }
    pub fn latency_samples(self) -> usize {
        self.frame_samples + self.intrinsic_delay_samples + self.worker_lead_samples
    }
    pub fn frame_budget_ns(self) -> u64 {
        self.frame_samples as u64 * 1_000_000_000 / self.sample_rate as u64
    }
    pub fn worker_allowance_ns(self) -> u64 {
        self.worker_lead_samples as u64 * 1_000_000_000 / self.sample_rate as u64
    }
}
impl From<rc2fx_core::SuppressionGeometry> for Geometry {
    fn from(value: rc2fx_core::SuppressionGeometry) -> Self {
        Self {
            sample_rate: value.sample_rate,
            frame_samples: value.frame_samples as usize,
            intrinsic_delay_samples: value.intrinsic_delay_samples as usize,
            worker_lead_samples: value.worker_lead_samples as usize,
        }
    }
}

/// Applied and read back inside the new worker before it is published. None
/// preserves the creating thread's scheduling; `realtime_priority` requests
/// SCHED_RR at that priority, `nice` requests SCHED_OTHER at that nice value
/// (the two are exclusive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkerOptions {
    pub cpu_mask: Option<u64>,
    pub realtime_priority: Option<u8>,
    pub nice: Option<i8>,
}
/// Whether workers started from now on time each preparation step; see
/// `set_preparation_step_timing`.
static STEP_TIMING: AtomicBool = AtomicBool::new(true);

/// Turns the per-step preparation timing (`Preparation::step_wall`) on or
/// off for workers started afterwards. It costs the worker one clock read
/// and one histogram update per step, five of each per frame for
/// DeepFilterNet3, and only a bench run reads the result, so a host that can
/// tell a bench run from a production run should turn it off for production.
/// It is on until the host says otherwise, so that no report loses
/// `step_wall` without the host having decided it. The per-pass timings and
/// the step counts are recorded either way. Control thread only; a running
/// worker keeps the setting it started with.
pub fn set_preparation_step_timing(on: bool) {
    STEP_TIMING.store(on, Ordering::Relaxed);
}

/// The mixer's suppression worker: SCHED_RR 5, pinned to CPU 0.
///
/// RR 5 sits below RØDE's audio threads (RR 50, JACK's driver FIFO 51), so
/// they always win the core. CPU 0 carries the USB interrupt thread and
/// RØDE's ordinary-priority threads (GUI, pads, HID) but none of the
/// real-time audio threads, which live on CPUs 1 to 3 with the callback on
/// CPU 2. Measured on the mixer with DeepFilterNet3 on mic-1 and USB
/// playback (2026-10-29), xruns per minute:
///
/// - RR 5 floating over CPUs 1–3, f16 weights: about 2,700. The worker
///   migrated about 2,000 times a second and never missed a deadline.
/// - nice 0 on CPU 3: 119, and under the RR 50 threads sharing the core the
///   worker missed 57% of its deadlines, so most frames played dry.
/// - RR 5 on CPU 3, f16: 172 to 221, worker 1% late. Memory traffic, not
///   scheduling: int8 weights (`rc2fx-dfn`) on the
///   same core gave 15 with playback and 0 without, the worker never late.
/// - RR 5 on CPU 1 or 3, all weights int8: 76 to 81, worker 3 to 10% late,
///   preempted for whole frames by jackd and the USB audio threads there.
/// - RR 5 on CPU 0, all weights int8: 1.5 to 9, worker never late. CPU 0
///   runs at about 95% while the model is on, about 82% of it real-time
///   (the worker 57%, the USB gadget's interrupt thread 22%) against the
///   kernel's 95% real-time budget; a worker that spins instead of sleeping
///   crosses it and the throttle stalls the gadget thread, wedging USB.
/// - Idling between layers (sleep or spin), an ordinary-priority worker on
///   CPU 0, moving RØDE's threads: all worse. The xruns are the callback on
///   CPU 2 stalling on the shared L2 and DRAM while the weights stream, not
///   the worker being late.
///
/// Pinning does not reserve the core and the four cores share one L2: the
/// callback on CPU 2 costs about 20% more CPU time while DeepFilterNet3
/// runs.
pub const PRODUCTION_WORKER: WorkerOptions = WorkerOptions {
    cpu_mask: Some(0b0001),
    realtime_priority: Some(5),
    nice: None,
};
/// An injected host can pin its control thread separately from audio workers.
/// KernelAllowed applies only to the new worker, respects its kernel cpuset and
/// requires exact affinity readback. It never changes the creating thread.
#[derive(Clone, Copy, Debug)]
pub enum AffinityScope {
    Inherited,
    KernelAllowed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub completed_frames: u64,
    pub input_drops: u64,
    pub output_drops: u64,
    /// Frames whose result was not there when their first sample played.
    pub missed_deadlines: u64,
    /// Of those, frames whose result arrived in time for some later sample
    /// of the frame; the rest of the frame then played processed.
    pub late_results: u64,
    pub stale_results: u64,
    pub failed: bool,
    pub wall: Timing,
    pub cpu: Timing,
    pub allowance_overruns: u64,
    /// Submission (before queue copy) to worker dequeue, including scheduling.
    pub queue_wait: Timing,
    /// Submission to successful result publication, including model execution.
    pub turnaround: Timing,
}
/// Staged preparation after each published frame. None of it is in `Snapshot::cpu` or
/// `Snapshot::wall`, which time the model's `process` only; `total_cpu_ns`
/// is the worker's CPU time in both. Live values are individually atomic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preparation {
    pub enabled: bool,
    /// Wall and thread CPU time per preparation pass (one per publication
    /// that started preparing), against the frame period. The CPU time runs
    /// from the end of the frame's `process`, so it also holds the
    /// publication and bookkeeping between the two (a few microseconds).
    pub pass_wall: Timing,
    pub pass_cpu: Timing,
    /// Whether this worker times each step (`set_preparation_step_timing`).
    /// When false, `step_wall` stays empty: that is "not measured", not "no
    /// slow steps". `pass_wall.max_ns` still bounds the longest step.
    pub step_timing: bool,
    /// Wall time of each step against a sixteenth of the frame period;
    /// `max_ns` is the longest step, which arriving input may wait out.
    /// Recorded only while `step_timing` is on.
    pub step_wall: Timing,
    pub steps: u64,
    /// Publications with preparation pending but input queued (or a stop or
    /// failure) already: no step ran.
    pub skipped: u64,
    /// Passes ended by arriving input, a stop or a failure with preparation
    /// still pending.
    pub abandoned: u64,
    /// `Snapshot::cpu.total_ns` plus `pass_cpu.total_ns`.
    pub total_cpu_ns: u64,
}
#[derive(Default)]
struct Preparing {
    pass_wall: metrics::Counters,
    pass_cpu: metrics::Counters,
    step_wall: metrics::Counters,
    /// Set before the worker starts; see `set_preparation_step_timing`.
    step_timing: AtomicBool,
    steps: AtomicU64,
    skipped: AtomicU64,
    abandoned: AtomicU64,
}
#[derive(Default)]
struct Shared {
    wake: wake::Wake,
    stop: AtomicU8,
    termination: AtomicU8,
    failed: AtomicBool,
    completed: AtomicU64,
    input_drops: AtomicU64,
    output_drops: AtomicU64,
    missed: AtomicU64,
    late: AtomicU64,
    stale: AtomicU64,
    wall: metrics::Counters,
    cpu: metrics::Counters,
    allowance_overruns: AtomicU64,
    queue_wait: metrics::Counters,
    turnaround: metrics::Counters,
    worker_outliers: outlier::WorkerOutliers,
    endpoint_outliers: outlier::EndpointOutliers,
    capture: capture::Capture,
    terminal: OnceLock<Terminal>,
    prep: Preparing,
}
/// Live snapshots are individually atomic, not transactions. Final joined
/// snapshots are quiescent and suitable for exact accounting.
#[derive(Clone)]
pub struct Telemetry(Arc<Shared>);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    Running,
    Stopping,
    TimedOut,
    Joined,
}
impl Telemetry {
    /// Published exactly once by control-side retirement after the worker joins.
    /// A live atomic snapshot or a timed-out join is never a terminal receipt.
    pub fn terminal(&self) -> Option<Terminal> {
        self.0.terminal.get().copied()
    }
    /// Retain this handle across control-side Dsp destruction. Only Joined
    /// confirms worker ownership ended; TimedOut must block replacement.
    pub fn termination(&self) -> Termination {
        match self.0.termination.load(Ordering::Acquire) {
            0 => Termination::Running,
            1 => Termination::Stopping,
            2 => Termination::TimedOut,
            _ => Termination::Joined,
        }
    }
    /// The slowest and latest slow frames and the latest missed and late
    /// deadlines. Never waits for a writer: a record mid-update reads Busy.
    pub fn outliers(&self) -> Outliers {
        outlier::snapshot(&self.0.worker_outliers, &self.0.endpoint_outliers)
    }
    /// Developer capture only; never resets counters or changes audio settings.
    pub fn arm_capture(&self, epoch: u32) -> bool {
        self.0.capture.arm(epoch)
    }
    pub fn capture_identity(&self) -> (u64, u32) {
        self.0.capture.identity()
    }
    pub fn capture_metadata(&self) -> Recorded<CaptureMetadata> {
        self.0.capture.metadata()
    }
    /// Cold diagnostic path, not routine telemetry: allocates a bounded copy.
    pub fn capture_window(&self) -> Recorded<CaptureWindow> {
        self.0.capture.snapshot()
    }
    pub fn preparation(&self) -> Preparation {
        let (s, p) = (&self.0, &self.0.prep);
        let pass_cpu = p.pass_cpu.snapshot();
        Preparation {
            enabled: true,
            pass_wall: p.pass_wall.snapshot(),
            pass_cpu,
            step_timing: p.step_timing.load(Ordering::Relaxed),
            step_wall: p.step_wall.snapshot(),
            steps: p.steps.load(Ordering::Relaxed),
            skipped: p.skipped.load(Ordering::Relaxed),
            abandoned: p.abandoned.load(Ordering::Relaxed),
            total_cpu_ns: s.cpu.snapshot().total_ns + pass_cpu.total_ns,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let s = &self.0;
        Snapshot {
            completed_frames: s.completed.load(Ordering::Relaxed),
            input_drops: s.input_drops.load(Ordering::Relaxed),
            output_drops: s.output_drops.load(Ordering::Relaxed),
            missed_deadlines: s.missed.load(Ordering::Relaxed),
            late_results: s.late.load(Ordering::Relaxed),
            stale_results: s.stale.load(Ordering::Relaxed),
            failed: s.failed.load(Ordering::Acquire),
            wall: s.wall.snapshot(),
            cpu: s.cpu.snapshot(),
            allowance_overruns: s.allowance_overruns.load(Ordering::Relaxed),
            queue_wait: s.queue_wait.snapshot(),
            turnaround: s.turnaround.snapshot(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopMode {
    Cancel,
    Drain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub stop_mode: StopMode,
    /// Remains true even when a later observation succeeds in joining.
    pub retirement_timed_out: bool,
    pub joined: bool,
    pub drained: bool,
    pub input_samples: u64,
    pub full_input_frames: u64,
    pub partial_input_samples: usize,
    pub submitted_frames: u64,
    pub pending_input_frames: u32,
    pub snapshot: Snapshot,
}

pub struct Worker {
    geometry: Geometry,
    shared: Arc<Shared>,
    input: Producer,
    output: Consumer,
    thread: Option<JoinHandle<()>>,
    assembling: Frame,
    playing: Frame,
    /// The frame now playing had no result when it started; its result is
    /// still looked for at every sample until it arrives or the frame ends.
    pending: Option<u64>,
    phase: usize,
    frame: u64,
    sample: u64,
    /// The next sample's place in the output: its offset in the frame that
    /// plays (`out_phase`) and that frame's sequence (`due`). Output starts
    /// one frame and the worker lead after input, at sample `start`; from
    /// there these are `(sample - start) % frame_samples` and
    /// `(sample - start) / frame_samples`, kept as counters so that no
    /// sample needs a division. Both are zero until then.
    out_phase: usize,
    due: u64,
    dry: [f32; MAX_SUPPRESSION_DELAY],
    dry_position: usize,
    /// Voice score of the frame the last output sample came from; None while
    /// that sample was the delayed original.
    voice: Option<f32>,
    finished: bool,
    retirement_mode: Option<StopMode>,
    retirement_timed_out: bool,
}
impl Worker {
    /// A threaded worker for the fixed geometry; `Endpoint::new` picks this
    /// or the inline endpoint by the geometry's worker lead.
    pub fn new(lookahead: u8, options: WorkerOptions) -> Result<Self, Error> {
        Self::new_with_affinity(lookahead, options, AffinityScope::Inherited)
    }
    pub fn new_with_affinity(
        lookahead: u8,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        Self::new_with_affinity_and_lead(lookahead, None, options, scope)
    }
    /// `lead` replaces the model's scheduling allowance, for testing or
    /// benchmarking a shorter or longer one; the prepared DSP must be
    /// built with the same value. None keeps the admitted allowance.
    pub fn new_with_affinity_and_lead(
        lookahead: u8,
        lead: Option<usize>,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        let mut geometry = Geometry::for_lookahead(lookahead)?;
        if let Some(lead) = lead {
            geometry.worker_lead_samples = lead;
        }
        if geometry.worker_lead_samples == 0 {
            return Err(Error::InvalidAllowance);
        }
        scheduling::validate(options)?;
        let model = VendorModel::new(lookahead)?;
        Self::spawn_with_affinity(geometry, Box::new(model), options, scope)
    }
}
impl Worker {
    /// A worker with an explicit geometry (a lead override); the model and
    /// the DSP prepared for the same geometry must match it.
    fn with_geometry(
        lookahead: u8,
        geometry: Geometry,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        scheduling::validate(options)?;
        let model = VendorModel::new(lookahead)?;
        Self::spawn_with_affinity(geometry, Box::new(model), options, scope)
    }
}
impl Worker {
    #[cfg(test)]
    fn spawn(
        geometry: Geometry,
        model: Box<dyn Model>,
        options: WorkerOptions,
    ) -> Result<Self, Error> {
        Self::spawn_with_affinity(geometry, model, options, AffinityScope::Inherited)
    }
    fn spawn_with_affinity(
        geometry: Geometry,
        model: Box<dyn Model>,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        let shared = Arc::new(Shared::default());
        shared
            .prep
            .step_timing
            .store(STEP_TIMING.load(Ordering::Relaxed), Ordering::Relaxed);
        let (input, worker_input) = queue::channel();
        let (worker_output, output) = queue::channel();
        let worker_shared = shared.clone();
        // Control-only readiness publication. Avoid parking the creating
        // thread in an mpsc receiver: native hosts may own thread-local state
        // in a different ELF namespace. The worker publishes one immutable
        // result; no callback participates in this bounded startup wait.
        let startup = Arc::new(OnceLock::new());
        let worker_startup = Arc::clone(&startup);
        let thread = thread::Builder::new()
            .name("rc2fx-ns-worker".into())
            .spawn(move || {
                let mut model = model;
                let configured = match scope {
                    AffinityScope::Inherited => scheduling::apply(options),
                    AffinityScope::KernelAllowed => scheduling::apply_kernel_allowed(options),
                }
                .and_then(|()| model.warm_thread());
                let good = configured.is_ok();
                let _ = worker_startup.set(configured);
                if !good {
                    return;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(model, worker_input, worker_output, &worker_shared, geometry)
                }));
                if result.is_err() {
                    worker_shared.failed.store(true, Ordering::Release);
                }
            })
            .map_err(|_| Error::ThreadCreation)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let ready = loop {
            if let Some(result) = startup.get() {
                break *result;
            }
            if thread.is_finished() {
                break Err(Error::StartupFailed);
            }
            if Instant::now() >= deadline {
                break Err(Error::StartupTimeout);
            }
            thread::sleep(Duration::from_millis(1));
        };
        if let Err(e) = ready {
            shared.stop.store(1, Ordering::Release);
            if !shared.wake.notify() {
                shared.failed.store(true, Ordering::Release);
            }
            // Never wait indefinitely for an unpublished thread. Its owned Arc
            // queue/model state remains alive until it actually exits.
            let end = Instant::now() + Duration::from_secs(2);
            while !thread.is_finished() && Instant::now() < end {
                thread::sleep(Duration::from_millis(1));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                return Err(Error::StartupTerminationUnresolved);
            }
            return Err(e);
        }
        Ok(Self {
            geometry,
            shared,
            input,
            output,
            thread: Some(thread),
            assembling: Frame::default(),
            playing: Frame::default(),
            pending: None,
            phase: 0,
            frame: 0,
            sample: 0,
            out_phase: 0,
            due: 0,
            dry: [0.; MAX_SUPPRESSION_DELAY],
            dry_position: 0,
            voice: None,
            finished: false,
            retirement_mode: None,
            retirement_timed_out: false,
        })
    }
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }
    pub fn latency_samples(&self) -> usize {
        self.geometry.latency_samples()
    }
    pub fn telemetry(&self) -> Telemetry {
        Telemetry(self.shared.clone())
    }
    pub fn voice(&self) -> Option<f32> {
        self.voice
    }
    /// Caller must first stop callback access (the mutable borrow enforces no
    /// concurrent safe-Rust access). Drain only complete queued input frames;
    /// retain original output-queue overflow faults during shutdown.
    pub fn finish(&mut self, mode: StopMode, timeout: Duration) -> Result<Terminal, Error> {
        if let Some(terminal) = self.shared.terminal.get() {
            return Ok(*terminal);
        }
        // Retirement is irreversible, including when a bounded wait times out.
        // Safe callers cannot accidentally admit new frames into a draining queue.
        self.finished = true;
        let mode = *self.retirement_mode.get_or_insert(mode);
        let mut panicked = false;
        if let Some(handle) = self.thread.as_ref() {
            self.shared.termination.store(1, Ordering::Release);
            self.shared.stop.store(
                if mode == StopMode::Drain { 2 } else { 1 },
                Ordering::Release,
            );
            if !self.shared.wake.notify() {
                self.shared.failed.store(true, Ordering::Release);
            }
            let Some(end) = Instant::now().checked_add(timeout) else {
                self.retirement_timed_out = true;
                self.shared.termination.store(2, Ordering::Release);
                return Err(Error::JoinTimeout);
            };
            if !completion_observed_before_deadline(
                end,
                || handle.is_finished(),
                Instant::now,
                || thread::sleep(Duration::from_millis(1)),
            ) {
                self.shared.termination.store(2, Ordering::Release);
                self.retirement_timed_out = true;
                return Err(Error::JoinTimeout);
            }
            let result = self.thread.take().expect("owned handle").join();
            self.finished = true;
            self.shared.termination.store(3, Ordering::Release);
            if result.is_err() {
                self.shared.failed.store(true, Ordering::Release);
                panicked = true;
            }
        }
        let snapshot = self.telemetry().snapshot();
        let submitted_frames = self.frame - snapshot.input_drops;
        let pending_input_frames = self.input.pending();
        let terminal = Terminal {
            stop_mode: mode,
            retirement_timed_out: self.retirement_timed_out,
            joined: true,
            drained: pending_input_frames == 0
                && snapshot.completed_frames == submitted_frames
                && !snapshot.failed,
            input_samples: self.sample,
            full_input_frames: self.frame,
            partial_input_samples: self.phase,
            submitted_frames,
            pending_input_frames,
            snapshot,
        };
        self.shared
            .terminal
            .set(terminal)
            .expect("unique control-side terminal writer");
        if panicked {
            Err(Error::WorkerPanicked)
        } else {
            Ok(terminal)
        }
    }
    /// Takes frame `due` from the output queue if it has arrived, discarding
    /// older results on the way. Leaves `playing` invalid otherwise.
    fn choose(&mut self, due: u64) -> bool {
        self.playing.valid = false;
        for _ in 0..queue::CAPACITY {
            let Some(sequence) = self.output.sequence() else {
                break;
            };
            if sequence > due {
                break;
            }
            let frame = self.output.pop().expect("unique consumer head");
            if sequence == due {
                self.playing = frame;
                return true;
            }
            self.shared.stale.fetch_add(1, Ordering::Relaxed);
        }
        false
    }
}
// Control-side only. An already-finished worker needs no waiting budget (and
// can be joined with Duration::ZERO). Once waiting begins, a late observation
// cannot establish completion before the deadline, even if the worker ended
// while this thread was descheduled.
fn completion_observed_before_deadline(
    deadline: Instant,
    mut is_finished: impl FnMut() -> bool,
    mut now: impl FnMut() -> Instant,
    mut wait: impl FnMut(),
) -> bool {
    if is_finished() {
        return true;
    }
    loop {
        if now() >= deadline {
            return false;
        }
        wait();
        let finished = is_finished();
        if now() >= deadline {
            return false;
        }
        if finished {
            return true;
        }
    }
}
impl Suppression for Worker {
    fn latency_samples(&self) -> usize {
        self.geometry.latency_samples()
    }
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        self.geometry.resolved()
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        self.drain_provider(timeout)
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        Worker::process_sample(self, input, wet)
    }
    fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        Worker::process_block(self, samples, wet, voice)
    }
    fn voice(&self) -> Option<f32> {
        self.voice
    }
}
impl Worker {
    fn drain_provider(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        let terminal = self
            .finish(StopMode::Drain, timeout)
            .map_err(|error| match error {
                Error::JoinTimeout => RetirementError::TimedOut,
                _ => RetirementError::WorkerFailed,
            })?;
        if terminal.retirement_timed_out {
            return Err(RetirementError::TimedOut);
        }
        if terminal.snapshot.failed {
            return Err(RetirementError::WorkerFailed);
        }
        if terminal.stop_mode != StopMode::Drain || !terminal.drained {
            return Err(RetirementError::Incomplete);
        }
        Ok(())
    }
    pub fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        let input = if input.is_finite() { input } else { 0. };
        let dry = self.dry[self.dry_position];
        self.dry[self.dry_position] = input;
        self.dry_position += 1;
        if self.dry_position == self.latency_samples() {
            self.dry_position = 0;
        }
        if self.finished {
            self.voice = None;
            return dry;
        }
        let frame = self.geometry.frame_samples;
        let start = (frame + self.geometry.worker_lead_samples) as u64;
        let started = self.sample >= start;
        let (phase, due) = (self.out_phase, self.due);
        // The counters are the division they replace, at every sample.
        debug_assert_eq!(
            (phase as u64, due),
            if started {
                let since = self.sample - start;
                (since % frame as u64, since / frame as u64)
            } else {
                (0, 0)
            }
        );
        if started {
            if phase == 0 {
                self.pending = None;
                if !self.choose(due) {
                    self.shared.missed.fetch_add(1, Ordering::Relaxed);
                    self.pending = Some(due);
                    // Rare: the only clock read this path adds.
                    let at = metrics::now(false).unwrap_or(0);
                    let event = EventRecord {
                        sequence: due,
                        at_ns: at,
                    };
                    self.shared.endpoint_outliers.missed(event);
                    self.shared.capture.missed(event);
                }
            } else if self.pending == Some(due) && self.choose(due) {
                // A late result still serves the rest of its frame.
                self.pending = None;
                self.shared.late.fetch_add(1, Ordering::Relaxed);
                let at = metrics::now(false).unwrap_or(0);
                self.shared.endpoint_outliers.late(EventRecord {
                    sequence: due,
                    at_ns: at,
                });
            }
        }
        self.assembling.samples[self.phase] = input;
        let available = started
            && self.playing.valid
            && self.playing.samples[phase].is_finite()
            && !self.shared.failed.load(Ordering::Acquire);
        let output = if available {
            self.voice = Some(self.playing.voice);
            self.playing.samples[phase]
        } else {
            self.voice = None;
            dry
        };
        self.sample += 1;
        if started {
            self.out_phase += 1;
            if self.out_phase == frame {
                self.out_phase = 0;
                self.due += 1;
            }
        }
        self.phase += 1;
        if self.phase == frame {
            self.assembling.sequence = self.frame;
            self.assembling.valid = true;
            self.assembling.submitted_ns = metrics::now(false);
            if !self.input.push(&self.assembling) {
                self.shared.input_drops.fetch_add(1, Ordering::Relaxed);
            } else if !self.shared.wake.notify() {
                self.shared.failed.store(true, Ordering::Release);
            }
            self.phase = 0;
            self.frame += 1;
        }
        dry + (output - dry) * wet
    }

    /// `process_sample` for a block, in place: `samples[i]` with `wet[i]`
    /// becomes the output sample, and `voice[i]` is what `voice()` returns
    /// after it. Output, voices, counters and the endpoint's state are bit
    /// for bit those of the per-sample calls.
    ///
    /// A sample that holds an event goes through `process_sample` itself:
    /// the first sample of an output frame (where a result is chosen), the
    /// sample that completes an input frame (queue push and wake), and
    /// every sample while a late result is still awaited or after `finish`.
    /// The samples between events run a plain loop with none of those
    /// checks; a run also ends on the dry ring's last slot, where the loop
    /// wraps the ring itself. The worker-failure flag is still read at
    /// every sample that would play a result, so a failure takes effect at
    /// the same sample as in `process_sample`.
    pub fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        assert!(wet.len() >= samples.len() && voice.len() >= samples.len());
        let frame = self.geometry.frame_samples;
        let start = (frame + self.geometry.worker_lead_samples) as u64;
        let latency = self.latency_samples();
        let mut t = 0;
        while t < samples.len() {
            let started = self.sample >= start;
            // Samples before the next event, which itself is left to
            // `process_sample`: the next output frame's first sample (or the
            // first output sample of all), ...
            let to_output = if !started {
                (start - self.sample) as usize
            } else if self.out_phase == 0 {
                0
            } else {
                frame - self.out_phase
            };
            // ... the sample completing the input frame, and the dry ring's
            // wrap (taken with the run that ends on it).
            let run = to_output
                .min((frame - 1).saturating_sub(self.phase))
                .min(latency - self.dry_position)
                .min(samples.len() - t);
            if self.finished || self.pending.is_some() || run == 0 {
                samples[t] = self.process_sample(samples[t], wet[t]);
                voice[t] = self.voice;
                t += 1;
                continue;
            }
            let end = t + run;
            self.interior(
                &mut samples[t..end],
                &wet[t..end],
                &mut voice[t..end],
                started,
            );
            t = end;
        }
    }

    /// `process_sample` for a run of samples without events: not finished,
    /// no late result awaited, no output frame starting, the input frame not
    /// completing, and the dry ring wrapping at the run's end at most.
    #[inline(always)]
    fn interior(
        &mut self,
        samples: &mut [f32],
        wet: &[f32],
        voice: &mut [Option<f32>],
        started: bool,
    ) {
        let n = samples.len();
        let failed = &self.shared.failed;
        let dry_ring = &mut self.dry[self.dry_position..self.dry_position + n];
        let assembling = &mut self.assembling.samples[self.phase..self.phase + n];
        let playing = &self.playing.samples[self.out_phase..self.out_phase + n];
        let (valid, score) = (self.playing.valid, self.playing.voice);
        let mut last = self.voice;
        for i in 0..n {
            let x = if samples[i].is_finite() {
                samples[i]
            } else {
                0.
            };
            let dry = dry_ring[i];
            dry_ring[i] = x;
            assembling[i] = x;
            let available =
                started && valid && playing[i].is_finite() && !failed.load(Ordering::Acquire);
            let output = if available {
                last = Some(score);
                playing[i]
            } else {
                last = None;
                dry
            };
            voice[i] = last;
            samples[i] = dry + (output - dry) * wet[i];
        }
        self.voice = last;
        self.dry_position += n;
        if self.dry_position == self.geometry.latency_samples() {
            self.dry_position = 0;
        }
        self.phase += n;
        self.sample += n as u64;
        if started {
            self.out_phase += n;
            if self.out_phase == self.geometry.frame_samples {
                self.out_phase = 0;
                self.due += 1;
            }
        }
    }
}
/// The callback endpoint: a threaded `Worker` when its
/// geometry has a worker lead, the `Inline` endpoint (the model runs on the
/// calling thread as each frame completes) when the lead is zero.
pub enum Endpoint {
    Worker(Worker),
    Inline(Inline),
}
impl Endpoint {
    pub fn new(lookahead: u8, options: WorkerOptions) -> Result<Self, Error> {
        Self::new_with_affinity(lookahead, options, AffinityScope::Inherited)
    }
    /// `options` and `scope` apply to a threaded worker only.
    pub fn new_with_affinity(
        lookahead: u8,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        Self::new_with_affinity_and_lead(lookahead, None, options, scope)
    }
    /// `lead` overrides the worker's scheduling allowance (see
    /// `Worker::new_with_affinity_and_lead`); the inline endpoint has none.
    pub fn new_with_affinity_and_lead(
        lookahead: u8,
        lead: Option<usize>,
        options: WorkerOptions,
        scope: AffinityScope,
    ) -> Result<Self, Error> {
        let mut geometry = Geometry::for_lookahead(lookahead)?;
        if let Some(lead) = lead {
            geometry.worker_lead_samples = lead;
        }
        if geometry.worker_lead_samples == 0 {
            Inline::new(lookahead).map(Self::Inline)
        } else {
            Worker::with_geometry(lookahead, geometry, options, scope).map(Self::Worker)
        }
    }
    pub fn geometry(&self) -> Geometry {
        match self {
            Self::Worker(w) => w.geometry(),
            Self::Inline(i) => i.geometry(),
        }
    }
    pub fn latency_samples(&self) -> usize {
        self.geometry().latency_samples()
    }
    pub fn telemetry(&self) -> Telemetry {
        match self {
            Self::Worker(w) => w.telemetry(),
            Self::Inline(i) => i.telemetry(),
        }
    }
    pub fn voice(&self) -> Option<f32> {
        match self {
            Self::Worker(w) => w.voice(),
            Self::Inline(i) => i.voice(),
        }
    }
    pub fn finish(&mut self, mode: StopMode, timeout: Duration) -> Result<Terminal, Error> {
        match self {
            Self::Worker(w) => w.finish(mode, timeout),
            Self::Inline(i) => Ok(i.finish(mode)),
        }
    }
    pub fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        match self {
            Self::Worker(w) => w.process_sample(input, wet),
            Self::Inline(i) => i.process_sample(input, wet),
        }
    }
    /// `process_sample` for a block, in place, with each sample's `voice()`;
    /// see `Worker::process_block`. The inline endpoint takes the samples one
    /// at a time.
    pub fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        match self {
            Self::Worker(w) => w.process_block(samples, wet, voice),
            Self::Inline(i) => {
                assert!(wet.len() >= samples.len() && voice.len() >= samples.len());
                for ((sample, &wet), voice) in samples.iter_mut().zip(wet).zip(voice) {
                    *sample = i.process_sample(*sample, wet);
                    *voice = i.voice();
                }
            }
        }
    }
}
impl Suppression for Endpoint {
    fn latency_samples(&self) -> usize {
        Endpoint::latency_samples(self)
    }
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        Endpoint::geometry(self).resolved()
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        match self {
            Self::Worker(w) => w.drain_provider(timeout),
            Self::Inline(i) => {
                if i.finish(StopMode::Drain).snapshot.failed {
                    Err(RetirementError::WorkerFailed)
                } else {
                    Ok(())
                }
            }
        }
    }
    fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
        Endpoint::process_sample(self, input, wet)
    }
    fn process_block(&mut self, samples: &mut [f32], wet: &[f32], voice: &mut [Option<f32>]) {
        Endpoint::process_block(self, samples, wet, voice)
    }
    fn voice(&self) -> Option<f32> {
        Endpoint::voice(self)
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Explicit finish is required for service health/accounting. This final
        // bounded cancellation prevents a stuck vendor from hanging destruction
        // forever; a detached worker owns its Arc data and cannot use freed state.
        let _ = self.finish(StopMode::Cancel, Duration::from_secs(2));
    }
}
fn run(
    mut model: Box<dyn Model>,
    mut input: Consumer,
    mut output: Producer,
    s: &Shared,
    g: Geometry,
) {
    let mut expected = 0;
    let mut recovery = 0;
    let mut capture = capture::Recorder::new(&s.capture);
    let time_steps = s.prep.step_timing.load(Ordering::Relaxed);
    loop {
        // Observe before checking stop/queue: any subsequent producer or
        // retirement notification prevents sleeping through that transition.
        let observed = s.wake.observe();
        let stop = s.stop.load(Ordering::Acquire);
        if stop == 1 || s.failed.load(Ordering::Acquire) {
            break;
        }
        let Some(mut frame) = input.pop() else {
            if stop == 2 {
                break;
            }
            if !s.wake.wait(observed) {
                s.failed.store(true, Ordering::Release);
                break;
            }
            continue;
        };
        let wall = metrics::now(false);
        let cpu = metrics::now(true);
        capture.begin(wall);
        let mut capture_frame = CaptureFrame {
            sequence: frame.sequence,
            submitted_ns: frame.submitted_ns,
            started_ns: wall,
            input_pending_after_pop: input.pending(),
            ..CaptureFrame::default()
        };
        if frame.sequence != expected {
            if model.reset().is_err() {
                s.failed.store(true, Ordering::Release);
                capture.record(capture_frame);
                break;
            }
            recovery = g.intrinsic_delay_samples.div_ceil(g.frame_samples);
        }
        frame.valid = recovery == 0;
        recovery = recovery.saturating_sub(1);
        match model.process(&mut frame.samples[..g.frame_samples]) {
            Ok(voice) => frame.voice = voice,
            Err(_) => {
                s.failed.store(true, Ordering::Release);
                capture.record(capture_frame);
                break;
            }
        }
        expected = frame.sequence + 1;
        let cpu_end = metrics::now(true);
        let wall_end = metrics::now(false);
        capture_frame.finished_ns = wall_end;
        capture_frame.cpu_ns = cpu.zip(cpu_end).and_then(|(a, b)| b.checked_sub(a));
        s.queue_wait
            .record(frame.submitted_ns, wall, g.worker_allowance_ns());
        s.cpu.record(cpu, cpu_end, g.frame_budget_ns());
        s.wall.record(wall, wall_end, g.frame_budget_ns());
        if wall
            .zip(wall_end)
            .and_then(|(a, b)| b.checked_sub(a))
            .is_some_and(|x| x > g.worker_allowance_ns())
        {
            s.allowance_overruns.fetch_add(1, Ordering::Relaxed);
        }
        if !output.push(&frame) {
            s.output_drops.fetch_add(1, Ordering::Relaxed);
        } else {
            let published = metrics::now(false);
            capture_frame.output_published = true;
            capture_frame.published_ns = published;
            s.turnaround
                .record(frame.submitted_ns, published, g.worker_allowance_ns());
            if let (Some(submitted), Some(started), Some(finished), Some(published)) =
                (frame.submitted_ns, wall, wall_end, published)
            {
                let elapsed = |from: u64, to: u64| to.saturating_sub(from);
                s.worker_outliers.record(
                    outlier::FrameRecord {
                        sequence: frame.sequence,
                        submitted_ns: submitted,
                        started_ns: started,
                        finished_ns: finished,
                        published_ns: published,
                        cpu_ns: cpu.zip(cpu_end).map_or(0, |(a, b)| elapsed(a, b)),
                        wall_ns: elapsed(started, finished),
                        queue_wait_ns: elapsed(submitted, started),
                        turnaround_ns: elapsed(submitted, published),
                    },
                    g.frame_budget_ns(),
                );
            }
        }
        capture.record(capture_frame);
        s.completed.fetch_add(1, Ordering::Release);
        prepare(model.as_mut(), &input, s, g, cpu_end, time_steps);
    }
}

/// After a frame's result is published and counted: the model's bounded
/// preparation steps for the next frame, while no input is queued, no stop
/// is requested and the worker has not failed. Checked before each step
/// only, so input arriving during a step waits for it to finish (the next
/// frame then uses what it prepared). Never spins or sleeps.
///
/// `process_cpu_end` is the thread CPU reading taken when the frame's
/// `process` returned; the pass's CPU time is counted from it, which saves a
/// clock system call per frame and adds the publication in between to
/// `pass_cpu`. With `time_steps` off the pass reads the wall clock twice,
/// whatever the number of steps.
fn prepare(
    model: &mut dyn Model,
    input: &Consumer,
    s: &Shared,
    g: Geometry,
    process_cpu_end: Option<u64>,
    time_steps: bool,
) {
    let p = &s.prep;
    let blocked = || {
        input.pending() != 0
            || s.stop.load(Ordering::Acquire) != 0
            || s.failed.load(Ordering::Acquire)
    };
    if model.prep_pending() == 0 {
        return;
    }
    if blocked() {
        p.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let wall = metrics::now(false);
    let mut at = wall;
    let mut steps = 0;
    loop {
        if !model.prepare_step() {
            break;
        }
        steps += 1;
        if time_steps {
            let now = metrics::now(false);
            p.step_wall.record(at, now, g.frame_budget_ns() / 16);
            at = now;
        }
        if model.prep_pending() == 0 {
            break;
        }
        if blocked() {
            p.abandoned.fetch_add(1, Ordering::Relaxed);
            break;
        }
    }
    if !time_steps && steps > 0 {
        at = metrics::now(false);
    }
    p.steps.fetch_add(steps, Ordering::Relaxed);
    p.pass_cpu
        .record(process_cpu_end, metrics::now(true), g.frame_budget_ns());
    p.pass_wall.record(wall, at, g.frame_budget_ns());
}

#[cfg(test)]
#[path = "tests/gru_prep.rs"]
mod gru_prep_tests;
#[cfg(test)]
mod tests;
