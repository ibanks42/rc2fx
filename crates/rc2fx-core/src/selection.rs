//! Explicit topology requests; effects saves never carry this request.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectionRequest {
    pub expected_revision: u64,
    pub expected_generation: u64,
    pub target_ids: Vec<String>,
}
