//! Pure device configuration and editor logic. No audio, network or GUI runtime.
pub mod apo;
pub mod diagnostics;
pub mod easyeffects;
pub mod eq;
pub mod inputs;
pub mod model;
pub mod selection;
pub mod session;
mod strict_json;
pub mod usb;
pub mod versions;

pub use diagnostics::*;
pub use inputs::*;
pub use model::*;
pub use selection::*;
pub use session::*;
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

/// Rejects duplicate keys at every depth, oversized input and excessive nesting.
pub fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    Ok(serde_json::from_value(strict_json::parse(
        text,
        512 * 1024,
    )?)?)
}

/// `from_json_limited`'s checks, returning the untyped tree.
pub fn parse_json_limited(
    text: &str,
    maximum_bytes: usize,
    maximum_nodes: usize,
) -> Result<serde_json::Value> {
    strict_json::parse_with_limits(text, maximum_bytes, maximum_nodes)
}

/// Strict parsing with explicit storage bounds. A bounded validation pass keeps
/// only the current object keys, then deserializes the typed result directly;
/// large saved states do not require a second, untyped JSON tree. Callers
/// should retain `from_json` and its default limits.
pub fn from_json_limited<T: serde::de::DeserializeOwned>(
    text: &str,
    maximum_bytes: usize,
    maximum_nodes: usize,
) -> Result<T> {
    strict_json::validate(text, maximum_bytes, maximum_nodes)?;
    Ok(serde_json::from_str(text)?)
}
