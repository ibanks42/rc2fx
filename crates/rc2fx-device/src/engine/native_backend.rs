//! The controller's front door for a device-local native callback owner.
//! This module neither loads the audit library nor launches/restarts vendor audio.
use crate::{coordinator::Error, recent::PathRecent, store::PreparedSave};
use rc2fx_core::{Config, Meter, MeterType, TargetKind, TargetMeter};
use rc2fx_native_control::{
    Client, ContextStatus, MAX_PATHS, METER_RING_WINDOWS, METER_WINDOW_FRAMES, Operation, PathMode,
    Publication, RESET_POLICY_VERSION, Reply, ResetPolicy, TargetStatus, Transaction, fingerprint,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Poll cadence while a client is subscribed or the engine is settling (~9 Hz).
const POLL_INTERVAL: Duration = Duration::from_millis(110);
/// Retry delay while the native library responds with Busy.
const BUSY_PAUSE: Duration = Duration::from_millis(100);
/// Max idle poll gap, kept under the 1 s diagnostics history cadence.
const IDLE_POLL_LIMIT: Duration = Duration::from_millis(900);
/// Headroom left in the native meter ring at idle so a slow save doesn't drop windows.
const RING_SLACK: Duration = Duration::from_millis(150);
/// Idle poll cadence when unwatched and healthy; drops back to `POLL_INTERVAL`
/// if any processed path stops advancing. Derived from `METER_RING_WINDOWS`.
const IDLE_POLL_INTERVAL: Duration = idle_poll_interval(METER_RING_WINDOWS);
const fn idle_poll_interval(ring_windows: usize) -> Duration {
    // A window is never shorter than its frame count at 48 kHz.
    let span_ms = ring_windows as u64 * METER_WINDOW_FRAMES * 1000 / 48_000;
    let room_ms = span_ms.saturating_sub(RING_SLACK.as_millis() as u64);
    let limit_ms = IDLE_POLL_LIMIT.as_millis() as u64;
    let fast_ms = POLL_INTERVAL.as_millis() as u64;
    let idle_ms = if room_ms < limit_ms {
        room_ms
    } else {
        limit_ms
    };
    Duration::from_millis(if idle_ms < fast_ms { fast_ms } else { idle_ms })
}
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// How long live edits play with nobody subscribed before the daemon saves
/// them itself. A reconnecting app (a new USB session, or a subscription
/// renewed after an overflow) subscribes again well within this.
const LIVE_ORPHAN_GRACE: Duration = Duration::from_secs(2);
const REGISTRY_WAIT: Duration = Duration::from_secs(60);
// Native USB ports exist before their hardware streams have finished startup.
// Only first progress gets this grace; a previously advancing path retains the
// ordinary short stall deadline.
const INITIAL_PROGRESS_TIMEOUT: Duration = Duration::from_secs(15);
struct Bound {
    id: String,
    context: u64,
    expected: [u8; 32],
    mode: PathMode,
    kind: TargetKind,
    channels: u8,
    publication: u64,
    frames: u64,
    last_progress: Instant,
    progressed: bool,
    /// The callback and worker peaks since the last read.
    recent: PathRecent,
    /// `publication` is a live edit the callback has not yet confirmed. Until
    /// it does, the path counts as applied when the library has published
    /// exactly that edit: a live edit never waits for an acknowledgement,
    /// so it must not make the engine look unsettled in between.
    unconfirmed: bool,
}
impl Bound {
    fn timed_out(&self, now: Instant, started: Instant, timeout: Duration) -> bool {
        if self.progressed {
            now.duration_since(self.last_progress) >= timeout
        } else {
            now.duration_since(started) >= INITIAL_PROGRESS_TIMEOUT.max(timeout)
        }
    }
    /// Files one inventory read: the context's callback peak and this path's
    /// worker peak. A read that timed nothing ages the older peaks out.
    fn observe_recent(&mut self, callback: Option<u64>, worker: Option<u64>, now: Instant) {
        self.recent.observe(callback, worker, now);
    }
    fn clear_recent(&mut self) {
        self.recent.clear();
    }
}
pub struct Runtime {
    client: Client,
    paths: Vec<Bound>,
    pending: Vec<Transaction>,
    revision: u64,
    selected: Vec<String>,
    enabled: bool,
    generation: u64,
    progress_timeout: Duration,
    /// The idle interval for this stall deadline.
    idle_interval: Duration,
    next_poll: Instant,
    /// The wait for `next_poll` is an idle one, which a new subscriber ends.
    idle: bool,
    /// The latest read found the engine Running, and nothing was written
    /// since. See `can_save_or_enable` and `can_disable` for what it admits.
    writable: bool,
    started: Instant,
    pub(crate) process: crate::diagnostic_history::NativeProcess,
    /// The targets live edits play, when they differ from the saved ones.
    /// Any publication of saved settings (a save, a routing or selection
    /// change) replaces them; a new connection starts without any.
    live: Option<Vec<rc2fx_core::Target>>,
    /// Since when live edits have played with nobody subscribed.
    orphaned_since: Option<Instant>,
}
/// One read of the engine.
pub(crate) struct Observation {
    pub progress: Progress,
    pub enabled: bool,
    pub active_ids: Vec<String>,
    pub message: String,
    pub native: Vec<ContextStatus>,
}
/// Where the engine stands after one read.
#[derive(Debug)]
pub(crate) enum Progress {
    /// The engine could not be read, a worker failed or a path stopped.
    Failed,
    /// Readable, but a path has not applied the saved settings yet, or the
    /// audio is not live. `advancing` says whether the audio itself moves: a
    /// path can advance before it applies a new publication.
    Settling { advancing: bool },
    /// Every path applied the saved settings, no publication is pending and
    /// the audio advances. The meter is None until every path has a window
    /// for the applied publication. `all_advanced` says this read saw every
    /// processed path advance, and at least one path; only then may polling
    /// slow to the idle interval.
    Running {
        meter: Option<Meter>,
        all_advanced: bool,
    },
}
impl Progress {
    /// Whether the audio moves, applied or not.
    pub(crate) fn advancing(&self) -> bool {
        match self {
            Progress::Failed => false,
            Progress::Settling { advancing } => *advancing,
            Progress::Running { .. } => true,
        }
    }
    /// Whether every path applied the saved settings and the audio advances.
    pub(crate) fn running(&self) -> bool {
        matches!(self, Progress::Running { .. })
    }
}
struct ExpectedPath {
    id: String,
    fingerprint: [u8; 32],
    revision: u64,
    mode: PathMode,
}
struct PreparedChange {
    transactions: Vec<Transaction>,
    expected: Vec<ExpectedPath>,
}
fn desired_mode(config: &Config, enabled: bool, id: &str) -> PathMode {
    if enabled
        && config
            .selected_targets
            .iter()
            .any(|selected| selected == id)
    {
        PathMode::Processed
    } else {
        PathMode::Passthrough
    }
}
fn failure(value: impl std::fmt::Display) -> Error {
    Error::Native(value.to_string())
}

fn inventory(client: &mut Client) -> Result<Vec<ContextStatus>, Error> {
    match client.request(Operation::Inventory).map_err(failure)?.reply {
        Reply::Inventory { contexts } => Ok(contexts),
        Reply::Error {
            code: rc2fx_native_control::ErrorCode::Busy,
        } => Err(Error::NativeBusy),
        reply => Err(failure(format!("inventory refused: {reply:?}"))),
    }
}
/// The control timing as the app's model: same fields, the histogram is the
/// last one.
/// No samples: `calls == 0` is "not measured", never "free".
fn no_timing() -> rc2fx_core::Timing {
    rc2fx_core::Timing {
        calls: 0,
        total_ns: 0,
        max_ns: 0,
        overruns: 0,
        clock_errors: 0,
        histogram: [0; 8],
    }
}

fn performance_timing(value: rc2fx_native_control::Timing) -> rc2fx_core::Timing {
    rc2fx_core::Timing {
        calls: value.calls,
        total_ns: value.total_ns,
        max_ns: value.max_ns,
        overruns: value.overruns,
        clock_errors: value.clock_errors,
        histogram: value.histogram,
    }
}

/// What the engine reports for one path: the context's callback timing (when
/// sampled), the engine-wide counters, and the worker's own timings when the
/// path has one. A path without a worker still reports the shared numbers;
/// its worker timings are zero and its calls count says "no worker". The two
/// recent maxima come from the reader's window, not from this reply.
fn engine_performance(
    contexts: &[ContextStatus],
    id: &str,
    worker: Option<rc2fx_native_control::WorkerMeter>,
    callback_recent_max_ns: Option<u64>,
    worker_recent_max_ns: Option<u64>,
) -> rc2fx_core::Performance {
    let context = contexts
        .iter()
        .find(|context| context.targets.iter().any(|target| target.id == id));
    let timing = context.and_then(|context| context.timing);
    let counters = context.map(|context| context.counters).unwrap_or_default();
    rc2fx_core::Performance {
        callback_budget_ns: timing.map_or(0, |value| value.callback_budget_ns),
        worker_budget_ns: worker.map_or(0, |value| value.worker_allowance_ns),
        xruns: counters.xruns,
        callback: timing.map_or_else(no_timing, |value| performance_timing(value.callback_wall)),
        worker_wall: worker.map_or_else(no_timing, |value| performance_timing(value.turnaround)),
        worker_cpu: worker.map_or_else(no_timing, |value| performance_timing(value.cpu)),
        helper_late: Some(counters.helper_late),
        helper_stalls: Some(counters.helper_stalls),
        callback_recent_max_ns,
        worker_recent_max_ns,
    }
}

fn locate<'a>(contexts: &'a [ContextStatus], path: &Bound) -> Result<&'a TargetStatus, Error> {
    let mut matches = contexts
        .iter()
        .filter(|context| context.context == path.context)
        .flat_map(|context| &context.targets)
        .filter(|target| target.id == path.id);
    let target = matches
        .next()
        .ok_or_else(|| failure("native path lifetime ended"))?;
    if matches.next().is_some() {
        return Err(failure("ambiguous native path"));
    }
    Ok(target)
}
impl Runtime {
    pub fn connect(
        path: &Path,
        config: &Config,
        progress_timeout: Duration,
    ) -> Result<Self, Error> {
        config.validate().map_err(failure)?;
        if progress_timeout < Duration::from_millis(200)
            || progress_timeout > Duration::from_secs(30)
        {
            return Err(failure("invalid native progress timeout"));
        }
        let mut client = Client::connect(path).map_err(failure)?;
        let process = crate::diagnostic_history::NativeProcess::capture(
            client
                .session()
                .ok_or_else(|| failure("native session identity unavailable"))?
                .pid,
        );
        // The library answers Busy while RØDE's mixer is still registering its
        // audio ports after a start.
        let deadline = Instant::now() + REGISTRY_WAIT;
        let contexts = loop {
            match inventory(&mut client) {
                Err(Error::NativeBusy) if Instant::now() < deadline => {
                    std::thread::sleep(BUSY_PAUSE);
                }
                result => break result?,
            }
        };
        let count: usize = contexts.iter().map(|context| context.targets.len()).sum();
        if count == 0 || count > MAX_PATHS {
            return Err(failure("invalid native bound path count"));
        }
        let enabled = config.selected_targets.is_empty()
            || contexts
                .iter()
                .flat_map(|context| &context.targets)
                .any(|target| target.mode == PathMode::Processed);
        let now = Instant::now();
        let mut paths = Vec::with_capacity(count);
        // Paths that play other settings than the saved ones: an unsaved
        // live edit left by a daemon that crashed or was killed before it could
        // put the saved settings back.
        let mut stale = Vec::new();
        for target in config.targets.iter().filter(|target| {
            contexts
                .iter()
                .flat_map(|context| &context.targets)
                .any(|status| status.id == target.id)
        }) {
            let expected = fingerprint(target).map_err(failure)?;
            let matches: Vec<_> = contexts
                .iter()
                .flat_map(|context| {
                    context
                        .targets
                        .iter()
                        .filter(|status| status.id == target.id)
                        .map(move |status| (context.context, status))
                })
                .collect();
            if matches.len() != 1
                || matches[0].0 == 0
                || matches[0].1.published == 0
                || matches[0].1.mode != desired_mode(config, enabled, &target.id)
            {
                return Err(failure(
                    "native path/settings do not match saved configuration",
                ));
            }
            let (context, status) = matches[0];
            if status.fingerprint != expected {
                stale.push(target.id.clone());
            }
            paths.push(Bound {
                id: target.id.clone(),
                context,
                // What the path plays now. For a stale path that differs from
                // the saved settings, so `prepare_change` below republishes it.
                expected: status.fingerprint,
                mode: status.mode,
                kind: target.kind,
                channels: target.channels,
                publication: status.published,
                frames: status.meter.map_or(0, |meter| meter.processed_frames),
                last_progress: now,
                progressed: false,
                recent: PathRecent::default(),
                unconfirmed: false,
            });
        }
        if paths.len() != count
            || config
                .selected_targets
                .iter()
                .any(|id| !paths.iter().any(|path| path.id == *id))
        {
            return Err(failure("saved selection includes an unbound native path"));
        }
        let mut runtime = Self {
            client,
            paths,
            pending: Vec::new(),
            revision: config.revision,
            selected: config.selected_targets.clone(),
            enabled,
            generation: 0,
            progress_timeout,
            // A Busy reply after an idle wait is retried at the fast rate
            // until the stall deadline, so the idle interval may use at most
            // half of that deadline.
            idle_interval: IDLE_POLL_INTERVAL.min((progress_timeout / 2).max(POLL_INTERVAL)),
            next_poll: now,
            idle: false,
            writable: false,
            started: now,
            process,
            live: None,
            orphaned_since: None,
        };
        if !stale.is_empty() {
            runtime.restore_saved(config, &stale)?;
        }
        Ok(runtime)
    }
    /// Restores saved settings on paths left playing unsaved live edits (e.g.
    /// after a daemon crash), using normal Prepare/Commit. Refuses any path
    /// that would require a processing reset.
    fn restore_saved(&mut self, config: &Config, stale: &[String]) -> Result<(), Error> {
        eprintln!(
            "Native paths {} play settings that were never saved (live edits left by an \
             earlier daemon); restoring the saved settings",
            stale.join(", ")
        );
        let change = self
            .prepare_retrying(config, self.enabled, ResetPolicy::Refuse)
            .map_err(|error| {
                failure(format!(
                    "native path/settings do not match saved configuration, and restoring \
                     them failed: {error}"
                ))
            })?;
        debug_assert!(change.expected.iter().all(|path| stale.contains(&path.id)));
        self.publish_change(change, config, self.enabled);
        Ok(())
    }
    pub(crate) fn attach(&mut self, config: &Config, generation: u64) -> Result<(), Error> {
        if self.revision != config.revision
            || generation == 0
            || self.selected != config.selected_targets
            || self.paths.iter().any(|path| {
                !config.targets.iter().any(|target| {
                    target.id == path.id && fingerprint(target).ok() == Some(path.expected)
                })
            })
        {
            return Err(failure("native attachment does not match the controller"));
        }
        self.generation = generation;
        // A new engine generation has its own maxima, which must not inherit
        // the previous one's spikes.
        for path in &mut self.paths {
            path.clear_recent();
        }
        Ok(())
    }
    /// A diagnostics reset starts the recent maxima over as well.
    pub(crate) fn clear_recent(&mut self) {
        for path in &mut self.paths {
            path.clear_recent();
        }
    }
    pub(crate) fn selected_ids(&self) -> Vec<String> {
        if self.enabled {
            self.selected.clone()
        } else {
            Vec::new()
        }
    }
    pub(crate) fn bound_ids(&self) -> Vec<String> {
        self.paths.iter().map(|path| path.id.clone()).collect()
    }
    /// The protocol the library agreed to for this session; below
    /// `rc2fx_native_control::VERSION` means it predates this daemon.
    pub fn protocol(&self) -> u32 {
        self.client.protocol()
    }
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }
    fn cancel(&mut self, prepared: &[Transaction]) {
        for transaction in prepared {
            let _ = self.client.request(Operation::Cancel {
                transaction: transaction.clone(),
            });
        }
    }
    /// Prepare all changed active paths before one durable save. Publications
    /// then advance independently at each path's callback boundary. A partial
    /// or lost commit acknowledgement is retained as pending, never rolled back
    /// or misreported as an atomic preset-wide audio transition.
    ///
    /// Returns whether anything was published. Nothing is when the audio
    /// already plays the saved settings, as after live edits of exactly them;
    /// such a save needs nothing from the library, so it is taken even while
    /// an earlier publication waits for its acknowledgement.
    pub(crate) fn save(&mut self, save: PreparedSave<'_>) -> Result<bool, Error> {
        if !self.can_save_or_enable() && !self.plays(save.candidate_config()) {
            return Err(failure(
                "Native audio is not ready for another settings change; no settings were saved",
            ));
        }
        let prepared =
            self.prepare_retrying(save.candidate_config(), self.enabled, ResetPolicy::Allow)?;
        let saved = match save.commit() {
            Ok(saved) => saved,
            Err(error) => {
                self.cancel(&prepared.transactions);
                self.writable = false;
                return Err(Error::Store(error));
            }
        };
        Ok(self.publish_change(prepared, &saved, self.enabled))
    }
    /// Every bound path already plays `config`'s settings in its mode.
    fn plays(&self, config: &Config) -> bool {
        self.paths.iter().all(|path| {
            path.mode == desired_mode(config, self.enabled, &path.id)
                && config.targets.iter().any(|target| {
                    target.id == path.id && fingerprint(target).ok() == Some(path.expected)
                })
        })
    }
    /// The targets live edits play, when they differ from the saved ones.
    pub(crate) fn live_targets(&self) -> Option<&[rc2fx_core::Target]> {
        self.live.as_deref()
    }
    /// The audio plays live edits that are not saved yet.
    pub(crate) fn live_editing(&self) -> bool {
        self.live.is_some()
    }
    /// Pushes `candidate` (parameter-only changes, same revision/selection) to
    /// the running engine without saving. Does not retry Busy and does not mark
    /// the engine unsettled or unwritable.
    pub(crate) fn live(&mut self, candidate: &Config, saved: &Config) -> Result<(), Error> {
        if !self.can_save_or_enable() {
            return Err(Error::NativeBusy);
        }
        if candidate.revision != self.revision
            || saved.revision != self.revision
            || candidate.selected_targets != self.selected
            || saved.selected_targets != self.selected
        {
            return Err(Error::InvalidRuntime);
        }
        let change = self
            .prepare_change(candidate, self.enabled, self.parameters_only())
            .map_err(|error| match error {
                // The library's own refusal to rebuild a path in place.
                Error::NativeUnsupported(message) => Error::LiveRequiresSave(message),
                error => error,
            })?;
        self.publish_unsaved(change);
        self.live = (candidate.targets != saved.targets).then(|| candidate.targets.clone());
        Ok(())
    }
    /// Puts the saved settings back on the audio in place of live edits,
    /// only when keeping the edits failed (see `Controller::keep_live`).
    /// `retry` waits out a busy library as a save does (for shutdown); without
    /// it a busy library is left for the next try.
    pub(crate) fn restore_live(&mut self, saved: &Config, retry: bool) -> Result<(), Error> {
        if self.live.is_none() {
            return Ok(());
        }
        if !self.pending.is_empty() {
            return Err(Error::NativeBusy);
        }
        if saved.revision != self.revision || saved.selected_targets != self.selected {
            return Err(Error::InvalidRuntime);
        }
        let change = if retry {
            self.prepare_retrying(saved, self.enabled, self.parameters_only())?
        } else {
            self.prepare_change(saved, self.enabled, self.parameters_only())?
        };
        self.publish_unsaved(change);
        self.live = None;
        self.orphaned_since = None;
        Ok(())
    }
    /// Whether live edits have played with nobody subscribed for
    /// `LIVE_ORPHAN_GRACE`: the app that made them went away without saving,
    /// so the daemon should keep them for it.
    pub(crate) fn live_orphaned(&mut self, watched: bool, now: Instant) -> bool {
        if watched || self.live.is_none() {
            self.orphaned_since = None;
            return false;
        }
        let since = *self.orphaned_since.get_or_insert(now);
        now.duration_since(since) >= LIVE_ORPHAN_GRACE
    }
    /// Commits a prepared change that nothing saved: a live edit, or the saved
    /// settings put back in its place. The revision and selection stay. A lost
    /// commit reply is kept as pending and retried with the same receipt, as
    /// for a save, since the library may already have published it.
    fn publish_unsaved(&mut self, change: PreparedChange) {
        for next in change.expected {
            let path = self
                .paths
                .iter_mut()
                .find(|path| path.id == next.id)
                .expect("bound native path");
            path.expected = next.fingerprint;
            path.publication = next.revision;
            path.mode = next.mode;
            path.unconfirmed = true;
        }
        for transaction in change.transactions {
            if matches!(
                self.client.commit_saved(&transaction),
                Publication::Pending { .. }
            ) {
                self.pending.push(transaction);
            }
        }
    }
    pub(crate) fn set_enabled(&mut self, config: &Config, enabled: bool) -> Result<(), Error> {
        let admitted = if enabled {
            self.can_save_or_enable()
        } else {
            self.can_disable()
        };
        if !admitted {
            return Err(Error::NativeBusy);
        }
        let prepared = self.prepare_retrying(config, enabled, ResetPolicy::Allow)?;
        self.publish_change(prepared, config, enabled);
        Ok(())
    }
    /// The policy for a change that must not rebuild processing (a live
    /// edit, or putting saved settings back in its place): Refuse, where the library
    /// knows it. An older library gets Allow; the daemon has already refused
    /// any such change it would be asked for.
    fn parameters_only(&self) -> ResetPolicy {
        if self.client.protocol() >= RESET_POLICY_VERSION {
            ResetPolicy::Refuse
        } else {
            ResetPolicy::Allow
        }
    }
    /// A settings save, a live edit or an enable needs the engine Running at
    /// the last read, and no publication pending. A save or an enable clears
    /// `writable` until the next read; a live edit does not, but may leave a
    /// publication pending, so both are checked.
    fn can_save_or_enable(&self) -> bool {
        self.writable && self.pending.is_empty()
    }
    /// An explicit disable can still remove a failed DSP path, so it does not
    /// need a Running engine. It must not overwrite an already-saved
    /// publication whose acknowledgement is pending.
    fn can_disable(&self) -> bool {
        self.pending.is_empty()
    }
    /// The native library refuses with Busy while RØDE's mixer registers or
    /// closes audio ports (USB streams starting or stopping) or a replaced
    /// worker is still retiring. Both clear within moments, and a refused
    /// preparation has canceled everything it staged, so try again briefly.
    fn prepare_retrying(
        &mut self,
        config: &Config,
        enabled: bool,
        reset: ResetPolicy,
    ) -> Result<PreparedChange, Error> {
        let deadline = Instant::now() + BUSY_RETRY;
        loop {
            match self.prepare_change(config, enabled, reset) {
                Err(Error::NativeBusy) if Instant::now() < deadline => {
                    std::thread::sleep(BUSY_PAUSE);
                }
                result => return result,
            }
        }
    }
    /// `reset` is sent with every Prepare; `Refuse` needs a library that
    /// speaks `RESET_POLICY_VERSION`, and fails here without one.
    fn prepare_change(
        &mut self,
        config: &Config,
        enabled: bool,
        reset: ResetPolicy,
    ) -> Result<PreparedChange, Error> {
        if reset == ResetPolicy::Refuse && self.client.protocol() < RESET_POLICY_VERSION {
            return Err(Error::NativeUnsupported(
                "the loaded native library cannot refuse a processing rebuild; restart the \
                 mixer's audio to load the current one"
                    .into(),
            ));
        }
        if config
            .selected_targets
            .iter()
            .any(|id| !self.paths.iter().any(|path| path.id == *id))
        {
            return Err(Error::NativeUnsupported(
                "This build can enable only ports already bound to the native callback".into(),
            ));
        }
        let mut prepared = Vec::new();
        let mut expected = Vec::new();
        for path in &self.paths {
            let target = config
                .targets
                .iter()
                .find(|target| target.id == path.id)
                .ok_or(Error::InvalidRuntime)?;
            if target.kind != path.kind || target.channels != path.channels {
                self.cancel(&prepared);
                return Err(Error::NativeUnsupported(
                    "Changing native port kind or channel count requires a new binding".into(),
                ));
            }
            let digest = fingerprint(target).map_err(failure)?;
            let mode = desired_mode(config, enabled, &path.id);
            if digest == path.expected && mode == path.mode {
                continue;
            }
            let revision = config.revision.max(
                path.publication
                    .checked_add(1)
                    .ok_or(Error::InvalidRuntime)?,
            );
            let transaction = match self.client.transaction(path.context, &path.id, revision) {
                Ok(transaction) => transaction,
                Err(error) => {
                    self.cancel(&prepared);
                    return Err(failure(error));
                }
            };
            let result = self.client.request(Operation::Prepare {
                transaction: transaction.clone(),
                target: Box::new(target.clone()),
                mode,
                reset,
            });
            prepared.push(transaction.clone());
            let preparation = match result {
                Ok(response) => match response.reply {
                    Reply::Prepared {
                        transaction: receipt,
                    } if receipt == transaction => Ok(()),
                    Reply::Error {
                        code: rc2fx_native_control::ErrorCode::RequiresReset,
                    } => Err(Error::NativeUnsupported(format!(
                        "Path {} requires signal-state replacement for this change; settings were not saved",
                        path.id
                    ))),
                    Reply::Error {
                        code: rc2fx_native_control::ErrorCode::SuppressionInUse,
                    } => Err(Error::SuppressionInUse),
                    Reply::Error {
                        code: rc2fx_native_control::ErrorCode::Busy,
                    } => Err(Error::NativeBusy),
                    Reply::Error {
                        code: rc2fx_native_control::ErrorCode::InvalidParameters,
                    } => Err(Error::Store(crate::store::StoreError::Invalid(format!(
                        "Native parameters for {} were rejected",
                        path.id
                    )))),
                    Reply::Error { code } => Err(failure(format!(
                        "Preparation for {} refused ({code:?}); settings were not saved",
                        path.id
                    ))),
                    _ => Err(failure(format!(
                        "Invalid preparation acknowledgement for {}; settings were not saved",
                        path.id
                    ))),
                },
                Err(error) => Err(failure(format!(
                    "Preparation for {} failed: {error}; settings were not saved",
                    path.id
                ))),
            };
            if let Err(error) = preparation {
                self.cancel(&prepared);
                return Err(error);
            }
            expected.push(ExpectedPath {
                id: path.id.clone(),
                fingerprint: digest,
                revision,
                mode,
            });
        }
        Ok(PreparedChange {
            transactions: prepared,
            expected,
        })
    }
    /// Returns whether anything was published.
    fn publish_change(&mut self, change: PreparedChange, config: &Config, enabled: bool) -> bool {
        self.revision = config.revision;
        self.selected = config.selected_targets.clone();
        self.enabled = enabled;
        // Every path now plays these saved settings, live edits or not.
        self.live = None;
        self.orphaned_since = None;
        let published = !change.transactions.is_empty();
        // Nothing published: nothing for the callback to acknowledge.
        if published {
            self.writable = false;
        }
        for next in change.expected {
            let path = self
                .paths
                .iter_mut()
                .find(|path| path.id == next.id)
                .expect("bound native path");
            path.expected = next.fingerprint;
            path.publication = next.revision;
            path.mode = next.mode;
            path.unconfirmed = false;
        }
        for transaction in change.transactions {
            if matches!(
                self.client.commit_saved(&transaction),
                Publication::Pending { .. }
            ) {
                self.pending.push(transaction);
            }
        }
        self.next_poll = Instant::now();
        published
    }
    /// A processed path stopped advancing, or no path advances at all.
    fn stalled(&self, now: Instant) -> bool {
        let timed_out = |path: &Bound| path.timed_out(now, self.started, self.progress_timeout);
        self.paths
            .iter()
            .any(|path| path.mode == PathMode::Processed && timed_out(path))
            || self.paths.iter().all(timed_out)
    }
    /// How long until the next read, after a successful `observation`.
    fn interval_after(&self, observation: &Observation, watched: bool) -> Duration {
        // A write since the read may have left a publication pending.
        let settled = matches!(
            observation.progress,
            Progress::Running {
                all_advanced: true,
                ..
            }
        ) && self.pending.is_empty();
        if settled && !watched {
            self.idle_interval
        } else {
            POLL_INTERVAL
        }
    }
    /// When the next read is due. The controller's loop sleeps until then.
    pub(crate) fn next_poll(&self) -> Instant {
        self.next_poll
    }
    /// Reads the engine when a read is due. `watched` says whether a client is
    /// subscribed to live events.
    pub(crate) fn poll(&mut self, watched: impl Fn() -> bool) -> Option<Observation> {
        let now = Instant::now();
        // A client that subscribes during an idle wait gets its first meter
        // and the fast rate now, not up to an idle interval later.
        if now < self.next_poll && !(self.idle && watched()) {
            return None;
        }
        self.idle = false;
        self.next_poll = now + POLL_INTERVAL;
        match self.observe(now) {
            Ok(observation) => {
                let interval = self.interval_after(&observation, watched());
                self.idle = interval > POLL_INTERVAL;
                self.next_poll = now + interval;
                Some(observation)
            }
            Err(Error::NativeBusy) if !self.stalled(now) => None,
            Err(error) => {
                self.writable = false;
                Some(Observation {
                    progress: Progress::Failed,
                    enabled: self.enabled,
                    active_ids: Vec::new(),
                    message: error.to_string(),
                    native: Vec::new(),
                })
            }
        }
    }
    fn observe(&mut self, now: Instant) -> Result<Observation, Error> {
        let contexts = inventory(&mut self.client)?;
        if contexts
            .iter()
            .map(|context| context.targets.len())
            .sum::<usize>()
            != self.paths.len()
        {
            return Err(failure("native path membership changed"));
        }
        // Only after a responsive inventory, retry at most one already-saved
        // receipt per tick. A dead/hung peer cannot cause one serial timeout per path.
        if let Some(transaction) = self.pending.first()
            && matches!(
                self.client.commit_saved(transaction),
                Publication::Published(_)
            )
        {
            self.pending.remove(0);
        }
        let mut applied = self.pending.is_empty();
        let mut advancing = true;
        let mut any_live = false;
        let mut processed_advanced = true;
        let mut any_advanced = false;
        let mut meters = Vec::with_capacity(self.paths.len());
        for path in &mut self.paths {
            let status = locate(&contexts, path)?;
            if status.worker.is_some_and(|value| value.failed) {
                return Err(failure("native suppression worker failed"));
            }
            // One peak per read from each producer; the path's window keeps
            // them for its ten seconds.
            let callback_recent = contexts
                .iter()
                .find(|context| context.context == path.context)
                .and_then(|context| context.recent_callback_wall_ns);
            path.observe_recent(callback_recent, status.worker_recent_max_ns, now);
            let (callback_recent_max_ns, worker_recent_max_ns) = path.recent.value(now);
            // Liveness is independent of desired effect publication. A fresh
            // native block must not become a fake stalled-audio report merely
            // because an external controller temporarily changed this slot.
            let mut advanced = false;
            if let Some(meter) = status.meter {
                if meter.processed_frames < path.frames {
                    return Err(failure("native progress moved backwards"));
                }
                if meter.processed_frames > path.frames {
                    path.frames = meter.processed_frames;
                    path.last_progress = now;
                    path.progressed = true;
                    advanced = true;
                }
            }
            any_advanced |= advanced;
            processed_advanced &= advanced || path.mode != PathMode::Processed;
            // A verified newer native token for exactly the saved desired
            // bytes/mode can be observed without changing settings or audio.
            if self.pending.is_empty()
                && status.published > path.publication
                && status.applied == status.published
                && status.fingerprint == path.expected
                && status.mode == path.mode
            {
                path.publication = status.published;
            }
            let live =
                path.progressed && now.duration_since(path.last_progress) < self.progress_timeout;
            // An idle passthrough source runs no block, so its callback never
            // reports the newest token as applied. Nothing is waiting to be
            // heard there: the next block reads the newest published descriptor.
            // A live path still confirms application before further writes.
            let idle_passthrough = path.mode == PathMode::Passthrough && !live;
            let confirmed = status.applied == path.publication || idle_passthrough;
            if confirmed {
                path.unconfirmed = false;
            }
            // A published live edit counts before the callback confirms it:
            // the next block plays it, and nothing waits on it.
            applied &= status.published == path.publication
                && (confirmed || path.unconfirmed)
                && status.mode == path.mode
                && status.fingerprint == path.expected;
            let meter = status
                .meter
                .filter(|meter| meter.revision == path.publication);
            if meter.is_none() && path.mode == PathMode::Passthrough {
                // An idle stock source (unplugged USB, disconnected Bluetooth)
                // has no fresh block. It keeps a silent placeholder so the
                // meter set stays complete; its frames do not advance.
                meters.push(TargetMeter {
                    id: path.id.clone(),
                    input_peak: 0.,
                    output_peak: 0.,
                    rms: 0.,
                    gain_reduction: 0.,
                    voice: 0.,
                    gate_open: 0.,
                    clipped: 0,
                    spectrum: Vec::new(),
                    callbacks: 0,
                    processed_frames: path.frames,
                    suppression_frames: 0,
                    missed_deadlines: 0,
                    stale_results: 0,
                    input_drops: 0,
                    output_drops: 0,
                    latency_samples: 0,
                    worker_failed: false,
                    // An idle stock source has no worker of its own, but the
                    // engine's own numbers (callback, xruns, helper) are just
                    // as true for it.
                    performance: Some(engine_performance(
                        &contexts,
                        &path.id,
                        None,
                        callback_recent_max_ns,
                        worker_recent_max_ns,
                    )),
                });
            }
            if let Some(meter) = meter {
                let worker = status.worker;
                let performance = engine_performance(
                    &contexts,
                    &path.id,
                    worker,
                    callback_recent_max_ns,
                    worker_recent_max_ns,
                );
                meters.push(TargetMeter {
                    id: path.id.clone(),
                    input_peak: meter.input_peak,
                    output_peak: meter.output_peak,
                    rms: meter.rms,
                    gain_reduction: meter.gain_reduction,
                    voice: meter.voice,
                    gate_open: meter.gate_open,
                    clipped: meter.clipped,
                    spectrum: Vec::new(),
                    callbacks: meter.callbacks,
                    processed_frames: meter.processed_frames,
                    suppression_frames: worker.map_or(0, |value| value.completed_frames),
                    missed_deadlines: worker.map_or(0, |value| value.missed_deadlines),
                    stale_results: worker.map_or(0, |value| value.stale_results),
                    input_drops: worker.map_or(0, |value| value.input_drops),
                    output_drops: worker.map_or(0, |value| value.output_drops),
                    latency_samples: meter.latency_samples,
                    worker_failed: false,
                    performance: Some(performance),
                });
            }
            any_live |= live;
            // RC2FX is in the audio path only for processed paths; those must
            // advance. A passthrough path carries untouched stock audio, so
            // an idle source there is not an engine fault.
            if path.mode == PathMode::Processed {
                advancing &= live;
                if path.timed_out(now, self.started, self.progress_timeout) {
                    return Err(failure(format!(
                        "native audio path {} stopped advancing",
                        path.id
                    )));
                }
            }
        }
        // With no processed path, liveness is the vendor callback itself.
        if self
            .paths
            .iter()
            .all(|path| path.timed_out(now, self.started, self.progress_timeout))
        {
            return Err(failure("native audio callback stopped advancing"));
        }
        advancing &= any_live;
        self.writable = advancing && applied;
        let message = if advancing && applied {
            "Native audio paths applied and advancing"
        } else if !self.pending.is_empty() {
            "Settings saved; native audio publication pending"
        } else {
            "Waiting for matching native audio progress"
        }
        .into();
        let meter = (meters.len() == self.paths.len()).then_some(Meter {
            message_type: MeterType::Meter,
            revision: self.revision,
            generation: self.generation,
            synthetic: false,
            // Filled by the controller's diagnostics log, which owns the
            // baseline epoch; the backend reports raw engine figures.
            diagnostics_epoch: 0,
            // The controller also stamps how long this generation has been
            // running, from the audio process's start or its first Running
            // report; this backend reports raw progress only.
            running_for_ns: None,
            targets: meters,
        });
        let progress = if advancing && applied {
            Progress::Running {
                meter,
                all_advanced: processed_advanced && any_advanced,
            }
        } else {
            Progress::Settling { advancing }
        };
        Ok(Observation {
            progress,
            enabled: self.enabled,
            active_ids: self.selected_ids(),
            message,
            native: contexts,
        })
    }
}

#[cfg(test)]
#[path = "native_backend/tests.rs"]
mod tests;
