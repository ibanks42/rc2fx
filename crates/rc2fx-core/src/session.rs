//! The device state, engine status and meter messages of the control API.
use crate::{Config, CoreError, Result, SelectionPhase, SelectionStatus, Target};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineMode {
    Simulation,
    Jack,
    Native,
    Offline,
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
    /// Slowest audio callback, and slowest suppression worker turnaround
    /// (wall time), within roughly the last 10 s of audio. Absent from a
    /// mixer that predates them, or before any timing in the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_recent_max_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_recent_max_ns: Option<u64>,
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
    /// How long this engine generation has been running when the reading
    /// was made, on the daemon's monotonic clock: from the start of the audio
    /// process that runs the engine (the mixer's own process), or from the
    /// moment it was first reported Running when that start cannot be read.
    /// Lets a reader place the startup window. Absent from older mixers.
    #[serde(
        default,
        rename = "runningForNs",
        skip_serializing_if = "Option::is_none"
    )]
    pub running_for_ns: Option<u64>,
    pub targets: Vec<TargetMeter>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeterType {
    Meter,
}

impl DeviceState {
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
