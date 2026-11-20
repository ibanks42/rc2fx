//! The device state, engine status and meter messages of the control API.
use crate::{Config, SelectionPhase, SelectionStatus, Target};
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
    /// Peak voice score (0..=1) from the suppression model in this window, or 0 when suppression is off.
    pub voice: f64,
    /// Peak voice gate openness (0 = closed, 1 = open) in this window. 1 when the gate is off; 0 when suppression is off.
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
impl TargetMeter {
    pub fn validate(&self) -> crate::Result<()> {
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
            return Err(crate::CoreError("Invalid meter values".into()));
        }
        Ok(())
    }
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
    /// `calls == 0` means callback timing wasn't sampled in this build, not zero duration.
    pub callback: Timing,
    /// Suppression worker wall and CPU time (all zeros if no worker). Cumulative
    /// across worker rebuilds within the current engine generation.
    pub worker_wall: Timing,
    pub worker_cpu: Timing,
    /// Helper routines that finished past their cycle deadline, and callbacks
    /// that found the helper still busy. Omitted by older firmware and on the JACK path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_late: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper_stalls: Option<u64>,
    /// Slowest audio callback and slowest suppression worker wall-time turnaround
    /// over roughly the last 10 s of audio. Omitted by older firmware or before any samples.
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
    /// Incremented on each diagnostics reset so clients know to rebaseline
    /// their counter deltas. 0 means never reset (or older firmware).
    #[serde(default, rename = "diagnosticsEpoch")]
    pub diagnostics_epoch: u64,
    /// Uptime of this engine generation on the daemon's monotonic clock
    /// (measured from audio process start, or from when it first reached `Running`
    /// if process start time isn't readable). Omitted by older firmware.
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
    /// Only the native engine takes processing controls.
    pub fn can_enable_processing(&self) -> bool {
        self.native_controls_ready()
            && self.routing.status == RoutingPhase::Bypassed
            && self.routing.channels == 0
            && self.selection.active_target_ids.is_empty()
    }
    pub fn can_disable_processing(&self) -> bool {
        self.engine.mode == EngineMode::Native
            && ((self.native_controls_ready() && self.routing.status == RoutingPhase::Active)
                || (self.engine.status == EnginePhase::Error
                    && self.routing.status == RoutingPhase::Error
                    && self.selection.status != SelectionPhase::Changing
                    && self.selection.pending_target_ids.is_empty()))
    }
}
