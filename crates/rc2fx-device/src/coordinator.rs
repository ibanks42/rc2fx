//! Bounded typed control messages, handled on one ordinary thread.
//! The owner holds the native engine connection; the control API only owns
//! the independent Handle.
use crate::{
    controller::Controller,
    diagnostic_history::ExportError,
    events::{EventError, Subscription},
    facade::Facade,
    store::StoreError,
};
use rc2fx_core::{
    ConfigUpdate, DeviceState, EngineMode, EngineRestartRequest, PresetSummary, RoutingRequest,
    SelectionRequest,
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
/// The loop's fixed tick where nothing gives it a deadline: with no native
/// engine attached, and while a resource sample cannot be taken yet.
const TICK: Duration = Duration::from_millis(20);
/// How often process resources and the USB status are sampled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// The shortest wait for a command, so a deadline that is already due costs
/// one pass of the loop and never a spin.
const MIN_WAIT: Duration = Duration::from_millis(1);

#[derive(Debug)]
pub enum Error {
    Store(StoreError),
    Events(EventError),
    Busy,
    Stopped,
    InvalidRuntime,
    Diagnostics(ExportError),
    Native(String),
    NativeBusy,
    SuppressionInUse,
    NativeUnsupported(String),
    /// A live edit would rebuild a path's processing (a new kind or channel
    /// count, noise suppression switched or re-shaped): only a save may do
    /// that. The message says what changed.
    LiveRequiresSave(String),
    /// Live editing needs the native engine; the store-only coordinator has none.
    LiveUnsupported,
    /// The engine is not running, so there is nothing to edit live.
    LiveUnavailable,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Events(error) => write!(f, "Event subscription failed: {error:?}"),
            Self::Busy => f.write_str("Controller queue is full"),
            Self::Stopped => f.write_str("Controller stopping"),
            Self::InvalidRuntime => f.write_str("Audio runtime does not match the saved selection"),
            Self::Diagnostics(error) => error.fmt(f),
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
            // The app matches this prefix (and the 422 status).
            Self::LiveRequiresSave(message) => write!(f, "liveRequiresSave: {message}"),
            // The app reads 404 as "this mixer cannot edit live" and saves instead.
            Self::LiveUnsupported => f.write_str("Unknown endpoint"),
            Self::LiveUnavailable => f.write_str("The audio engine is not running"),
        }
    }
}
impl std::error::Error for Error {}
impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

type Answer<T> = oneshot::Sender<Result<T, Error>>;
pub type Reply<T> = oneshot::Receiver<Result<T, Error>>;
enum Command {
    Apply(ConfigUpdate, Answer<DeviceState>),
    /// Effect settings on the running audio at once, saved later: answers
    /// with the revision they were based on.
    Live(ConfigUpdate, Answer<u64>),
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
    Transition(Request, Answer<DeviceState>),
    /// Nothing to do: ends the loop's wait so it looks at its deadlines again.
    Wake,
    /// Panics the loop, for tests of what a panicking pass leaves behind.
    #[cfg(test)]
    Panic,
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
    #[cfg(test)]
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
    /// Puts effect settings on the running native audio at once, without
    /// saving them yet (`PUT api/v1/live`). Answers with the saved revision,
    /// unchanged.
    pub fn live(&self, update: ConfigUpdate) -> Result<Reply<u64>, Error> {
        self.enqueue(|reply| Command::Live(update, reply))
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
    /// Wait for all accepted commands and event subscriber disconnection.
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
pub struct RuntimeDescriptor {
    pub generation: u64,
    pub mode: EngineMode,
    pub sample_rate: u32,
    pub block_size: u32,
}

/// Sets the loop's completion when dropped, on return or during a panic.
struct Completed(watch::Sender<bool>);
impl Drop for Completed {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

pub struct Coordinator {
    controller: Controller,
    native: Option<crate::native_backend::Runtime>,
    commands: mpsc::Receiver<Command>,
    completed: watch::Sender<bool>,
    handle: Handle,
    /// When the next resource sample is due.
    next_sample: Instant,
}
impl Coordinator {
    /// The coordinator rc2fxd runs: the bounded commands, control Handle and
    /// event stream, connected to the native engine in the mixer process.
    pub fn new_native(
        mut controller: Controller,
        mut runtime: crate::native_backend::Runtime,
    ) -> Result<(Handle, Self), Error> {
        controller.attach_native(&mut runtime)?;
        let (handle, mut owner) = Self::new(controller)?;
        owner.native = Some(runtime);
        Ok((handle, owner))
    }

    /// A coordinator with no audio engine attached: effects and presets are
    /// only saved, and routing, selection and restart requests are refused.
    /// rc2fxd always attaches the native engine through `new_native`, so on
    /// its own this store-only path serves only the tests of the API, UI and
    /// USB front ends.
    pub fn new(controller: Controller) -> Result<(Handle, Self), Error> {
        let handle = Handle::detached(&controller);
        // One slot more than the commands admitted: see `Admission::queued`.
        let (sender, commands) = mpsc::sync_channel(QUEUED_COMMANDS + 1);
        handle.install(sender)?;
        let owner = Self {
            controller,
            native: None,
            commands,
            completed: handle.0.completion.clone(),
            handle: handle.clone(),
            next_sample: Instant::now(),
        };
        Ok((handle, owner))
    }

    /// Run on an ordinary thread, never an async executor or callback.
    /// Returns the controller only after all admitted saves finish.
    pub fn run(mut self) -> Controller {
        self.run_loop();
        // Live edits still playing are what the user has: they are saved
        // before the daemon stops. The native library outlives this daemon,
        // and the next start checks that it plays the saved settings.
        if let Some(native) = &mut self.native
            && native.live_editing()
            && let Err(error) = self.controller.keep_live(native, true)
        {
            eprintln!("Could not keep the live settings at shutdown: {error}");
        }
        self.controller.disconnect_clients();
        self.controller
    }
    fn run_loop(&mut self) {
        // Marks the loop complete even when a pass panics, so `shutdown`
        // never waits for a loop that is gone and the daemon can exit.
        let _completed = Completed(self.completed.clone());
        while self.pass() {}
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

    /// The loop's timed work: read the native engine and sample resources
    /// when each is due. Returns how long the loop may then wait for a command.
    ///
    /// With the native engine the wait ends at the earlier of the next engine
    /// read and the next resource sample, so the thread sleeps between them. A
    /// command ends the wait at once, and the deadline is worked out again
    /// after every command, because a save makes the next read due now.
    /// Without an engine the loop keeps the fixed tick.
    pub(crate) fn turn(&mut self) -> Duration {
        if let Some(native) = &mut self.native {
            let controller = &self.controller;
            if let Some(observation) = native.poll(|| controller.has_clients()) {
                self.controller.native_progress(observation);
            }
            // Live edits nobody is watching any more (the app went away
            // without saving) are kept: the daemon saves what plays. A library
            // that is not ready is tried again on the next pass.
            let watched = native.live_editing() && self.controller.has_clients();
            if native.live_orphaned(watched, Instant::now()) {
                let _ = self.controller.keep_live(native, false);
            }
        }
        let now = Instant::now();
        if now >= self.next_sample {
            // The history decides whether a sample is taken. It takes none
            // before the first state is published; ask again on the next tick.
            let taken = self.controller.front.sample_resources(now);
            self.next_sample = now + if taken { SAMPLE_INTERVAL } else { TICK };
        }
        if let Some(native) = &self.native {
            return native
                .next_poll()
                .min(self.next_sample)
                .saturating_duration_since(Instant::now())
                .max(MIN_WAIT);
        }
        TICK
    }

    /// The native runtime, for tests that set its timing.
    #[cfg(test)]
    pub(crate) fn native_mut(&mut self) -> &mut crate::native_backend::Runtime {
        self.native.as_mut().expect("native coordinator")
    }

    fn execute(&mut self, command: Command) {
        // Reply cancellation never cancels an admitted operation or file commit.
        match command {
            Command::Wake => {}
            #[cfg(test)]
            Command::Panic => panic!("a test panicked the control loop"),
            Command::ResetDiagnostics(reply) => {
                if let Some(native) = &mut self.native {
                    native.clear_recent();
                }
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
                if let Some(native) = &mut self.native {
                    let result = self.controller.apply_native(update, native);
                    let _ = reply.send(result);
                    return;
                }
                let _ = reply.send(self.controller.apply(update));
            }
            Command::Live(update, reply) => {
                if let Some(native) = &mut self.native {
                    let result = self.controller.live_native(update, native);
                    let _ = reply.send(result);
                    return;
                }
                // Without the native engine nothing plays to edit live.
                drop(update);
                let _ = reply.send(Err(Error::LiveUnsupported));
            }
            Command::CreatePreset(name, revision, reply) => {
                let result = self
                    .controller
                    .create_preset(&name, revision)
                    .map_err(Error::Store);
                let _ = reply.send(result);
            }
            Command::ApplyPreset(id, revision, reply) => {
                if let Some(native) = &mut self.native {
                    let result = self.controller.apply_native_preset(&id, revision, native);
                    let _ = reply.send(result);
                    return;
                }
                let _ = reply.send(self.controller.apply_preset(&id, revision));
            }
            Command::Settings(reply) => {
                let _ = reply.send(self.controller.settings().map_err(Error::Store));
            }
            Command::DeletePreset(id, reply) => {
                let result = self.controller.delete_preset(&id).map_err(Error::Store);
                let _ = reply.send(result);
            }
            Command::Transition(request, reply) => {
                if let Some(native) = &mut self.native {
                    let result = self.controller.native_transition(request, native);
                    let _ = reply.send(result);
                    return;
                }
                // Routing, selection and restart act on a running engine.
                drop(request);
                let _ = reply.send(Err(Error::NativeUnsupported(
                    "No audio engine is attached".into(),
                )));
            }
        }
        if let Some(fault) = self.controller.durability_fault().map(str::to_owned) {
            self.controller
                .unavailable(&format!("Durable save uncertain: {fault}"));
        }
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use crate::store::Store;
    use rc2fx_core::Config;

    fn samples(handle: &Handle) -> usize {
        let report: serde_json::Value =
            serde_json::from_slice(&handle.diagnostic_report().unwrap()).unwrap();
        report["samples"].as_array().unwrap().len()
    }

    /// The deadline-driven wait belongs to the native engine. Without one
    /// the loop keeps its fixed tick.
    #[test]
    fn without_the_native_engine_the_loop_keeps_its_fixed_tick() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (_handle, mut owner) = Coordinator::new(Controller::new(store)).unwrap();
        for _ in 0..3 {
            assert_eq!(owner.turn(), TICK);
        }
    }

    /// A subscriber wakes the loop through the command channel. The wake has
    /// a slot of its own: it is never refused behind queued commands, and it
    /// never makes the queue admit fewer of them.
    #[test]
    fn a_wake_neither_takes_nor_needs_a_command_slot() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, mut owner) = Coordinator::new(Controller::new(store)).unwrap();
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
        let (handle, owner) = Coordinator::new(Controller::new(store)).unwrap();
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

    /// A pass that panics still marks the loop complete: later commands are
    /// refused as stopped, and `shutdown` returns instead of waiting for a
    /// loop that is gone, so rc2fxd can exit and the supervisor react.
    #[test]
    fn a_panicking_pass_still_completes_the_loop() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, owner) = Coordinator::new(Controller::new(store)).unwrap();
        let task = std::thread::spawn(move || owner.run());
        let _reply = handle.enqueue(|_: Answer<()>| Command::Panic).unwrap();
        assert!(task.join().is_err(), "the loop did not panic");
        assert!(matches!(handle.settings(), Err(Error::Stopped)));
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let shutdown = executor.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), handle.shutdown()).await
        });
        assert!(
            matches!(shutdown, Ok(Ok(()))),
            "shutdown waited for a dead loop"
        );
    }

    #[test]
    fn resources_are_sampled_once_a_second_and_the_history_has_the_last_word() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), Config::default()).unwrap();
        let (handle, mut owner) = Coordinator::new(Controller::new(store)).unwrap();
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
