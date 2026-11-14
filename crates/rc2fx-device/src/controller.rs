//! State owned exclusively by the scoped coordinator. Audio never borrows it.
use crate::{
    audio::{Control, PrepareError},
    coordinator::{Action, Error, GenerationPhase, GenerationUpdate, Request, RuntimeDescriptor},
    diagnostic_history::{CallbackDiagnostic, ProcessStart},
    events::{EventError, Subscription},
    facade::Facade,
    store::{PreparedSave, Store, StoreError},
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
pub(crate) enum EffectApply {
    Saved(Box<DeviceState>),
    Replacement(rc2fx_core::Config),
}
/// Where `running_for_ns` measures a generation from.
#[derive(Clone, Copy, Debug)]
enum RunningAnchor {
    /// The start of the process that runs the engine, on the same
    /// `CLOCK_MONOTONIC` the diagnostics events are stamped with.
    Process(ProcessStart),
    /// Fallback: the first Running report this daemon saw, on `Instant` (also
    /// `CLOCK_MONOTONIC` on Linux). Used when the process start cannot be
    /// read, and for JACK, whose server pid is not known to this daemon.
    FirstReport(std::time::Instant),
}

#[cfg(all(test, target_os = "linux"))]
mod capacity_tests {
    use super::*;

    #[test]
    fn overfull_selection_does_not_reserve_or_replace_a_running_generation() {
        let directory = tempfile::tempdir().unwrap();
        let config = rc2fx_core::Config {
            selected_targets: vec!["mic-1".into()],
            ..Default::default()
        };
        let mut store = Store::open(directory.path(), config).unwrap();
        store.reserve_generation().unwrap();
        let mut control = Controller::new(store);
        control.engine.mode = EngineMode::Jack;
        control.engine.status = EnginePhase::Running;
        control.engine.audio_advancing = true;
        control.engine.applied_revision = control.config().revision;
        control.engine.sample_rate = 48_000;
        control.engine.block_size = 112;
        control.routing.status = RoutingPhase::Active;
        control.routing.channels = 1;
        control.selection.allowed = true;
        control.selection.channel_capacity = Some(1);
        control.selection.qualified_target_ids = vec!["mic-1".into(), "usb-main".into()];
        control.selection.active_target_ids = vec!["mic-1".into()];
        let before = control.state();
        before.validate().unwrap();
        let path = directory.path().join("state.json");
        let bytes = std::fs::read(&path).unwrap();
        let result = control.reserve_transition(Request::Selection(rc2fx_core::SelectionRequest {
            expected_revision: before.config.revision,
            expected_generation: before.engine.generation,
            target_ids: vec!["mic-1".into(), "usb-main".into()],
        }));
        assert!(matches!(result, Err(Error::Store(StoreError::Invalid(_)))));
        assert_eq!(control.state(), before);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        assert_eq!(control.store.generation(), before.engine.generation);
    }
}

/// Parameter-only saves retain the exact reserve/save/publish ordering. A
/// structural candidate leaves both Store and live audio untouched here.
fn save_effects(
    save: PreparedSave<'_>,
    control: Option<&mut Control<'_>>,
) -> Result<Option<rc2fx_core::Config>, Error> {
    if let Some(control) = control {
        match control.reserve(
            &save.candidate_config().targets,
            save.candidate_config().revision,
        ) {
            Ok(reservation) => {
                save.commit()?;
                reservation.commit();
            }
            Err(PrepareError::RequiresRestart) => return Ok(Some(save.candidate_config().clone())),
            Err(error) => return Err(Error::Audio(error)),
        }
    } else {
        save.commit()?;
    }
    Ok(None)
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
                progress_timeout: std::time::Duration::from_secs(2),
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
    #[cfg(all(feature = "native-pipeline", target_os = "linux"))]
    fn native_saved(&mut self, result: Result<(), Error>) -> Result<DeviceState, Error> {
        if let Some(fault) = self.durability_fault().map(str::to_owned) {
            self.unavailable(&format!("Durable save uncertain: {fault}"));
        }
        result?;
        self.engine.status = EnginePhase::Starting;
        self.engine.audio_advancing = false;
        self.engine.applied_revision = 0;
        self.selection.allowed = false;
        self.engine.message = "Settings saved; waiting for native audio acknowledgement".into();
        Ok(self.saved())
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
                    self.native_saved(result)?;
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
                        .and_then(|save| runtime.save(save));
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

    pub(crate) fn reserve_transition(
        &mut self,
        request: Request,
    ) -> Result<(Action, DeviceState), Error> {
        let (revision, generation, action) = match request {
            Request::Routing(value) => (
                value.expected_revision,
                value.expected_generation,
                if value.enabled {
                    Action::Enable
                } else {
                    Action::Disable
                },
            ),
            Request::Restart(value) => (
                value.expected_revision,
                value.expected_generation,
                Action::Restart,
            ),
            Request::Selection(value) => (
                value.expected_revision,
                value.expected_generation,
                Action::Select(value.target_ids),
            ),
        };
        if revision != self.config().revision
            || generation != self.engine.generation
            || generation != self.store.generation()
        {
            return Err(StoreError::Conflict.into());
        }
        let state = self.state();
        let action = match action {
            Action::Enable if state.can_enable_processing() => Action::Enable,
            Action::Disable if state.can_disable_processing() => Action::Disable,
            Action::Restart if self.engine.restart_eligible() => Action::Restart,
            Action::Select(ids) if self.selection.allowed => {
                let ids = self
                    .config()
                    .canonical_selection(&ids)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?;
                if ids
                    .iter()
                    .any(|id| !self.selection.qualified_target_ids.contains(id))
                {
                    return Err(StoreError::Invalid("unqualified processing path".into()).into());
                }
                let previous = self
                    .config()
                    .canonical_selection(&self.config().selected_targets)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?;
                if ids == previous && self.selection.status != SelectionPhase::Error {
                    return Err(Error::RuntimeFault);
                }
                // Validate revision capacity before reserving a generation.
                self.selection
                    .check_capacity(self.config(), &ids)
                    .map_err(|error| StoreError::Invalid(error.to_string()))?;
                drop(self.store.prepare_selection(revision, &ids)?);
                Action::Select(ids)
            }
            _ => return Err(Error::RuntimeFault),
        };
        let selection = if let Action::Select(ids) = &action {
            Some(ids.as_slice())
        } else {
            None
        };
        let state = self.reserve_lifecycle(
            selection,
            matches!(action, Action::Disable),
            "Explicit lifecycle request accepted; previous owners must retire",
        )?;
        Ok((action, state))
    }

    fn reserve_lifecycle(
        &mut self,
        selection: Option<&[String]>,
        disabling: bool,
        message: &str,
    ) -> Result<DeviceState, Error> {
        let next = self.store.reserve_generation()?;
        self.engine.generation = next;
        self.running_since = None;
        self.engine.status = EnginePhase::Restarting;
        self.engine.restart_allowed = false;
        self.engine.audio_advancing = false;
        self.engine.applied_revision = 0;
        self.engine.message = message.into();
        self.routing.status = if disabling {
            RoutingPhase::Bypassing
        } else {
            RoutingPhase::Starting
        };
        self.routing.message = "Lifecycle transition pending native verification".into();
        self.selection.allowed = false;
        self.selection.pending_target_ids.clear();
        self.selection.status = SelectionPhase::Idle;
        if let Some(ids) = selection {
            self.selection.status = SelectionPhase::Changing;
            self.selection.pending_target_ids = ids.to_vec();
        }
        Ok(self.saved())
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

    /// The diagnostics epoch: bumped by each reset, carried by every meter.
    pub fn diagnostics_epoch(&self) -> u64 {
        self.diagnostics.epoch()
    }
    /// Records the first Running report for `generation`, from the native or
    /// the JACK progress poll (both on the manager thread). A later report for
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

    pub fn subscribe(&mut self) -> Result<Subscription, EventError> {
        self.front.subscribe()
    }

    fn saved(&mut self) -> DeviceState {
        let state = self.state();
        self.front.publish(state.clone(), self.store.presets());
        state
    }

    pub(crate) fn apply(
        &mut self,
        update: ConfigUpdate,
        control: Option<&mut Control<'_>>,
    ) -> Result<EffectApply, Error> {
        let save = self.store.prepare_apply(update)?;
        if let Some(candidate) = save_effects(save, control)? {
            Ok(EffectApply::Replacement(candidate))
        } else {
            Ok(EffectApply::Saved(Box::new(self.saved())))
        }
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

    pub(crate) fn apply_preset(
        &mut self,
        id: &str,
        revision: u64,
        control: Option<&mut Control<'_>>,
    ) -> Result<EffectApply, Error> {
        let save = self.store.prepare_preset(id, revision)?;
        if let Some(candidate) = save_effects(save, control)? {
            Ok(EffectApply::Replacement(candidate))
        } else {
            Ok(EffectApply::Saved(Box::new(self.saved())))
        }
    }

    pub(crate) fn attach_runtime(&mut self, descriptor: &RuntimeDescriptor, selected: Vec<String>) {
        self.engine.generation = descriptor.generation;
        self.running_since = None;
        // A new generation gets the process identity its caller supplies, if
        // any; JACK has none, and keeps the first-Running fallback.
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

    pub(crate) fn audio_progress(&mut self, revision: u64, advancing: bool, failure: Option<&str>) {
        let prior = self.engine.clone();
        if revision != 0 {
            self.engine.applied_revision = revision;
        }
        self.engine.audio_advancing = advancing;
        if let Some(failure) = failure {
            self.engine.status = EnginePhase::Error;
            self.engine.message = format!("{failure}; explicit recovery required");
        } else if advancing && revision != 0 {
            self.engine.status = EnginePhase::Running;
            self.engine.message = "Allocated audio callbacks are advancing".into();
            self.note_running(self.engine.generation);
        }
        if self.engine != prior {
            self.saved();
        }
    }

    pub(crate) fn generation_update(
        &mut self,
        update: GenerationUpdate,
    ) -> Result<DeviceState, Error> {
        if update.generation != self.engine.generation || self.engine.mode != EngineMode::Jack {
            return Err(Error::InvalidRuntime);
        }
        let channels = self.store.config().selected_channels();
        match update.phase {
            GenerationPhase::Starting => {
                if self.routing.status != RoutingPhase::Unmanaged {
                    return Err(Error::InvalidRuntime);
                }
                self.routing = RoutingStatus {
                    status: RoutingPhase::Starting,
                    channels,
                    recovered: 0,
                    message: "Waiting for watcher readiness and verified route insertion".into(),
                };
            }
            GenerationPhase::Active => {
                if self.routing.status != RoutingPhase::Starting
                    || self.engine.status == EnginePhase::Error
                    || !self.engine.audio_advancing
                {
                    return Err(Error::InvalidRuntime);
                }
                self.routing = RoutingStatus {
                    status: RoutingPhase::Active,
                    channels,
                    recovered: 0,
                    message: "Native generation owner verified every selected route".into(),
                };
                self.selection.status = SelectionPhase::Idle;
                self.selection.pending_target_ids.clear();
                self.selection.allowed = self.store.durability_fault().is_none();
                self.engine.restart_allowed = false;
            }
            GenerationPhase::Recovering(message) => {
                if message.len() > 1024 {
                    return Err(Error::InvalidRuntime);
                }
                self.routing.status = RoutingPhase::Bypassing;
                self.routing.message = message;
            }
            GenerationPhase::Restored => {
                if self.routing.status != RoutingPhase::Bypassing {
                    return Err(Error::InvalidRuntime);
                }
                self.routing = RoutingStatus { status: RoutingPhase::Bypassed, channels, recovered: channels,
                    message: "Watcher joined and complete stock baseline verified; audio clients still allocated".into() };
            }
            GenerationPhase::Stopped => {
                if self.routing.status != RoutingPhase::Bypassed {
                    return Err(Error::InvalidRuntime);
                }
                self.engine.status = EnginePhase::Offline;
                self.engine.audio_advancing = false;
                self.engine.message =
                    "Native clients closed, workers joined and stock-only graph verified".into();
                self.selection.active_target_ids.clear();
                self.selection.message = "No audio paths allocated".into();
                self.routing.channels = 0;
                self.routing.recovered = 0;
                self.routing.message = "Stock-only graph verified after audio shutdown".into();
            }
            GenerationPhase::Fault(message) => {
                if message.is_empty() || message.len() > 1024 {
                    return Err(Error::InvalidRuntime);
                }
                self.engine.status = EnginePhase::Error;
                self.engine.audio_advancing = false;
                self.engine.message = message.clone();
                self.routing.status = RoutingPhase::Error;
                self.routing.message = message;
            }
        }
        Ok(self.saved())
    }

    pub(crate) fn publish_meter(&mut self, mut meter: Meter, callback: &[CallbackDiagnostic]) {
        if self.prepare_meter(&mut meter) {
            self.front.meter(&meter, callback);
        }
    }
    fn prepare_meter(&self, meter: &mut Meter) -> bool {
        let expected_ids = if self.engine.mode == EngineMode::Native {
            &self.selection.qualified_target_ids
        } else {
            &self.selection.active_target_ids
        };
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
        // Engine age, not counter age: stamped here so the native and the JACK
        // paths both report the controller's anchor (the audio process's start
        // when known, else the first Running report), and a diagnostics reset
        // (which rewrites only the counters) leaves it.
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

#[cfg(test)]
mod running_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn descriptor(generation: u64) -> RuntimeDescriptor {
        RuntimeDescriptor {
            generation,
            mode: EngineMode::Jack,
            sample_rate: 48_000,
            block_size: 112,
            progress_timeout: Duration::from_secs(2),
        }
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
        // JACK (no process start known here) keeps the first-Running anchor.
        let directory = tempfile::tempdir().unwrap();
        let config = rc2fx_core::Config::default();
        let mut control = Controller::new(Store::open(directory.path(), config).unwrap());
        control.attach_runtime(&descriptor(1), Vec::new());
        assert_eq!(control.running_for_ns(1), None, "not running yet");

        let attach = Instant::now();
        control.audio_progress(1, true, None);
        std::thread::sleep(Duration::from_millis(30));
        let mut first = meter(&control);
        assert!(control.prepare_meter(&mut first));
        let age = Duration::from_nanos(first.running_for_ns.expect("the generation runs"));
        assert!(
            age >= Duration::from_millis(25) && age <= attach.elapsed(),
            "about the time since Running: {age:?}"
        );
        // The same generation reported again keeps its original start.
        control.audio_progress(1, true, None);
        assert!(control.running_for_ns(1).unwrap() >= age.as_nanos() as u64);

        // A new generation starts over, and the old one is not reported.
        control.attach_runtime(&descriptor(2), Vec::new());
        assert_eq!(control.running_for_ns(1), None);
        control.audio_progress(1, true, None);
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
        control.audio_progress(1, true, None);
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
        control.audio_progress(1, true, None);
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
        control.audio_progress(1, true, None);
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
