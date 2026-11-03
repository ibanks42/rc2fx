//! The controller's front door for a device-local native callback owner.
//! This module neither loads the audit library nor launches/restarts vendor audio.
use crate::{coordinator::Error, store::PreparedSave};
use rc2fx_core::{Config, Meter, MeterType, TargetKind, TargetMeter};
use rc2fx_native_control::{
    Client, ContextStatus, MAX_PATHS, Operation, PathMode, Publication, Reply, TargetStatus,
    Transaction, fingerprint,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const BUSY_RETRY: Duration = Duration::from_secs(2);
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
}
impl Bound {
    fn timed_out(&self, now: Instant, started: Instant, timeout: Duration) -> bool {
        if self.progressed {
            now.duration_since(self.last_progress) >= timeout
        } else {
            now.duration_since(started) >= INITIAL_PROGRESS_TIMEOUT.max(timeout)
        }
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
    next_poll: Instant,
    writable: bool,
    started: Instant,
    pub(crate) process: crate::diagnostic_history::NativeProcess,
}
pub(crate) struct Observation {
    pub failure: bool,
    pub applied: bool,
    pub advancing: bool,
    pub enabled: bool,
    pub active_ids: Vec<String>,
    pub message: String,
    pub meter: Option<Meter>,
    pub native: Vec<ContextStatus>,
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
/// its worker timings are zero and its calls count says "no worker".
fn engine_performance(
    contexts: &[ContextStatus],
    id: &str,
    worker: Option<rc2fx_native_control::WorkerMeter>,
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
                    std::thread::sleep(POLL_INTERVAL);
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
                || matches[0].1.fingerprint != expected
                || matches[0].1.mode != desired_mode(config, enabled, &target.id)
            {
                return Err(failure(
                    "native path/settings do not match saved configuration",
                ));
            }
            let (context, status) = matches[0];
            paths.push(Bound {
                id: target.id.clone(),
                context,
                expected,
                mode: status.mode,
                kind: target.kind,
                channels: target.channels,
                publication: status.published,
                frames: status.meter.map_or(0, |meter| meter.processed_frames),
                last_progress: now,
                progressed: false,
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
        Ok(Self {
            client,
            paths,
            pending: Vec::new(),
            revision: config.revision,
            selected: config.selected_targets.clone(),
            enabled,
            generation: 0,
            progress_timeout,
            next_poll: now,
            writable: false,
            started: now,
            process,
        })
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
        Ok(())
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
    pub(crate) fn save(&mut self, save: PreparedSave<'_>) -> Result<(), Error> {
        if !self.writable || !self.pending.is_empty() {
            return Err(failure(
                "Native audio is not ready for another settings change; no settings were saved",
            ));
        }
        let prepared = self.prepare_retrying(save.candidate_config(), self.enabled)?;
        let saved = match save.commit() {
            Ok(saved) => saved,
            Err(error) => {
                self.cancel(&prepared.transactions);
                self.writable = false;
                return Err(Error::Store(error));
            }
        };
        self.publish_change(prepared, &saved, self.enabled);
        Ok(())
    }
    pub(crate) fn set_enabled(&mut self, config: &Config, enabled: bool) -> Result<(), Error> {
        // Explicit disable can still remove a failed DSP path. It must not
        // overwrite an already-saved publication whose acknowledgement is pending.
        if !self.pending.is_empty() || (enabled && !self.writable) {
            return Err(Error::NativeBusy);
        }
        let prepared = self.prepare_retrying(config, enabled)?;
        self.publish_change(prepared, config, enabled);
        Ok(())
    }
    /// The native library refuses with Busy while RØDE's mixer registers or
    /// closes audio ports (USB streams starting or stopping) or a replaced
    /// worker is still retiring. Both clear within moments, and a refused
    /// preparation has cancelled everything it staged, so try again briefly.
    fn prepare_retrying(
        &mut self,
        config: &Config,
        enabled: bool,
    ) -> Result<PreparedChange, Error> {
        let deadline = Instant::now() + BUSY_RETRY;
        loop {
            match self.prepare_change(config, enabled) {
                Err(Error::NativeBusy) if Instant::now() < deadline => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                result => return result,
            }
        }
    }
    fn prepare_change(&mut self, config: &Config, enabled: bool) -> Result<PreparedChange, Error> {
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
    fn publish_change(&mut self, change: PreparedChange, config: &Config, enabled: bool) {
        self.revision = config.revision;
        self.selected = config.selected_targets.clone();
        self.enabled = enabled;
        self.writable = false;
        for next in change.expected {
            let path = self
                .paths
                .iter_mut()
                .find(|path| path.id == next.id)
                .expect("bound native path");
            path.expected = next.fingerprint;
            path.publication = next.revision;
            path.mode = next.mode;
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
    }
    /// A processed path stopped advancing, or no path advances at all.
    fn stalled(&self, now: Instant) -> bool {
        let timed_out = |path: &Bound| path.timed_out(now, self.started, self.progress_timeout);
        self.paths
            .iter()
            .any(|path| path.mode == PathMode::Processed && timed_out(path))
            || self.paths.iter().all(timed_out)
    }
    pub(crate) fn poll(&mut self) -> Option<Observation> {
        let now = Instant::now();
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + POLL_INTERVAL;
        match self.observe(now) {
            Ok(observation) => Some(observation),
            Err(Error::NativeBusy) if !self.stalled(now) => None,
            Err(error) => {
                self.writable = false;
                Some(Observation {
                    failure: true,
                    applied: false,
                    advancing: false,
                    enabled: self.enabled,
                    active_ids: Vec::new(),
                    message: error.to_string(),
                    meter: None,
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
        let mut meters = Vec::with_capacity(self.paths.len());
        for path in &mut self.paths {
            let status = locate(&contexts, path)?;
            if status.worker.is_some_and(|value| value.failed) {
                return Err(failure("native suppression worker failed"));
            }
            // Liveness is independent of desired effect publication. A fresh
            // native block must not become a fake stalled-audio report merely
            // because an external controller temporarily changed this slot.
            if let Some(meter) = status.meter {
                if meter.processed_frames < path.frames {
                    return Err(failure("native progress moved backwards"));
                }
                if meter.processed_frames > path.frames {
                    path.frames = meter.processed_frames;
                    path.last_progress = now;
                    path.progressed = true;
                }
            }
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
            applied &= status.published == path.publication
                && (status.applied == path.publication || idle_passthrough)
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
                    performance: Some(engine_performance(&contexts, &path.id, None)),
                });
            }
            if let Some(meter) = meter {
                let worker = status.worker;
                let performance = engine_performance(&contexts, &path.id, worker);
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
        let meter = (advancing && applied && meters.len() == self.paths.len()).then_some(Meter {
            message_type: MeterType::Meter,
            revision: self.revision,
            generation: self.generation,
            synthetic: false,
            // Filled by the controller's diagnostics log, which owns the
            // baseline epoch; the backend reports raw engine figures.
            diagnostics_epoch: 0,
            targets: meters,
        });
        Ok(Observation {
            failure: false,
            applied,
            advancing,
            enabled: self.enabled,
            active_ids: self.selected_ids(),
            message,
            meter,
            native: contexts,
        })
    }
}

#[cfg(test)]
#[path = "native_backend/tests.rs"]
mod tests;
