//! State owned exclusively by the coordinator. Audio never borrows it.
// Engine observations, and the engine age they carry, come only from the
// native engine. A build without it (the store-only coordinator that tests
// and non-Linux hosts compile) has no caller for them.
#![cfg_attr(
    not(all(feature = "native-pipeline", target_os = "linux")),
    allow(dead_code)
)]
#[cfg(all(feature = "native-pipeline", target_os = "linux"))]
use crate::coordinator::Request;
use crate::{
    coordinator::{Error, RuntimeDescriptor},
    diagnostic_history::ProcessStart,
    facade::Facade,
    store::{Store, StoreError},
};
use rc2fx_core::{
    ConfigUpdate, DeviceState, EngineMode, EnginePhase, EngineStatus, Meter, PresetSummary,
    RoutingPhase, RoutingStatus, SelectionPhase, SelectionStatus,
};

pub struct Controller {
    store: Store,
    pub(crate) front: Facade,
    engine: EngineStatus,
    routing: RoutingStatus,
    selection: SelectionStatus,
    /// Diagnostics baselines and the event log: off the audio path, fed from
    /// the control side's readings.
    diagnostics: crate::diagnostics_log::DiagnosticsLog,
    /// The generation reported Running, and the anchor its age is measured
    /// from. Only the first report for a generation counts, so the engine's age
    /// never restarts at a poll, a stall, or a diagnostics reset.
    running_since: Option<(u64, RunningAnchor)>,
    /// The audio process that runs this generation, when its start is known:
    /// the native mixer process. Adopted once on the control thread from the
    /// identity read at attach; no audio-path code reads `/proc`.
    process_start: Option<ProcessStart>,
}
/// Where `running_for_ns` measures a generation from.
#[derive(Clone, Copy, Debug)]
enum RunningAnchor {
    /// The start of the process that runs the engine, on the same
    /// `CLOCK_MONOTONIC` the diagnostics events are stamped with.
    Process(ProcessStart),
    /// Fallback: the first Running report this daemon saw, on `Instant` (also
    /// `CLOCK_MONOTONIC` on Linux). Used when the process start cannot be
    /// read.
    FirstReport(std::time::Instant),
}

/// Why moving the running audio from `running` to `candidate` needs a save
/// rather than a live edit, or None when a live edit can play it. A live edit
/// may only hand a path new parameters; it never rebuilds processing. The rule is
/// the native library's own (`rc2fx_dsp::Prepared::requires_reset`, which
/// covers noise suppression switched on or off, its lookahead, and the effect
/// order around it), plus a changed kind, channel count or target set. The
/// app predicts the same thing before it asks.
#[cfg_attr(
    not(all(feature = "native-pipeline", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) fn live_needs_save(
    running: &[rc2fx_core::Target],
    candidate: &[rc2fx_core::Target],
) -> Option<String> {
    if running.len() != candidate.len() {
        return Some("the target list changed".into());
    }
    for next in candidate {
        let Some(now) = running.iter().find(|target| target.id == next.id) else {
            return Some(format!("{} is not playing", next.id));
        };
        if now == next {
            continue;
        }
        if now.kind != next.kind || now.channels != next.channels {
            return Some(format!("{} changes its kind or channel count", next.id));
        }
        let (Ok(before), Ok(after)) = (
            rc2fx_dsp::Prepared::new(now, 48_000),
            rc2fx_dsp::Prepared::new(next, 48_000),
        ) else {
            return Some(format!("{} cannot be prepared for live editing", next.id));
        };
        if before.requires_reset(&after) {
            return Some(format!(
                "{} changes noise suppression or its effect order",
                next.id
            ));
        }
    }
    None
}

impl Controller {
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn attach_native(
        &mut self,
        runtime: &mut crate::native_backend::Runtime,
    ) -> Result<(), Error> {
        // Validate the saved target binding before reserving a controller epoch.
        // Runtime::attach revalidates after reservation; no audio is mutated.
        runtime.attach(self.config(), self.store.generation().max(1))?;
        let generation = self.store.reserve_generation()?;
        runtime.attach(self.config(), generation)?;
        self.front.native_process(runtime.process.clone());
        self.attach_runtime(
            &RuntimeDescriptor {
                generation,
                mode: EngineMode::Native,
                sample_rate: 48_000,
                block_size: 112,
            },
            runtime.selected_ids(),
        );
        self.selection.qualified_target_ids = runtime.bound_ids();
        self.selection.allowed = false;
        self.engine.message = "Waiting for the native callback owner".into();
        // The mixer process that runs the engine is already up when this
        // daemon connects: anchoring at its start makes startup cover the
        // xruns and stalls before the first observation, not just after it.
        self.note_process_start(runtime.process.start());
        self.saved();
        Ok(())
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn native_progress(&mut self, observation: crate::native_backend::Observation) {
        if let Some(fault) = self.durability_fault().map(str::to_owned) {
            self.unavailable(&format!("Durable save uncertain: {fault}"));
            return;
        }
        let before = (
            self.engine.clone(),
            self.routing.clone(),
            self.selection.clone(),
        );
        self.engine.audio_advancing = observation.advancing;
        self.engine.applied_revision = if observation.applied && observation.advancing {
            self.config().revision
        } else {
            0
        };
        if observation.advancing && !observation.failure {
            self.note_running(self.engine.generation);
        }
        self.engine.status = if observation.failure {
            EnginePhase::Error
        } else if observation.advancing {
            EnginePhase::Running
        } else {
            EnginePhase::Starting
        };
        self.engine.message = observation.message.clone();
        self.routing.status = if observation.failure {
            RoutingPhase::Error
        } else if observation.advancing && observation.applied {
            if observation.enabled {
                RoutingPhase::Active
            } else {
                RoutingPhase::Bypassed
            }
        } else {
            RoutingPhase::Unmanaged
        };
        self.routing.channels = if self.routing.status == RoutingPhase::Active {
            self.config().selected_channels()
        } else {
            0
        };
        self.routing.message = observation.message;
        self.selection.allowed =
            observation.applied && observation.advancing && !observation.failure;
        if observation.failure {
            self.selection.status = SelectionPhase::Error;
        } else if observation.applied && observation.advancing {
            self.selection.status = SelectionPhase::Idle;
            self.selection.active_target_ids = observation.active_ids;
            self.selection.pending_target_ids.clear();
        }
        if self.engine != before.0 || self.routing != before.1 || self.selection != before.2 {
            self.saved();
        }
        // Diagnostics: collect events from this reading, and report the
        // cumulative figures relative to the last reset. The meter is None
        // while the engine settles; the engine's own events are still kept.
        let now_ns = crate::diagnostics_log::monotonic_ns();
        let meter = observation.meter;
        self.diagnostics
            .observe(meter.as_ref(), &observation.native, now_ns);
        if let Some(mut meter) = meter
            && self.prepare_meter(&mut meter)
        {
            // The export keeps the raw figures; the window gets the ones
            // relative to the last diagnostics reset. `shown` is derived
            // before the meter is moved, so no second raw clone is made.
            let shown = self.diagnostics.shown(&meter);
            self.front.native_meter(shown, meter, observation.native);
        }
    }
    /// Whether anyone is subscribed to live events.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn has_clients(&self) -> bool {
        self.front.has_clients()
    }
    /// `result` says whether the save published anything to the audio.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    fn native_saved(&mut self, result: Result<bool, Error>) -> Result<DeviceState, Error> {
        if let Some(fault) = self.durability_fault().map(str::to_owned) {
            self.unavailable(&format!("Durable save uncertain: {fault}"));
        }
        let published = result?;
        if !published && self.settled() {
            // The audio already plays exactly these settings (live edits of
            // them, or a save that changes nothing audible): nothing waits for
            // an acknowledgement, so the engine stays Running, routing stays
            // as it is, and the new revision is the applied one at once.
            self.engine.applied_revision = self.config().revision;
            return Ok(self.saved());
        }
        self.engine.status = EnginePhase::Starting;
        self.engine.audio_advancing = false;
        self.engine.applied_revision = 0;
        self.selection.allowed = false;
        self.engine.message = "Settings saved; waiting for native audio acknowledgement".into();
        Ok(self.saved())
    }
    /// The engine runs, advancing, with everything it was given applied.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    fn settled(&self) -> bool {
        self.engine.status == EnginePhase::Running
            && self.engine.audio_advancing
            && self.engine.applied_revision != 0
            && self.durability_fault().is_none()
    }
    /// `PUT api/v1/live` on the native engine: the settings go on the
    /// running audio at once and stay there; they are what the user has.
    /// Nothing is stored yet, the revision stays, no state is published, and
    /// the engine stays Running. The app saves them once edits stop, or the
    /// daemon does (`keep_live`). Answers with the saved revision the edit was
    /// based on.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn live_native(
        &mut self,
        update: ConfigUpdate,
        runtime: &mut crate::native_backend::Runtime,
    ) -> Result<u64, Error> {
        let saved = self.config();
        // The same checks, in the same order, as a save.
        if update.expected_revision != saved.revision {
            return Err(StoreError::Conflict.into());
        }
        saved
            .validate_update(&update.targets)
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if !self.settled() {
            return Err(Error::LiveUnavailable);
        }
        // Compared with what plays now: the last live edit, or the saved settings.
        let running = runtime.live_targets().unwrap_or(saved.targets.as_slice());
        if let Some(reason) = live_needs_save(running, &update.targets) {
            return Err(Error::LiveRequiresSave(reason));
        }
        let revision = saved.revision;
        let candidate = rc2fx_core::Config {
            targets: update.targets,
            ..saved.clone()
        };
        runtime.live(&candidate, saved)?;
        Ok(revision)
    }
    /// Saves what live edits play, for an app that went away without saving
    /// them or a daemon that stops (`last`): the user keeps what they hear.
    /// Nothing reaches the audio, which already plays it, so the engine stays
    /// Running and routed and the new revision is applied at once; the new
    /// state is published as for any save. When the save itself fails (a
    /// store or durability fault), and on the `last` try whatever failed, the
    /// saved settings go back on the audio instead, so the audio and the
    /// store agree. A library that is not ready otherwise leaves the edits
    /// playing for the next try. Ok(None) when there was nothing to keep.
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn keep_live(
        &mut self,
        runtime: &mut crate::native_backend::Runtime,
        last: bool,
    ) -> Result<Option<DeviceState>, Error> {
        let Some(targets) = runtime.live_targets().map(<[_]>::to_vec) else {
            return Ok(None);
        };
        let update = ConfigUpdate {
            expected_revision: self.config().revision,
            targets,
        };
        let result = self
            .store
            .prepare_apply(update)
            .map_err(Error::Store)
            .and_then(|save| runtime.save(save));
        match result {
            Ok(published) => self.native_saved(Ok(published)).map(Some),
            Err(error) if last || matches!(error, Error::Store(_)) => {
                eprintln!("Could not save the live settings ({error}); restoring the saved ones");
                if let Err(restore) = runtime.restore_live(self.config(), last) {
                    eprintln!("Could not restore the saved settings either: {restore}");
                }
                // A durability fault makes the controller unavailable.
                self.native_saved(Err(error)).map(Some)
            }
            Err(error) => Err(error),
        }
    }
    /// Makes the store's next commit fail at `phase`, for tests.
    #[cfg(all(test, feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn fail_next_commit(&mut self, phase: crate::persistence::CommitPhase) {
        self.store.fail_next_commit(phase);
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn native_transition(
        &mut self,
        request: Request,
        runtime: &mut crate::native_backend::Runtime,
    ) -> Result<DeviceState, Error> {
        let (operation, revision, generation, targets) = match &request {
            Request::Routing(value) => (
                if value.enabled {
                    "enable-processing"
                } else {
                    "disable-processing"
                },
                value.expected_revision,
                value.expected_generation,
                &[][..],
            ),
            Request::Selection(value) => (
                "select-paths",
                value.expected_revision,
                value.expected_generation,
                value.target_ids.as_slice(),
            ),
            Request::Restart(value) => (
                "restart-processing",
                value.expected_revision,
                value.expected_generation,
                &[][..],
            ),
        };
        let result = (|| {
            if revision != self.config().revision
                || generation != self.engine.generation
                || generation != self.store.generation()
            {
                return Err(Error::Store(StoreError::Conflict));
            }
            match &request {
                Request::Routing(value) => {
                    let result = runtime.set_enabled(self.config(), value.enabled);
                    self.native_saved(result.map(|_| true))?;
                    self.routing.status = if value.enabled {
                        RoutingPhase::Starting
                    } else {
                        RoutingPhase::Bypassing
                    };
                    self.routing.channels = 0;
                    self.selection.status = SelectionPhase::Idle;
                    self.selection.pending_target_ids.clear();
                }
                Request::Selection(value) => {
                    let ids = self
                        .config()
                        .canonical_selection(&value.target_ids)
                        .map_err(|error| StoreError::Invalid(error.to_string()))?;
                    if ids
                        .iter()
                        .any(|id| !self.selection.qualified_target_ids.contains(id))
                    {
                        return Err(Error::NativeUnsupported(
                            "This build can enable only already-bound native ports".into(),
                        ));
                    }
                    let result = self
                        .store
                        .prepare_selection(value.expected_revision, &ids)
                        .map_err(Error::Store)
                        .and_then(|save| runtime.save(save))
                        // A selection always changes a path's mode; keep the
                        // acknowledgement wait it has always had.
                        .map(|_| true);
                    self.native_saved(result)?;
                    self.selection.status = SelectionPhase::Changing;
                    self.selection.pending_target_ids = ids;
                    self.routing.status = if runtime.enabled() {
                        RoutingPhase::Starting
                    } else {
                        RoutingPhase::Bypassing
                    };
                    self.routing.channels = 0;
                }
                Request::Restart(_) => {
                    return Err(Error::NativeUnsupported(
                        "Native per-path updates do not restart the vendor audio engine".into(),
                    ));
                }
            }
            Ok(self.saved())
        })();
        if let Err(error) = &result {
            self.record_rejection(operation, Some(revision), targets, error);
        }
        result
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn apply_native(
        &mut self,
        update: ConfigUpdate,
        runtime: &mut crate::native_backend::Runtime,
    ) -> Result<DeviceState, Error> {
        let expected = update.expected_revision;
        let targets: Vec<_> = update
            .targets
            .iter()
            .take(rc2fx_core::MAX_TARGETS)
            .map(|target| target.id.clone())
            .collect();
        let result = self
            .store
            .prepare_apply(update)
            .map_err(Error::Store)
            .and_then(|save| runtime.save(save));
        let result = self.native_saved(result);
        if let Err(error) = &result {
            self.record_rejection("apply-effects", Some(expected), &targets, error);
        }
        result
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn apply_native_preset(
        &mut self,
        id: &str,
        revision: u64,
        runtime: &mut crate::native_backend::Runtime,
    ) -> Result<DeviceState, Error> {
        let result = self
            .store
            .prepare_preset(id, revision)
            .map_err(Error::Store)
            .and_then(|save| runtime.save(save));
        let result = self.native_saved(result);
        if let Err(error) = &result {
            self.record_rejection("apply-preset", Some(revision), &[], error);
        }
        result
    }
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    pub(crate) fn record_rejection(
        &self,
        operation: &str,
        expected: Option<u64>,
        targets: &[String],
        error: &Error,
    ) {
        self.front.rejection(
            self.engine.generation,
            self.config().revision,
            operation,
            expected,
            targets,
            &error.to_string(),
        );
    }
    pub(crate) fn config(&self) -> &rc2fx_core::Config {
        self.store.config()
    }
    pub(crate) fn durability_fault(&self) -> Option<&str> {
        self.store.durability_fault()
    }

    pub(crate) fn unavailable(&mut self, message: &str) {
        self.engine.status = EnginePhase::Error;
        self.engine.restart_allowed = false;
        self.engine.audio_advancing = false;
        self.engine.message = message.chars().take(512).collect();
        self.routing.status = RoutingPhase::Error;
        self.routing.message = self.engine.message.clone();
        self.selection.allowed = false;
        self.saved();
    }

    pub fn new(store: Store) -> Self {
        let generation = store.generation();
        let mut controller = Self {
            store,
            front: Facade::default(),
            engine: EngineStatus {
                generation,
                restart_allowed: false,
                mode: EngineMode::Offline,
                status: EnginePhase::Offline,
                applied_revision: 0,
                sample_rate: 0,
                block_size: 0,
                message: "Audio engine has not been started".into(),
                audio_advancing: false,
            },
            routing: RoutingStatus {
                status: RoutingPhase::Unmanaged,
                channels: 0,
                recovered: 0,
                message: "No audio routes installed".into(),
            },
            selection: SelectionStatus {
                channel_capacity: None,
                status: SelectionPhase::Idle,
                allowed: false,
                qualified_target_ids: vec![],
                active_target_ids: vec![],
                pending_target_ids: vec![],
                message: "Audio engine has not been started".into(),
            },
            diagnostics: crate::diagnostics_log::DiagnosticsLog::default(),
            running_since: None,
            process_start: None,
        };
        controller.saved();
        controller
    }

    /// Records the first Running report for `generation`, from the native
    /// progress poll on the manager thread. A later report for
    /// the same generation keeps the original anchor, so an engine that
    /// stalls and recovers does not appear to restart. When the daemon knows
    /// when the audio process started, that is the anchor and the age covers
    /// everything since; otherwise the earliest trustworthy point is this
    /// first Running report.
    fn note_running(&mut self, generation: u64) {
        if self
            .running_since
            .is_none_or(|(running, _)| running != generation)
        {
            let anchor = self.process_start.map_or_else(
                || RunningAnchor::FirstReport(std::time::Instant::now()),
                RunningAnchor::Process,
            );
            self.running_since = Some((generation, anchor));
        }
    }
    /// Adopts the start of the audio process that runs the engine. Called on
    /// the control thread with the identity captured there; `None` (an
    /// unreadable `/proc/<pid>/stat`) leaves the first-Running fallback. A
    /// restarted process replaces the anchor for the current generation, so
    /// its age starts from its own start, not the old process's.
    fn note_process_start(&mut self, start: Option<ProcessStart>) {
        self.process_start = start;
        if let (Some((generation, RunningAnchor::Process(current))), Some(start)) =
            (self.running_since, start)
            && (current.pid, current.start_ticks) != (start.pid, start.start_ticks)
        {
            self.running_since = Some((generation, RunningAnchor::Process(start)));
        }
    }
    /// How long `generation` has been running on the diagnostics clock, from
    /// the audio process's start when known, or its first Running report
    /// otherwise. `None` until it runs, and for any generation other than the
    /// one recorded. A diagnostics reset never touches it: this is the
    /// engine's age, not the counters'.
    pub(crate) fn running_for_ns(&self, generation: u64) -> Option<u64> {
        let (running, anchor) = self.running_since?;
        if running != generation {
            return None;
        }
        Some(match anchor {
            RunningAnchor::Process(start) => {
                start.running_for_ns(crate::diagnostics_log::monotonic_ns())
            }
            RunningAnchor::FirstReport(at) => {
                at.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
            }
        })
    }
    /// Resets the diagnostics baselines and starts the event log over with a
    /// `reset` entry; sequence numbers continue. No audio-path effect.
    pub fn reset_diagnostics(&mut self) -> DeviceState {
        self.diagnostics
            .reset(crate::diagnostics_log::monotonic_ns());
        self.saved()
    }
    /// One page of the diagnostics event log.
    pub fn diagnostics_events(
        &mut self,
        after: u64,
        limit: usize,
    ) -> rc2fx_core::DiagnosticsEvents {
        self.diagnostics
            .page(after, limit, crate::diagnostics_log::monotonic_ns())
    }
    pub fn state(&self) -> DeviceState {
        let config = self.store.config();
        let mut selection = self.selection.clone();
        // Saved effects may reorder the complete catalog. Wire selection lists
        // follow that order; the Group's per-callback slots keep their own fixed
        // identity order and are never reordered by this presentation step.
        for ids in [
            &mut selection.qualified_target_ids,
            &mut selection.active_target_ids,
            &mut selection.pending_target_ids,
        ] {
            ids.sort_by_key(|id| {
                config
                    .targets
                    .iter()
                    .position(|target| target.id == *id)
                    .unwrap_or(usize::MAX)
            });
        }
        DeviceState {
            config: config.clone(),
            engine: self.engine.clone(),
            routing: self.routing.clone(),
            selection,
        }
    }

    fn saved(&mut self) -> DeviceState {
        let state = self.state();
        self.front.publish(state.clone(), self.store.presets());
        state
    }

    /// Saves effects with no audio engine attached: the store-only
    /// coordinator, which only tests run.
    pub(crate) fn apply(&mut self, update: ConfigUpdate) -> Result<DeviceState, Error> {
        self.store.prepare_apply(update)?.commit()?;
        Ok(self.saved())
    }

    /// Applies a preset with no audio engine attached, as `apply` does.
    pub(crate) fn apply_preset(&mut self, id: &str, revision: u64) -> Result<DeviceState, Error> {
        self.store.prepare_preset(id, revision)?.commit()?;
        Ok(self.saved())
    }

    pub fn presets(&self) -> Vec<PresetSummary> {
        self.store.presets()
    }

    pub fn create_preset(
        &mut self,
        name: &str,
        revision: u64,
    ) -> Result<PresetSummary, StoreError> {
        let preset = self.store.create_preset(name, revision)?;
        self.saved();
        Ok(preset)
    }

    pub(crate) fn attach_runtime(&mut self, descriptor: &RuntimeDescriptor, selected: Vec<String>) {
        self.engine.generation = descriptor.generation;
        self.running_since = None;
        // A new generation gets the process identity its caller supplies, if
        // any; without one it keeps the first-Running fallback.
        self.note_process_start(None);
        self.engine.mode = descriptor.mode;
        self.engine.status = EnginePhase::Starting;
        self.engine.restart_allowed = false;
        self.selection.allowed = false;
        self.engine.audio_advancing = false;
        self.engine.sample_rate = descriptor.sample_rate;
        self.engine.block_size = descriptor.block_size;
        self.engine.message = "Waiting for every allocated audio path".into();
        self.routing = RoutingStatus {
            status: RoutingPhase::Unmanaged,
            channels: 0,
            recovered: 0,
            message: "New generation routing has not been verified".into(),
        };
        self.selection.active_target_ids = selected;
        self.selection.message =
            "Allocated audio paths; hardware routes are not verified here".into();
        self.saved();
    }

    fn prepare_meter(&self, meter: &mut Meter) -> bool {
        // The native engine meters every bound path, active or not.
        let expected_ids = &self.selection.qualified_target_ids;
        if meter.generation != self.engine.generation
            || meter.revision != self.store.config().revision
            || meter.revision != self.engine.applied_revision
            || meter.targets.len() != expected_ids.len()
            || meter.targets.iter().enumerate().any(|(index, target)| {
                target.validate().is_err()
                    || !expected_ids.contains(&target.id)
                    || meter.targets[..index]
                        .iter()
                        .any(|prior| prior.id == target.id)
            })
        {
            return false;
        }
        // Engine age, not counter age: stamped here so the meter reports the
        // controller's anchor (the audio process's start when known, else the
        // first Running report), and a diagnostics reset (which rewrites only
        // the counters) leaves it.
        meter.running_for_ns = self.running_for_ns(meter.generation);
        meter.targets.sort_by_key(|target| {
            self.store
                .config()
                .targets
                .iter()
                .position(|saved| saved.id == target.id)
                .unwrap()
        });
        true
    }

    /// The saved settings, encoded exactly as `state.json`.
    pub fn settings(&self) -> Result<Vec<u8>, StoreError> {
        self.store.encoded_snapshot()
    }

    pub fn delete_preset(&mut self, id: &str) -> Result<(), StoreError> {
        self.store.delete_preset(id)?;
        self.saved();
        Ok(())
    }

    pub fn disconnect_clients(&mut self) {
        self.front.disconnect();
    }
}

/// What a live edit may play: new parameters on the same processing.
#[cfg(test)]
mod live_rule_tests {
    use super::live_needs_save;
    use rc2fx_core::{Config, Target};

    fn edited(edit: impl FnOnce(&mut Target)) -> (Vec<Target>, Vec<Target>) {
        let running = Config::default().targets;
        let mut candidate = running.clone();
        edit(candidate.iter_mut().find(|t| t.id == "mic-1").unwrap());
        (running, candidate)
    }

    #[test]
    fn parameters_go_live_and_structure_needs_a_save() {
        let (running, same) = edited(|_| {});
        assert_eq!(live_needs_save(&running, &same), None);
        let (running, gain) = edited(|t| t.gain.db = -6.0);
        assert_eq!(live_needs_save(&running, &gain), None);
        // Effect order without suppression is only parameters.
        let (running, order) = edited(|t| t.order.reverse());
        assert_eq!(live_needs_save(&running, &order), None);

        let (running, on) = edited(|t| t.suppression.enabled = true);
        assert!(live_needs_save(&running, &on).is_some(), "suppression on");
        // Suppression on is the starting point for the rest of these.
        let (_, suppressed) = edited(|t| t.suppression.enabled = true);
        let mut back = suppressed.clone();
        back.iter_mut()
            .find(|t| t.id == "mic-1")
            .unwrap()
            .suppression
            .enabled = false;
        assert!(
            live_needs_save(&suppressed, &back).is_some(),
            "suppression off"
        );
        // With suppression on: its strength goes live, its lookahead and the
        // order around it do not.
        let mut wet = suppressed.clone();
        wet.iter_mut()
            .find(|t| t.id == "mic-1")
            .unwrap()
            .suppression
            .wet = 0.5;
        assert_eq!(live_needs_save(&suppressed, &wet), None);
        let mut lookahead = suppressed.clone();
        lookahead
            .iter_mut()
            .find(|t| t.id == "mic-1")
            .unwrap()
            .suppression
            .lookahead = 1;
        assert!(
            live_needs_save(&suppressed, &lookahead).is_some(),
            "lookahead"
        );
        let mut reordered = suppressed.clone();
        reordered
            .iter_mut()
            .find(|t| t.id == "mic-1")
            .unwrap()
            .order
            .reverse();
        assert!(live_needs_save(&suppressed, &reordered).is_some(), "order");

        // A different target set or shape is never a live edit.
        let (running, _) = edited(|_| {});
        assert!(live_needs_save(&running, &running[1..]).is_some());
        let (running, channels) = edited(|t| t.channels = 2);
        assert!(live_needs_save(&running, &channels).is_some());
    }
}

#[cfg(test)]
mod running_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn descriptor(generation: u64) -> RuntimeDescriptor {
        RuntimeDescriptor {
            generation,
            mode: EngineMode::Native,
            sample_rate: 48_000,
            block_size: 112,
        }
    }

    /// What an observation of the engine advancing on the saved revision
    /// records (see `native_progress`).
    fn running(control: &mut Controller) {
        control.engine.applied_revision = control.config().revision;
        control.engine.audio_advancing = true;
        control.engine.status = EnginePhase::Running;
        control.note_running(control.engine.generation);
    }

    /// A meter the controller accepts as current: this generation, this
    /// revision, and no allocated paths.
    fn meter(control: &Controller) -> Meter {
        Meter {
            message_type: rc2fx_core::MeterType::Meter,
            revision: control.config().revision,
            generation: control.engine.generation,
            synthetic: false,
            diagnostics_epoch: 0,
            running_for_ns: None,
            targets: Vec::new(),
        }
    }

    #[test]
    fn engine_age_starts_at_running_and_survives_a_diagnostics_reset() {
        // No process start is known here: the first Running report anchors.
        let directory = tempfile::tempdir().unwrap();
        let config = rc2fx_core::Config::default();
        let mut control = Controller::new(Store::open(directory.path(), config).unwrap());
        control.attach_runtime(&descriptor(1), Vec::new());
        assert_eq!(control.running_for_ns(1), None, "not running yet");

        let attach = Instant::now();
        running(&mut control);
        std::thread::sleep(Duration::from_millis(30));
        let mut first = meter(&control);
        assert!(control.prepare_meter(&mut first));
        let age = Duration::from_nanos(first.running_for_ns.expect("the generation runs"));
        assert!(
            age >= Duration::from_millis(25) && age <= attach.elapsed(),
            "about the time since Running: {age:?}"
        );
        // The same generation reported again keeps its original start.
        running(&mut control);
        assert!(control.running_for_ns(1).unwrap() >= age.as_nanos() as u64);

        // A new generation starts over, and the old one is not reported.
        control.attach_runtime(&descriptor(2), Vec::new());
        assert_eq!(control.running_for_ns(1), None);
        running(&mut control);
        let mut second = meter(&control);
        assert!(control.prepare_meter(&mut second));
        let second_age =
            Duration::from_nanos(second.running_for_ns.expect("the new generation runs"));
        assert!(second_age < age, "a new generation's age starts over");

        // A diagnostics reset rebaselines the counters, never the engine age.
        let before_reset = control.running_for_ns(2).unwrap();
        control.reset_diagnostics();
        let after_reset = control.running_for_ns(2).unwrap();
        assert!(
            after_reset >= before_reset,
            "a reset does not restart the engine clock"
        );
        let mut third = meter(&control);
        assert!(control.prepare_meter(&mut third));
        let shown = control.diagnostics.shown(&third);
        assert_eq!(shown.running_for_ns, third.running_for_ns);
        assert_eq!(
            shown.diagnostics_epoch, 1,
            "the reset still bumps the epoch"
        );
    }

    fn process(pid: u32, start_ticks: u64, at_ns: u64) -> ProcessStart {
        ProcessStart {
            pid,
            start_ticks,
            at_ns,
        }
    }

    /// The age includes the time before this daemon first saw the engine:
    /// that is the startup window the app has to hide.
    #[test]
    fn a_known_process_start_anchors_before_the_first_running_report() {
        let directory = tempfile::tempdir().unwrap();
        let mut control =
            Controller::new(Store::open(directory.path(), rc2fx_core::Config::default()).unwrap());
        control.attach_runtime(&descriptor(1), Vec::new());
        let observed = crate::diagnostics_log::monotonic_ns();
        let started = observed.saturating_sub(10_000_000_000);
        control.note_process_start(Some(process(4242, 987, started)));
        running(&mut control);
        let age = control.running_for_ns(1).expect("the generation runs");
        assert!(
            age >= observed.saturating_sub(started),
            "the age covers everything since the audio process started: {age}"
        );
        let mut meter = meter(&control);
        assert!(control.prepare_meter(&mut meter));
        assert!(meter.running_for_ns.expect("stamped") >= age);
        // A diagnostics reset rebaselines the counters, never the anchor.
        control.reset_diagnostics();
        assert!(control.running_for_ns(1).expect("still running") >= age);
    }

    /// Without a readable process start the first Running report anchors, as
    /// it did before the process start was known.
    #[test]
    fn an_unknown_process_start_falls_back_to_the_first_running_report() {
        let directory = tempfile::tempdir().unwrap();
        let mut control =
            Controller::new(Store::open(directory.path(), rc2fx_core::Config::default()).unwrap());
        control.attach_runtime(&descriptor(1), Vec::new());
        control.note_process_start(None);
        let attached = Instant::now();
        running(&mut control);
        std::thread::sleep(Duration::from_millis(10));
        let age = Duration::from_nanos(control.running_for_ns(1).expect("the generation runs"));
        assert!(
            age >= Duration::from_millis(5) && age <= attached.elapsed(),
            "about the time since Running: {age:?}"
        );
    }

    /// A restarted audio process re-anchors the same generation at its own
    /// start instead of inheriting the old process's age.
    #[test]
    fn a_new_process_identity_re_anchors_the_generation() {
        let directory = tempfile::tempdir().unwrap();
        let mut control =
            Controller::new(Store::open(directory.path(), rc2fx_core::Config::default()).unwrap());
        control.attach_runtime(&descriptor(1), Vec::new());
        // A process that started at boot: its age is the whole uptime.
        let before = crate::diagnostics_log::monotonic_ns();
        control.note_process_start(Some(process(11, 100, 0)));
        running(&mut control);
        let old = control.running_for_ns(1).expect("the generation runs");
        assert!(old >= before);

        // The new process started just now: the generation's age restarts
        // from that start, not from the old process's.
        let started = crate::diagnostics_log::monotonic_ns();
        control.note_process_start(Some(process(12, 200, started)));
        let new = control
            .running_for_ns(1)
            .expect("the generation still runs");
        assert!(new < old, "the new process's start replaces the old");
        assert!(new < 1_000_000_000, "the new process has only just started");
    }
}
