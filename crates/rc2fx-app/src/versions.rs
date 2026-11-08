//! Version notices. The app, the RC2FX mixer software and RØDE's
//! firmware are versioned separately. A version the mixer did not report is
//! unknown: it is never treated as older or newer than anything.
use rc2fx_installer::app_update::MixerUpdateState;
use rc2fx_installer::mixer_update::MixerOffer;
use std::cmp::Ordering;

pub use rc2fx_core::compare_versions as compare;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The RC2FX mixer software built into this app; `None` without a payload.
pub fn bundled_mixer_software() -> Option<String> {
    rc2fx_installer::bundled_release().ok().map(|r| r.version)
}

/// Whether this app can install a signed offer on firmware `profile`.
pub fn offer_fits(offer: &MixerOffer, profile: &str) -> bool {
    offer
        .metadata
        .require_compatible(APP_VERSION, rc2fx_core::MIXER_PROTOCOL_VERSION, profile)
        .is_ok()
}

/// The newest version this app could install, signed or built in. The
/// firmware may change in the same flash, so any firmware the offer supports
/// counts.
pub fn newest_available(offers: &[MixerOffer], bundled: Option<&str>) -> Option<String> {
    offers
        .iter()
        .filter(|offer| {
            offer
                .metadata
                .firmware_profiles
                .iter()
                .any(|profile| offer_fits(offer, profile))
        })
        .map(|offer| offer.metadata.version.as_str())
        .chain(bundled)
        .filter(|version| compare(version, version).is_some())
        .max_by(|a, b| compare(a, b).expect("both versions are comparable"))
        .map(str::to_owned)
}

/// The newest supported RØDE firmware for the same model as the mixer's
/// reported firmware profile; `None` when that is unknown.
pub fn newest_supported_firmware(firmware_profile: Option<&str>) -> Option<String> {
    firmware_profile.and_then(rc2fx_installer::newest_supported_firmware)
}

/// Whether `minimum` rules out this app.
pub fn app_too_old(minimum: &str) -> bool {
    compare(APP_VERSION, minimum) == Some(Ordering::Less)
}

/// A signed release newer than `available` that only a newer app can install,
/// as its version and the app version it needs.
pub fn needs_newer_app(offers: &[MixerOffer], available: Option<&str>) -> Option<(String, String)> {
    offers
        .iter()
        .map(|offer| &offer.metadata)
        .filter(|m| app_too_old(&m.minimum_app_version))
        .filter(|m| available.is_none_or(|a| compare(&m.version, a) == Some(Ordering::Greater)))
        .max_by(|a, b| compare(&a.version, &b.version).unwrap_or(Ordering::Equal))
        .map(|m| (m.version.clone(), m.minimum_app_version.clone()))
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
pub struct Summary {
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
    /// The mixer software is older than this app supports; installing
    /// compatible mixer software fixes it.
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
            // Compatible mixer software is the fix, as long as the combined
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
                     Installing compatible mixer software flashes the mixer, which then \
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
                    "This mixer has {newer} than this app can install. The installer \
                     flashes RC2FX mixer software and RØDE firmware together, so installing \
                     from this app would replace it with older versions. Update the app first."
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
    /// The newest mixer software this app could install, signed or built in.
    pub available_runtime: Option<&'a str>,
    /// A signed release check has answered, so what isn't in
    /// [`Inputs::available_runtime`] really isn't available to this app.
    pub signed_known: bool,
    /// Newer signed mixer software that needs a newer app, and the app
    /// version it needs.
    pub needs_app: Option<(&'a str, &'a str)>,
    pub installed_firmware: Option<&'a str>,
    pub newest_supported_firmware: Option<&'a str>,
    /// RØDE's newest stable firmware for this mixer, from its manifest;
    /// `None` until a check answers, or for a mixer RØDE's manifest omits.
    pub rode_firmware: Option<&'a str>,
    pub device_protocol: Option<u32>,
}

/// RØDE's newest firmware, when it is newer than the mixer's and than
/// anything this app can install with RC2FX.
fn rode_ahead<'a>(i: &Inputs<'a>) -> Option<&'a str> {
    let rode = i.rode_firmware?;
    let newer = |than: &str| compare(rode, than) == Some(Ordering::Greater);
    (newer(i.installed_firmware?) && i.newest_supported_firmware.is_none_or(newer)).then_some(rode)
}

pub fn summarize(i: &Inputs) -> Summary {
    Summary {
        runtime: standing(i.installed_runtime, i.available_runtime),
        firmware: standing(i.installed_firmware, i.newest_supported_firmware),
        compatibility: compatibility(i.device_protocol),
    }
}

/// One part of the system in the Versions dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    App,
    Mixer,
    Firmware,
}

/// How much a line's status asks of the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Unknown,
    Quiet,
    Update,
    Warn,
    Bad,
}

/// What the last update check said about this app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppStanding {
    /// A check is running.
    Checking,
    /// No check has answered yet, or the last one failed.
    Unknown,
    Current {
        latest: String,
    },
    Update {
        version: String,
    },
}

/// One line of the Versions dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub part: Part,
    pub name: &'static str,
    pub installed: String,
    pub available: String,
    pub status: &'static str,
    pub tone: Tone,
    pub read_changes: bool,
}

/// The answer Settings leads with: what matters, and what to read about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lead {
    pub title: String,
    pub detail: Option<String>,
    /// The part whose changes What's new opens.
    pub changes: Option<Part>,
}

/// Everything Settings shows about versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub lead: Lead,
    pub lines: Vec<Line>,
    /// `App 1.0.0 · Mixer 0.1.0 · Firmware 1.7.7`.
    pub summary: String,
    pub flag: String,
    pub flag_tone: Tone,
    /// Why nothing can be installed, and what a newer app would bring.
    pub details: Vec<String>,
    /// Whether to offer the installer: this app can install something newer,
    /// or only its signed release check can tell. The update feed is never
    /// enough. Opening the installer changes nothing on the mixer.
    pub offer_installer: bool,
}

const UP_TO_DATE: (&str, Tone) = ("Up to date", Tone::Quiet);
const UPDATE: (&str, Tone) = ("Update available", Tone::Update);
const BLOCKED: (&str, Tone) = ("Blocked", Tone::Warn);
const NEWER: (&str, Tone) = ("Newer", Tone::Warn);
const UPDATE_APP_FIRST: (&str, Tone) = ("Update the app first", Tone::Warn);
const UNKNOWN: (&str, Tone) = ("Unknown", Tone::Unknown);
/// RØDE has released newer firmware than RC2FX supports. Nothing to do yet.
const RODE_NEWER: (&str, Tone) = ("Newer from RØDE", Tone::Quiet);

fn mixer_status(
    s: &Summary,
    mixer: &MixerUpdateState,
    signed_known: bool,
) -> ((&'static str, Tone), bool) {
    if s.compatibility == Compatibility::MixerNewer {
        return (("Update the app", Tone::Warn), false);
    }
    let too_old = s.compatibility == Compatibility::MixerOlder;
    match mixer {
        MixerUpdateState::AppUpdateRequired { .. } if signed_known => {
            (("Needs app update", Tone::Warn), true)
        }
        // A signed release may carry it: only that check can say.
        MixerUpdateState::AppUpdateRequired { .. } => (UNKNOWN, true),
        MixerUpdateState::Newer => (NEWER, false),
        MixerUpdateState::Bundled { .. } | MixerUpdateState::BundledThenApp { .. } => {
            let status = match (s.update_available(), too_old) {
                (false, _) => BLOCKED,
                (true, true) => ("Too old", Tone::Bad),
                (true, false) => UPDATE,
            };
            (status, true)
        }
        MixerUpdateState::Current | MixerUpdateState::Unknown if too_old => {
            (("Too old", Tone::Bad), false)
        }
        MixerUpdateState::Current | MixerUpdateState::Unknown => (
            match s.runtime {
                Standing::Current => UP_TO_DATE,
                Standing::Newer => NEWER,
                Standing::UpdateAvailable if s.update_available() => UPDATE,
                Standing::UpdateAvailable => BLOCKED,
                Standing::Unknown => UNKNOWN,
            },
            false,
        ),
    }
}

fn firmware_status(s: &Summary) -> ((&'static str, Tone), bool) {
    match s.firmware {
        Standing::Current => (UP_TO_DATE, false),
        Standing::UpdateAvailable if s.update_available() => (UPDATE, true),
        Standing::UpdateAvailable => (BLOCKED, true),
        Standing::Newer => (NEWER, false),
        Standing::Unknown => (UNKNOWN, false),
    }
}

/// What to show for the app, the mixer software and the firmware.
/// `mixer` is informational: `offer_installer` ignores it.
pub fn overview(i: &Inputs, app: &AppStanding, mixer: &MixerUpdateState) -> Overview {
    let s = summarize(i);
    let show = |v: Option<&str>| v.map_or_else(|| "Unknown".to_owned(), str::to_owned);
    let (app_available, (app_status, app_tone), app_read) = match app {
        AppStanding::Checking => ("Unknown".to_owned(), ("Checking…", Tone::Unknown), false),
        AppStanding::Unknown => ("Unknown".to_owned(), UNKNOWN, false),
        AppStanding::Current { latest } => (latest.clone(), UP_TO_DATE, false),
        AppStanding::Update { version } => (version.clone(), UPDATE, true),
    };
    let ((mixer_text, mixer_tone), mixer_read) = match i.needs_app {
        // What can be installed now comes first; the note below says the rest.
        Some(_) if !s.update_available() => (("Needs app update", Tone::Warn), false),
        _ => match (mixer_status(&s, mixer, i.signed_known), app) {
            // A newer app is available, and it's what this mixer is waiting for.
            ((NEWER, read), AppStanding::Update { .. }) => (UPDATE_APP_FIRST, read),
            (status, _) => status,
        },
    };
    let rode = rode_ahead(i);
    let ((firmware_text, firmware_tone), firmware_read) = match rode {
        // "Up to date" would be wrong: RØDE has newer, RC2FX can't install it.
        Some(_) if s.firmware == Standing::Current => (RODE_NEWER, false),
        _ => firmware_status(&s),
    };
    let lines = vec![
        Line {
            part: Part::App,
            name: "App",
            installed: i.app.to_owned(),
            available: app_available,
            status: app_status,
            tone: app_tone,
            read_changes: app_read,
        },
        Line {
            part: Part::Mixer,
            name: "Mixer software",
            installed: show(i.installed_runtime),
            available: match (i.needs_app, mixer) {
                (Some((version, _)), _) => version.to_owned(),
                (_, MixerUpdateState::AppUpdateRequired { latest, .. }) => latest.clone(),
                _ => show(i.available_runtime),
            },
            status: mixer_text,
            tone: mixer_tone,
            read_changes: mixer_read,
        },
        Line {
            part: Part::Firmware,
            name: "RØDE firmware",
            installed: show(i.installed_firmware),
            available: show(rode.or(i.newest_supported_firmware)),
            status: firmware_text,
            tone: firmware_tone,
            read_changes: firmware_read,
        },
    ];
    let notice = s.notice();
    let mut details: Vec<String> = notice
        .iter()
        .filter(|n| !n.offer_installer)
        .map(|n| n.text.clone())
        .collect();
    if let MixerUpdateState::BundledThenApp {
        latest,
        app_version,
        ..
    }
    | MixerUpdateState::AppUpdateRequired {
        latest,
        app_version,
    } = mixer
        && i.signed_known
        && i.needs_app.is_none()
    {
        details.push(format!(
            "Mixer software {latest} isn't available for this app. Update to RC2FX {app_version} to get it."
        ));
    }
    if let Some((version, minimum)) = i.needs_app {
        details.push(format!(
            "Mixer software {version} needs RC2FX {minimum} or newer. Update the app first."
        ));
    }
    let connected = i.installed_runtime.is_some() || i.installed_firmware.is_some();
    if connected && *mixer == MixerUpdateState::Unknown && details.is_empty() {
        details.push("The latest published mixer software is unknown.".into());
    }
    let check_in_installer = connected
        && !i.signed_known
        && s.compatibility != Compatibility::MixerNewer
        && s.runtime != Standing::Newer
        && s.firmware != Standing::Newer;
    if check_in_installer {
        details.push("Open the installer to check for signed mixer software.".into());
    }
    let updates = lines.iter().filter(|l| l.tone == Tone::Update).count();
    let worst = lines.iter().map(|l| l.tone).max().unwrap_or(Tone::Unknown);
    let (flag, flag_tone) = match worst {
        Tone::Bad => ("Needs update".to_owned(), Tone::Bad),
        Tone::Warn => ("Needs attention".to_owned(), Tone::Warn),
        Tone::Update if updates == 1 => ("Update available".to_owned(), Tone::Update),
        Tone::Update => (format!("{updates} updates available"), Tone::Update),
        _ if !connected => ("Not connected".to_owned(), Tone::Unknown),
        _ if lines.iter().all(|l| l.tone == Tone::Quiet) => (
            if rode.is_some() {
                "RC2FX up to date"
            } else {
                "Up to date"
            }
            .to_owned(),
            Tone::Quiet,
        ),
        _ => ("Unknown".to_owned(), Tone::Unknown),
    };
    let waiting = lines
        .iter()
        .find(|l| l.tone == Tone::Update && l.part != Part::App);
    let lead = match (app, waiting) {
        (AppStanding::Update { version }, _) => Lead {
            title: format!("RC2FX {version} is available"),
            detail: Some(format!("You have {}.", i.app)),
            changes: Some(Part::App),
        },
        (AppStanding::Checking, _) => Lead {
            title: "Checking for updates…".into(),
            detail: None,
            changes: None,
        },
        (_, Some(_)) => Lead {
            title: "A mixer update is ready".into(),
            detail: Some("Mixer software and RØDE firmware install together.".into()),
            changes: lines
                .iter()
                .find(|l| l.tone == Tone::Update && l.read_changes)
                .map(|l| l.part),
        },
        _ if flag_tone == Tone::Quiet => match rode {
            Some(rode) => Lead {
                title: "RC2FX is up to date".into(),
                detail: Some(format!(
                    "RØDE firmware {rode} is out, but RC2FX doesn't support it yet."
                )),
                changes: None,
            },
            None => Lead {
                title: "Everything is up to date".into(),
                detail: None,
                changes: None,
            },
        },
        _ => Lead {
            title: flag.clone(),
            detail: None,
            changes: None,
        },
    };
    // Said in the lead when nothing else is going on; otherwise here.
    if let Some(rode) = rode
        && flag_tone != Tone::Quiet
    {
        details.push(format!(
            "RØDE firmware {rode} is out, but RC2FX doesn't support it yet, so this app can't install it."
        ));
    }
    Overview {
        lead,
        summary: format!(
            "App {} · Mixer {} · Firmware {}",
            lines[0].installed, lines[1].installed, lines[2].installed
        ),
        lines,
        flag,
        flag_tone,
        details,
        offer_installer: check_in_installer || notice.is_some_and(|n| n.offer_installer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(rt: Option<&'a str>, fw: Option<&'a str>) -> Inputs<'a> {
        Inputs {
            app: "0.3.0",
            installed_runtime: rt,
            available_runtime: Some("0.3.0"),
            signed_known: true,
            needs_app: None,
            installed_firmware: fw,
            newest_supported_firmware: Some("1.8.0"),
            rode_firmware: None,
            device_protocol: Some(rc2fx_core::MIXER_PROTOCOL_VERSION),
        }
    }

    #[test]
    fn newer_rode_firmware_rc2fx_cant_install_is_not_called_up_to_date() {
        let mut i = inputs(Some("0.3.0"), Some("1.8.0"));
        let current = AppStanding::Current {
            latest: "0.3.0".into(),
        };
        let o = overview(&i, &current, &MixerUpdateState::Current);
        assert_eq!(o.lead.title, "Everything is up to date");

        i.rode_firmware = Some("1.9.0");
        let o = overview(&i, &current, &MixerUpdateState::Current);
        assert_eq!(o.lead.title, "RC2FX is up to date");
        assert!(o.lead.detail.unwrap().contains("1.9.0"));
        let firmware = &o.lines[2];
        assert_eq!((firmware.status, firmware.tone), RODE_NEWER);
        assert_eq!(firmware.available, "1.9.0");
        assert!(!o.offer_installer, "nothing this app can install");

        // RØDE's newest is what RC2FX supports, or what the mixer has.
        for rode in ["1.8.0", "1.7.0"] {
            i.rode_firmware = Some(rode);
            let o = overview(&i, &current, &MixerUpdateState::Current);
            assert_eq!(o.lead.title, "Everything is up to date", "{rode}");
        }

        // An update RC2FX can install still leads; RØDE's news is a note.
        i.installed_firmware = Some("1.7.0");
        i.rode_firmware = Some("1.9.0");
        let o = overview(&i, &current, &MixerUpdateState::Current);
        assert_eq!(o.lines[2].tone, Tone::Update);
        assert!(o.details.iter().any(|d| d.contains("1.9.0")));
    }

    #[test]
    fn protocol_mismatch_blocks_updates_and_says_update_the_app() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.device_protocol = Some(rc2fx_core::MIXER_PROTOCOL_VERSION + 1);
        let s = summarize(&i);
        assert!(!s.update_available());
        let n = s.notice().unwrap();
        assert!(!n.offer_installer && n.text.contains("Update the app"));
    }

    #[test]
    fn older_protocol_offers_compatible_mixer_software() {
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

    fn current() -> AppStanding {
        AppStanding::Current {
            latest: "0.3.0".into(),
        }
    }

    fn line(o: &Overview, part: Part) -> &Line {
        o.lines.iter().find(|l| l.part == part).unwrap()
    }

    fn update() -> AppStanding {
        AppStanding::Update {
            version: "0.4.0".into(),
        }
    }

    #[test]
    fn an_app_update_leads_and_offers_its_changes() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let o = overview(&i, &update(), &MixerUpdateState::Current);
        assert_eq!(o.lead.title, "RC2FX 0.4.0 is available");
        assert_eq!(o.lead.detail.as_deref(), Some("You have 0.3.0."));
        assert_eq!(o.lead.changes, Some(Part::App));
    }

    #[test]
    fn a_mixer_update_leads_when_the_app_is_current() {
        let i = inputs(Some("0.2.0"), Some("1.7.0"));
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert_eq!(o.lead.title, "A mixer update is ready");
        assert!(o.offer_installer);
    }

    #[test]
    fn nothing_to_do_says_so_and_offers_nothing() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Current);
        assert_eq!(o.lead.title, "Everything is up to date");
        assert_eq!(o.lead.changes, None);
    }

    #[test]
    fn a_mixer_newer_than_the_app_points_at_the_app_update() {
        let mut i = inputs(Some("0.4.0"), Some("1.8.0"));
        i.available_runtime = Some("0.3.0");
        let waiting = overview(&i, &update(), &MixerUpdateState::Current);
        assert_eq!(line(&waiting, Part::Mixer).status, "Update the app first");
        let no_update = overview(&i, &current(), &MixerUpdateState::Current);
        assert_eq!(line(&no_update, Part::Mixer).status, "Newer");
    }

    #[test]
    fn an_unknown_latest_version_is_only_said_when_nothing_better_is() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Unknown);
        assert_eq!(
            o.details,
            ["The latest published mixer software is unknown."]
        );
        let mut i = inputs(Some("0.3.0"), Some("1.8.0"));
        i.needs_app = Some(("9.0.0", "99.0.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Unknown);
        assert!(o.details.iter().all(|d| !d.contains("unknown")));
    }

    fn offer(version: &str, minimum_app: &str, protocol: u32) -> MixerOffer {
        MixerOffer {
            target: format!("mixer-{version}.payload"),
            sha256: "00".repeat(32),
            length: 1,
            metadata: rc2fx_installer::mixer_update::MixerTarget {
                version: version.into(),
                payload_format: 1,
                protocol_version: protocol,
                minimum_app_version: minimum_app.into(),
                firmware_profiles: vec!["rodecaster-pro-ii-1.7.7".into()],
            },
        }
    }

    #[test]
    fn only_compatible_offers_count_as_available() {
        let protocol = rc2fx_core::MIXER_PROTOCOL_VERSION;
        let offers = [
            offer("9.0.0", "99.0.0", protocol),
            offer("8.0.0", "0.0.1", protocol + 1),
            offer("2.0.0", "0.0.1", protocol),
        ];
        assert_eq!(newest_available(&offers, None).as_deref(), Some("2.0.0"));
        assert_eq!(
            newest_available(&offers, Some("3.0.0")).as_deref(),
            Some("3.0.0"),
            "a newer built-in copy is available too"
        );
        assert_eq!(newest_available(&[], None), None);
    }

    #[test]
    fn newer_software_that_needs_a_newer_app_is_named_with_the_version_it_needs() {
        let protocol = rc2fx_core::MIXER_PROTOCOL_VERSION;
        let offers = [
            offer("9.0.0", "99.0.0", protocol),
            offer("2.0.0", "0.0.1", protocol),
        ];
        let needed = Some(("9.0.0".to_owned(), "99.0.0".to_owned()));
        assert_eq!(needs_newer_app(&offers, Some("2.0.0")), needed);
        assert_eq!(needs_newer_app(&offers, None), needed);
        assert_eq!(needs_newer_app(&offers, Some("9.0.0")), None);
        assert_eq!(needs_newer_app(&offers[1..], Some("1.0.0")), None);

        let mut i = inputs(Some("0.3.0"), Some("1.8.0"));
        i.needs_app = Some(("9.0.0", "99.0.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Current);
        let mixer = line(&o, Part::Mixer);
        assert_eq!(
            (mixer.status, mixer.available.as_str()),
            ("Needs app update", "9.0.0")
        );
        assert!(!o.offer_installer, "nothing here can install it");
        assert!(o.details.iter().any(|d| d.contains("RC2FX 99.0.0")));
    }

    #[test]
    fn what_can_be_installed_now_still_comes_first() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.needs_app = Some(("9.0.0", "99.0.0"));
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert_eq!(line(&o, Part::Mixer).status, "Update available");
        assert!(o.offer_installer);
        assert!(o.details.iter().any(|d| d.contains("RC2FX 99.0.0")));
    }

    #[test]
    fn a_signed_update_needs_no_payload_in_the_app() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.available_runtime = Some("0.3.0");
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert!(o.offer_installer);
        assert_eq!(line(&o, Part::Mixer).status, "Update available");
    }

    #[test]
    fn without_a_signed_answer_the_app_is_not_blamed_and_the_installer_can_check() {
        let mut i = inputs(Some("0.3.0"), Some("1.8.0"));
        i.available_runtime = None;
        i.signed_known = false;
        let state = MixerUpdateState::AppUpdateRequired {
            latest: "0.4.0".into(),
            app_version: "0.4.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert_eq!(line(&o, Part::Mixer).status, "Unknown");
        assert!(o.offer_installer);
        assert!(o.details.iter().all(|d| !d.contains("RC2FX 0.4.0")));
        assert!(
            o.details
                .iter()
                .any(|d| d.contains("signed mixer software"))
        );
        // Newer than anything known: nothing to look for.
        i.installed_firmware = Some("1.9.0");
        assert!(!overview(&i, &current(), &MixerUpdateState::Unknown).offer_installer);
    }

    #[test]
    fn a_published_mixer_never_offers_the_installer() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let state = MixerUpdateState::AppUpdateRequired {
            latest: "0.4.0".into(),
            app_version: "0.4.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert!(!o.offer_installer);
        let mixer = line(&o, Part::Mixer);
        assert_eq!(
            (mixer.status, mixer.available.as_str()),
            ("Needs app update", "0.4.0")
        );
        assert!(mixer.read_changes);
        assert!(o.details.iter().any(|d| d.contains("RC2FX 0.4.0")));
    }

    #[test]
    fn a_bundled_update_offers_the_installer_and_its_changes() {
        let i = inputs(Some("0.2.0"), Some("1.8.0"));
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert!(o.offer_installer);
        assert_eq!(line(&o, Part::Mixer).status, "Update available");
        assert!(line(&o, Part::Mixer).read_changes);
        assert_eq!(
            (o.flag.as_str(), o.flag_tone),
            ("Update available", Tone::Update)
        );
        assert_eq!(o.summary, "App 0.3.0 · Mixer 0.2.0 · Firmware 1.8.0");
    }

    #[test]
    fn a_blocked_update_explains_itself_and_offers_nothing() {
        let i = inputs(Some("0.2.0"), Some("1.9.0"));
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert!(!o.offer_installer);
        assert_eq!(line(&o, Part::Mixer).status, "Blocked");
        assert_eq!(line(&o, Part::Firmware).status, "Newer");
        assert!(o.details.iter().any(|d| d.contains("Update the app first")));
        assert_eq!(o.flag_tone, Tone::Warn);
    }

    #[test]
    fn too_old_mixer_software_is_the_most_urgent() {
        let mut i = inputs(Some("0.2.0"), Some("1.8.0"));
        i.device_protocol = Some(rc2fx_core::MIXER_PROTOCOL_VERSION - 1);
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let o = overview(&i, &current(), &state);
        assert!(o.offer_installer);
        assert_eq!(line(&o, Part::Mixer).status, "Too old");
        assert_eq!((o.flag.as_str(), o.flag_tone), ("Needs update", Tone::Bad));
    }

    #[test]
    fn a_disconnected_mixer_stays_unknown() {
        let mut i = inputs(None, None);
        i.device_protocol = None;
        i.newest_supported_firmware = None;
        let o = overview(&i, &AppStanding::Unknown, &MixerUpdateState::Unknown);
        assert_eq!(o.flag, "Not connected");
        assert_eq!(line(&o, Part::Mixer).installed, "Unknown");
        assert!(o.lines.iter().all(|l| !l.read_changes));
        assert!(o.details.is_empty());
    }

    #[test]
    fn an_unknown_published_version_stays_visible() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let o = overview(&i, &AppStanding::Unknown, &MixerUpdateState::Unknown);
        assert_eq!(line(&o, Part::Mixer).status, "Up to date");
        assert!(
            o.details
                .iter()
                .any(|d| d.contains("published mixer software is unknown"))
        );
    }

    #[test]
    fn newer_software_offers_nothing() {
        let i = inputs(Some("0.4.0"), Some("1.8.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Newer);
        assert_eq!(line(&o, Part::Mixer).status, "Newer");
        assert!(!line(&o, Part::Mixer).read_changes);
        assert!(!o.offer_installer);
    }

    #[test]
    fn updates_are_counted_and_a_checking_app_says_so() {
        let i = inputs(Some("0.2.0"), Some("1.7.0"));
        let state = MixerUpdateState::Bundled {
            version: "0.3.0".into(),
        };
        let update = AppStanding::Update {
            version: "0.4.0".into(),
        };
        let o = overview(&i, &update, &state);
        assert_eq!(o.flag, "3 updates available");
        assert!(o.lines.iter().all(|l| l.read_changes));
        let o = overview(&i, &AppStanding::Checking, &state);
        assert_eq!(line(&o, Part::App).status, "Checking…");
    }

    #[test]
    fn all_current_is_up_to_date() {
        let i = inputs(Some("0.3.0"), Some("1.8.0"));
        let o = overview(&i, &current(), &MixerUpdateState::Current);
        assert_eq!((o.flag.as_str(), o.flag_tone), ("Up to date", Tone::Quiet));
        assert!(o.lines.iter().all(|l| !l.read_changes));
        assert!(o.details.is_empty());
    }
}
