use rc2fx_core::{UsbLinkState, UsbStatus};
use std::time::{Duration, Instant};

/// Hardware freshness is independent of audio callback and network freshness.
#[derive(Default)]
pub(super) struct Observation(Option<(UsbStatus, Instant)>);
impl Observation {
    pub(super) fn receive(&mut self, value: serde_json::Value, now: Instant) {
        self.0 = serde_json::from_value(value)
            .ok()
            .map(|status| (status, now));
    }
    pub(super) fn clear(&mut self) {
        self.0 = None;
    }
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self
            .0
            .is_some_and(|(_, at)| now.saturating_duration_since(at) >= Duration::from_secs(3))
        {
            self.clear();
            true
        } else {
            false
        }
    }
    pub(super) fn label(&self) -> &'static str {
        match self.0.map(|(status, _)| status.usb1) {
            None | Some(UsbLinkState::Unavailable) => "USB status unavailable",
            Some(UsbLinkState::Configured) => "USB connected",
            Some(UsbLinkState::NotAttached) => "USB disconnected",
            Some(UsbLinkState::Suspended) => "USB suspended",
            Some(_) => "USB connection not ready",
        }
    }
    pub(super) fn needs_attention(&self) -> bool {
        matches!(self.0, Some((UsbStatus { usb1 }, _)) if !matches!(usb1, UsbLinkState::Configured | UsbLinkState::Unavailable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn live_meters_do_not_hide_usb_loss_and_stale_status_is_not_current() {
        let now = Instant::now();
        let mut value = Observation::default();
        value.receive(json!({"usb1":"configured"}), now);
        assert!(!value.needs_attention());
        value.receive(json!({"usb1":"notAttached"}), now);
        assert!(value.needs_attention());
        assert_eq!(value.label(), "USB disconnected");
        assert!(!value.expire(now + Duration::from_millis(2999)));
        assert!(value.expire(now + Duration::from_secs(3)));
        assert!(!value.needs_attention());
        assert_eq!(value.label(), "USB status unavailable");
        value.receive(json!({"usb1":"configured"}), now);
        value.clear(); // stream or device change must discard the previous observation
        assert_eq!(value.label(), "USB status unavailable");
    }
    #[test]
    fn unknown_or_invalid_observation_is_not_treated_as_disconnect() {
        let now = Instant::now();
        let mut value = Observation::default();
        value.receive(json!({"usb1":"notAttached"}), now);
        for raw in [
            json!({"usb1":"unavailable"}),
            json!({"usb1":"invented"}),
            json!({"usb1":"configured","extra":true}),
        ] {
            value.receive(raw, now);
            assert!(!value.needs_attention());
            assert_eq!(value.label(), "USB status unavailable");
        }
        value.receive(json!({"usb1":"suspended"}), now);
        assert!(value.needs_attention());
        assert_eq!(value.label(), "USB suspended");
    }
}
