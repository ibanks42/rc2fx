//! The installer as one plan, independent of GPUI.
//!
//! RC2FX is installed by flashing firmware that contains it, so every
//! install or update is the same plan: save the mixer's settings, verify the
//! chosen mixer software, build the firmware, switch to USB update mode,
//! flash, and wait for RC2FX to answer.
//! Steps the mixer already satisfies are done. One start runs the rest in
//! order, waiting for the mixer where it has to and stopping at the first
//! error, which can be retried from that step.

use rc2fx_installer::{
    CANCELLED, DeviceModel, Prepared, Release, Reply, Request, Service, Status, UsbMixer,
    compare_versions, mixer_update::MixerOffer,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Backup,
    Mixer,
    Prepare,
    UsbMode,
    Flash,
    Start,
}

pub(crate) const STEPS: [Step; 6] = [
    Step::Backup,
    Step::Mixer,
    Step::Prepare,
    Step::UsbMode,
    Step::Flash,
    Step::Start,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Goal {
    /// No RC2FX answers at the mixer's address.
    Setup,
    /// RC2FX answers; flashing again updates or repairs it.
    Reinstall,
    /// Flash unchanged RØDE firmware, removing RC2FX.
    Stock,
}

/// The mixer software a run installs. Only something the user can see in the
/// menu: a signed offer, or the copy built into this app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MixerSource {
    Signed(MixerOffer),
    Bundled(String),
}

impl MixerSource {
    pub fn version(&self) -> &str {
        match self {
            MixerSource::Signed(offer) => &offer.metadata.version,
            MixerSource::Bundled(version) => version,
        }
    }
}

/// The check for supported firmware. It can need the network, so it runs
/// once and is repeated only when asked, never by the page's polling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirmwareCheck {
    Unchecked,
    Checking,
    /// It answered, or failed and said why in [`Model::error`].
    Done,
}

/// What the signed release check has found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Offers {
    Unchecked,
    Checking,
    Ready(Vec<MixerOffer>),
    Failed(String),
}

/// How a mixer software version relates to what the mixer runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixerChange {
    /// The mixer did not report a comparable version.
    Unknown,
    Upgrade,
    Same,
    Downgrade,
}

fn change(installed: Option<&str>, target: &str) -> MixerChange {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match installed.and_then(|i| compare_versions(i, target)) {
        None => MixerChange::Unknown,
        Some(Less) => MixerChange::Upgrade,
        Some(Equal) => MixerChange::Same,
        Some(Greater) => MixerChange::Downgrade,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StepState {
    Pending,
    Done,
    Running,
    Waiting(String),
    Failed(String),
}

/// A plan being carried out.
#[derive(Clone, Debug, Default)]
pub(crate) struct Run {
    pub index: usize,
    pub failed: Option<String>,
    pub waiting: Option<(String, Instant)>,
    /// The mixer this run is for, from USB.
    pub serial: String,
    pub device: DeviceModel,
    pub backed_up: bool,
    /// The mixer software the user reviewed; `None` for stock firmware. Held
    /// here so nothing that changes on the page can alter a run.
    pub mixer: Option<MixerSource>,
    /// [`Run::mixer`] was verified and selected for this run.
    pub selected: bool,
    pub prepared: bool,
    pub flashed: bool,
    /// USB update mode or the flash was requested: past this the mixer is
    /// not left as it was, so the run can no longer be cancelled.
    pub committed: bool,
    /// Cancelled while a request was in flight; stops when it returns.
    pub cancelling: bool,
}

/// How long after a flash the mixer counts as still updating.
const UPDATE_WINDOW: u64 = 20 * 60;
/// How long to wait for the mixer before reporting it.
const WAIT_LIMIT: Duration = Duration::from_secs(20 * 60);

/// Reading RC2FX's settings over the app's USB connection, for Save settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SettingsRead {
    #[default]
    Idle,
    /// The plan needs them; the app has not asked yet.
    Wanted,
    /// The app asked; waiting for the reply.
    Asked,
}

pub(crate) struct Model {
    pub device: DeviceModel,
    pub allow_duo: bool,
    /// Whether this computer may switch the mixer to update mode and flash
    /// it: false on Linux until the user grants administrator access.
    pub privileged: bool,
    pub status: Option<Status>,
    /// What RC2FX reports over the app's USB connection; None while the app
    /// is not connected to it.
    pub service: Option<Service>,
    settings_read: SettingsRead,
    pub releases: Vec<Release>,
    /// Qualified firmware only a newer app may install, newest first.
    pub needs_app: Vec<Release>,
    pub firmware_check: FirmwareCheck,
    pub choice: Option<(String, String)>,
    pub offers: Offers,
    /// The mixer software built into this app, if any. Chosen only by hand:
    /// it never stands in for a signed release that could not be reached.
    pub bundled: Option<String>,
    pub mixer: Option<MixerSource>,
    /// The user picked [`Model::mixer`], rather than it being the default.
    mixer_chosen: bool,
    /// The versions this mixer reports over the app's USB connection.
    pub installed: Option<String>,
    pub installed_firmware: Option<String>,
    /// Flash unchanged RØDE firmware rather than firmware with RC2FX.
    pub stock: bool,
    pub run: Option<Run>,
    pub busy: Option<Request>,
    /// Requests in flight, oldest first; `true` for background refreshes,
    /// which never disable the page. The runner answers in order.
    queue: VecDeque<bool>,
    pub error: String,
    /// The last run finished.
    pub finished: bool,
    /// The finished run, for showing what it did.
    pub last: Option<Run>,
    last_poll: Option<Instant>,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Model {
    pub fn new() -> Self {
        Model {
            device: DeviceModel::ProIi,
            allow_duo: false,
            privileged: true,
            status: None,
            service: None,
            settings_read: SettingsRead::Idle,
            releases: Vec::new(),
            needs_app: Vec::new(),
            firmware_check: FirmwareCheck::Unchecked,
            choice: None,
            offers: Offers::Unchecked,
            bundled: None,
            mixer: None,
            mixer_chosen: false,
            installed: None,
            installed_firmware: None,
            stock: false,
            run: None,
            busy: None,
            queue: VecDeque::new(),
            error: String::new(),
            finished: false,
            last: None,
            last_poll: None,
        }
    }

    /// Whether a Duo is on USB now: the experimental Duo opt-in is only
    /// offered for one.
    pub fn duo_connected(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.usb.iter().any(|u| u.model == DeviceModel::Duo))
    }

    pub fn set_allow_duo(&mut self, allow: bool) -> Option<Request> {
        if self.busy.is_some() || self.run.is_some() {
            return None;
        }
        self.allow_duo = allow;
        self.choice = None;
        self.releases.clear();
        self.needs_app.clear();
        self.firmware_check = FirmwareCheck::Unchecked;
        self.error.clear();
        self.finished = false;
        self.send(Request::AllowExperimentalDuo(allow))
    }

    fn send(&mut self, request: Request) -> Option<Request> {
        if self.busy.is_some() {
            return None;
        }
        self.busy = Some(request.clone());
        self.queue.push_back(false);
        Some(request)
    }

    /// A request the user does not wait on.
    fn quiet(&mut self, request: Request) -> Option<Request> {
        self.queue.push_back(true);
        Some(request)
    }

    fn polling(&self) -> bool {
        self.queue.iter().any(|background| *background)
    }

    /// Refresh what the mixer reports, in the background.
    fn refresh(&mut self) -> Option<Request> {
        if self.polling() {
            return None;
        }
        self.last_poll = Some(Instant::now());
        self.quiet(Request::Probe)
    }

    pub fn probe(&mut self) -> Option<Request> {
        self.last_poll = Some(Instant::now());
        self.recheck_firmware();
        self.send(Request::Probe)
    }

    /// The next status looks for firmware again if the last check found none.
    fn recheck_firmware(&mut self) {
        if self.releases.is_empty() && self.firmware_check == FirmwareCheck::Done {
            self.firmware_check = FirmwareCheck::Unchecked;
        }
    }

    fn check_firmware(&mut self) -> Option<Request> {
        if self.firmware_check != FirmwareCheck::Unchecked {
            return None;
        }
        self.firmware_check = FirmwareCheck::Checking;
        self.quiet(Request::Releases)
    }

    // ---- What the mixer reports --------------------------------------------

    pub fn usb(&self) -> &[UsbMixer] {
        self.status.as_ref().map_or(&[], |s| s.usb.as_slice())
    }

    /// Whether the app's USB 1 session is with the mixer the installer sees:
    /// the only one connected, in normal mode, with the same serial. RC2FX's
    /// readiness from any other mixer must not count for this one.
    pub fn is_session_mixer(&self, session_serial: Option<&str>) -> bool {
        self.usb_one(false)
            .is_some_and(|m| !m.serial.is_empty() && Some(m.serial.as_str()) == session_serial)
    }

    fn usb_one(&self, update_mode: bool) -> Option<&UsbMixer> {
        match self.usb() {
            [one] if one.update_mode == update_mode => Some(one),
            _ => None,
        }
    }

    pub fn in_update_mode(&self) -> bool {
        self.usb().iter().any(|m| m.update_mode)
    }

    pub fn service(&self) -> Option<&Service> {
        self.service.as_ref()
    }

    /// What the app's USB connection says about RC2FX; None while it is not
    /// connected.
    pub fn set_service(&mut self, service: Option<Service>) {
        self.service = service;
    }

    /// The app should read RC2FX's settings now; true once per request.
    pub fn take_settings_request(&mut self) -> bool {
        let wanted = self.settings_read == SettingsRead::Wanted;
        if wanted {
            self.settings_read = SettingsRead::Asked;
        }
        wanted
    }

    /// RC2FX's settings as the app read them: keep them, or stop the run.
    pub fn settings_read(&mut self, result: Result<String, String>) -> Option<Request> {
        if self.settings_read != SettingsRead::Asked {
            return None;
        }
        self.settings_read = SettingsRead::Idle;
        let run = self.run.as_mut()?;
        run.waiting = None;
        let serial = run.serial.clone();
        match result {
            Ok(settings) => self.send(Request::SaveSettings { serial, settings }),
            Err(e) => self.fail(format!("Couldn't read RC2FX's settings: {e}")),
        }
    }

    /// RC2FX answers over the app's USB connection.
    pub fn answering(&self) -> bool {
        matches!(
            self.service(),
            Some(Service::Ready | Service::Starting { .. })
        )
    }

    pub fn prepared(&self) -> Option<&Prepared> {
        let s = self.status.as_ref()?;
        s.saved.prepared.as_ref().filter(|p| {
            s.prepared_present
                // Firmware prepared with an SSH key is never reused.
                && p.key_sha.is_empty()
                && p.key.is_empty()
                && p.host_key.is_empty()
                && p.model.unwrap_or_default() == self.device
                && (self.device != DeviceModel::Duo || self.allow_duo)
        })
    }

    /// The last USB command was a flash in the last few minutes, so the mixer
    /// may still be updating.
    fn recently_flashed(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.saved.last_command())
            .is_some_and(|c| {
                c.command == "flash" && now_unix().saturating_sub(c.at_unix) < UPDATE_WINDOW
            })
    }

    fn flashed(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.flashed)
    }

    pub fn goal(&self) -> Goal {
        if self.stock {
            Goal::Stock
        } else if self.answering() && !self.flashed() {
            Goal::Reinstall
        } else {
            Goal::Setup
        }
    }

    fn satisfied(&self, step: Step) -> bool {
        let run = self.run.as_ref();
        let flashed = self.flashed();
        match step {
            Step::Backup => flashed || run.is_some_and(|r| r.backed_up) || !self.answering(),
            // Stock firmware carries no mixer software.
            Step::Mixer => flashed || self.stock || run.is_some_and(|r| r.selected),
            Step::Prepare => flashed || run.is_some_and(|r| r.prepared),
            Step::UsbMode => flashed || self.in_update_mode(),
            Step::Flash => flashed,
            // Stock firmware is done once the mixer is back in normal mode.
            Step::Start if self.stock => flashed && self.usb_one(false).is_some(),
            Step::Start => {
                flashed && !self.in_update_mode() && matches!(self.service(), Some(Service::Ready))
            }
        }
    }

    pub fn state(&self, step: Step) -> StepState {
        if let Some(run) = &self.run {
            let position = STEPS.iter().position(|s| *s == step);
            if position == Some(run.index) {
                if let Some(e) = &run.failed {
                    return StepState::Failed(e.clone());
                }
                if let Some((why, _)) = &run.waiting {
                    return StepState::Waiting(why.clone());
                }
                // Between an action and the refresh that follows it.
                if self.busy.is_some() || self.polling() {
                    return StepState::Running;
                }
            }
            if position.is_some_and(|p| p < run.index) {
                return StepState::Done;
            }
        }
        if self.satisfied(step) {
            StepState::Done
        } else {
            StepState::Pending
        }
    }

    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.failed.is_none())
    }

    /// Whether starting changes the mixer's firmware, which needs one
    /// confirmation first.
    pub fn needs_confirmation(&self) -> bool {
        !self.flashed()
    }

    pub fn can_start(&self) -> bool {
        self.privileged
            && self.busy.is_none()
            && !self.running()
            && self.choice.as_ref().is_some_and(|(channel, version)| {
                self.releases
                    .iter()
                    .any(|r| r.supported && &r.channel == channel && &r.version == version)
            })
            && (self.stock
                || self
                    .mixer
                    .as_ref()
                    .is_some_and(|m| self.mixer_options().contains(m)))
            && self
                .status
                .as_ref()
                .is_some_and(|s| s.usb_error.is_none() && s.usb.len() <= 1)
            && (self.device != DeviceModel::Duo || self.allow_duo)
    }

    // ---- Events -------------------------------------------------------------

    pub fn choose(&mut self, index: usize) {
        if self.running() || self.busy.is_some() {
            return;
        }
        // Both modes are restricted to this build's qualified catalog.
        if let Some(r) = self.releases.get(index).filter(|r| r.supported) {
            self.choice = Some((r.channel.clone(), r.version.clone()));
        }
        self.settle_mixer();
    }

    // ---- Mixer software -----------------------------------------------------

    /// The catalog profile of the chosen firmware.
    fn firmware_profile(&self) -> Option<&str> {
        let (channel, version) = self.choice.as_ref()?;
        self.releases
            .iter()
            .find(|r| &r.channel == channel && &r.version == version)
            .map(|r| r.profile.as_str())
            .filter(|profile| !profile.is_empty())
    }

    /// What can go with the chosen firmware: signed offers for this app,
    /// newest first, then the copy built into this app.
    pub fn mixer_options(&self) -> Vec<MixerSource> {
        let mut options = Vec::new();
        if let (Offers::Ready(offers), Some(profile)) = (&self.offers, self.firmware_profile()) {
            options.extend(
                offers
                    .iter()
                    .filter(|offer| crate::versions::offer_fits(offer, profile))
                    .cloned()
                    .map(MixerSource::Signed),
            );
        }
        options.extend(self.bundled.clone().map(MixerSource::Bundled));
        options
    }

    /// The menu: each option, its name, and whether it is the choice.
    pub fn mixer_menu(&self) -> Vec<(MixerSource, String, bool)> {
        self.mixer_options()
            .into_iter()
            .map(|source| {
                let mut name = match &source {
                    MixerSource::Signed(_) => format!("{} · signed download", source.version()),
                    MixerSource::Bundled(_) => {
                        format!("{} · included with this app", source.version())
                    }
                };
                if self.mixer_change(&source) == MixerChange::Downgrade {
                    name.push_str(" · older than installed");
                }
                let chosen = self.mixer.as_ref() == Some(&source);
                (source, name, chosen)
            })
            .collect()
    }

    pub fn mixer_change(&self, source: &MixerSource) -> MixerChange {
        change(self.installed.as_deref(), source.version())
    }

    /// Keep a choice that still applies. Otherwise use the newest signed
    /// offer that doesn't go backwards, or nothing, which leaves the choice to
    /// the user: a downgrade or the bundled copy is never picked for them.
    fn settle_mixer(&mut self) {
        if self.run.is_some() || matches!(self.offers, Offers::Unchecked | Offers::Checking) {
            return;
        }
        let options = self.mixer_options();
        if self.mixer_chosen && self.mixer.as_ref().is_some_and(|m| options.contains(m)) {
            return;
        }
        self.mixer_chosen = false;
        self.mixer = options.into_iter().find(|source| {
            matches!(source, MixerSource::Signed(_))
                && self.mixer_change(source) != MixerChange::Downgrade
        });
    }

    pub fn choose_mixer(&mut self, source: MixerSource) {
        if self.run.is_none() && self.busy.is_none() && self.mixer_options().contains(&source) {
            self.mixer = Some(source);
            self.mixer_chosen = true;
        }
    }

    /// The versions this mixer reports; `None` for what it did not report.
    pub fn set_installed(&mut self, mixer: Option<String>, firmware: Option<String>) {
        if self.installed != mixer || self.installed_firmware != firmware {
            self.installed = mixer;
            self.installed_firmware = firmware;
            self.settle_mixer();
        }
    }

    fn check_mixers(&mut self) -> Option<Request> {
        if self.offers != Offers::Unchecked {
            return None;
        }
        self.offers = Offers::Checking;
        self.quiet(Request::MixerReleases)
    }

    /// Check again after a failed or cancelled check.
    pub fn recheck_mixers(&mut self) -> Option<Request> {
        if !matches!(self.offers, Offers::Failed(_)) {
            return None;
        }
        self.offers = Offers::Unchecked;
        self.check_mixers()
    }

    /// Firmware that is qualified but held back until the app is updated.
    pub fn firmware_note(&self) -> Option<String> {
        let release = self.needs_app.first()?;
        Some(format!(
            "RØDE {} needs RC2FX {} or newer. Update the app first.",
            release.version, release.minimum_app_version
        ))
    }

    /// Why the page has no mixer software to offer, or what the user should
    /// know about the choices it has.
    pub fn mixer_note(&self) -> Option<String> {
        let fallback = self
            .bundled
            .as_ref()
            .map(|version| format!(" You can choose {version}, included with this app, instead."))
            .unwrap_or_default();
        match &self.offers {
            Offers::Unchecked | Offers::Checking => None,
            Offers::Failed(e) if e == CANCELLED => Some(format!(
                "The check for mixer software was cancelled.{fallback}"
            )),
            Offers::Failed(e) => Some(format!("Couldn't check for mixer software. {e}{fallback}")),
            Offers::Ready(offers) if self.mixer_options().is_empty() => {
                let version = self.choice.as_ref().map_or("", |(_, v)| v.as_str());
                Some(match crate::versions::needs_newer_app(offers, None) {
                    Some((mixer, minimum)) => format!(
                        "Mixer software {mixer} needs RC2FX {minimum} or newer. Update the app first."
                    ),
                    None => {
                        format!("No signed mixer software works with RØDE {version} and this app.")
                    }
                })
            }
            Offers::Ready(_) if self.mixer.is_none() => Some(match &self.installed {
                Some(installed) => format!(
                    "Nothing newer than this mixer's {installed} is available. Choosing a version below replaces it with an older one."
                ),
                None => "Choose the mixer software to install.".into(),
            }),
            Offers::Ready(offers) => {
                let options = self.mixer_options();
                let newest = options
                    .iter()
                    .map(MixerSource::version)
                    .max_by(|a, b| compare_versions(a, b).unwrap_or(std::cmp::Ordering::Equal));
                crate::versions::needs_newer_app(offers, newest).map(|(mixer, minimum)| {
                    format!(
                        "Mixer software {mixer} needs RC2FX {minimum} or newer. Update the app first to get it."
                    )
                })
            }
        }
    }

    /// What the confirmation must say before this replaces something newer,
    /// or something the mixer didn't report.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(source) = self.mixer.as_ref().filter(|_| !self.stock) {
            let version = source.version();
            match (self.mixer_change(source), &self.installed) {
                (MixerChange::Downgrade, Some(installed)) => warnings.push(format!(
                    "This mixer has RC2FX mixer software {installed}. This replaces it with the older {version}."
                )),
                (MixerChange::Unknown, _) => warnings.push(format!(
                    "RC2FX can't read which mixer software this mixer has. If it already has RC2FX, {version} could be older than what it replaces."
                )),
                _ => {}
            }
        }
        if let Some(((_, version), installed)) =
            self.choice.as_ref().zip(self.installed_firmware.as_ref())
            && change(Some(installed), version) == MixerChange::Downgrade
        {
            warnings.push(format!(
                "This mixer has RØDE firmware {installed}. This replaces it with the older {version}."
            ));
        }
        warnings
    }

    /// Choose between firmware with RC2FX and unchanged RØDE firmware,
    /// keeping the version choice valid for the mode.
    pub fn set_stock(&mut self, stock: bool) {
        if self.running() || self.busy.is_some() {
            return;
        }
        self.stock = stock;
        self.run = None;
        self.finished = false;
        let valid = self.choice.as_ref().is_some_and(|(channel, version)| {
            self.releases
                .iter()
                .any(|r| r.supported && &r.channel == channel && &r.version == version)
        });
        if !valid {
            self.choice = default_choice(&self.releases);
        }
        self.settle_mixer();
    }

    /// Both modes offer only qualified catalog releases.
    pub fn releases_for(&self, _stock: bool) -> impl Iterator<Item = (usize, &Release)> {
        self.releases
            .iter()
            .enumerate()
            .filter(|(_, r)| r.supported)
    }

    /// The release a mode would use: the current choice when it fits the
    /// mode, as [`Model::set_stock`] keeps it, else the mode's default.
    pub fn choice_for(&self, _stock: bool) -> Option<(String, String)> {
        let fits = self.choice.as_ref().is_some_and(|(channel, version)| {
            self.releases
                .iter()
                .any(|r| r.supported && &r.channel == channel && &r.version == version)
        });
        if fits {
            self.choice.clone()
        } else {
            default_choice(&self.releases)
        }
    }

    /// Pick the mode and release together, as the firmware menu does.
    pub fn select(&mut self, stock: bool, index: usize) {
        self.set_stock(stock);
        self.choose(index);
    }

    pub fn start(&mut self) -> Option<Request> {
        if !self.can_start() {
            return None;
        }
        self.error.clear();
        self.finished = false;
        self.last = None;
        self.settings_read = SettingsRead::Idle;
        self.run = Some(Run {
            device: self.device,
            serial: self
                .usb()
                .first()
                .map(|m| m.serial.clone())
                .unwrap_or_default(),
            mixer: self.mixer.clone().filter(|_| !self.stock),
            ..Default::default()
        });
        self.advance()
    }

    pub fn retry(&mut self) -> Option<Request> {
        if !self.privileged {
            return None;
        }
        let run = self.run.as_mut()?;
        if self.busy.is_some() {
            return None;
        }
        run.failed = None;
        run.waiting = None;
        self.recheck_firmware();
        // Act on what the mixer reports now.
        self.refresh()
    }

    pub fn stop(&mut self) {
        if self.busy.is_none() {
            self.run = None;
            self.settings_read = SettingsRead::Idle;
            // A failed selection may mean the offers changed: look again, so
            // the user reviews what is published now.
            if matches!(self.offers, Offers::Ready(_)) {
                self.offers = Offers::Unchecked;
            }
        }
    }

    /// The run has not touched the mixer's mode or firmware yet.
    pub fn can_cancel(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| !r.committed && !r.flashed && !r.cancelling)
    }

    /// Back to choosing firmware after a finished run.
    pub fn start_over(&mut self) {
        if self.busy.is_none() && self.run.is_none() {
            self.finished = false;
            self.last = None;
        }
    }

    /// The firmware is flashed and the run only waits for the mixer to
    /// come back.
    pub fn waiting_after_flash(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| self.flashed() && r.waiting.is_some() && r.failed.is_none())
    }

    pub fn cancelling(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.cancelling)
    }

    /// Cancel before the point of no return. A request in flight (saving
    /// settings or preparing firmware) is asked to stop and the run ends
    /// when it returns. Returns whether a request is in flight.
    pub fn cancel(&mut self) -> bool {
        if !self.can_cancel() {
            return false;
        }
        if self.busy.is_some() {
            if let Some(run) = self.run.as_mut() {
                run.cancelling = true;
            }
            true
        } else {
            self.run = None;
            self.settings_read = SettingsRead::Idle;
            false
        }
    }

    fn fail(&mut self, message: String) -> Option<Request> {
        self.settings_read = SettingsRead::Idle;
        if let Some(run) = self.run.as_mut() {
            run.failed = Some(message);
            run.waiting = None;
        }
        None
    }

    fn wait(&mut self, why: &str) -> Option<Request> {
        let run = self.run.as_mut()?;
        let since = match &run.waiting {
            Some((_, since)) => *since,
            None => Instant::now(),
        };
        if since.elapsed() > WAIT_LIMIT {
            return self.fail(format!(
                "{why} The mixer did not get there in time; check it and retry."
            ));
        }
        run.waiting = Some((why.to_owned(), since));
        None
    }

    /// Carry out the next unsatisfied step, or wait for the mixer.
    fn advance(&mut self) -> Option<Request> {
        loop {
            let run = self.run.as_mut()?;
            if run.failed.is_some() || self.busy.is_some() {
                return None;
            }
            if self.status.as_ref().is_some_and(|s| {
                s.usb.len() > 1
                    || s.usb.iter().any(|m| {
                        m.model != run.device || (!run.serial.is_empty() && m.serial != run.serial)
                    })
            }) {
                return self.fail("The connected mixer changed. Reconnect the original model and serial; nothing else will be flashed.".into());
            }
            if run.device == DeviceModel::Duo && !self.allow_duo {
                return self.fail("Experimental Duo flashing is disabled.".into());
            }
            if run.serial.is_empty()
                && let Some(m) = self.status.as_ref().and_then(|s| s.usb.first())
            {
                run.serial = m.serial.clone();
            }
            let Some(&step) = STEPS.get(run.index) else {
                self.finished = true;
                self.last = self.run.take();
                return None;
            };
            if self.satisfied(step) {
                let run = self.run.as_mut()?;
                run.index += 1;
                run.waiting = None;
                continue;
            }
            let serial = self
                .run
                .as_ref()
                .map(|r| r.serial.clone())
                .unwrap_or_default();
            return match step {
                Step::Backup if serial.is_empty() => {
                    self.wait("Connect the mixer so its settings can be matched to it.")
                }
                Step::Backup => {
                    if self.settings_read == SettingsRead::Idle {
                        self.settings_read = SettingsRead::Wanted;
                    }
                    self.wait("Reading RC2FX's settings over USB.")
                }
                Step::Mixer => {
                    let mixer = self.run.as_ref().and_then(|r| r.mixer.clone());
                    match (mixer, self.firmware_profile()) {
                        (Some(MixerSource::Signed(offer)), Some(profile)) => {
                            let firmware_profile = profile.to_owned();
                            self.send(Request::SelectMixer {
                                offer,
                                firmware_profile,
                            })
                        }
                        (Some(MixerSource::Bundled(_)), _) => {
                            self.send(Request::SelectBundledMixer)
                        }
                        _ => self.fail("Choose the mixer software to install.".into()),
                    }
                }
                Step::Prepare => match self.choice.clone() {
                    Some((channel, version)) => self.send(Request::Prepare {
                        channel,
                        version,
                        serial,
                        stock: self.stock,
                        // SSH is managed over USB 1 from Settings, never
                        // built into firmware.
                        ssh: false,
                    }),
                    None => self.fail("Choose a firmware version.".into()),
                },
                Step::UsbMode => match self.usb() {
                    [] => self.wait("Connect the mixer to this computer."),
                    [one] if !one.update_mode => {
                        let serial = one.serial.clone();
                        if self.run.as_ref().is_some_and(|r| r.waiting.is_some())
                            && self.usb_mode_sent()
                        {
                            return self.wait("Waiting for the mixer to switch to USB update mode.");
                        }
                        if let Some(run) = self.run.as_mut() {
                            run.committed = true;
                        }
                        self.send(Request::EnterUsbMode { serial })
                    }
                    _ => self.fail(
                        "More than one RØDECaster is connected. Connect only one.".into(),
                    ),
                },
                Step::Flash => match self.usb_one(true) {
                    Some(m) if self.status.as_ref().is_some_and(|s| s.update_drive) => {
                        let serial = m.serial.clone();
                        if let Some(run) = self.run.as_mut() {
                            run.committed = true;
                        }
                        self.send(Request::Flash { serial })
                    }
                    Some(_) => self
                        .wait("Waiting for the mixer's update drive to appear on this computer."),
                    None => self.wait("Waiting for the mixer to enter USB update mode."),
                },
                Step::Start if self.stock => match self.usb() {
                    [] => self.wait(
                        "The mixer is installing the firmware and restarting. Keep it connected to see when it is back.",
                    ),
                    _ => self.wait("The mixer is installing the firmware and restarting."),
                },
                Step::Start => match self.service() {
                    Some(Service::Starting { detail }) => {
                        let why = format!("RC2FX is starting: {detail}");
                        self.wait(&why)
                    }
                    _ => self.wait(
                        "The mixer is updating and will restart a couple of times. Leave it on; this takes a few minutes.",
                    ),
                },
            };
        }
    }

    /// Whether the USB-mode command was sent in the last two minutes.
    fn usb_mode_sent(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.saved.last_command())
            .is_some_and(|c| c.command == "usb-mode" && now_unix().saturating_sub(c.at_unix) < 120)
    }

    /// Polls: often while a run waits for the mixer, slowly otherwise so the
    /// page follows USB and network changes.
    pub fn tick(&mut self, now: Instant) -> Option<Request> {
        if self.busy.is_some()
            || self.polling()
            || self.run.as_ref().is_some_and(|r| r.failed.is_some())
        {
            return None;
        }
        let interval = match self.run.as_ref().and_then(|r| r.waiting.as_ref()) {
            Some(_) => Duration::from_secs(3),
            None if self.run.is_some() => return None,
            None => Duration::from_secs(10),
        };
        if self
            .last_poll
            .is_some_and(|t| now.duration_since(t) < interval)
        {
            return None;
        }
        self.refresh()
    }

    /// Apply a finished request; returns the next request, if any.
    pub fn finish(&mut self, request: &Request, result: Result<Reply, String>) -> Option<Request> {
        let background = self.queue.pop_front().unwrap_or(false);
        if !background {
            self.busy = None;
        }
        // A cancelled run ends when its request returns, whatever it says.
        if !background && self.cancelling() {
            self.run = None;
            return self.refresh();
        }
        let reply = match result {
            Ok(reply) => reply,
            Err(e) => {
                return match request {
                    Request::MixerReleases => {
                        self.offers = Offers::Failed(e);
                        self.settle_mixer();
                        None
                    }
                    // A failed poll while waiting is not the end of the run.
                    Request::Probe if self.run.is_some() => self.advance(),
                    Request::Probe => {
                        self.error = e;
                        None
                    }
                    Request::Releases => {
                        self.firmware_check = FirmwareCheck::Done;
                        self.error = if e == CANCELLED {
                            "The check for supported firmware was cancelled.".into()
                        } else {
                            format!("Couldn't check for supported firmware. {e}")
                        };
                        None
                    }
                    _ => {
                        self.fail(e);
                        // Show what the mixer reports after the failure.
                        self.refresh()
                    }
                };
            }
        };
        match reply {
            Reply::Status(status) => {
                let first = self.status.is_none();
                if self.run.is_none()
                    && let [one] = status.usb.as_slice()
                    && one.model != self.device
                {
                    self.device = one.model;
                    self.choice = None;
                    self.releases.clear();
                    self.needs_app.clear();
                    self.firmware_check = FirmwareCheck::Unchecked;
                    self.error.clear();
                }
                self.status = Some(*status);
                if self.choice.is_none()
                    && let Some(p) = self.prepared()
                {
                    self.choice = Some((p.channel.clone(), p.version.clone()));
                }
                // Opened during an update: wait for what was flashed. Only
                // on opening, so a finished update never comes back.
                if first
                    && self.run.is_none()
                    && self.recently_flashed()
                    && let Some(p) = self.prepared()
                {
                    self.stock = p.stock;
                    self.run = Some(Run {
                        device: self.device,
                        backed_up: true,
                        prepared: true,
                        flashed: true,
                        ..Default::default()
                    });
                }
                if self.run.is_some() {
                    let next = self.advance();
                    // Opened during an update: RC2FX's readiness is judged
                    // against the catalog, which this process may not have.
                    return next.or_else(|| {
                        if self.flashed() {
                            self.check_firmware()
                        } else {
                            None
                        }
                    });
                }
                if !self.releases.is_empty()
                    || self.status.as_ref().is_some_and(|s| s.usb_error.is_some())
                {
                    return self.check_mixers();
                }
                return self.check_firmware();
            }
            Reply::Releases(items) => {
                self.firmware_check = FirmwareCheck::Done;
                self.error.clear();
                let device = self.device.id();
                let (items, unavailable): (Vec<_>, Vec<_>) = items
                    .into_iter()
                    .filter(|r| r.model.is_empty() || r.model == device)
                    .partition(|r| r.supported);
                self.needs_app = unavailable
                    .into_iter()
                    .filter(|r| crate::versions::app_too_old(&r.minimum_app_version))
                    .collect();
                if !self.choice.as_ref().is_some_and(|(channel, version)| {
                    items
                        .iter()
                        .any(|r| &r.channel == channel && &r.version == version)
                }) {
                    self.choice = default_choice(&items);
                }
                self.releases = items;
                self.settle_mixer();
                return self.check_mixers();
            }
            Reply::MixerReleases(offers) => {
                self.offers = Offers::Ready(offers);
                self.settle_mixer();
                return None;
            }
            Reply::MixerSelected(identity) => {
                if let Some(run) = self.run.as_mut() {
                    let wanted = run.mixer.as_ref().map(MixerSource::version);
                    if wanted == Some(identity.release.as_str()) {
                        run.selected = true;
                    } else {
                        let wanted = wanted.unwrap_or_default().to_owned();
                        self.fail(format!(
                            "The verified mixer software is {} rather than the {wanted} you chose. Nothing was flashed.",
                            identity.release
                        ));
                    }
                }
            }
            Reply::Backup(_) => {
                if let Some(run) = self.run.as_mut() {
                    run.backed_up = true;
                }
            }
            Reply::Prepared(_) => {
                if let Some(run) = self.run.as_mut() {
                    run.prepared = true;
                }
            }
            Reply::Done if matches!(request, Request::Flash { .. }) => {
                if let Some(run) = self.run.as_mut() {
                    run.flashed = true;
                }
            }
            Reply::Done if matches!(request, Request::EnterUsbMode { .. }) => {
                if let Some(run) = self.run.as_mut() {
                    run.waiting = Some((
                        "Waiting for the mixer to switch to USB update mode.".into(),
                        Instant::now(),
                    ));
                }
            }
            _ => {}
        }
        self.refresh()
    }
}

/// Stable when RC2FX supports it, else the first supported release.
fn default_choice(releases: &[Release]) -> Option<(String, String)> {
    releases
        .iter()
        .find(|r| r.supported && r.channel == "stable")
        .or_else(|| releases.iter().find(|r| r.supported))
        .map(|r| (r.channel.clone(), r.version.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_installer::mixer_update::MixerTarget;
    use rc2fx_installer::{CommandRecord, Identity, Saved};

    const PROFILE: &str = "rodecaster-pro-ii-1.7.6";

    fn offer_for(version: &str, profile: &str) -> MixerOffer {
        MixerOffer {
            target: format!("mixer-{version}.payload"),
            sha256: "00".repeat(32),
            length: 1,
            metadata: MixerTarget {
                version: version.into(),
                payload_format: 1,
                protocol_version: rc2fx_core::MIXER_PROTOCOL_VERSION,
                minimum_app_version: "0.0.1".into(),
                firmware_profiles: vec![profile.into()],
            },
        }
    }

    fn offer(version: &str) -> MixerOffer {
        offer_for(version, PROFILE)
    }

    /// The engine verified the selection; the plan goes on to the next step.
    fn selected(m: &mut Model, request: Option<Request>) -> Option<Request> {
        assert!(matches!(
            request,
            Some(Request::SelectMixer { .. } | Request::SelectBundledMixer)
        ));
        let release = m.run.as_ref().unwrap().mixer.as_ref().unwrap().version();
        let identity = Identity {
            release: release.to_owned(),
            ..Default::default()
        };
        let refresh = answer(m, request, Reply::MixerSelected(identity));
        let seen = Seen {
            status: m.status.clone().unwrap(),
            service: m.service.clone(),
        };
        report(m, refresh, seen)
    }

    /// What the installer sees over USB, and RC2FX as the app's connection
    /// sees it.
    struct Seen {
        status: Status,
        service: Option<Service>,
    }

    fn status(usb: &[(&str, bool)], service: Option<Service>, prepared: bool, drive: bool) -> Seen {
        let status = Status {
            usb: usb
                .iter()
                .map(|(serial, update_mode)| UsbMixer {
                    model: DeviceModel::ProIi,
                    serial: (*serial).into(),
                    update_mode: *update_mode,
                })
                .collect(),
            usb_error: None,
            saved: Saved {
                prepared: prepared.then(|| Prepared {
                    version: "1.7.6".into(),
                    channel: "stable".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            prepared_present: prepared,
            update_drive: drive,
        };
        Seen { status, service }
    }

    #[test]
    fn duo_preference_gates_preparation_and_model_changes_drop_cached_choices() {
        let mut m = Model::new();
        let mut seen = status(&[("DUO", false)], None, true, false);
        seen.status.usb[0].model = DeviceModel::Duo;
        let probe = m.probe().unwrap();
        let releases = m
            .finish(&probe, Ok(Reply::Status(Box::new(seen.status.clone()))))
            .unwrap();
        let check = m.finish(&releases, Ok(Reply::Releases(vec![]))).unwrap();
        m.finish(&check, Ok(Reply::MixerReleases(vec![offer("1.0.0")])));
        assert_eq!(m.device, DeviceModel::Duo);
        assert!(
            m.prepared().is_none(),
            "a Pro II cached archive is not a Duo choice"
        );
        m.choice = Some(("stable".into(), "1.7.6".into()));
        assert!(!m.can_start());
        assert!(m.start().is_none());

        let enable = m.set_allow_duo(true).unwrap();
        assert_eq!(enable, Request::AllowExperimentalDuo(true));
        assert!(
            m.set_allow_duo(false).is_none(),
            "don't mutate in-flight policy"
        );
        let releases = m
            .finish(&enable, Ok(Reply::Status(Box::new(seen.status))))
            .unwrap();
        m.finish(
            &releases,
            Ok(Reply::Releases(vec![Release {
                model: DeviceModel::Duo.id().into(),
                channel: "stable".into(),
                version: "1.7.6".into(),
                profile: PROFILE.into(),
                supported: true,
                ..Default::default()
            }])),
        );
        assert!(m.needs_confirmation());
        let select = m.start();
        let request = selected(&mut m, select).unwrap();
        assert!(matches!(request, Request::Prepare { .. }));
        assert_eq!(m.run.as_ref().unwrap().device, DeviceModel::Duo);
        let probe = m
            .finish(&request, Ok(Reply::Prepared(Prepared::default())))
            .unwrap();
        let changed = status(&[("DUO", false)], None, false, false);
        m.finish(&probe, Ok(Reply::Status(Box::new(changed.status))));
        assert!(
            m.run
                .as_ref()
                .unwrap()
                .failed
                .as_deref()
                .unwrap()
                .contains("mixer changed")
        );
    }

    #[test]
    fn flashing_waits_for_administrator_access() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        assert!(m.can_start());
        m.privileged = false;
        assert!(!m.can_start());
        assert!(m.start().is_none());
        m.run = Some(Run {
            failed: Some("Grant administrator access first.".into()),
            ..Default::default()
        });
        assert!(m.retry().is_none(), "a failed run waits too");
        m.privileged = true;
        assert!(m.retry().is_some());
    }

    #[test]
    fn the_duo_opt_in_is_offered_only_while_a_duo_is_connected() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        assert!(!m.duo_connected(), "a Pro II");
        m.status.as_mut().unwrap().usb[0].model = DeviceModel::Duo;
        assert!(m.duo_connected());
        m.status.as_mut().unwrap().usb.clear();
        m.device = DeviceModel::Duo;
        assert!(!m.duo_connected(), "a Duo seen earlier but unplugged now");
    }

    #[test]
    fn turning_off_duo_hides_its_preparation_and_blocks_start() {
        let mut s = status(&[("DUO", true)], None, true, true);
        s.status.usb[0].model = DeviceModel::Duo;
        s.status.saved.prepared.as_mut().unwrap().model = Some(DeviceModel::Duo);
        let mut m = model(s);
        m.device = DeviceModel::Duo;
        m.allow_duo = true;
        assert!(m.prepared().is_some());
        let disable = m.set_allow_duo(false).unwrap();
        assert_eq!(disable, Request::AllowExperimentalDuo(false));
        assert!(m.prepared().is_none());
        assert!(!m.can_start());
        assert!(m.choice.is_none());
    }

    /// The app is not connected to RC2FX.
    fn absent() -> Option<Service> {
        None
    }

    fn model(s: Seen) -> Model {
        let mut m = Model::new();
        m.status = Some(s.status);
        m.set_service(s.service);
        m.releases = vec![Release {
            channel: "stable".into(),
            version: "1.7.6".into(),
            profile: PROFILE.into(),
            supported: true,
            ..Default::default()
        }];
        m.choice = Some(("stable".into(), "1.7.6".into()));
        m.firmware_check = FirmwareCheck::Done;
        m.offers = Offers::Ready(vec![offer("1.0.0")]);
        m.settle_mixer();
        m
    }

    fn answer(m: &mut Model, request: Option<Request>, reply: Reply) -> Option<Request> {
        m.finish(&request.expect("a request"), Ok(reply))
    }

    fn report(m: &mut Model, request: Option<Request>, s: Seen) -> Option<Request> {
        m.set_service(s.service);
        answer(m, request, Reply::Status(Box::new(s.status)))
    }

    /// The app reads the settings the plan asked for.
    fn read_settings(m: &mut Model) -> Option<Request> {
        assert!(m.take_settings_request(), "the plan asks for the settings");
        m.settings_read(Ok(r#"{"config":{}}"#.into()))
    }

    #[test]
    fn goal_follows_whether_rc2fx_answers() {
        assert_eq!(
            model(status(&[], Some(Service::Ready), false, false)).goal(),
            Goal::Reinstall
        );
        assert_eq!(
            model(status(&[], absent(), false, false)).goal(),
            Goal::Setup
        );
        assert_eq!(model(status(&[], None, false, false)).goal(), Goal::Setup);
    }

    #[test]
    fn a_reinstall_runs_end_to_end_after_one_start() {
        let mut m = model(status(
            &[("GV1", false)],
            Some(Service::Ready),
            false,
            false,
        ));
        assert!(m.needs_confirmation());
        assert!(m.start().is_none(), "the app reads the settings first");
        assert!(matches!(m.state(Step::Backup), StepState::Waiting(_)));
        let backup = read_settings(&mut m);
        assert_eq!(
            backup,
            Some(Request::SaveSettings {
                serial: "GV1".into(),
                settings: r#"{"config":{}}"#.into(),
            })
        );
        let refresh = answer(&mut m, backup, Reply::Backup(Default::default()));
        let select = report(
            &mut m,
            refresh,
            status(&[("GV1", false)], Some(Service::Ready), false, false),
        );
        assert_eq!(
            select,
            Some(Request::SelectMixer {
                offer: offer("1.0.0"),
                firmware_profile: PROFILE.into(),
            }),
            "the exact offer, before anything is built from it"
        );
        let prepare = selected(&mut m, select);
        assert_eq!(
            prepare,
            Some(Request::Prepare {
                channel: "stable".into(),
                version: "1.7.6".into(),
                serial: "GV1".into(),
                stock: false,
                ssh: false,
            }),
            "no SSH key unless the developer option is on"
        );
        let refresh = answer(&mut m, prepare, Reply::Prepared(Prepared::default()));
        let usb = report(
            &mut m,
            refresh,
            status(&[("GV1", false)], Some(Service::Ready), true, false),
        );
        assert_eq!(
            usb,
            Some(Request::EnterUsbMode {
                serial: "GV1".into()
            })
        );
        let refresh = answer(&mut m, usb, Reply::Done);
        // Still normal mode a moment later: wait, never resend.
        let mut s = status(&[("GV1", false)], Some(Service::Ready), true, false);
        s.status.saved.commands.push(CommandRecord {
            command: "usb-mode".into(),
            at_unix: now_unix(),
            acknowledged: true,
            ..Default::default()
        });
        assert!(report(&mut m, refresh, s).is_none());
        assert!(matches!(m.state(Step::UsbMode), StepState::Waiting(_)));
        // Update mode, drive not yet visible: wait; then flash.
        let poll = m.probe();
        assert!(
            report(
                &mut m,
                poll,
                status(&[("GV1", true)], absent(), true, false)
            )
            .is_none()
        );
        assert!(matches!(m.state(Step::Flash), StepState::Waiting(_)));
        let poll = m.probe();
        let flash = report(&mut m, poll, status(&[("GV1", true)], absent(), true, true));
        assert_eq!(
            flash,
            Some(Request::Flash {
                serial: "GV1".into()
            })
        );
        let refresh = answer(&mut m, flash, Reply::Done);
        // Updating and restarting: nothing answers, then RC2FX starts.
        assert!(report(&mut m, refresh, status(&[], absent(), true, false)).is_none());
        assert!(matches!(m.state(Step::Start), StepState::Waiting(_)));
        let poll = m.probe();
        let starting = Some(Service::Starting {
            detail: "waiting for stock audio".into(),
        });
        assert!(
            report(
                &mut m,
                poll,
                status(&[("GV1", false)], starting, true, false)
            )
            .is_none()
        );
        assert!(matches!(m.state(Step::Start), StepState::Waiting(w) if w.contains("stock audio")));
        let poll = m.probe();
        report(
            &mut m,
            poll,
            status(&[("GV1", false)], Some(Service::Ready), true, false),
        );
        assert!(m.run.is_none() && m.finished);
    }

    #[test]
    fn stock_firmware_saves_settings_flashes_unchanged_and_waits_for_normal_mode() {
        let mut m = model(status(
            &[("GV1", false)],
            Some(Service::Ready),
            false,
            false,
        ));
        m.releases = vec![
            Release {
                channel: "stable".into(),
                version: "1.7.6".into(),
                supported: true,
                ..Default::default()
            },
            Release {
                channel: "beta".into(),
                version: "1.8.0".into(),
                supported: false,
                ..Default::default()
            },
        ];
        m.choose(1);
        assert_eq!(
            m.choice,
            Some(("stable".into(), "1.7.6".into())),
            "RC2FX needs support"
        );
        m.set_stock(true);
        assert_eq!(m.goal(), Goal::Stock);
        m.choose(1);
        assert_eq!(
            m.choice,
            Some(("stable".into(), "1.7.6".into())),
            "stock also requires a qualified catalog release"
        );
        assert!(m.needs_confirmation());
        assert!(m.start().is_none());
        let backup = read_settings(&mut m);
        assert!(
            matches!(backup, Some(Request::SaveSettings { .. })),
            "settings are kept for later"
        );
        let refresh = answer(&mut m, backup, Reply::Backup(Default::default()));
        let prepare = report(
            &mut m,
            refresh,
            status(&[("GV1", false)], Some(Service::Ready), false, false),
        );
        assert!(matches!(
            prepare,
            Some(Request::Prepare { stock: true, .. })
        ));
        // Flashed; RC2FX is gone, but normal mode is enough.
        m.run.as_mut().unwrap().flashed = true;
        m.busy = None;
        m.queue.clear();
        let poll = m.probe();
        assert!(report(&mut m, poll, status(&[], absent(), true, false)).is_none());
        assert!(matches!(m.state(Step::Start), StepState::Waiting(_)));
        let poll = m.probe();
        report(
            &mut m,
            poll,
            status(&[("GV1", false)], absent(), true, false),
        );
        assert!(m.run.is_none() && m.finished);
        // Back to RC2FX: an unsupported choice is replaced.
        m.set_stock(false);
        assert_eq!(m.choice, Some(("stable".into(), "1.7.6".into())));
    }

    #[test]
    fn readiness_counts_only_for_the_same_mixer() {
        let m = model(status(&[("GV1", false)], absent(), false, false));
        assert!(m.is_session_mixer(Some("GV1")));
        assert!(!m.is_session_mixer(Some("GV2")));
        assert!(!m.is_session_mixer(None));
        let updating = model(status(&[("GV1", true)], absent(), false, false));
        assert!(!updating.is_session_mixer(Some("GV1")));
        let two = model(status(
            &[("GV1", false), ("GV2", false)],
            absent(),
            false,
            false,
        ));
        assert!(!two.is_session_mixer(Some("GV1")));
    }

    #[test]
    fn firmware_is_never_prepared_with_ssh_and_keyed_images_are_not_reused() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        let select = m.start();
        assert!(matches!(
            selected(&mut m, select),
            Some(Request::Prepare { ssh: false, .. })
        ));
        assert!(
            model(status(&[("GV1", false)], absent(), true, false))
                .prepared()
                .is_some()
        );
        let mut s = status(&[("GV1", false)], absent(), true, false);
        s.status.saved.prepared.as_mut().unwrap().key_sha = "abc".into();
        assert!(model(s).prepared().is_none());
        let mut s = status(&[("GV1", false)], absent(), true, false);
        s.status.saved.prepared.as_mut().unwrap().host_key = "abc".into();
        assert!(
            model(s).prepared().is_none(),
            "a partial keyed cache fails closed"
        );
    }

    #[test]
    fn each_mode_card_shows_the_release_it_would_use() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        m.releases = vec![
            Release {
                channel: "stable".into(),
                version: "1.7.6".into(),
                supported: true,
                ..Default::default()
            },
            Release {
                channel: "beta".into(),
                version: "1.8.0".into(),
                supported: false,
                ..Default::default()
            },
        ];
        assert_eq!(m.releases_for(false).count(), 1);
        assert_eq!(m.releases_for(true).count(), 1);
        m.select(true, 1);
        assert_eq!(m.choice_for(true), Some(("stable".into(), "1.7.6".into())));
        assert_eq!(
            m.choice_for(false),
            Some(("stable".into(), "1.7.6".into())),
            "an unsupported release falls back for RC2FX"
        );
        m.set_stock(false);
        assert_eq!(m.choice, m.choice_for(false));
    }

    #[test]
    fn cancelling_works_until_usb_mode_is_requested() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        let select = m.start();
        let prepare = selected(&mut m, select);
        assert!(matches!(prepare, Some(Request::Prepare { .. })));
        assert!(m.can_cancel());
        assert!(m.cancel(), "the download is asked to stop");
        assert!(m.cancelling() && !m.can_cancel());
        let refresh = m.finish(
            prepare.as_ref().unwrap(),
            Err(rc2fx_installer::CANCELLED.into()),
        );
        assert!(m.run.is_none(), "cancelled, not failed");
        assert!(matches!(refresh, Some(Request::Probe)));

        let mut m = model(status(&[("GV1", false)], absent(), true, false));
        let select = m.start();
        let prepare = selected(&mut m, select);
        let refresh = answer(&mut m, prepare, Reply::Prepared(Prepared::default()));
        let usb = report(
            &mut m,
            refresh,
            status(&[("GV1", false)], absent(), true, false),
        );
        assert!(matches!(usb, Some(Request::EnterUsbMode { .. })));
        assert!(
            !m.can_cancel() && !m.cancel(),
            "past the point of no return"
        );
        assert!(m.run.is_some());
    }

    #[test]
    fn a_stock_mixer_skips_saving_settings() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        assert_eq!(m.goal(), Goal::Setup);
        let select = m.start();
        assert!(matches!(select, Some(Request::SelectMixer { .. })));
        assert_eq!(m.state(Step::Backup), StepState::Done);
    }

    #[test]
    fn settings_need_the_mixer_on_usb_to_be_matched() {
        let mut m = model(status(&[], Some(Service::Ready), false, false));
        assert!(m.start().is_none());
        assert!(matches!(m.state(Step::Backup), StepState::Waiting(_)));
        let poll = m.probe();
        report(
            &mut m,
            poll,
            status(&[("GV1", false)], Some(Service::Ready), false, false),
        );
        let backup = read_settings(&mut m);
        assert!(matches!(backup, Some(Request::SaveSettings { serial, .. }) if serial == "GV1"));
    }

    #[test]
    fn a_failure_stops_at_its_step_and_retry_continues_from_there() {
        let mut m = model(status(&[("GV1", true)], absent(), true, true));
        m.run = Some(Run {
            index: 4,
            serial: "GV1".into(),
            backed_up: true,
            prepared: true,
            ..Default::default()
        });
        let flash = m.advance();
        assert!(matches!(flash, Some(Request::Flash { .. })));
        let refresh = m.finish(flash.as_ref().unwrap(), Err("drive busy".into()));
        assert!(matches!(refresh, Some(Request::Probe)));
        report(
            &mut m,
            refresh,
            status(&[("GV1", true)], absent(), true, true),
        );
        assert_eq!(m.state(Step::Flash), StepState::Failed("drive busy".into()));
        assert!(
            m.tick(Instant::now() + Duration::from_secs(60)).is_none(),
            "no polling while failed"
        );
        let poll = m.retry();
        let flash = report(&mut m, poll, status(&[("GV1", true)], absent(), true, true));
        assert!(
            matches!(flash, Some(Request::Flash { .. })),
            "retried from the failed step"
        );
    }

    #[test]
    fn a_recent_flash_resumes_by_waiting_rather_than_flashing_again() {
        let mut s = status(&[], absent(), true, false);
        s.status.saved.commands.push(CommandRecord {
            command: "flash".into(),
            at_unix: now_unix() - 60,
            ..Default::default()
        });
        let mut m = Model::new();
        let poll = m.probe();
        assert_eq!(
            report(&mut m, poll, s),
            Some(Request::Releases),
            "readiness needs the catalog, and only that is requested"
        );
        assert!(m.running(), "resumed on opening");
        assert!(matches!(m.state(Step::Start), StepState::Waiting(_)));
        assert_eq!(m.state(Step::Flash), StepState::Done);
    }

    #[test]
    fn a_resumed_run_that_could_not_get_the_catalog_can_retry_it() {
        let mut s = status(&[], absent(), true, false);
        s.status.saved.commands.push(CommandRecord {
            command: "flash".into(),
            at_unix: now_unix() - 60,
            ..Default::default()
        });
        let mut m = Model::new();
        let poll = m.probe();
        let check = report(&mut m, poll, s).unwrap();
        m.finish(&check, Err("offline".into()));
        assert!(m.error.contains("offline") && m.running());
        m.run.as_mut().unwrap().failed = Some("did not get there in time".into());
        let poll = m.retry();
        let seen = Seen {
            status: m.status.clone().unwrap(),
            service: None,
        };
        assert_eq!(report(&mut m, poll, seen), Some(Request::Releases));
    }

    #[test]
    fn a_failed_firmware_check_says_why_and_only_repeats_when_asked() {
        let mut m = Model::new();
        let poll = m.probe();
        let seen = |m: &Model| Seen {
            status: m
                .status
                .clone()
                .unwrap_or(status(&[], None, false, false).status),
            service: None,
        };
        let s = seen(&m);
        let check = report(&mut m, poll, s).unwrap();
        assert_eq!(check, Request::Releases);
        assert_eq!(m.firmware_check, FirmwareCheck::Checking);
        m.finish(&check, Err("the signed catalog expired".into()));
        assert!(m.error.contains("the signed catalog expired"));
        assert!(m.releases.is_empty() && !m.can_start());

        let poll = m.tick(Instant::now() + Duration::from_secs(60));
        let s = seen(&m);
        assert!(report(&mut m, poll, s).is_none(), "polling doesn't retry");

        let poll = m.probe();
        let s = seen(&m);
        let check = report(&mut m, poll, s).unwrap();
        assert_eq!(check, Request::Releases, "Check again does");
        m.finish(&check, Ok(Reply::Releases(vec![])));
        assert!(m.error.is_empty(), "the old failure is gone");
    }

    #[test]
    fn a_cancelled_firmware_check_is_not_reported_as_a_failure() {
        let mut m = Model::new();
        m.queue.push_back(true);
        m.finish(&Request::Releases, Err(CANCELLED.into()));
        assert_eq!(m.error, "The check for supported firmware was cancelled.");
    }

    #[test]
    fn a_finished_update_stays_finished_and_the_next_plan_is_fresh() {
        let recent = |service| {
            let mut s = status(&[("GV1", false)], service, true, false);
            s.status.saved.commands.push(CommandRecord {
                command: "flash".into(),
                at_unix: now_unix() - 60,
                ..Default::default()
            });
            s
        };
        let mut m = Model::new();
        let poll = m.probe();
        report(&mut m, poll, recent(Some(Service::Ready)));
        assert!(m.finished && m.run.is_none(), "already running RC2FX");
        m.set_stock(true);
        let poll = m.tick(Instant::now() + Duration::from_secs(60));
        report(&mut m, poll, recent(Some(Service::Ready)));
        assert!(m.stock, "a refresh keeps the chosen mode");
        assert!(m.run.is_none());
        assert!(m.needs_confirmation());
        assert_eq!(m.state(Step::Backup), StepState::Pending);
        assert_eq!(m.state(Step::Flash), StepState::Pending);
    }

    #[test]
    fn background_refreshes_never_disable_the_page() {
        let mut m = model(status(&[], Some(Service::Ready), false, false));
        let poll = m.tick(Instant::now());
        assert!(matches!(poll, Some(Request::Probe)));
        assert!(m.busy.is_none(), "a refresh is not busy");
        assert!(m.can_start());
        assert!(
            m.tick(Instant::now() + Duration::from_secs(60)).is_none(),
            "one refresh at a time"
        );
    }

    #[test]
    fn unknown_cached_choice_cannot_start_before_catalog_discovery() {
        let mut m = model(status(&[("GV1", false)], absent(), true, false));
        m.choice = Some(("stable".into(), "1.8.0".into()));
        assert!(!m.can_start());
        assert!(m.start().is_none());
        m.choice = Some(("stable".into(), "1.7.6".into()));
        m.releases.clear();
        assert!(!m.can_start());
        assert!(m.start().is_none());
    }

    fn idle() -> Model {
        model(status(&[("GV1", false)], absent(), false, false))
    }

    fn two_firmwares() -> Model {
        let mut m = idle();
        m.releases.push(Release {
            channel: "stable".into(),
            version: "1.7.7".into(),
            profile: "rodecaster-pro-ii-1.7.7".into(),
            supported: true,
            ..Default::default()
        });
        m
    }

    #[test]
    fn the_newest_offer_for_the_chosen_firmware_is_the_default() {
        let mut m = two_firmwares();
        m.offers = Offers::Ready(vec![
            offer_for("2.0.0", "rodecaster-pro-ii-1.7.7"),
            offer("1.0.0"),
        ]);
        m.settle_mixer();
        assert_eq!(m.mixer, Some(MixerSource::Signed(offer("1.0.0"))));
        m.choose(1);
        assert_eq!(
            m.mixer.as_ref().map(MixerSource::version),
            Some("2.0.0"),
            "a choice for other firmware doesn't survive changing firmware"
        );
    }

    #[test]
    fn offers_this_app_cannot_use_are_not_choices() {
        let mut m = idle();
        let mut newer_app = offer("3.0.0");
        newer_app.metadata.minimum_app_version = "99.0.0".into();
        let mut protocol = offer("2.0.0");
        protocol.metadata.protocol_version += 1;
        m.offers = Offers::Ready(vec![newer_app, protocol]);
        m.settle_mixer();
        assert!(m.mixer_options().is_empty() && m.mixer.is_none());
        assert!(!m.can_start());
        let note = m.mixer_note().unwrap();
        assert!(note.contains("99.0.0"), "{note}");
    }

    #[test]
    fn firmware_for_a_newer_app_is_explained_and_can_never_be_chosen() {
        let mut m = Model::new();
        m.status = Some(status(&[("GV1", false)], None, false, false).status);
        m.queue.push_back(true);
        m.finish(
            &Request::Releases,
            Ok(Reply::Releases(vec![
                Release {
                    channel: "stable".into(),
                    version: "1.7.8".into(),
                    profile: "rodecaster-pro-ii-1.7.8".into(),
                    minimum_app_version: "99.0.0".into(),
                    ..Default::default()
                },
                Release {
                    channel: "stable".into(),
                    version: "1.7.6".into(),
                    profile: PROFILE.into(),
                    supported: true,
                    ..Default::default()
                },
            ])),
        );
        assert_eq!(m.choice, Some(("stable".into(), "1.7.6".into())));
        assert_eq!(m.releases_for(false).count(), 1);
        let note = m.firmware_note().unwrap();
        assert!(note.contains("1.7.8") && note.contains("99.0.0"), "{note}");
        assert!(note.contains("Update the app first"));
    }

    #[test]
    fn when_every_firmware_needs_a_newer_app_the_page_still_says_so() {
        let mut m = Model::new();
        m.status = Some(status(&[("GV1", false)], None, false, false).status);
        m.queue.push_back(true);
        m.finish(
            &Request::Releases,
            Ok(Reply::Releases(vec![Release {
                channel: "stable".into(),
                version: "1.7.8".into(),
                minimum_app_version: "99.0.0".into(),
                ..Default::default()
            }])),
        );
        assert!(m.releases.is_empty() && !m.can_start());
        assert!(m.firmware_note().is_some());
    }

    #[test]
    fn firmware_held_back_for_another_reason_is_not_blamed_on_the_app() {
        let mut m = Model::new();
        m.queue.push_back(true);
        m.finish(
            &Request::Releases,
            Ok(Reply::Releases(vec![Release {
                version: "1.7.8".into(),
                minimum_app_version: "0.0.1".into(),
                ..Default::default()
            }])),
        );
        assert!(m.firmware_note().is_none());
    }

    #[test]
    fn a_newer_offer_for_a_newer_app_is_mentioned_beside_what_can_be_installed() {
        let mut m = idle();
        let mut newer = offer("2.0.0");
        newer.metadata.minimum_app_version = "99.0.0".into();
        m.offers = Offers::Ready(vec![newer, offer("1.0.0")]);
        m.settle_mixer();
        assert_eq!(m.mixer, Some(MixerSource::Signed(offer("1.0.0"))));
        assert!(m.can_start());
        let note = m.mixer_note().unwrap();
        assert!(note.contains("2.0.0") && note.contains("99.0.0"), "{note}");
    }

    #[test]
    fn a_failed_check_never_falls_back_to_the_bundled_copy() {
        let mut m = Model {
            bundled: Some("0.9.0".into()),
            ..idle()
        };
        m.mixer = None;
        m.offers = Offers::Checking;
        m.queue.push_back(true);
        m.finish(
            &Request::MixerReleases,
            Err("no configured trust root".into()),
        );
        assert!(m.mixer.is_none(), "nothing is picked for the user");
        assert!(!m.can_start());
        let note = m.mixer_note().unwrap();
        assert!(note.contains("no configured trust root") && note.contains("0.9.0"));
        m.choose_mixer(MixerSource::Bundled("0.9.0".into()));
        assert!(m.can_start(), "the user may choose it by hand");
        assert_eq!(m.start(), Some(Request::SelectBundledMixer));
    }

    #[test]
    fn the_menu_says_where_each_version_comes_from_and_what_goes_backwards() {
        let mut m = Model {
            bundled: Some("0.9.0".into()),
            ..idle()
        };
        m.set_installed(Some("0.9.5".into()), None);
        let menu: Vec<_> = m
            .mixer_menu()
            .into_iter()
            .map(|(_, name, chosen)| (name, chosen))
            .collect();
        assert_eq!(
            menu,
            [
                ("1.0.0 · signed download".to_owned(), true),
                (
                    "0.9.0 · included with this app · older than installed".to_owned(),
                    false
                ),
            ]
        );
    }

    #[test]
    fn a_build_without_a_payload_offers_no_bundled_copy() {
        let mut m = idle();
        m.choose_mixer(MixerSource::Bundled("0.9.0".into()));
        assert_eq!(m.mixer, Some(MixerSource::Signed(offer("1.0.0"))));
        assert!(
            m.mixer_options()
                .iter()
                .all(|o| matches!(o, MixerSource::Signed(_)))
        );
    }

    #[test]
    fn a_failed_or_cancelled_check_can_be_repeated_and_never_repeats_itself() {
        let mut m = idle();
        m.offers = Offers::Failed(CANCELLED.into());
        assert!(m.mixer_note().unwrap().contains("cancelled"));
        assert!(
            m.check_mixers().is_none(),
            "polling doesn't retry by itself"
        );
        assert_eq!(m.recheck_mixers(), Some(Request::MixerReleases));
        assert_eq!(m.offers, Offers::Checking);
        assert!(m.recheck_mixers().is_none());
    }

    #[test]
    fn a_downgrade_is_never_the_default_and_is_always_called_out() {
        let mut m = idle();
        m.set_installed(Some("1.1.0".into()), None);
        assert_eq!(m.mixer, None, "only an older offer: nothing is picked");
        assert!(!m.can_start());
        assert!(m.mixer_note().unwrap().contains("1.1.0"));
        let older = MixerSource::Signed(offer("1.0.0"));
        assert_eq!(m.mixer_change(&older), MixerChange::Downgrade);
        m.choose_mixer(older.clone());
        assert!(m.can_start(), "the user may choose it by hand");
        let warnings = m.warnings();
        assert!(warnings[0].contains("1.1.0") && warnings[0].contains("older 1.0.0"));
        m.set_stock(true);
        assert!(m.warnings().is_empty(), "stock replaces RC2FX on purpose");
    }

    #[test]
    fn an_unknown_installed_version_is_said_and_a_known_upgrade_is_quiet() {
        let mut m = idle();
        assert!(m.warnings()[0].contains("can't read"));
        m.set_installed(Some("not a version".into()), None);
        assert!(m.warnings()[0].contains("can't read"));
        m.set_installed(Some("0.9.0".into()), None);
        assert!(m.warnings().is_empty());
        m.set_installed(Some("1.0.0".into()), None);
        assert!(m.warnings().is_empty(), "reinstalling the same version");
        assert!(m.mixer.is_some());
    }

    #[test]
    fn going_back_on_firmware_is_said_too() {
        let mut m = idle();
        m.set_installed(Some("0.9.0".into()), Some("1.7.7".into()));
        let warnings = m.warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("RØDE firmware 1.7.7"));
        m.set_stock(true);
        assert_eq!(m.warnings(), warnings);
    }

    #[test]
    fn a_run_installs_what_was_reviewed_whatever_changes_on_the_page() {
        let mut m = idle();
        let select = m.start();
        m.offers = Offers::Ready(vec![offer("2.0.0")]);
        m.set_installed(Some("3.0.0".into()), None);
        let pinned = m.run.as_ref().unwrap().mixer.clone();
        assert_eq!(pinned, Some(MixerSource::Signed(offer("1.0.0"))));
        assert_eq!(m.mixer, pinned, "not re-chosen during a run");
        assert!(matches!(
            selected(&mut m, select),
            Some(Request::Prepare { .. })
        ));
    }

    #[test]
    fn a_failed_selection_stops_before_preparing_and_retry_repeats_the_same_offer() {
        let mut m = idle();
        let select = m.start().unwrap();
        let refresh = m.finish(&select, Err("the signature is invalid".into()));
        let s = status(&[("GV1", false)], absent(), false, false);
        assert!(report(&mut m, refresh, s).is_none());
        assert_eq!(
            m.state(Step::Mixer),
            StepState::Failed("the signature is invalid".into())
        );
        assert_eq!(m.state(Step::Prepare), StepState::Pending);
        let poll = m.retry();
        let again = report(
            &mut m,
            poll,
            status(&[("GV1", false)], absent(), false, false),
        );
        assert_eq!(again, Some(select));
    }

    #[test]
    fn stopping_after_a_failure_reviews_what_is_published_now() {
        let mut m = idle();
        let select = m.start().unwrap();
        m.finish(&select, Err("the selected mixer release changed".into()));
        m.queue.clear();
        m.stop();
        assert_eq!(m.offers, Offers::Unchecked);
        let poll = m.probe();
        let check = report(
            &mut m,
            poll,
            status(&[("GV1", false)], absent(), false, false),
        );
        assert_eq!(check, Some(Request::MixerReleases));
        m.finish(
            &check.unwrap(),
            Ok(Reply::MixerReleases(vec![offer("1.2.0")])),
        );
        assert_eq!(m.mixer, Some(MixerSource::Signed(offer("1.2.0"))));
    }

    #[test]
    fn a_selection_can_be_cancelled_and_touches_nothing() {
        let mut m = idle();
        let select = m.start().unwrap();
        assert!(m.can_cancel());
        assert!(m.cancel(), "the download is asked to stop");
        let refresh = m.finish(&select, Err(CANCELLED.into()));
        assert!(m.run.is_none());
        assert!(matches!(refresh, Some(Request::Probe)));
    }

    #[test]
    fn verified_software_must_be_the_version_that_was_reviewed() {
        let mut m = idle();
        let select = m.start();
        let identity = Identity {
            release: "6.6.6".into(),
            ..Default::default()
        };
        let refresh = answer(&mut m, select, Reply::MixerSelected(identity));
        let s = status(&[("GV1", false)], absent(), false, false);
        assert!(report(&mut m, refresh, s).is_none());
        assert!(matches!(m.state(Step::Mixer), StepState::Failed(e) if e.contains("6.6.6")));
        assert_eq!(m.state(Step::Prepare), StepState::Pending);
    }

    #[test]
    fn stock_firmware_needs_no_mixer_software_at_all() {
        let mut m = idle();
        m.offers = Offers::Failed("offline".into());
        m.settle_mixer();
        m.set_stock(true);
        assert!(m.can_start());
        assert!(matches!(
            m.start(),
            Some(Request::Prepare { stock: true, .. })
        ));
        assert_eq!(m.run.as_ref().unwrap().mixer, None);
    }

    #[test]
    fn signed_discovery_follows_the_firmware_list_once() {
        let mut m = Model::new();
        m.status = Some(status(&[], None, false, false).status);
        m.queue.push_back(true);
        let check = m
            .finish(&Request::Releases, Ok(Reply::Releases(vec![])))
            .unwrap();
        assert_eq!(check, Request::MixerReleases);
        let poll = m.tick(Instant::now() + Duration::from_secs(60));
        assert!(poll.is_none(), "polling waits for the check");
        m.finish(&check, Ok(Reply::MixerReleases(vec![])));
        assert_eq!(m.offers, Offers::Ready(vec![]));
        assert!(m.check_mixers().is_none());
    }

    #[test]
    fn releases_default_to_stable_and_prepared_firmware_wins() {
        let mut m = Model::new();
        m.status = Some(status(&[], None, false, false).status);
        let releases = vec![
            Release {
                channel: "stable".into(),
                version: "1.7.6".into(),
                supported: true,
                ..Default::default()
            },
            Release {
                channel: "beta".into(),
                version: "1.7.7".into(),
                supported: true,
                ..Default::default()
            },
        ];
        m.finish(&Request::Releases, Ok(Reply::Releases(releases.clone())));
        assert_eq!(m.choice, Some(("stable".into(), "1.7.6".into())));
        let mut m = Model::new();
        let mut s = status(&[], None, true, false);
        s.status.saved.prepared.as_mut().unwrap().channel = "beta".into();
        s.status.saved.prepared.as_mut().unwrap().version = "1.7.7".into();
        let next = m.finish(&Request::Probe, Ok(Reply::Status(Box::new(s.status))));
        assert_eq!(next, Some(Request::Releases));
        m.finish(&Request::Releases, Ok(Reply::Releases(releases)));
        assert_eq!(m.choice, Some(("beta".into(), "1.7.7".into())));

        // A cached beta choice must disappear after stable promotion; an
        // unknown or unsupported release must never remain in the menu.
        m.finish(
            &Request::Releases,
            Ok(Reply::Releases(vec![
                Release {
                    channel: "stable".into(),
                    version: "1.7.7".into(),
                    supported: true,
                    ..Default::default()
                },
                Release {
                    channel: "stable".into(),
                    version: "1.8.0".into(),
                    supported: false,
                    ..Default::default()
                },
            ])),
        );
        assert_eq!(m.choice, Some(("stable".into(), "1.7.7".into())));
        assert_eq!(m.releases.len(), 1);
    }
}
