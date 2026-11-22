//! The device state, engine status and meter messages of the control API.
use crate::SelectionPhase;
pub use crate::schema_gen::{
    ConfigUpdate, DeviceState, EngineMode, EnginePhase, EngineRestartRequest, EngineStatus, Meter,
    MeterType, Performance, PresetSummary, RoutingPhase, RoutingRequest, RoutingStatus,
    TargetMeter, Timing,
};

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
