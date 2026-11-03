//! The installer as one plan, independent of GPUI.
//!
//! RC2FX is installed by flashing firmware that contains it, so every
//! install or update is the same plan: save the mixer's settings, build the
//! firmware, switch to USB update mode, flash, and wait for RC2FX to answer.
//! Steps the mixer already satisfies are done. One start runs the rest in
//! order, waiting for the mixer where it has to and stopping at the first
//! error, which can be retried from that step.

use rc2fx_installer::{DeviceModel, Prepared, Release, Reply, Request, Service, Status, UsbMixer};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Backup,
    Prepare,
    UsbMode,
    Flash,
    Start,
}

pub(crate) const STEPS: [Step; 5] = [
    Step::Backup,
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
    pub choice: Option<(String, String)>,
    /// Flash unchanged RØDE firmware rather than firmware with RC2FX.
    pub stock: bool,
    /// Developer option: build RC2FX's SSH key into the firmware.
    pub ssh: bool,
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
            choice: None,
            stock: false,
            ssh: false,
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
        self.send(Request::Probe)
    }

    // ---- What the mixer reports --------------------------------------------

    pub fn usb(&self) -> &[UsbMixer] {
        self.status.as_ref().map_or(&[], |s| s.usb.as_slice())
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

    pub fn set_ssh(&mut self, ssh: bool) {
        if self.running() || self.busy.is_some() {
            return;
        }
        self.ssh = ssh;
        self.run = None;
        self.finished = false;
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
        // Act on what the mixer reports now.
        self.refresh()
    }

    pub fn stop(&mut self) {
        if self.busy.is_none() {
            self.run = None;
            self.settings_read = SettingsRead::Idle;
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
                Step::Prepare => match self.choice.clone() {
                    Some((channel, version)) => self.send(Request::Prepare {
                        channel,
                        version,
                        serial,
                        stock: self.stock,
                        ssh: self.ssh && !self.stock,
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
                    // A failed poll while waiting is not the end of the run.
                    Request::Probe if self.run.is_some() => self.advance(),
                    Request::Probe | Request::Releases => {
                        self.error = e;
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
                    return self.advance();
                }
                if !self.releases.is_empty()
                    || self.status.as_ref().is_some_and(|s| s.usb_error.is_some())
                {
                    return None;
                }
                return self.quiet(Request::Releases);
            }
            Reply::Releases(items) => {
                let items: Vec<_> = items
                    .into_iter()
                    .filter(|r| r.supported && (r.model.is_empty() || r.model == self.device.id()))
                    .collect();
                if !self.choice.as_ref().is_some_and(|(channel, version)| {
                    items
                        .iter()
                        .any(|r| &r.channel == channel && &r.version == version)
                }) {
                    self.choice = default_choice(&items);
                }
                self.releases = items;
                return None;
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
    use rc2fx_installer::{CommandRecord, Saved};

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
        m.finish(&releases, Ok(Reply::Releases(vec![])));
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
                supported: true,
                ..Default::default()
            }])),
        );
        assert!(m.needs_confirmation());
        let request = m.start().unwrap();
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
            supported: true,
            ..Default::default()
        }];
        m.choice = Some(("stable".into(), "1.7.6".into()));
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
        let prepare = report(
            &mut m,
            refresh,
            status(&[("GV1", false)], Some(Service::Ready), false, false),
        );
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
    fn ssh_is_a_developer_option_and_never_goes_into_stock_firmware() {
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        m.set_ssh(true);
        assert!(matches!(
            m.start(),
            Some(Request::Prepare { ssh: true, .. })
        ));
        let mut m = model(status(&[("GV1", false)], absent(), false, false));
        m.set_ssh(true);
        m.set_stock(true);
        assert!(matches!(
            m.start(),
            Some(Request::Prepare {
                ssh: false,
                stock: true,
                ..
            })
        ));
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
        let prepare = m.start();
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
        let prepare = m.start();
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
        let prepare = m.start();
        assert!(matches!(prepare, Some(Request::Prepare { .. })));
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
            index: 3,
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
        assert!(report(&mut m, poll, s).is_none());
        assert!(m.running(), "resumed on opening");
        assert!(matches!(m.state(Step::Start), StepState::Waiting(_)));
        assert_eq!(m.state(Step::Flash), StepState::Done);
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
