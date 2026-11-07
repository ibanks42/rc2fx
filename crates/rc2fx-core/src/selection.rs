//! Explicit topology requests; effects saves never carry this request.
use crate::{Config, CoreError, DeviceState, EngineMode, EnginePhase, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectionPhase {
    Idle,
    Changing,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionStatus {
    /// Audio-channel capacity from the last verified stock graph, not a CPU
    /// guarantee or reservation. None means it has not been observed.
    pub channel_capacity: Option<u32>,
    pub status: SelectionPhase,
    pub allowed: bool,
    pub qualified_target_ids: Vec<String>,
    /// Allocated by the actual engine. Routing independently reports wet edges.
    pub active_target_ids: Vec<String>,
    pub pending_target_ids: Vec<String>,
    pub message: String,
}
impl SelectionStatus {
    pub fn check_capacity(&self, config: &Config, ids: &[String]) -> Result<u32> {
        config.validate_selection(ids)?;
        let channels = config
            .targets
            .iter()
            .filter(|t| ids.contains(&t.id))
            .map(|t| u32::from(t.channels))
            .sum();
        if let Some(limit) = self.channel_capacity
            && channels > limit
        {
            return Err(CoreError(format!(
                "These paths need {channels} audio channels; the mixer has room for {limit}. Turn off another processing path first."
            )));
        }
        Ok(channels)
    }
    pub fn validate(&self, config: &Config) -> Result<()> {
        if self.channel_capacity.is_some_and(|limit| limit > 128) {
            return Err(CoreError("Invalid processing channel capacity".into()));
        }
        if !crate::valid_display_text(&self.message, crate::MAX_STATUS_TEXT) {
            return Err(CoreError("Invalid selection status message".into()));
        }
        for ids in [
            &self.qualified_target_ids,
            &self.active_target_ids,
            &self.pending_target_ids,
        ] {
            let canonical: Vec<_> = config
                .targets
                .iter()
                .filter(|t| ids.contains(&t.id))
                .map(|t| t.id.clone())
                .collect();
            if &canonical != ids {
                return Err(CoreError("Invalid processing path catalog or order".into()));
            }
        }
        for ids in [&self.active_target_ids, &self.pending_target_ids] {
            config.validate_selection(ids)?;
        }
        // Allocation is observed, not a promise of physical qualification
        // (for example, a synthetic engine can allocate unqualified paths).
        if self
            .pending_target_ids
            .iter()
            .any(|id| !self.qualified_target_ids.contains(id))
        {
            return Err(CoreError("Unqualified pending processing path".into()));
        }
        if self.status == SelectionPhase::Changing && self.allowed {
            return Err(CoreError(
                "A pending path change cannot allow another transition".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionRequest {
    pub expected_revision: u64,
    pub expected_generation: u64,
    pub target_ids: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct PendingSelection {
    pub request: SelectionRequest,
    pub(crate) config: Config,
    pub(crate) mode: EngineMode,
}
impl PendingSelection {
    pub fn validate_acknowledgement(&self, state: &DeviceState) -> Result<()> {
        state.validate()?;
        let native = self.mode == EngineMode::Native;
        let mut expected_config = self.config.clone();
        if native {
            expected_config.revision = expected_config
                .revision
                .checked_add(1)
                .ok_or_else(|| CoreError("Settings revision exhausted".into()))?;
            expected_config.selected_targets = self.request.target_ids.clone();
        }
        let expected_generation = if native {
            Some(self.request.expected_generation)
        } else {
            self.request.expected_generation.checked_add(1)
        };
        if state.config != expected_config
            || expected_generation != Some(state.engine.generation)
            || state.engine.mode != self.mode
            || state.engine.status
                != if native {
                    EnginePhase::Starting
                } else {
                    EnginePhase::Restarting
                }
            || state.engine.restart_allowed
            || state.engine.audio_advancing
            || (native && state.engine.applied_revision != 0)
            || state.selection.status != SelectionPhase::Changing
            || state.selection.pending_target_ids != self.request.target_ids
        {
            return Err(CoreError(
                "Path-change response does not match the requested selection and saved settings"
                    .into(),
            ));
        }
        Ok(())
    }
}
