//! Bounded typed control messages, handled on one ordinary scoped thread.
//! The owner may borrow an audio Group; the control API only owns the independent Handle.
use crate::{
    audio::{Control, PrepareError, SaveError},
    controller::Controller,
    diagnostic_history::{CallbackDiagnostic, ExportError, FaultObservation},
    diagnostics,
    events::{EventError, Subscription},
    facade::Facade,
    metrics::{Consumer, Reading},
    store::StoreError,
};
use rc2fx_core::{
    ConfigUpdate, DeviceState, EngineMode, EngineRestartRequest, Meter, MeterType, Performance,
    PresetSummary, RoutingRequest, SelectionRequest, TargetMeter, Timing,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};

pub const QUEUED_COMMANDS: usize = 8;
/// The loop's fixed tick where nothing gives it a deadline: with a JACK runtime
/// in this process, and while a resource sample cannot be taken yet.
const TICK: Duration = Duration::from_millis(20);
/// How often process resources and the USB status are sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// The shortest wait for a command, so a deadline that is already due costs
/// one pass of the loop and never a spin.
#[cfg(all(feature = "native-pipeline", target_os = "linux"))]
const MIN_WAIT: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub enum Error {
    Store(StoreError),
    Audio(PrepareError),
    Events(EventError),
    Busy,
    Stopped,
    RuntimeFault,
    InvalidRuntime,
    Diagnostics(ExportError),
    Replacement(String),
    Native(String),
    NativeBusy,
    SuppressionInUse,
    NativeUnsupported(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Audio(error) => error.fmt(f),
            Self::Events(error) => write!(f, "Event subscription failed: {error:?}"),
            Self::Busy => f.write_str("Controller queue is full"),
            Self::Stopped => f.write_str("Controller stopping"),
            Self::RuntimeFault => f.write_str("Audio recovery is required before applying effects"),
            Self::InvalidRuntime => f.write_str("Audio runtime does not match the saved selection"),
            Self::Diagnostics(error) => error.fmt(f),
            Self::Replacement(message) => f.write_str(message),
            Self::Native(message) => write!(f, "Native audio: {message}"),
            Self::NativeBusy => {
                f.write_str("The mixer is still applying an earlier change; try again in a moment")
            }
            Self::SuppressionInUse => f.write_str(
                "Noise suppression runs on one microphone or the Chat voice mix at a time; turn it off on the other target first",
            ),
            Self::NativeUnsupported(message) => {
                write!(f, "Unsupported native audio change: {message}")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<SaveError> for Error {
    fn from(error: SaveError) -> Self {
        match error {
            SaveError::Audio(error) => Self::Audio(error),
            SaveError::Store(error) => Self::Store(error),
        }
    }
}

type Answer<T> = oneshot::Sender<Result<T, Error>>;
pub type Reply<T> = oneshot::Receiver<Result<T, Error>>;
enum Command {
    Apply(ConfigUpdate, Answer<DeviceState>),
    ResetDiagnostics(Answer<DeviceState>),
    DiagnosticsEvents {
        after: u64,
        limit: usize,
        reply: Answer<rc2fx_core::DiagnosticsEvents>,
    },
    CreatePreset(String, u64, Answer<PresetSummary>),
    ApplyPreset(String, u64, Answer<DeviceState>),
    DeletePreset(String, Answer<()>),
    Settings(Answer<Vec<u8>>),
    Lifecycle(GenerationUpdate, Answer<DeviceState>),
    Transition(Request, Answer<DeviceState>),
    /// Nothing to do: ends the loop's wait so it looks at its deadlines again.
    Wake,
}

#[derive(Debug)]
pub(crate) enum Action {
    Enable,
    Disable,
    Restart,
    Select(Vec<String>),
}
/// Owns an accepted Apply reply across native generation retirement. Dropping
/// The control API's receiver never cancels this work; only the generation manager decides
/// its durable outcome. A lost owner must not synthesize a successful save.
#[derive(Debug)]
pub(crate) enum Request {
    Routing(RoutingRequest),
    Restart(EngineRestartRequest),
    Selection(SelectionRequest),
}

/// Facts supplied by the native generation owner after the corresponding
/// routing/cleanup operation; never inferred from callback frame progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerationPhase {
    Starting,
    Active,
    Recovering(String),
    Restored,
    Stopped,
    Fault(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationUpdate {
    pub generation: u64,
    pub phase: GenerationPhase,
}

struct Admission {
    // This lock guards only command admission/closure, never disk or audio.
    sender: Mutex<Option<mpsc::SyncSender<Command>>>,
    /// Commands admitted and not yet taken by the loop, at most
    /// QUEUED_COMMANDS. The channel has one more slot, which only a wake
    /// uses, so a wake never takes a command's place. Changed by senders
    /// under the `sender` lock.
    queued: AtomicUsize,
    /// A wake is in the channel; a second one would add nothing.
    waking: AtomicBool,
    completed: watch::Receiver<bool>,
    completion: watch::Sender<bool>,
    closed: AtomicBool,
    front: Facade,
}
#[derive(Clone)]
pub struct Handle(Arc<Admission>);
impl Handle {
    fn enqueue<T>(&self, make: impl FnOnce(Answer<T>) -> Command) -> Result<Reply<T>, Error> {
        let sender = self.0.sender.lock().map_err(|_| Error::Stopped)?;
        let sender = sender.as_ref().ok_or(Error::Stopped)?;
        if self.0.queued.load(Ordering::Acquire) >= QUEUED_COMMANDS {
            return Err(Error::Busy);
        }
        let (answer, reply) = oneshot::channel();
        self.0.queued.fetch_add(1, Ordering::AcqRel);
        sender.try_send(make(answer)).map_err(|error| {
            self.0.queued.fetch_sub(1, Ordering::AcqRel);
            match error {
                mpsc::TrySendError::Full(_) => Error::Busy,
                mpsc::TrySendError::Disconnected(_) => Error::Stopped,
            }
        })?;
        Ok(reply)
    }
    pub fn state(&self) -> Result<Reply<DeviceState>, Error> {
        self.read(Ok(self.0.front.state()))
    }
    pub fn subscribe(&self) -> Result<Reply<Subscription>, Error> {
        let subscription = self.0.front.subscribe().map_err(Error::Events);
        if subscription.is_ok() {
            self.wake();
        }
        self.read(subscription)
    }
    /// Ends the loop's wait. A new subscriber must not wait out an idle
    /// interval for its first meter. The wake has its own slot in the channel,
    /// so it neither fails behind queued commands nor takes a command's place.
    fn wake(&self) {
        if let Ok(sender) = self.0.sender.lock()
            && let Some(sender) = sender.as_ref()
            && !self.0.waking.swap(true, Ordering::AcqRel)
            && sender.try_send(Command::Wake).is_err()
        {
            self.0.waking.store(false, Ordering::Release);
        }
    }
    /// Whether anyone is subscribed to live events.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn has_clients(&self) -> bool {
        self.0.front.has_clients()
    }
    /// Restart the diagnostics baselines and the event log (no audio effect).
    pub fn reset_diagnostics(&self) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(Command::ResetDiagnostics)
    }
    /// One page of the diagnostics event log.
    pub fn diagnostics_events(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<Reply<rc2fx_core::DiagnosticsEvents>, Error> {
        self.enqueue(|reply| Command::DiagnosticsEvents {
            after,
            limit,
            reply,
        })
    }
    pub fn apply(&self, update: ConfigUpdate) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::Apply(update, reply))
    }
    pub fn presets(&self) -> Result<Reply<Vec<PresetSummary>>, Error> {
        self.read(Ok(self.0.front.presets()))
    }
    /// Control-side bounded export; callers may use a bounded blocking task.
    /// No disk or process probe occurs here, and paused mutation admission does
    /// not make retained diagnostics unavailable during recovery.
    pub fn diagnostic_report(&self) -> Result<Vec<u8>, Error> {
        if self.is_closed() {
            return Err(Error::Stopped);
        }
        self.0.front.diagnostic_report().map_err(Error::Diagnostics)
    }
    pub fn create_preset(
        &self,
        name: String,
        revision: u64,
    ) -> Result<Reply<PresetSummary>, Error> {
        self.enqueue(|reply| Command::CreatePreset(name, revision, reply))
    }
    pub fn apply_preset(&self, id: String, revision: u64) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::ApplyPreset(id, revision, reply))
    }
    pub fn delete_preset(&self, id: String) -> Result<Reply<()>, Error> {
        self.enqueue(|reply| Command::DeletePreset(id, reply))
    }
    /// The saved settings file, exactly as stored, for backups.
    pub fn settings(&self) -> Result<Reply<Vec<u8>>, Error> {
        self.enqueue(Command::Settings)
    }
    pub fn lifecycle(&self, update: GenerationUpdate) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::Lifecycle(update, reply))
    }
    pub fn routing(&self, request: RoutingRequest) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::Transition(Request::Routing(request), reply))
    }
    pub fn restart(&self, request: EngineRestartRequest) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::Transition(Request::Restart(request), reply))
    }
    pub fn selection(&self, request: SelectionRequest) -> Result<Reply<DeviceState>, Error> {
        self.enqueue(|reply| Command::Transition(Request::Selection(request), reply))
    }
    fn read<T>(&self, value: Result<T, Error>) -> Result<Reply<T>, Error> {
        if self.is_closed() {
            return Err(Error::Stopped);
        }
        let (sender, receiver) = oneshot::channel();
        let _ = sender.send(value);
        Ok(receiver)
    }
    pub(crate) fn detached(controller: &Controller) -> Self {
        let (completion, completed) = watch::channel(true);
        Self(Arc::new(Admission {
            sender: Mutex::new(None),
            queued: AtomicUsize::new(0),
            waking: AtomicBool::new(false),
            completed,
            completion,
            closed: AtomicBool::new(false),
            front: controller.front.clone(),
        }))
    }
    pub(crate) fn pause(&self) {
        if let Ok(mut sender) = self.0.sender.lock() {
            sender.take();
        }
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }
    fn install(&self, sender: mpsc::SyncSender<Command>) -> Result<(), Error> {
        let mut slot = self.0.sender.lock().map_err(|_| Error::Stopped)?;
        if self.is_closed() {
            return Err(Error::Stopped);
        }
        if slot.is_some() {
            return Err(Error::Busy);
        }
        self.0.completion.send_replace(false);
        // A new channel starts empty, whatever the last one still held.
        self.0.queued.store(0, Ordering::Release);
        self.0.waking.store(false, Ordering::Release);
        *slot = Some(sender);
        Ok(())
    }

    /// Close admission atomically with enqueue. Accepted commands still execute,
    /// even if the control API drops their replies. No command can be admitted after this.
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::Release);
        if let Ok(mut sender) = self.0.sender.lock() {
            sender.take();
        }
        self.0.front.disconnect();
    }
    /// Wait for all accepted commands and event subscriber disconnection. This does not
    /// deactivate JACK; the outer generation owner must close its callbacks.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.close();
        let mut completed = self.0.completed.clone();
        while !*completed.borrow_and_update() {
            completed.changed().await.map_err(|_| Error::Stopped)?;
        }
        Ok(())
    }
}

pub async fn receive<T>(submission: Result<Reply<T>, Error>) -> Result<T, Error> {
    submission?.await.map_err(|_| Error::Stopped)?
}

/// Supplied by the generation owner, not inferred from software callback data.
/// Routing remains unmanaged here; this descriptor cannot claim a wet graph.
pub struct RuntimeDescriptor {
    pub generation: u64,
    pub mode: EngineMode,
    pub sample_rate: u32,
    pub block_size: u32,
    pub progress_timeout: Duration,
}
pub struct Runtime<'owner> {
    pub control: Control<'owner>,
    pub descriptor: RuntimeDescriptor,
    pub observations: Option<Vec<TargetObservation<'owner>>>,
}
/// Presence is supplied by actual worker construction, never guessed from the
/// target's UI category. Live snapshots are individually atomic, not terminal
/// or transactional worker accounting.
pub enum WorkerSource {
    Absent,
    Live {
        telemetry: rc2fx_suppression::Telemetry,
        geometry: rc2fx_suppression::Geometry,
    },
}
pub struct TargetObservation<'owner> {
    pub target_id: String,
    pub consumer: Consumer<'owner>,
    /// Captured from the actual constructed callback, not saved metadata.
    pub latency_samples: u32,
    pub worker: WorkerSource,
    pub diagnostics: Option<diagnostics::Reader>,
}
struct Live<'owner> {
    runtime: Runtime<'owner>,
    frames: Vec<u64>,
    last_progress: Vec<Instant>,
    progressed: Vec<bool>,
    faulted: bool,
    writes_ready: bool,
    latest: Vec<Option<Reading>>,
    overload: crate::overload::Overload,
}

pub struct Coordinator<'owner> {
    controller: Controller,
    live: Option<Live<'owner>>,
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    native: Option<crate::native_backend::Runtime>,
    commands: mpsc::Receiver<Command>,
    completed: watch::Sender<bool>,
    handle: Handle,
    action: Option<Action>,
    /// When the next resource sample is due.
    next_sample: Instant,
}
impl<'owner> Coordinator<'owner> {
    /// Reuses the same bounded commands, control Handle and event stream without
    /// constructing a Group, JACK client or suppression worker in this process.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub fn new_native(
        mut controller: Controller,
        mut runtime: crate::native_backend::Runtime,
    ) -> Result<(Handle, Self), Error> {
        controller.attach_native(&mut runtime)?;
        let (handle, mut owner) = Self::new(controller, None)?;
        owner.native = Some(runtime);
        Ok((handle, owner))
    }
    pub fn new(
        controller: Controller,
        runtime: Option<Runtime<'owner>>,
    ) -> Result<(Handle, Self), Error> {
        Self::try_new(controller, runtime).map_err(|(error, _controller)| error)
    }

    /// Preserve Store ownership when validating a generation fails. No native
    /// registrations or controller threads are created by this constructor.
    #[allow(clippy::result_large_err)] // Cold owner transfer; keep Store intact without another allocation.
    pub fn try_new(
        controller: Controller,
        runtime: Option<Runtime<'owner>>,
    ) -> Result<(Handle, Self), (Error, Controller)> {
        let handle = Handle::detached(&controller);
        Self::attach(controller, runtime, &handle).map(|owner| (handle, owner))
    }

    #[allow(clippy::result_large_err)]
    pub(crate) fn attach(
        mut controller: Controller,
        runtime: Option<Runtime<'owner>>,
        handle: &Handle,
    ) -> Result<Self, (Error, Controller)> {
        let prepare = || -> Result<Option<Live<'owner>>, Error> {
            let live = if let Some(mut runtime) = runtime {
                let config = controller.config();
                let canonical: Vec<_> = config
                    .targets
                    .iter()
                    .filter(|target| config.selected_targets.contains(&target.id))
                    .map(|target| target.id.clone())
                    .collect();
                if canonical != runtime.control.selected_ids()
                    || canonical.is_empty()
                    || runtime.control.published_revision() != config.revision
                    || runtime.descriptor.generation == 0
                    || runtime.descriptor.sample_rate != 48_000
                    || runtime.descriptor.block_size == 0
                    || runtime.descriptor.mode == EngineMode::Offline
                    || runtime.descriptor.progress_timeout.is_zero()
                {
                    return Err(Error::InvalidRuntime);
                }
                let count = canonical.len();
                if let Some(observations) = &mut runtime.observations {
                    let budget = u64::from(runtime.descriptor.block_size) * 1_000_000_000
                        / u64::from(runtime.descriptor.sample_rate);
                    if observations.len() != count {
                        return Err(Error::InvalidRuntime);
                    }
                    for (index, observation) in observations.iter().enumerate() {
                        if !canonical.contains(&observation.target_id)
                            || observations[..index]
                                .iter()
                                .any(|prior| prior.target_id == observation.target_id)
                            || observation.consumer.budget_ns() != budget
                            || observation.latency_samples as usize
                                > rc2fx_dsp::MAX_SUPPRESSION_DELAY
                        {
                            return Err(Error::InvalidRuntime);
                        }
                        if let WorkerSource::Live { geometry, .. } = &observation.worker {
                            let target = config
                                .targets
                                .iter()
                                .find(|target| target.id == observation.target_id)
                                .expect("validated target identity");
                            let delay = geometry
                                .frame_samples
                                .checked_add(geometry.intrinsic_delay_samples)
                                .and_then(|delay| delay.checked_add(geometry.worker_lead_samples));
                            if target.channels != 1
                                || geometry.sample_rate != runtime.descriptor.sample_rate
                                || geometry.frame_samples == 0
                                || geometry.frame_samples > 480
                                || delay != Some(observation.latency_samples as usize)
                            {
                                return Err(Error::InvalidRuntime);
                            }
                        }
                    }
                    observations.sort_by_key(|observation| {
                        canonical
                            .iter()
                            .position(|id| *id == observation.target_id)
                            .unwrap()
                    });
                }
                let frames = (0..count)
                    .map(|index| {
                        runtime
                            .control
                            .processed_frames(index)
                            .expect("validated slot")
                    })
                    .collect();
                let writes_ready = runtime.descriptor.mode != EngineMode::Jack;
                Some(Live {
                    runtime,
                    frames,
                    last_progress: vec![Instant::now(); count],
                    progressed: vec![false; count],
                    faulted: false,
                    writes_ready,
                    latest: vec![None; count],
                    overload: crate::overload::Overload::default(),
                })
            } else {
                None
            };
            Ok(live)
        };
        let live = match prepare() {
            Ok(live) => live,
            Err(error) => return Err((error, controller)),
        };
        if let Some(live) = &live {
            controller.attach_runtime(
                &live.runtime.descriptor,
                live.runtime.control.selected_ids().to_vec(),
            );
        }
        // One slot more than the commands admitted: see `Admission::queued`.
        let (sender, commands) = mpsc::sync_channel(QUEUED_COMMANDS + 1);
        if let Err(error) = handle.install(sender) {
            return Err((error, controller));
        }
        Ok(Self {
            controller,
            live,
            #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
            native: None,
            commands,
            completed: handle.0.completion.clone(),
            handle: handle.clone(),
            action: None,
            next_sample: Instant::now(),
        })
    }

    /// Run on an ordinary scoped thread, never an async executor or callback.
    /// Returning releases borrowed Control only after all admitted saves finish.
    pub fn run(mut self) -> Controller {
        self.run_loop();
        self.controller.disconnect_clients();
        self.controller
    }
    fn run_loop(&mut self) {
        while self.pass() {}
        self.completed.send_replace(true);
    }

    /// One pass of the loop: the timed work, then a wait for a command or the
    /// next deadline. Returns false once admission is closed and every
    /// accepted command has run.
    pub(crate) fn pass(&mut self) -> bool {
        let wait = self.turn();
        match self.commands.recv_timeout(wait) {
            // Nothing to run: the next pass looks at the deadlines again.
            Ok(Command::Wake) => self.handle.0.waking.store(false, Ordering::Release),
            Ok(command) => {
                // Its place in the queue is free once the loop has taken it.
                let queued = &self.handle.0.queued;
                let _ = queued.try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    count.checked_sub(1)
                });
                self.execute(command);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
        true
    }

    /// The loop's timed work: read the audio side and sample resources when
    /// each is due. Returns how long the loop may then wait for a command.
    ///
    /// With the native engine the wait ends at the earlier of the next engine
    /// read and the next resource sample, so the thread sleeps between them. A
    /// command ends the wait at once, and the deadline is worked out again
    /// after every command, because a save makes the next read due now. A JACK
    /// runtime in this process keeps the fixed tick: its reclaim and stall
    /// checks have no deadline of their own.
    pub(crate) fn turn(&mut self) -> Duration {
        if self.action.is_none() {
            self.poll_audio();
        }
        let now = Instant::now();
        if now >= self.next_sample {
            // The history decides whether a sample is taken. It takes none
            // before the first state is published; ask again on the next tick.
            let taken = self.controller.front.sample_resources(now);
            self.next_sample = now + if taken { SAMPLE_INTERVAL } else { TICK };
        }
        #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
        if let Some(native) = &self.native
            && self.action.is_none()
        {
            return native
                .next_poll()
                .min(self.next_sample)
                .saturating_duration_since(Instant::now())
                .max(MIN_WAIT);
        }
        TICK
    }

    /// The native runtime, for tests that set its timing.
    #[cfg(all(test, feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn native_mut(&mut self) -> &mut crate::native_backend::Runtime {
        self.native.as_mut().expect("native coordinator")
    }

    fn poll_audio(&mut self) {
        #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
        if let Some(native) = &mut self.native {
            let controller = &self.controller;
            if let Some(observation) = native.poll(|| controller.has_clients()) {
                self.controller.native_progress(observation);
            }
            return;
        }
        let Some(live) = &mut self.live else {
            return;
        };
        live.runtime.control.reclaim();
        if live.faulted {
            return;
        }
        let now = Instant::now();
        for index in 0..live.frames.len() {
            let frames = live
                .runtime
                .control
                .processed_frames(index)
                .expect("validated slot");
            if frames != live.frames[index] {
                live.frames[index] = frames;
                live.last_progress[index] = now;
                live.progressed[index] = true;
            }
        }
        let mut failure = live
            .last_progress
            .iter()
            .position(|last| now.duration_since(*last) >= live.runtime.descriptor.progress_timeout)
            .map(|index| {
                format!(
                    "Audio path {} stopped advancing",
                    live.runtime.control.selected_ids()[index]
                )
            });
        let mut overload_counts = crate::overload::Counts::default();
        if let Some(observations) = &live.runtime.observations {
            for observation in observations {
                if let Some(reader) = &observation.diagnostics {
                    overload_counts.xruns = overload_counts.xruns.max(reader.snapshot().xruns);
                }
                if let Some(snapshot) = observation
                    .diagnostics
                    .as_ref()
                    .map(|reader| reader.snapshot())
                    && snapshot.fault != 0
                {
                    failure = Some(format!(
                        "Audio path {} native fault {}",
                        observation.target_id, snapshot.fault
                    ));
                    break;
                }
                if let WorkerSource::Live { telemetry, .. } = &observation.worker {
                    overload_counts.late = overload_counts
                        .late
                        .saturating_add(telemetry.snapshot().missed_deadlines);
                    let termination = telemetry.termination();
                    if telemetry.snapshot().failed
                        || termination != rc2fx_suppression::Termination::Running
                    {
                        failure = Some(format!(
                            "Audio path {} suppression worker failed or terminated ({termination:?})",
                            observation.target_id
                        ));
                        break;
                    }
                }
            }
        }
        if failure.is_none() && live.runtime.descriptor.mode == EngineMode::Jack {
            failure = live.overload.observe(now, overload_counts);
        }
        live.faulted = failure.is_some();
        let revision = live.runtime.control.applied_revision();
        if let Some(message) = &failure {
            let targets: Vec<_> = live
                .runtime
                .observations
                .as_ref()
                .map(|observations| {
                    observations
                        .iter()
                        .map(|observation| {
                            FaultObservation::new(
                                &observation.target_id,
                                observation.diagnostics.as_ref().map(|v| v.snapshot()),
                                match &observation.worker {
                                    WorkerSource::Absent => None,
                                    WorkerSource::Live { telemetry, .. } => {
                                        Some((telemetry.snapshot(), telemetry.termination()))
                                    }
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.controller.front.fault(
                live.runtime.descriptor.generation,
                live.runtime.control.published_revision(),
                revision,
                message,
                &targets,
            );
        }
        let advancing = live.progressed.iter().all(|progressed| *progressed) && !live.faulted;
        self.controller
            .audio_progress(revision, advancing, failure.as_deref());
        self.poll_meters();
    }

    fn poll_meters(&mut self) {
        let live = self.live.as_mut().expect("called only with live runtime");
        let revision = live.runtime.control.published_revision();
        let Some(observations) = &mut live.runtime.observations else {
            return;
        };
        for (observation, latest) in observations.iter_mut().zip(&mut live.latest) {
            if latest.is_some_and(|reading| reading.window.revision != revision) {
                *latest = None;
            }
            if let Some(reading) = observation.consumer.drain_latest(revision) {
                *latest = Some(reading);
            }
        }
        // Never mix partial revisions or repeatedly relabel one stalled path's
        // old amplitudes as fresh. Every published meter consumes all windows.
        if live.runtime.control.applied_revision() != revision
            || live.latest.iter().any(Option::is_none)
        {
            return;
        }
        let mut callback = Vec::with_capacity(observations.len());
        let targets = observations
            .iter()
            .zip(&mut live.latest)
            .map(|(observation, latest)| {
                let reading = latest
                    .take()
                    .expect("every path has a fresh current window");
                let mut details = CallbackDiagnostic::new(&observation.target_id, reading.totals)
                    .with_window_revision(Some(reading.window.revision));
                let (worker, worker_budget_ns) = match &observation.worker {
                    WorkerSource::Absent => (rc2fx_suppression::Snapshot::default(), 0),
                    WorkerSource::Live {
                        telemetry,
                        geometry,
                    } => {
                        let snapshot = telemetry.snapshot();
                        details = details.with_worker(*geometry, snapshot, telemetry.termination());
                        (snapshot, geometry.frame_budget_ns())
                    }
                };
                callback.push(details);
                target_meter_snapshot(observation, reading, worker, worker_budget_ns)
            })
            .collect();
        self.controller.publish_meter(
            Meter {
                message_type: MeterType::Meter,
                revision,
                generation: live.runtime.descriptor.generation,
                synthetic: live.runtime.descriptor.mode == EngineMode::Simulation,
                // The JACK path does not keep diagnostics baselines; the epoch
                // still tells the app whether a reset happened.
                diagnostics_epoch: self.controller.diagnostics_epoch(),
                targets,
            },
            &callback,
        );
    }

    fn execute(&mut self, command: Command) {
        // Reply cancellation never cancels an admitted operation or file commit.
        match command {
            Command::Wake => {}
            Command::ResetDiagnostics(reply) => {
                let state = self.controller.reset_diagnostics();
                let _ = reply.send(Ok(state));
            }
            Command::DiagnosticsEvents {
                after,
                limit,
                reply,
            } => {
                let page = self.controller.diagnostics_events(after, limit);
                let _ = reply.send(Ok(page));
            }
            Command::Apply(update, reply) => {
                #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
                if let Some(native) = &mut self.native {
                    let result = self.controller.apply_native(update, native);
                    let _ = reply.send(result);
                    return;
                }
                let result = if self.action.is_some()
                    || self
                        .live
                        .as_ref()
                        .is_some_and(|live| live.faulted || !live.writes_ready)
                {
                    Err(Error::RuntimeFault)
                } else {
                    self.controller.apply(
                        update,
                        self.live.as_mut().map(|live| &mut live.runtime.control),
                    )
                };
                self.finish_effects(result, reply);
            }
            Command::CreatePreset(name, revision, reply) => {
                let result = if self.action.is_some()
                    || self.live.as_ref().is_some_and(|live| !live.writes_ready)
                {
                    Err(Error::RuntimeFault)
                } else {
                    self.controller
                        .create_preset(&name, revision)
                        .map_err(Error::Store)
                };
                let _ = reply.send(result);
            }
            Command::ApplyPreset(id, revision, reply) => {
                #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
                if let Some(native) = &mut self.native {
                    let result = self.controller.apply_native_preset(&id, revision, native);
                    let _ = reply.send(result);
                    return;
                }
                let result = if self.action.is_some()
                    || self
                        .live
                        .as_ref()
                        .is_some_and(|live| live.faulted || !live.writes_ready)
                {
                    Err(Error::RuntimeFault)
                } else {
                    self.controller.apply_preset(
                        &id,
                        revision,
                        self.live.as_mut().map(|live| &mut live.runtime.control),
                    )
                };
                self.finish_effects(result, reply);
            }
            Command::Settings(reply) => {
                let _ = reply.send(self.controller.settings().map_err(Error::Store));
            }
            Command::DeletePreset(id, reply) => {
                let result = if self.action.is_some()
                    || self.live.as_ref().is_some_and(|live| !live.writes_ready)
                {
                    Err(Error::RuntimeFault)
                } else {
                    self.controller.delete_preset(&id).map_err(Error::Store)
                };
                let _ = reply.send(result);
            }
            Command::Lifecycle(update, reply) => {
                let result = if self.action.is_some() {
                    Err(Error::RuntimeFault)
                } else if let Some(live) = &mut self.live {
                    if update.generation != live.runtime.descriptor.generation
                        || live.runtime.descriptor.mode != EngineMode::Jack
                        || (live.faulted
                            && matches!(
                                update.phase,
                                GenerationPhase::Starting | GenerationPhase::Active
                            ))
                    {
                        Err(Error::InvalidRuntime)
                    } else {
                        let ready = update.phase == GenerationPhase::Active;
                        let fault = matches!(update.phase, GenerationPhase::Fault(_));
                        self.controller.generation_update(update).inspect(|_| {
                            live.writes_ready = ready;
                            live.faulted |= fault;
                        })
                    }
                } else {
                    Err(Error::InvalidRuntime)
                };
                let _ = reply.send(result);
            }
            Command::Transition(request, reply) => {
                #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
                if let Some(native) = &mut self.native {
                    let result = self.controller.native_transition(request, native);
                    let _ = reply.send(result);
                    return;
                }
                let result = if self.action.is_some()
                    || self.live.as_ref().is_some_and(|live| !live.writes_ready)
                {
                    Err(Error::RuntimeFault)
                } else {
                    self.controller
                        .reserve_transition(request)
                        .map(|(action, state)| {
                            self.action = Some(action);
                            self.handle.pause();
                            state
                        })
                };
                let _ = reply.send(result);
            }
        }
        if let Some(fault) = self.controller.durability_fault().map(str::to_owned) {
            if let Some(live) = &mut self.live {
                live.faulted = true;
                live.writes_ready = false;
            }
            self.controller
                .unavailable(&format!("Durable save uncertain: {fault}"));
        }
    }

    fn finish_effects(
        &mut self,
        result: Result<crate::controller::EffectApply, Error>,
        reply: Answer<DeviceState>,
    ) {
        match result {
            Ok(crate::controller::EffectApply::Saved(state)) => {
                let _ = reply.send(Ok(*state));
            }
            Ok(crate::controller::EffectApply::Replacement(candidate)) => {
                self.replace_effects(candidate, reply);
            }
            Err(error) => {
                let _ = reply.send(Err(error));
            }
        }
    }

    fn replace_effects(&mut self, _candidate: rc2fx_core::Config, reply: Answer<DeviceState>) {
        let _ = reply.send(Err(Error::Audio(PrepareError::RequiresRestart)));
    }
}

fn worker_timing(timing: rc2fx_suppression::Timing) -> Timing {
    Timing {
        calls: timing.calls,
        total_ns: timing.total_ns,
        max_ns: timing.max_ns,
        overruns: timing.overruns,
        clock_errors: timing.clock_errors,
        histogram: timing.bins,
    }
}

fn target_meter_snapshot(
    observation: &TargetObservation<'_>,
    reading: Reading,
    worker: rc2fx_suppression::Snapshot,
    worker_budget_ns: u64,
) -> TargetMeter {
    let performance = observation.diagnostics.as_ref().and_then(|reader| {
        // A missing timing source is not zero-cost processing. Real timing
        // errors remain visible when instrumentation was requested.
        (reading.totals.unmeasured_callbacks == 0).then(|| Performance {
            callback_budget_ns: observation.consumer.budget_ns(),
            worker_budget_ns,
            xruns: reader.snapshot().xruns,
            callback: reading.totals.wall.to_core(),
            worker_wall: worker_timing(worker.wall),
            worker_cpu: worker_timing(worker.cpu),
            // The JACK path carries no helper counters.
            helper_late: None,
            helper_stalls: None,
        })
    });
    TargetMeter {
        id: observation.target_id.clone(),
        input_peak: reading.window.meter.input_peak,
        output_peak: reading.window.meter.output_peak,
        rms: reading.window.rms(),
        gain_reduction: reading.window.meter.gain_reduction,
        voice: reading.window.meter.voice,
        gate_open: reading.window.meter.gate_open,
        clipped: reading.window.meter.clipped,
        spectrum: vec![],
        callbacks: reading.totals.callbacks,
        processed_frames: reading.totals.processed_frames,
        suppression_frames: worker.completed_frames,
        missed_deadlines: worker.missed_deadlines,
        stale_results: worker.stale_results,
        input_drops: worker.input_drops,
        output_drops: worker.output_drops,
        latency_samples: observation.latency_samples,
        worker_failed: worker.failed,
        performance,
    }
}

// The store needs Unix directory durability and file locking.
#[cfg(all(test, unix))]
mod loop_tests {
    use super::*;
    use crate::{
        audio::{Chain, Group},
        store::Store,
    };
    use rc2fx_core::{Config, Target, TargetKind};

    fn samples(handle: &Handle) -> usize {
        let report: serde_json::Value =
            serde_json::from_slice(&handle.diagnostic_report().unwrap()).unwrap();
        report["samples"].as_array().unwrap().len()
    }

    /// The deadline-driven wait belongs to the native engine. A JACK runtime
    /// in this process, or none, keeps the fixed tick that its reclaim and
    /// stall checks run on.
    #[test]
    fn without_the_native_engine_the_loop_keeps_its_fixed_tick() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (_handle, mut owner) = Coordinator::new(Controller::new(store), None).unwrap();
        for _ in 0..3 {
            assert_eq!(owner.turn(), TICK);
        }
        drop(owner);

        let config = Config {
            revision: 7,
            targets: vec![Target::new("usb-main", "USB", TargetKind::Playback, 2)],
            selected_targets: vec!["usb-main".into()],
            ..Config::default()
        };
        let mut group = Group::new(
            vec![Chain::new(&config.targets[0], [None, None]).unwrap()],
            7,
        )
        .unwrap();
        let (control, _callbacks) = group.split().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), config).unwrap();
        let runtime = Runtime {
            control,
            observations: None,
            descriptor: RuntimeDescriptor {
                generation: 9,
                mode: EngineMode::Jack,
                sample_rate: 48_000,
                block_size: 112,
                progress_timeout: Duration::from_millis(60),
            },
        };
        let (handle, mut owner) = Coordinator::new(Controller::new(store), Some(runtime)).unwrap();
        assert_eq!(owner.turn(), TICK);
        assert!(!owner.live.as_ref().unwrap().faulted);
        // Each pass still runs the stall check: no callback advances this
        // path, so a pass after the deadline reports it.
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(owner.turn(), TICK);
        assert!(owner.live.as_ref().unwrap().faulted);
        assert!(
            handle.0.front.state().engine.message.contains("stopped"),
            "{}",
            handle.0.front.state().engine.message
        );
    }

    /// A subscriber wakes the loop through the command channel. The wake has
    /// a slot of its own: it is never refused behind queued commands, and it
    /// never makes the queue admit fewer of them.
    #[test]
    fn a_wake_neither_takes_nor_needs_a_command_slot() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, mut owner) = Coordinator::new(Controller::new(store), None).unwrap();
        let queue = |handle: &Handle| -> Vec<_> {
            (0..QUEUED_COMMANDS)
                .map(|_| handle.settings().expect("a free command slot"))
                .collect()
        };
        // Several subscribers before the loop runs leave one wake queued.
        let _subscriptions: Vec<_> = (0..3).map(|_| handle.subscribe().unwrap()).collect();
        let replies = queue(&handle);
        assert!(matches!(handle.settings(), Err(Error::Busy)));
        // The wake, then every command in order.
        for _ in 0..=QUEUED_COMMANDS {
            assert!(owner.pass());
        }
        for mut reply in replies {
            assert!(reply.try_recv().unwrap().is_ok());
        }
        // A full queue does not refuse a wake, and the wake is delivered.
        let replies = queue(&handle);
        let _late = handle.subscribe().unwrap();
        assert!(handle.0.waking.load(Ordering::Acquire));
        for _ in 0..=QUEUED_COMMANDS {
            assert!(owner.pass());
        }
        assert!(!handle.0.waking.load(Ordering::Acquire));
        assert_eq!(handle.0.queued.load(Ordering::Acquire), 0);
        drop(replies);
        // Every slot is free again.
        drop(queue(&handle));
        assert!(matches!(handle.settings(), Err(Error::Busy)));
    }

    /// Several threads send bursts of commands and subscribe while the loop
    /// runs. The senders count a command in and the loop counts it out, so the
    /// count must never pass the limit, must come back to zero, and must leave
    /// no wake behind. Every admitted command is answered, and a full queue
    /// is the only refusal.
    #[test]
    fn concurrent_commands_and_wakes_keep_the_queue_count_exact() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, owner) = Coordinator::new(Controller::new(store), None).unwrap();
        let task = std::thread::spawn(move || owner.run());
        let senders: Vec<_> = (0..6)
            .map(|_| {
                let handle = handle.clone();
                std::thread::spawn(move || {
                    let (mut answered, mut refused) = (0u32, 0u32);
                    for round in 0..300 {
                        if round % 2 == 0 {
                            // More subscribers than the hub takes is refused
                            // there; only an accepted one wakes the loop.
                            drop(handle.subscribe());
                        }
                        // A burst, so that six threads overfill eight slots.
                        let burst: Vec<_> = (0..4).map(|_| handle.settings()).collect();
                        assert!(handle.0.queued.load(Ordering::Acquire) <= QUEUED_COMMANDS);
                        for reply in burst {
                            match reply {
                                Ok(reply) => {
                                    reply.blocking_recv().unwrap().unwrap();
                                    answered += 1;
                                }
                                Err(Error::Busy) => refused += 1,
                                Err(error) => panic!("{error:?}"),
                            }
                        }
                    }
                    (answered, refused)
                })
            })
            .collect();
        let (mut answered, mut refused) = (0, 0);
        for sender in senders {
            let (a, r) = sender.join().unwrap();
            answered += a;
            refused += r;
        }
        assert_eq!(answered + refused, 6 * 300 * 4);
        assert!(answered >= 300 * 4, "{answered} answered");
        // A command behind any wake still queued: once it is answered, the
        // loop has taken everything before it.
        handle.settings().unwrap().blocking_recv().unwrap().unwrap();
        assert_eq!(handle.0.queued.load(Ordering::Acquire), 0);
        assert!(!handle.0.waking.load(Ordering::Acquire));
        handle.close();
        task.join().unwrap();
    }

    #[test]
    fn resources_are_sampled_once_a_second_and_the_history_has_the_last_word() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, mut owner) = Coordinator::new(Controller::new(store), None).unwrap();
        // The first pass samples, and the next sample is a second away.
        let before = Instant::now();
        owner.turn();
        assert_eq!(samples(&handle), 1);
        assert!(owner.next_sample >= before + SAMPLE_INTERVAL);
        assert!(owner.next_sample <= Instant::now() + SAMPLE_INTERVAL);
        // Passes before that deadline do not ask the history at all.
        let due = owner.next_sample;
        for _ in 0..5 {
            owner.turn();
        }
        assert_eq!((samples(&handle), owner.next_sample), (1, due));
        // A deadline that comes early does not make a sample: the history
        // takes one a second. The loop asks again on its next tick.
        owner.next_sample = Instant::now();
        owner.turn();
        assert_eq!(samples(&handle), 1);
        assert!(owner.next_sample <= Instant::now() + TICK);
        assert!(owner.next_sample < due);
    }
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;
    #[test]
    fn worker_clock_fields_are_copied_without_reusing_callback_cpu() {
        let source = rc2fx_suppression::Timing {
            calls: 9,
            total_ns: 101,
            max_ns: 23,
            overruns: 4,
            clock_errors: 5,
            bins: [1, 2, 3, 4, 5, 6, 7, 8],
        };
        let target = worker_timing(source);
        assert_eq!(
            (
                target.calls,
                target.total_ns,
                target.max_ns,
                target.overruns,
                target.clock_errors
            ),
            (9, 101, 23, 4, 5)
        );
        assert_eq!(target.histogram, source.bins);
    }
    #[test]
    fn unmeasured_callback_has_no_performance_even_with_real_diagnostics() {
        let mut slot = crate::metrics::Slot::new(2_333_333, false).unwrap();
        let (mut producer, mut consumer) = slot.split();
        producer.record(
            Some(rc2fx_dsp::Meter {
                samples: 2400,
                ..rc2fx_dsp::Meter::default()
            }),
            1,
            2400,
            None,
        );
        let reading = consumer.pop().unwrap();
        let (_writer, reader) = diagnostics::channel();
        let observation = TargetObservation {
            target_id: "one".into(),
            consumer,
            latency_samples: 0,
            worker: WorkerSource::Absent,
            diagnostics: Some(reader),
        };
        assert!(
            target_meter_snapshot(&observation, reading, Default::default(), 0)
                .performance
                .is_none()
        );
    }
    #[test]
    fn explicit_worker_snapshot_preserves_every_nonzero_counter_and_clock() {
        let mut slot = crate::metrics::Slot::new(2_333_333, true).unwrap();
        let (mut producer, mut consumer) = slot.split();
        producer.record(
            Some(rc2fx_dsp::Meter {
                samples: 2400,
                ..rc2fx_dsp::Meter::default()
            }),
            1,
            2400,
            Some(crate::metrics::Elapsed::from_durations(Some(100), Some(20))),
        );
        let reading = consumer.pop().unwrap();
        let (_writer, reader) = diagnostics::channel();
        let observation = TargetObservation {
            target_id: "one".into(),
            consumer,
            latency_samples: 1184,
            worker: WorkerSource::Absent,
            diagnostics: Some(reader),
        };
        // This pure projection fixture supplies explicit nonzero worker facts;
        // it does not construct or pretend to exercise a model/worker thread.
        let source = rc2fx_suppression::Snapshot {
            completed_frames: 10,
            input_drops: 2,
            output_drops: 3,
            missed_deadlines: 4,
            stale_results: 5,
            failed: true,
            allowance_overruns: 6,
            wall: rc2fx_suppression::Timing {
                calls: 10,
                total_ns: 900,
                max_ns: 110,
                bins: [1, 2, 3, 4, 0, 0, 0, 0],
                ..Default::default()
            },
            cpu: rc2fx_suppression::Timing {
                calls: 10,
                total_ns: 700,
                max_ns: 80,
                bins: [2, 3, 4, 1, 0, 0, 0, 0],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = target_meter_snapshot(&observation, reading, source, 10_000_000);
        assert_eq!(
            (
                result.suppression_frames,
                result.input_drops,
                result.output_drops,
                result.missed_deadlines,
                result.stale_results
            ),
            (10, 2, 3, 4, 5)
        );
        assert!(result.worker_failed);
        assert_eq!(result.latency_samples, 1184);
        let performance = result.performance.unwrap();
        assert_eq!(performance.worker_budget_ns, 10_000_000);
        assert_eq!(performance.callback.total_ns, 100);
        assert_eq!(performance.worker_wall.total_ns, 900);
        assert_eq!(performance.worker_cpu.total_ns, 700);
        assert_eq!(performance.worker_wall.histogram, source.wall.bins);
        assert_eq!(performance.worker_cpu.histogram, source.cpu.bins);
        // The existing wire has no allowance-overrun field: it must not be
        // silently relabelled as frame-budget overruns.
        assert_eq!(performance.worker_wall.overruns, 0);
    }
}
