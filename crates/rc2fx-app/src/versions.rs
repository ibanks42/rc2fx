//! Version notices. The app, the RC2FX mixer software and RØDE's
//! firmware are versioned separately. A version the mixer did not report is
//! unknown: it is never treated as older or newer than anything.
use std::cmp::Ordering;

pub use rc2fx_core::compare_versions as compare;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The RC2FX mixer software this app installs; `None` without a payload.
pub fn bundled_mixer_software() -> Option<String> {
    rc2fx_installer::bundled_release().ok().map(|r| r.version)
}

/// The newest supported RØDE firmware for the same model as the mixer's
/// reported firmware profile; `None` when that is unknown.
pub fn newest_supported_firmware(firmware_profile: Option<&str>) -> Option<String> {
    firmware_profile.and_then(rc2fx_installer::newest_supported_firmware)
}

/// How an installed version relates to the one available here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The mixer did not report it, or it cannot be compared.
    Unknown,
    Current,
    /// A newer version is available; installing it means flashing.
    UpdateAvailable,
    /// The mixer is newer than this app knows about. Never offer a downgrade.
    Newer,
}

pub fn standing(installed: Option<&str>, available: Option<&str>) -> Standing {
    match installed.zip(available).and_then(|(i, a)| compare(i, a)) {
        None => Standing::Unknown,
        Some(Ordering::Equal) => Standing::Current,
        Some(Ordering::Less) => Standing::UpdateAvailable,
        Some(Ordering::Greater) => Standing::Newer,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub rows: Vec<Row>,
    pub runtime: Standing,
    pub firmware: Standing,
    pub compatibility: Compatibility,
}

/// Whether the mixer software speaks this app's control protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// Not reported (older mixer software, or not connected).
    Unknown,
    Compatible,
    /// The mixer software is older than this app supports; installing the
    /// bundled mixer software fixes it.
    MixerOlder,
    /// The mixer software is newer than this app; only an app update helps.
    MixerNewer,
}

pub fn compatibility(device_protocol: Option<u32>) -> Compatibility {
    match device_protocol {
        None => Compatibility::Unknown,
        Some(p) => match p.cmp(&rc2fx_core::MIXER_PROTOCOL_VERSION) {
            Ordering::Equal => Compatibility::Compatible,
            Ordering::Less => Compatibility::MixerOlder,
            Ordering::Greater => Compatibility::MixerNewer,
        },
    }
}

/// What to tell the user. The installer flashes RC2FX mixer software and RØDE
/// firmware as one image, so an update is offered only when neither would go
/// backwards and both are known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    /// Whether to offer opening the installer.
    pub offer_installer: bool,
}

impl Summary {
    /// Both parts known, neither newer than what this app installs, and at
    /// least one older.
    pub fn update_available(&self) -> bool {
        use Standing::{Current, UpdateAvailable};
        if self.compatibility == Compatibility::MixerOlder {
            // The bundled mixer software is the fix, as long as the combined
            // image would not take the firmware backwards.
            return self.runtime != Standing::Newer
                && matches!(self.firmware, Current | UpdateAvailable);
        }
        self.compatibility == Compatibility::Compatible
            && matches!(
                (self.runtime, self.firmware),
                (UpdateAvailable, Current | UpdateAvailable) | (Current, UpdateAvailable)
            )
    }

    pub fn notice(&self) -> Option<Notice> {
        if self.compatibility == Compatibility::MixerOlder {
            let offer_installer = self.update_available();
            return Some(Notice {
                text: if offer_installer {
                    "The RC2FX mixer software on this mixer is older than this app supports. \
                     Installing the bundled mixer software flashes the mixer, which then \
                     restarts; audio stops until it is back. Your drafts are kept."
                } else {
                    "The RC2FX mixer software on this mixer is older than this app supports, \
                     and its RØDE firmware is newer or unknown, so this app cannot update it \
                     safely."
                }
                .into(),
                offer_installer,
            });
        }
        if self.compatibility == Compatibility::MixerNewer {
            return Some(Notice {
                text: "The RC2FX mixer software on this mixer was made for a different \
                       version of this app, so compatibility is unknown. Update the app \
                       before changing anything on the mixer."
                    .into(),
                offer_installer: false,
            });
        }
        let newer = match (self.runtime, self.firmware) {
            (Standing::Newer, Standing::Newer) => {
                Some("newer RC2FX mixer software and RØDE firmware")
            }
            (Standing::Newer, _) => Some("newer RC2FX mixer software"),
            (_, Standing::Newer) => Some("newer RØDE firmware"),
            _ => None,
        };
        if let Some(newer) = newer {
            return Some(Notice {
                text: format!(
                    "This mixer has {newer} than this app installs. The installer flashes \
                     RC2FX mixer software and RØDE firmware together, so installing from \
                     this app would replace it with older versions. Update the app first."
                ),
                offer_installer: false,
            });
        }
        if !self.update_available() {
            return None;
        }
        let what = match (self.runtime, self.firmware) {
            (Standing::UpdateAvailable, Standing::UpdateAvailable) => {
                "Newer RC2FX mixer software and a newer supported RØDE firmware are available"
            }
            (Standing::UpdateAvailable, _) => "Newer RC2FX mixer software is available",
            _ => "A newer supported RØDE firmware is available",
        };
        Some(Notice {
            text: format!(
                "{what}. Installing flashes the mixer, which then restarts; \
                 audio stops until it is back. Your drafts are kept."
            ),
            offer_installer: true,
        })
    }
}

pub struct Inputs<'a> {
    pub app: &'a str,
    pub installed_runtime: Option<&'a str>,
    pub bundled_runtime: Option<&'a str>,
    pub installed_firmware: Option<&'a str>,
    pub newest_supported_firmware: Option<&'a str>,
    pub device_protocol: Option<u32>,
}

pub fn summarize(i: &Inputs) -> Summary {
    let show = |v: Option<&str>| v.map_or_else(|| "Unknown".to_owned(), str::to_owned);
    Summary {
        rows: vec![
            Row {
                label: "App",
                value: i.app.to_owned(),
            },
            Row {
                label: "RC2FX mixer software (installed)",
                value: show(i.installed_runtime),
            },
            Row {
                label: "RC2FX mixer software bundled with this app",
                value: show(i.bundled_runtime),
            },
            Row {
                label: "RØDE firmware (installed)",
                value: show(i.installed_firmware),
            },
            Row {
                label: "Newest supported RØDE firmware",
                value: show(i.newest_supported_firmware),
            },
            Row {
                label: "Works with this app",
                value: match compatibility(i.device_protocol) {
                    Compatibility::Unknown => "Unknown",
                    Compatibility::Compatible => "Yes",
                    Compatibility::MixerOlder | Compatibility::MixerNewer => "No",
                }
                .into(),
            },
        ],
        runtime: standing(i.installed_runtime, i.bundled_runtime),
        firmware: standing(i.installed_firmware, i.newest_supported_firmware),
        compatibility: compatibility(i.device_protocol),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(rt: Option<&'a str>, fw: Option<&'a str>) -> Inputs<'a> {
        Inputs {
            app: "0.3.0",
            installed_runtime: rt,
            bundled_runtime: Some("0.3.0"),
            installed_firmware: fw,
            newest_supported_firmware: Some("1.8.0"),
            device_protocol: Some(rc2fx_core::MIXER_PROTOCOL_VERSION),
        }
    }

    #[test]
    fn protocol_mismatch_blocks_updates_and_says_update_the_app() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.device_protocol = Some(rc2fx_core::MIXER_PROTOCOL_VERSION + 1);
        let s = summarize(&i);
        assert!(!s.update_available());
        let n = s.notice().unwrap();
        assert!(!n.offer_installer && n.text.contains("Update the app"));
        assert_eq!(s.rows.last().unwrap().value, "No");
    }

    #[test]
    fn older_protocol_offers_the_bundled_mixer_software() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.device_protocol = Some(rc2fx_core::MIXER_PROTOCOL_VERSION - 1);
        let s = summarize(&i);
        assert_eq!(s.compatibility, Compatibility::MixerOlder);
        let n = s.notice().unwrap();
        assert!(n.offer_installer && n.text.contains("older than this app supports"));
        // Never if the combined image would take the firmware backwards.
        i.installed_firmware = Some("1.9.0");
        assert!(!summarize(&i).notice().unwrap().offer_installer);
        i.installed_firmware = None;
        assert!(!summarize(&i).notice().unwrap().offer_installer);
    }

    #[test]
    fn unknown_protocol_never_offers_an_update() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.device_protocol = None;
        let s = summarize(&i);
        assert_eq!(s.compatibility, Compatibility::Unknown);
        assert!(!s.update_available());
        assert_eq!(s.notice(), None);
        assert_eq!(s.rows.last().unwrap().value, "Unknown");
    }

    #[test]
    fn compares_numerically() {
        assert_eq!(compare("1.7.6", "1.10.0"), Some(Ordering::Less));
        assert_eq!(compare("1.8", "1.8.0"), Some(Ordering::Equal));
        assert_eq!(compare("2.0.0", "1.9.9"), Some(Ordering::Greater));
        assert_eq!(compare("1.8.0-rc1", "1.8.0"), None);
        assert_eq!(compare("", "1.0"), None);
    }

    #[test]
    fn unknown_device_versions_never_prompt() {
        let s = summarize(&inputs(None, None));
        assert_eq!(
            (s.runtime, s.firmware),
            (Standing::Unknown, Standing::Unknown)
        );
        assert!(!s.update_available());
        assert_eq!(s.notice(), None);
        assert_eq!(s.rows[1].value, "Unknown");
        assert_eq!(s.rows[3].value, "Unknown");
    }

    #[test]
    fn unparseable_is_unknown() {
        assert_eq!(
            standing(Some("dev-build"), Some("0.3.0")),
            Standing::Unknown
        );
    }

    #[test]
    fn newer_device_is_not_a_downgrade_offer() {
        let s = summarize(&inputs(Some("0.4.0"), Some("1.9.0")));
        assert_eq!((s.runtime, s.firmware), (Standing::Newer, Standing::Newer));
        assert!(!s.update_available());
        let n = s.notice().unwrap();
        assert!(!n.offer_installer && n.text.contains("Update the app first"));
    }

    #[test]
    fn mixed_versions_never_offer_the_combined_flash() {
        // Older mixer software but newer RØDE firmware, and the reverse.
        for (rt, fw) in [("0.2.0", "1.9.0"), ("0.4.0", "1.7.6")] {
            let s = summarize(&inputs(Some(rt), Some(fw)));
            assert!(!s.update_available(), "{rt} {fw}");
            assert!(!s.notice().unwrap().offer_installer, "{rt} {fw}");
        }
    }

    #[test]
    fn one_unknown_part_never_offers_the_combined_flash() {
        for (rt, fw) in [(Some("0.2.0"), None), (None, Some("1.7.6"))] {
            let s = summarize(&inputs(rt, fw));
            assert!(!s.update_available());
            assert_eq!(s.notice(), None);
        }
    }

    #[test]
    fn older_runtime_offers_flash_and_restart() {
        let s = summarize(&inputs(Some("0.2.1"), Some("1.8.0")));
        assert_eq!(s.runtime, Standing::UpdateAvailable);
        assert_eq!(s.firmware, Standing::Current);
        let n = s.notice().unwrap();
        assert!(n.offer_installer);
        assert!(n.text.contains("RC2FX mixer software") && n.text.contains("flashes"));
        assert!(n.text.contains("restarts"));
    }

    #[test]
    fn older_firmware_offers_update() {
        let s = summarize(&inputs(Some("0.3.0"), Some("1.7.6")));
        assert!(s.notice().unwrap().text.contains("RØDE firmware"));
    }

    #[test]
    fn labels_are_explicit() {
        let labels: Vec<_> = summarize(&inputs(None, None))
            .rows
            .iter()
            .map(|r| r.label)
            .collect();
        assert_eq!(
            labels,
            [
                "App",
                "RC2FX mixer software (installed)",
                "RC2FX mixer software bundled with this app",
                "RØDE firmware (installed)",
                "Newest supported RØDE firmware",
                "Works with this app"
            ]
        );
    }
}
