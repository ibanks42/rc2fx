//! Observed USB link state, independent of saved effects and DSP progress.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UsbLinkState {
    Unavailable,
    NotAttached,
    Attached,
    Powered,
    Reconnecting,
    Unauthenticated,
    Default,
    Addressed,
    Configured,
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsbStatus {
    /// Pro II USB 1 only. This does not describe USB 2, network connectivity,
    /// host playback activity, or whether a configured link carries audio.
    pub usb1: UsbLinkState,
}
