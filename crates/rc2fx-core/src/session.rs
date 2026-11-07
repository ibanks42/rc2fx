//! Desired settings, local edits and observed applied state are kept separate.
use crate::{
    Config, CoreError, PendingSelection, Result, SelectionPhase, SelectionRequest, SelectionStatus,
    Target,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineMode {
    Simulation,
    Jack,
    Native,
    Offline,
}
impl EngineMode {
    pub fn has_processing_controls(self) -> bool {
        matches!(self, Self::Jack | Self::Native)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnginePhase {
    Starting,
    Restarting,
    Running,
    Offline,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineStatus {
    pub generation: u64,
    pub restart_allowed: bool,
    pub mode: EngineMode,
    pub status: EnginePhase,
    pub applied_revision: u64,
    pub sample_rate: u32,
    pub block_size: u32,
    pub message: String,
    pub audio_advancing: bool,
}
impl EngineStatus {
    pub fn validate(&self) -> Result<()> {
        if !crate::valid_display_text(&self.message, crate::MAX_STATUS_TEXT) {
            return Err(CoreError("Invalid engine status message".into()));
        }
        Ok(())
    }
    pub fn restart_eligible(&self) -> bool {
        self.restart_allowed && self.mode == EngineMode::Jack && self.status == EnginePhase::Error
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineRestartRequest {
    pub expected_revision: u64,
    pub expected_generation: u64,
}

/// Global processing enable/disable. Generation checks distinguish this from
/// per-channel effect bypass and reject requests from a retired audio owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingRequest {
    pub enabled: bool,
    pub expected_revision: u64,
    pub expected_generation: u64,
}

#[derive(Clone, Debug)]
pub struct PendingRouting {
    pub request: RoutingRequest,
    config: Config,
    runtime: (EngineStatus, RoutingStatus, SelectionStatus),
}
impl PendingRouting {
    pub fn validate_acknowledgement(&self, state: &DeviceState) -> Result<()> {
        state.validate()?;
        let phase = if self.request.enabled {
            RoutingPhase::Starting
        } else {
            RoutingPhase::Bypassing
        };
        let native = self.runtime.0.mode == EngineMode::Native;
        let expected_generation = if native {
            Some(self.request.expected_generation)
        } else {
            self.request.expected_generation.checked_add(1)
        };
        if state.config != self.config
            || expected_generation != Some(state.engine.generation)
            || state.engine.mode != self.runtime.0.mode
            || state.engine.status
                != if native {
                    EnginePhase::Starting
                } else {
                    EnginePhase::Restarting
                }
            || state.engine.restart_allowed
            || state.engine.audio_advancing
            || state.engine.applied_revision != 0
            || state.routing.status != phase
            || state.selection.status != SelectionPhase::Idle
            || !state.selection.pending_target_ids.is_empty()
        {
            return Err(CoreError(
                "Processing response does not match the requested transition and saved settings"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct PendingRestart {
    pub request: EngineRestartRequest,
    config: Config,
}
impl PendingRestart {
    pub fn validate_acknowledgement(&self, state: &DeviceState) -> Result<()> {
        state.validate()?;
        if state.config != self.config
            || self.request.expected_generation.checked_add(1) != Some(state.engine.generation)
            || state.engine.mode != EngineMode::Jack
            || state.engine.status != EnginePhase::Restarting
            || state.engine.restart_allowed
            || state.engine.audio_advancing
        {
            return Err(CoreError(
                "Restart response does not match the requested engine and saved settings".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceState {
    pub config: Config,
    pub engine: EngineStatus,
    pub routing: RoutingStatus,
    pub selection: SelectionStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingPhase {
    Unmanaged,
    Starting,
    Active,
    Bypassing,
    Bypassed,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingStatus {
    pub status: RoutingPhase,
    pub channels: u32,
    pub recovered: u32,
    pub message: String,
}
impl RoutingStatus {
    pub fn validate(&self) -> Result<()> {
        if !crate::valid_display_text(&self.message, crate::MAX_STATUS_TEXT) {
            return Err(CoreError("Invalid routing status message".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigUpdate {
    pub expected_revision: u64,
    pub targets: Vec<Target>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetSummary {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetMeter {
    pub id: String,
    pub input_peak: f64,
    pub output_peak: f64,
    pub rms: f64,
    pub gain_reduction: f64,
    /// Highest suppression-model voice score in the window, 0 to 1 (0 without
    /// suppression).
    pub voice: f64,
    /// How far open the voice gate was at its most open in the window, 0
    /// (fully closed) to 1. 1 while the gate is off; 0 without suppression.
    pub gate_open: f64,
    pub clipped: u64,
    pub spectrum: Vec<f64>,
    pub callbacks: u64,
    pub processed_frames: u64,
    pub suppression_frames: u64,
    pub missed_deadlines: u64,
    pub stale_results: u64,
    pub input_drops: u64,
    pub output_drops: u64,
    pub latency_samples: u32,
    pub worker_failed: bool,
    pub performance: Option<Performance>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Timing {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub overruns: u64,
    pub clock_errors: u64,
    pub histogram: [u64; 8],
}
impl Timing {
    pub fn mean_ms(&self) -> f64 {
        if self.calls == 0 {
            0.
        } else {
            self.total_ns as f64 / self.calls as f64 / 1_000_000.
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Performance {
    pub callback_budget_ns: u64,
    pub worker_budget_ns: u64,
    pub xruns: u64,
    /// `calls == 0` means callback timing was not sampled in this build, not
    /// that callbacks take no time.
    pub callback: Timing,
    /// The suppression worker's wall and CPU time; zeros when the path has no
    /// worker. Cumulative for the engine generation, across worker rebuilds.
    pub worker_wall: Timing,
    pub worker_cpu: Timing,
    /// Helper routines that finished after their frame's cycle had ended, and
    /// callbacks that found the helper still busy. Absent from a mixer that
    /// predates them (and from the JACK path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_late: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_stalls: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    #[serde(rename = "type")]
    pub message_type: MeterType,
    pub revision: u64,
    pub generation: u64,
    pub synthetic: bool,
    /// Bumped by each diagnostics reset. The counters below are relative to
    /// that reset, so a reader rebaselines its fault tracking when this
    /// changes. 0 means "never reset" (and is what an older mixer sends).
    #[serde(default, rename = "diagnosticsEpoch")]
    pub diagnostics_epoch: u64,
    pub targets: Vec<TargetMeter>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeterType {
    Meter,
}

impl DeviceState {
    pub fn processing_transition_pending(&self) -> bool {
        self.engine.status == EnginePhase::Restarting
            || (self.engine.mode == EngineMode::Native
                && self.engine.status == EnginePhase::Starting)
    }
    fn native_controls_ready(&self) -> bool {
        self.engine.mode == EngineMode::Native
            && self.engine.status == EnginePhase::Running
            && self.engine.audio_advancing
            && self.engine.applied_revision == self.config.revision
            && !self.engine.restart_allowed
            && self.selection.status == SelectionPhase::Idle
            && self.selection.allowed
            && self.selection.pending_target_ids.is_empty()
            && self.routing.recovered == 0
    }
    /// Eligibility is a device claim following complete stock-only recovery.
    /// An offline engine produces no meters; stale meters cannot prove readiness.
    pub fn can_enable_processing(&self) -> bool {
        if self.engine.mode == EngineMode::Native {
            return self.native_controls_ready()
                && self.routing.status == RoutingPhase::Bypassed
                && self.routing.channels == 0
                && self.selection.active_target_ids.is_empty();
        }
        self.engine.mode == EngineMode::Jack
            && self.engine.status == EnginePhase::Offline
            && self.engine.restart_allowed
            && self.engine.generation < 9_007_199_254_740_991
            && !self.engine.audio_advancing
            && self.routing.status == RoutingPhase::Bypassed
            && self.routing.channels == 0
            && self.routing.recovered == 0
            && !self.config.selected_targets.is_empty()
            && self.selection.status == SelectionPhase::Idle
            && self.selection.active_target_ids.is_empty()
            && self.selection.pending_target_ids.is_empty()
    }
    pub fn can_disable_processing(&self) -> bool {
        if self.engine.mode == EngineMode::Native {
            return (self.native_controls_ready() && self.routing.status == RoutingPhase::Active)
                || (self.engine.status == EnginePhase::Error
                    && self.routing.status == RoutingPhase::Error
                    && self.selection.status != SelectionPhase::Changing
                    && self.selection.pending_target_ids.is_empty());
        }
        self.engine.mode == EngineMode::Jack
            && self.engine.generation < 9_007_199_254_740_991
            && self.engine.status == EnginePhase::Running
            && self.routing.status == RoutingPhase::Active
            && self.selection.status == SelectionPhase::Idle
            && self.selection.pending_target_ids.is_empty()
    }
    /// Strict current-schema parsing.
    pub fn from_json(text: &str) -> Result<Self> {
        let value = crate::strict_json::parse(text, 512 * 1024)?;
        let state: Self = serde_json::from_value(value)?;
        state.validate()?;
        Ok(state)
    }
    pub fn validate(&self) -> Result<()> {
        self.config.validate()?;
        self.engine.validate()?;
        self.routing.validate()?;
        self.selection.validate(&self.config)
    }
    pub fn stock_selected(&self) -> bool {
        if self.engine.mode == EngineMode::Native {
            return self.native_controls_ready()
                && self.config.selected_targets.is_empty()
                && self.selection.active_target_ids.is_empty()
                && matches!(
                    self.routing.status,
                    RoutingPhase::Active | RoutingPhase::Bypassed
                )
                && self.routing.channels == 0;
        }
        self.selection.status == SelectionPhase::Idle
            && self.config.selected_targets.is_empty()
            && self.selection.active_target_ids.is_empty()
            && self.engine.status == EnginePhase::Offline
            && !self.engine.audio_advancing
            && self.routing.status == RoutingPhase::Bypassed
            && self.routing.channels == 0
            && self.routing.recovered == 0
    }
    pub fn is_applied(&self) -> bool {
        self.selection.status == SelectionPhase::Idle
            && self
                .config
                .canonical_selection(&self.config.selected_targets)
                .is_ok_and(|ids| self.selection.active_target_ids == ids)
            && !self.config.selected_targets.is_empty()
            && self.engine.status == EnginePhase::Running
            && self.engine.audio_advancing
            && self.engine.applied_revision == self.config.revision
            && (self.engine.mode == EngineMode::Simulation
                || (self.routing.status == RoutingPhase::Active
                    && self.routing.channels == self.config.selected_channels()))
    }
}
impl TargetMeter {
    pub fn validate(&self) -> Result<()> {
        if !crate::valid_id(&self.id)
            || [
                self.input_peak,
                self.output_peak,
                self.rms,
                self.gain_reduction,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
            || [self.voice, self.gate_open]
                .iter()
                .any(|v| !(0.0..=1.0).contains(v))
            || self.spectrum.len() > 64
            || self.spectrum.iter().any(|v| !v.is_finite())
        {
            return Err(CoreError("Invalid meter values".into()));
        }
        Ok(())
    }
}
/// A request snapshot prevents a late save response from deleting newer edits.
#[derive(Clone, Debug)]
pub struct PendingApply {
    pub request: ConfigUpdate,
    selected_targets: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteUpdate {
    Loaded,
    Unchanged,
    Conflict,
    Stale,
}
#[derive(Clone, Debug)]
pub struct Session {
    pub saved: Option<DeviceState>,
    pub draft: Vec<Target>,
    pub selection_draft: Vec<String>,
    pub conflict: bool,
    pub connected: bool,
    base_revision: Option<u64>,
    observed_engine_generation: Option<u64>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            saved: None,
            draft: Config::default().targets,
            selection_draft: Config::default().selected_targets,
            conflict: false,
            connected: false,
            base_revision: None,
            observed_engine_generation: None,
        }
    }
}
impl Session {
    pub fn selection_dirty(&self) -> bool {
        self.saved.as_ref().map_or_else(
            || !self.selection_draft.is_empty(),
            |s| s.config.selected_targets != self.selection_draft,
        )
    }
    pub fn any_dirty(&self) -> bool {
        self.dirty() || self.selection_dirty()
    }
    pub fn dirty(&self) -> bool {
        self.saved.as_ref().map_or_else(
            || self.draft != Config::default().targets,
            |s| s.config.targets != self.draft,
        )
    }
    pub fn is_applied(&self) -> bool {
        self.connected
            && !self.any_dirty()
            && self.saved.as_ref().is_some_and(DeviceState::is_applied)
    }
    pub fn disconnect(&mut self) {
        self.connected = false;
        self.reset_engine_observation();
    }
    /// A deliberate new connection may address a different device sequence.
    fn reset_engine_observation(&mut self) {
        self.observed_engine_generation = None;
    }
    pub fn receive_engine(&mut self, engine: EngineStatus) -> bool {
        if self
            .observed_engine_generation
            .is_some_and(|g| engine.generation < g)
        {
            return false;
        }
        let Some(saved) = self.saved.as_mut() else {
            return false;
        };
        self.observed_engine_generation = Some(engine.generation);
        saved.engine = engine;
        true
    }
    /// Observe the live portion of our pending settings write's stream echo.
    /// The caller identifies the exact request/revision; the request's reply will
    /// acknowledge its settings. Keep all saved settings and local drafts here.
    pub fn receive_runtime(&mut self, state: DeviceState) -> Result<RemoteUpdate> {
        state.validate()?;
        let saved = self
            .saved
            .as_mut()
            .ok_or_else(|| CoreError("Read device settings before observing runtime".into()))?;
        if state.config.revision < saved.config.revision
            || self
                .observed_engine_generation
                .is_some_and(|generation| state.engine.generation < generation)
        {
            return Ok(RemoteUpdate::Stale);
        }
        saved.config.validate_update(&state.config.targets)?;
        if state.config.selected_targets != saved.config.selected_targets {
            return Err(CoreError(
                "A settings echo cannot change processing-path selection".into(),
            ));
        }
        self.observed_engine_generation = Some(state.engine.generation);
        saved.engine = state.engine;
        saved.routing = state.routing;
        saved.selection = state.selection;
        Ok(RemoteUpdate::Unchanged)
    }
    pub fn receive(&mut self, state: DeviceState) -> Result<RemoteUpdate> {
        state.validate()?;
        if self
            .observed_engine_generation
            .is_some_and(|g| state.engine.generation < g)
        {
            return Ok(RemoteUpdate::Stale);
        }
        if let Some(saved) = &self.saved
            && state.config.revision < saved.config.revision
        {
            return Ok(RemoteUpdate::Stale);
        }
        let dirty = self.dirty();
        let selection_dirty = self.selection_dirty();
        let topology_only = self.saved.as_ref().is_some_and(|s| {
            s.config.targets == state.config.targets
                && (s.config.selected_targets != state.config.selected_targets
                    || (s.selection.status == SelectionPhase::Changing
                        && state.selection.status == SelectionPhase::Idle
                        && state.engine.generation == s.engine.generation
                        && s.config.revision.checked_add(1) == Some(state.config.revision)
                        && state
                            .config
                            .canonical_selection(&state.config.selected_targets)
                            .is_ok_and(|ids| ids == s.selection.pending_target_ids)))
        });
        self.observed_engine_generation = Some(state.engine.generation);
        let revision = state.config.revision;
        let changed = self
            .saved
            .as_ref()
            .is_none_or(|s| s.config.revision != revision);
        if !selection_dirty {
            self.selection_draft = state.config.selected_targets.clone();
        }
        if dirty && changed && !topology_only {
            self.conflict = true;
            self.saved = Some(state);
            self.connected = true;
            return Ok(RemoteUpdate::Conflict);
        }
        if !dirty {
            self.draft = state.config.targets.clone();
            self.base_revision = Some(revision);
            self.conflict = false;
        } else if changed && topology_only {
            // Selection commits preserve all effects. Rebase local effect
            // edits without treating that topology-only revision as a conflict.
            self.base_revision = Some(revision);
        }
        self.saved = Some(state);
        self.connected = true;
        Ok(if changed {
            RemoteUpdate::Loaded
        } else {
            RemoteUpdate::Unchanged
        })
    }
    /// A request's snapshot can predate a later stream event at the same engine
    /// generation. Call only while the stream's runtime observation is fresh.
    pub fn receive_snapshot(&mut self, mut state: DeviceState) -> Result<RemoteUpdate> {
        if let Some(saved) = &self.saved
            && self.observed_engine_generation == Some(state.engine.generation)
        {
            state.engine = saved.engine.clone();
            state.routing = saved.routing.clone();
            // Do not let an older same-generation reply regress a
            // completed selection after its streamed state has committed.
            if state.config.revision == saved.config.revision {
                state.selection = saved.selection.clone();
            }
        }
        self.receive(state)
    }
    pub fn reload_saved(&mut self) {
        if let Some(saved) = &self.saved {
            self.draft = saved.config.targets.clone();
            self.selection_draft = saved.config.selected_targets.clone();
            self.base_revision = Some(saved.config.revision);
            self.conflict = false;
        }
    }
    pub fn begin_apply(&self) -> Result<PendingApply> {
        if !self.connected {
            return Err(CoreError(
                "Connect to the mixer before applying settings".into(),
            ));
        }
        if self.conflict {
            return Err(CoreError(
                "Settings changed on another controller; reload or explicitly rebase your edits"
                    .into(),
            ));
        }
        let saved = self
            .saved
            .as_ref()
            .ok_or_else(|| CoreError("Read device settings before applying".into()))?;
        if saved.processing_transition_pending()
            || saved.selection.status == SelectionPhase::Changing
        {
            return Err(CoreError(
                "Wait for the processing change before applying settings".into(),
            ));
        }
        saved.config.validate_update(&self.draft)?;
        Ok(PendingApply {
            selected_targets: saved.config.selected_targets.clone(),
            request: ConfigUpdate {
                expected_revision: self.base_revision.unwrap_or(saved.config.revision),
                targets: self.draft.clone(),
            },
        })
    }
    pub fn begin_selection(&self) -> Result<PendingSelection> {
        let saved = self
            .saved
            .as_ref()
            .filter(|_| self.connected)
            .ok_or_else(|| {
                CoreError("Connect to the mixer before changing processing paths".into())
            })?;
        if !saved.selection.allowed
            || saved.selection.status == SelectionPhase::Changing
            || saved.processing_transition_pending()
            || saved.engine.generation == u64::MAX
            || saved.config.revision == u64::MAX
        {
            return Err(CoreError(
                "The mixer is not ready to change processing paths".into(),
            ));
        }
        let target_ids = saved.config.canonical_selection(&self.selection_draft)?;
        saved.selection.check_capacity(&saved.config, &target_ids)?;
        if (target_ids
            == saved
                .config
                .canonical_selection(&saved.config.selected_targets)?
            && saved.selection.status != SelectionPhase::Error)
            || target_ids
                .iter()
                .any(|id| !saved.selection.qualified_target_ids.contains(id))
        {
            return Err(CoreError(
                "Choose a changed selection from the mixer's qualified paths".into(),
            ));
        }
        Ok(PendingSelection {
            request: SelectionRequest {
                expected_revision: saved.config.revision,
                expected_generation: saved.engine.generation,
                target_ids,
            },
            config: saved.config.clone(),
            mode: saved.engine.mode,
        })
    }
    pub fn acknowledge_selection(
        &mut self,
        pending: &PendingSelection,
        state: DeviceState,
    ) -> Result<()> {
        pending.validate_acknowledgement(&state)?;
        let native_revision = state.engine.mode == EngineMode::Native
            && self.observed_engine_generation == Some(state.engine.generation)
            && self
                .saved
                .as_ref()
                .is_some_and(|saved| saved.config.revision < state.config.revision);
        if native_revision
            || self
                .observed_engine_generation
                .is_none_or(|g| g < state.engine.generation)
        {
            self.receive(state)?;
        }
        Ok(())
    }
    pub fn begin_routing(&self, enabled: bool) -> Result<PendingRouting> {
        let saved = self
            .saved
            .as_ref()
            .filter(|_| self.connected)
            .ok_or_else(|| CoreError("Connect to the mixer before changing processing".into()))?;
        if !(if enabled {
            saved.can_enable_processing()
        } else {
            saved.can_disable_processing()
        }) {
            return Err(CoreError(
                "The mixer is not ready for this processing transition".into(),
            ));
        }
        if enabled && (self.dirty() || self.conflict) {
            return Err(CoreError(
                "Apply or reload effect changes before enabling processing".into(),
            ));
        }
        Ok(PendingRouting {
            request: RoutingRequest {
                enabled,
                expected_revision: saved.config.revision,
                expected_generation: saved.engine.generation,
            },
            config: saved.config.clone(),
            runtime: (
                saved.engine.clone(),
                saved.routing.clone(),
                saved.selection.clone(),
            ),
        })
    }
    pub fn acknowledge_routing(
        &mut self,
        pending: &PendingRouting,
        state: DeviceState,
    ) -> Result<()> {
        pending.validate_acknowledgement(&state)?;
        let native_transition = state.engine.mode == EngineMode::Native
            && self.observed_engine_generation == Some(state.engine.generation)
            && self.saved.as_ref().is_some_and(|saved| {
                saved.config == pending.config
                    && saved.engine == pending.runtime.0
                    && saved.routing == pending.runtime.1
                    && saved.selection == pending.runtime.2
            });
        if native_transition
            || self
                .observed_engine_generation
                .is_none_or(|g| g < state.engine.generation)
        {
            self.receive(state)?;
        }
        Ok(())
    }
    pub fn begin_restart(&self) -> Result<PendingRestart> {
        let saved = self
            .saved
            .as_ref()
            .filter(|_| self.connected)
            .ok_or_else(|| CoreError("Connect to the mixer before restarting processing".into()))?;
        if !saved.engine.restart_eligible() || saved.engine.generation == u64::MAX {
            return Err(CoreError(
                "The mixer is not ready for an engine restart".into(),
            ));
        }
        Ok(PendingRestart {
            request: EngineRestartRequest {
                expected_revision: saved.config.revision,
                expected_generation: saved.engine.generation,
            },
            config: saved.config.clone(),
        })
    }
    /// The stream may have already reported the result of this reservation.
    /// Preserve newer runtime state and all local edits in that case.
    pub fn acknowledge_restart(
        &mut self,
        pending: &PendingRestart,
        state: DeviceState,
    ) -> Result<()> {
        pending.validate_acknowledgement(&state)?;
        if self
            .observed_engine_generation
            .is_none_or(|g| g < state.engine.generation)
        {
            self.receive(state)?;
        }
        Ok(())
    }
    /// Only call with the successful response belonging to this request.
    pub fn acknowledge(&mut self, pending: &PendingApply, mut state: DeviceState) -> Result<()> {
        state.validate()?;
        if state.config.revision <= pending.request.expected_revision
            || state.config.targets != pending.request.targets
            || state.config.selected_targets != pending.selected_targets
        {
            return Err(CoreError(
                "Save response does not match the requested settings".into(),
            ));
        }
        if self
            .saved
            .as_ref()
            .is_some_and(|s| s.config.revision > state.config.revision)
        {
            return Err(CoreError(
                "Save response is older than the known device state".into(),
            ));
        }
        self.preserve_observed_runtime(&mut state);
        if self.draft == pending.request.targets {
            self.draft = state.config.targets.clone();
        }
        // The device-wide screen setting is not a per-target edit: the
        // acknowledged state is what the mixer now runs.
        self.base_revision = Some(state.config.revision);
        self.observed_engine_generation = Some(state.engine.generation);
        self.saved = Some(state);
        self.connected = true;
        self.conflict = false;
        Ok(())
    }
    /// Accept a preset response while preserving edits made during the request.
    pub fn acknowledge_preset(
        &mut self,
        expected_revision: u64,
        original_draft: &[Target],
        mut state: DeviceState,
    ) -> Result<()> {
        state.validate()?;
        if expected_revision.checked_add(1) != Some(state.config.revision)
            || self
                .saved
                .as_ref()
                .is_some_and(|s| s.config.selected_targets != state.config.selected_targets)
            || self
                .saved
                .as_ref()
                .is_some_and(|s| s.config.revision > state.config.revision)
        {
            return Err(CoreError(
                "Preset response does not match the requested revision".into(),
            ));
        }
        self.preserve_observed_runtime(&mut state);
        let edited = self.draft != original_draft;
        if !edited {
            self.draft = state.config.targets.clone();
        }
        self.base_revision = Some(state.config.revision);
        self.observed_engine_generation = Some(state.engine.generation);
        self.saved = Some(state);
        self.connected = true;
        // A preset can replace every target. Keep edits made during the request
        // but require an explicit choice before overwriting its result.
        self.conflict = edited;
        Ok(())
    }

    /// A successful effects write confirms its settings, not current audio.
    /// The ordered stream may already have reported failure, recovery or a new
    /// generation after the server captured this reply. Call only after
    /// validating the response and its unchanged processing-path selection.
    fn preserve_observed_runtime(&self, state: &mut DeviceState) {
        if let Some(saved) = &self.saved
            && self
                .observed_engine_generation
                .is_some_and(|generation| generation >= state.engine.generation)
        {
            state.engine = saved.engine.clone();
            state.routing = saved.routing.clone();
            state.selection = saved.selection.clone();
        }
    }

    /// Deliberate user choice: keep local edits against the latest revision.
    pub fn rebase_edits(&mut self) -> Result<()> {
        let saved = self
            .saved
            .as_ref()
            .ok_or_else(|| CoreError("No device state is available".into()))?;
        saved.config.validate_update(&self.draft)?;
        self.base_revision = Some(saved.config.revision);
        self.conflict = false;
        Ok(())
    }
}
