//! Pure device configuration and control API types (no audio, network, or GUI runtime).
pub mod diagnostics;
pub mod eq;
pub mod image_check;
pub mod inputs;
pub mod model;
pub mod selection;
pub mod session;
pub mod ssh;
mod strict_json;
pub mod usb;
pub mod versions;

pub use diagnostics::*;
pub use image_check::{ImageCheck, ImageStatus};
pub use inputs::*;
pub use model::*;
pub use selection::*;
pub use session::*;
pub use ssh::*;
pub use usb::*;
pub use versions::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError(pub String);
impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CoreError {}
impl From<serde_json::Error> for CoreError {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}
pub type Result<T> = std::result::Result<T, CoreError>;

/// Rejects duplicate keys at any depth, oversized inputs, and excessive nesting.
pub fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    from_json_limited(text, 512 * 1024, 200_000)
}

/// Strict JSON parsing with explicit byte and node limits.
///
/// Validates structure in a streaming pass (retaining only the current object's
/// keys) before deserializing directly into `T`, avoiding a second untyped tree.
/// Most callers should use `from_json`.
pub fn from_json_limited<T: serde::de::DeserializeOwned>(
    text: &str,
    maximum_bytes: usize,
    maximum_nodes: usize,
) -> Result<T> {
    strict_json::validate(text, maximum_bytes, maximum_nodes)?;
    Ok(serde_json::from_str(text)?)
}
