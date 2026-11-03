//! Install or update RC2FX on a RØDECaster Pro II.
//!
//! Requests run one at a time on a thread that owns the installer engine.
//! On Linux without root, switching to USB update mode and flashing run in a
//! helper started through pkexec, because raw USB, mounting and ejection need
//! it. It starts only when the user grants access on the page.

mod flow;
pub(super) mod signature;

use super::*;
use flow::{Goal, Model, STEPS, Step, StepState};
use gpui_kit::component::{
    collapsible::Collapsible, progress::Progress as ProgressBar, spinner::Spinner,
};
use gpui_kit::prelude::FluentBuilder as _;
use rc2fx_installer::{Engine, Progress, Reply, Request, helper::Helper};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

type Outcome = (Request, std::result::Result<Reply, String>);

/// Administrator access for the steps that need it: on Linux without root,
/// switching the mixer to USB update mode and flashing it run in a helper
/// started through pkexec. The page asks for it only when the user chooses
/// to grant it; everything else runs in this process.
#[derive(Clone, Debug, PartialEq)]
enum Access {
    /// This system lets the app do everything itself.
    NotNeeded,
    Needed,
    /// The system's password prompt is open.
    Asking,
    Granted,
    /// The last attempt failed or was dismissed.
    Refused(String),
}

impl Access {
    fn allows_flashing(&self) -> bool {
        matches!(self, Access::NotNeeded | Access::Granted)
    }
}

/// Presentation gate only: never hide an in-flight operation or completion
/// behind a new permission request. A failed run can request access again.
fn needs_access(model: &Model, access: &Access) -> bool {
    let idle_or_failed = (model.run.is_none() && !model.finished)
        || model.run.as_ref().is_some_and(|run| run.failed.is_some());
    idle_or_failed && !access.allows_flashing()
}

/// Requests that need administrator access where [`Access`] applies.
fn privileged(request: &Request) -> bool {
    matches!(
        request,
        Request::EnterUsbMode { .. } | Request::Flash { .. }
    )
}

struct Runner {
    jobs: mpsc::Sender<Request>,
    done: mpsc::Receiver<Outcome>,
    progress: Arc<Mutex<Option<Progress>>>,
    /// Stops a download in progress, in this process or in the helper.
    cancel: Arc<AtomicBool>,
    helper: Arc<Mutex<Option<rc2fx_installer::helper::Canceller>>>,
    /// The privileged helper, once access is granted.
    privileged: Arc<Mutex<Option<Helper>>>,
    /// The outcome of each grant attempt, and a lost helper.
    access: mpsc::Receiver<std::result::Result<(), String>>,
    access_sender: mpsc::Sender<std::result::Result<(), String>>,
    work: PathBuf,
}

impl Runner {
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Ok(helper) = self.helper.lock()
            && let Some(helper) = helper.as_ref()
        {
            helper.cancel();
        }
    }

    /// Start the helper, which opens the system's password prompt, and
    /// check that it answers before handing it to the request thread.
    fn grant(&self) {
        let work = self.work.clone();
        let slot = self.privileged.clone();
        let canceller = self.helper.clone();
        let report = self.access_sender.clone();
        std::thread::Builder::new()
            .name("rc2fx-installer-access".into())
            .spawn(move || {
                let outcome = Helper::start_embedded(&work).and_then(|mut helper| {
                    helper.handle(&Request::Probe, |_| {})?;
                    Ok(helper)
                });
                let _ = report.send(match outcome {
                    Ok(helper) => {
                        if let Ok(mut slot) = canceller.lock() {
                            *slot = Some(helper.canceller());
                        }
                        if let Ok(mut slot) = slot.lock() {
                            *slot = Some(helper);
                        }
                        Ok(())
                    }
                    Err(e) => Err(e.0),
                });
            })
            .expect("start installer access thread");
    }
}

impl Runner {
    fn start(work: PathBuf) -> Self {
        let (jobs, requests) = mpsc::channel::<Request>();
        let (finished, done) = mpsc::channel();
        let (access_sender, access) = mpsc::channel();
        let progress = Arc::new(Mutex::new(None));
        let shared = progress.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let helper = Arc::new(Mutex::new(None));
        let canceller = helper.clone();
        let privileged_helper: Arc<Mutex<Option<Helper>>> = Arc::new(Mutex::new(None));
        let slot = privileged_helper.clone();
        let lost = access_sender.clone();
        let thread_work = work.clone();
        std::thread::Builder::new()
            .name("rc2fx-installer".into())
            .spawn(move || {
                let report = move |p: Progress| {
                    if let Ok(mut slot) = shared.lock() {
                        *slot = Some(p);
                    }
                };
                let mut engine = Engine::embedded(thread_work);
                engine.progress = Some(Box::new(report.clone()));
                engine.cancel = stop.clone();
                let separate = rc2fx_installer::helper::needed();
                // What the helper was last told about Duo flashing.
                let mut allow_duo = false;
                let mut helper_duo = None;
                for request in requests {
                    stop.store(false, Ordering::Relaxed);
                    report(Progress::default());
                    if let Request::AllowExperimentalDuo(allow) = request {
                        allow_duo = allow;
                    }
                    let result = if separate && privileged(&request) {
                        let mut guard = slot.lock().ok();
                        match guard.as_mut().and_then(|g| g.as_mut()) {
                            None => Err("Grant administrator access first.".into()),
                            Some(helper) => {
                                let synced = if helper_duo == Some(allow_duo) {
                                    Ok(())
                                } else {
                                    helper
                                        .handle(&Request::AllowExperimentalDuo(allow_duo), |_| {})
                                        .map(|_| helper_duo = Some(allow_duo))
                                };
                                let result = synced
                                    .and_then(|()| helper.handle(&request, &report))
                                    .map_err(|e| e.0);
                                if result.is_err() && !helper.alive() {
                                    if let Some(guard) = guard.as_mut() {
                                        guard.take();
                                    }
                                    if let Ok(mut c) = canceller.lock() {
                                        c.take();
                                    }
                                    helper_duo = None;
                                    let _ = lost.send(Err(
                                        "Administrator access ended. Grant it again to continue."
                                            .into(),
                                    ));
                                }
                                result
                            }
                        }
                    } else {
                        engine.handle(request.clone()).map_err(|e| e.0)
                    };
                    if finished.send((request, result)).is_err() {
                        break;
                    }
                }
            })
            .expect("start installer thread");
        Runner {
            jobs,
            done,
            progress,
            cancel,
            helper,
            privileged: privileged_helper,
            access,
            access_sender,
            work,
        }
    }
}

impl Runner {
    fn send(&mut self, request: Request) -> std::result::Result<(), Request> {
        self.jobs.send(request).map_err(|error| error.0)
    }
    fn take_access(&mut self) -> Option<std::result::Result<(), String>> {
        self.access.try_recv().ok()
    }
    fn take_outcome(&mut self) -> Option<Outcome> {
        self.done.try_recv().ok()
    }
    fn progress(&self) -> Option<Progress> {
        self.progress
            .lock()
            .ok()
            .and_then(|progress| progress.clone())
            .filter(|progress| !progress.phase.is_empty())
    }
}

pub(super) struct Installer {
    model: Model,
    runner: Runner,
    /// The engine state readiness was last judged from.
    engine: Option<String>,
    progress: Option<Progress>,
    confirming: bool,
    developer_open: bool,
    controls_scroll: ScrollHandle,
    developer_anchor: ScrollAnchor,
    /// The SSH key the developer option builds in.
    key: PathBuf,
    access: Access,
}

impl Installer {
    pub(super) fn needs_access(&self) -> bool {
        needs_access(&self.model, &self.access)
    }

    pub fn busy(&self) -> bool {
        self.model.busy.is_some()
    }

    fn send(&mut self, request: Option<Request>) {
        if let Some(request) = request {
            self.progress = None;
            if let Err(request) = self.runner.send(request) {
                self.model.finish(
                    &request,
                    Err("The installer stopped unexpectedly. Close and reopen it.".into()),
                );
            }
        }
    }
}

fn paragraph(text: impl Into<SharedString>, color: u32) -> Div {
    div().text_sm().text_color(rgb(color)).child(text.into())
}

fn step_title(step: Step) -> &'static str {
    match step {
        Step::Backup => "Save settings",
        Step::Prepare => "Prepare firmware",
        Step::UsbMode => "Switch to USB update mode",
        Step::Flash => "Flash firmware",
        Step::Start => "Wait for RC2FX",
    }
}

fn progress_text(p: &Progress) -> String {
    let megabytes = |what: &str| {
        if p.total > 0 {
            format!(
                "{what} · {:.0} of {:.0} MB",
                p.completed as f64 / 1e6,
                p.total as f64 / 1e6
            )
        } else {
            what.to_owned()
        }
    };
    match p.phase.as_str() {
        "download" if p.total == 0 => {
            format!("Downloading firmware · {:.0} MB", p.completed as f64 / 1e6)
        }
        "download" => megabytes("Downloading firmware"),
        "verification" => format!("Checking the {} checksum", p.detail),
        "stage" => megabytes(&format!("Copying {} to the mixer", p.detail)),
        "verify" => megabytes(&p.detail),
        _ => p.detail.clone(),
    }
}

impl Rc2FxApp {
    pub(super) fn open_installer(&mut self, cx: &mut Context<Self>) {
        if self.installer.is_some() {
            return;
        }
        // Branch before any work-directory/key discovery or live runner startup.
        // Only working downloads/backups need a directory; installer inputs
        // are immutable bytes embedded in this executable and its helper.
        let work = match std::env::var_os("RC2FX_INSTALLER_WORK").map(PathBuf::from) {
            Some(path) if path.is_absolute() => Some(path),
            Some(path) => std::env::current_dir().ok().map(|cwd| cwd.join(path)),
            None => rc2fx_installer::default_work(),
        };
        let Some(work) = work else {
            self.message = "The installer has no folder for its downloads and backups. Set RC2FX_INSTALLER_WORK to one.".into();
            cx.notify();
            return;
        };
        // The key the engine builds in, as it chooses it.
        let key = rc2fx_installer::default_key();
        let access = if rc2fx_installer::helper::needed() {
            Access::Needed
        } else {
            Access::NotNeeded
        };
        let runner = Runner::start(work);
        let controls_scroll = ScrollHandle::new();
        let developer_anchor = ScrollAnchor::for_handle(controls_scroll.clone());
        let mut installer = Installer {
            model: Model::new(),
            runner,
            engine: None,
            progress: None,
            confirming: false,
            developer_open: false,
            controls_scroll,
            developer_anchor,
            key,
            access,
        };
        installer.model.privileged = installer.access.allows_flashing();
        let first = installer.model.set_allow_duo(connection::remembered_duo());
        installer.send(first);
        self.installer = Some(installer);
        cx.notify();
    }

    /// Ask the system for administrator access: this is what opens its
    /// password prompt, and only when the user chooses to.
    fn grant_installer_access(&mut self, cx: &mut Context<Self>) {
        let Some(installer) = self.installer.as_mut() else {
            return;
        };
        if matches!(installer.access, Access::Needed | Access::Refused(_)) {
            installer.access = Access::Asking;
            installer.runner.grant();
            cx.notify();
        }
    }

    fn close_installer(&mut self, cx: &mut Context<Self>) {
        if self.installer.as_ref().is_some_and(Installer::busy) {
            return;
        }
        self.installer = None;
        self.installer_page = false;
        cx.notify();
    }

    /// Run a model event, then send any request it makes.
    pub(super) fn installer_event(
        &mut self,
        cx: &mut Context<Self>,
        event: impl FnOnce(&mut Model) -> Option<Request>,
    ) {
        let Some(installer) = self.installer.as_mut() else {
            return;
        };
        let request = event(&mut installer.model);
        installer.send(request);
        cx.notify();
    }

    /// Start the plan. Changing firmware asks once, in a dialog.
    fn start_installer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(installer) = self.installer.as_mut() else {
            return;
        };
        let retry = installer
            .model
            .run
            .as_ref()
            .is_some_and(|r| r.failed.is_some());
        if !installer.model.needs_confirmation()
            || (retry && installer.model.device != rc2fx_installer::DeviceModel::Duo)
        {
            self.installer_event(cx, |m| if retry { m.retry() } else { m.start() });
            return;
        }
        if installer.confirming {
            return;
        }
        installer.confirming = true;
        let version = installer
            .model
            .choice
            .as_ref()
            .map(|(_, v)| format!("RØDE {v}"))
            .unwrap_or_else(|| "the firmware".into());
        let (title, detail, action) = if installer.model.stock {
            (
                format!("Flash stock {version}?"),
                "This removes RC2FX. The mixer's audio stops while it updates, which takes a few minutes. Keep it powered on and connected until it's done. Your RC2FX settings are saved on this computer first.",
                "Flash stock firmware",
            )
        } else {
            (
                format!("Flash {version} with RC2FX?"),
                "The mixer's audio stops while it updates, which takes a few minutes. Keep it powered on and connected until it's done. Your settings are saved first and carried over.",
                "Flash and install",
            )
        };
        let device = installer.model.device;
        let choice = installer.model.choice.clone();
        let stock = installer.model.stock;
        let ssh = installer.model.ssh;
        let usb = installer.model.usb().to_vec();
        // The device sits above the question, so the title stays short and
        // clear of the dialog's close button.
        let eyebrow = device.label().to_owned();
        let detail = if device == rc2fx_installer::DeviceModel::Duo {
            format!("{}\n\n{detail}", rc2fx_installer::DUO_WARNING)
        } else {
            detail.to_owned()
        };
        let action = if device == rc2fx_installer::DeviceModel::Duo {
            "Flash Duo at my own risk"
        } else {
            action
        };
        let confirm_app = cx.weak_entity();
        let close_app = cx.weak_entity();
        ui::confirm(
            window,
            cx,
            ui::Confirm {
                eyebrow: Some(eyebrow),
                title,
                detail,
                cancel: Some("Cancel"),
                action: action.into(),
            },
            move |_, cx| {
                let _ = confirm_app.update(cx, |app, cx| {
                    if let Some(installer) = app.installer.as_mut() {
                        installer.confirming = false;
                    }
                    // A dialog is not permission to flash a different device,
                    // release or option set than the one the user reviewed.
                    if app.installer.as_ref().is_some_and(|i| {
                        i.model.device == device
                            && i.model.choice == choice
                            && i.model.stock == stock
                            && i.model.ssh == ssh
                            && i.model.usb() == usb
                            && i.access.allows_flashing()
                            && !i.busy()
                    }) {
                        app.installer_event(cx, |m| if retry { m.retry() } else { m.start() });
                    }
                    cx.notify();
                });
            },
            move |_, cx| {
                let _ = close_app.update(cx, |app, cx| {
                    if let Some(installer) = app.installer.as_mut() {
                        installer.confirming = false;
                    }
                    cx.notify();
                });
            },
        );
    }

    /// Collect finished requests, polls and progress. Returns whether to
    /// redraw.
    pub(super) fn poll_installer(&mut self, cx: &mut Context<Self>) -> bool {
        // RC2FX's readiness, as this app's USB connection sees it.
        let engine = self
            .session
            .saved
            .as_ref()
            .filter(|_| self.session.connected && self.stream_connected)
            .map(|s| {
                serde_json::json!({"engine": s.engine, "config": {"revision": s.config.revision}})
                    .to_string()
            });
        let Some(installer) = self.installer.as_mut() else {
            return false;
        };
        let mut changed = false;
        if engine != installer.engine {
            let service = engine
                .as_deref()
                .map(|e| rc2fx_installer::embed::service(e.as_bytes()));
            installer.model.set_service(service);
            installer.engine = engine;
            changed = true;
        }
        if installer.model.take_settings_request() {
            let generation = self.generation;
            if !self.command(Command::Settings { generation }) {
                let error = std::mem::take(&mut self.message);
                self.installer_event(cx, |m| m.settings_read(Err(error)));
            }
            return true;
        }
        let Some(installer) = self.installer.as_mut() else {
            return false;
        };
        while let Some(outcome) = installer.runner.take_access() {
            changed = true;
            installer.access = match outcome {
                Ok(()) => Access::Granted,
                Err(e) if e.contains("not granted") => Access::Refused(
                    "The password prompt was dismissed or the password wasn't accepted.".into(),
                ),
                Err(e) => Access::Refused(e),
            };
            installer.model.privileged = installer.access.allows_flashing();
        }
        while let Some((request, result)) = installer.runner.take_outcome() {
            changed = true;
            let next = installer.model.finish(&request, result);
            installer.send(next);
        }
        let poll = installer.model.tick(Instant::now());
        if poll.is_some() {
            changed = true;
            installer.send(poll);
        }
        if installer.busy() {
            let progress = installer.runner.progress();
            if progress != installer.progress {
                installer.progress = progress;
                changed = true;
            }
        }
        changed
    }

    /// Close the installer and connect the mixer controls to the mixer.
    fn open_mixer_controls(&mut self, cx: &mut Context<Self>) {
        let Some(installer) = self.installer.as_ref() else {
            return;
        };
        if installer.busy() {
            return;
        }
        self.close_installer(cx);
        if !self.session.connected {
            self.connect(cx);
        }
    }

    /// The header's installer button: bring the installer to the front,
    /// opening it if needed, or put it away. Put away while idle it closes;
    /// a run keeps going behind other pages.
    pub(super) fn toggle_installer_page(&mut self, cx: &mut Context<Self>) {
        if self.installer_page {
            self.installer_page = false;
            if self
                .installer
                .as_ref()
                .is_some_and(|i| !i.busy() && i.model.run.is_none())
            {
                self.installer = None;
            }
        } else {
            self.set_settings_open(false);
            self.open_installer(cx);
            self.installer_page = self.installer.is_some();
        }
        cx.notify();
    }

    /// A run has switched the mixer to USB update mode and not yet finished
    /// flashing it: the mixer cannot run RC2FX meanwhile.
    pub(super) fn installer_flashing(&self) -> bool {
        self.installer
            .as_ref()
            .and_then(|i| i.model.run.as_ref())
            .is_some_and(|r| r.committed && !r.flashed)
    }

    /// The installer's part of the window-close decision. Closing is only
    /// held back while a step is running.
    pub(super) fn confirm_close_installer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        if !self.installer.as_ref().is_some_and(Installer::busy) {
            return None;
        }
        ui::confirm(
            window,
            cx,
            ui::Confirm {
                eyebrow: None,
                title: "The installer is working".into(),
                detail: "Wait for the current step to finish before closing RC2FX. Keep the mixer connected and powered on.".into(),
                cancel: None,
                action: "OK".into(),
            },
            |_, _| {},
            |_, _| {},
        );
        Some(false)
    }

    /// The installer itself: the mixer on the left, what to do on the right.
    /// Shown on its own screen without a connection, and inside Settings
    /// with one.
    pub(super) fn installer_body(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .gap_8()
            .child(self.installer_content(installer, cx))
            .child(self.installer_actions(installer, cx))
    }

    fn installer_content(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        // Permission is its own step, not a competing card below unusable
        // firmware choices. Once granted this step disappears completely.
        if installer.needs_access() {
            return div()
                .v_flex()
                .gap_6()
                .pt_8()
                .child(self.access_card(installer, cx))
                .when(installer.model.run.is_some(), |page| {
                    page.child(self.timeline(installer))
                });
        }
        let m = &installer.model;
        let idle = m.run.is_none() && !m.finished;
        let mut content = div().v_flex().gap_5().w_full();
        if !m.error.is_empty() {
            content = content.child(paragraph(m.error.clone(), DANGER));
        }
        if let Some(error) = m
            .status
            .as_ref()
            .and_then(|status| status.usb_error.as_ref())
        {
            content = content.child(paragraph(format!("USB check failed: {error}"), DANGER));
        }
        if m.usb().len() > 1 {
            content = content.child(paragraph(
                "More than one mixer is connected. Connect only the mixer you want to update.",
                WARN,
            ));
        }
        if !m.error.is_empty()
            || m.status
                .as_ref()
                .is_some_and(|status| status.usb_error.is_some())
            || m.usb().len() > 1
        {
            content = content.child(
                div().child(
                    Button::new("installer-check")
                        .ghost()
                        .small()
                        .label("Check again")
                        .disabled(installer.busy() || m.running())
                        .on_click(
                            cx.listener(|app, _, _, cx| app.installer_event(cx, Model::probe)),
                        ),
                ),
            );
        }
        content = content.child(self.action_card(installer, cx));
        if idle {
            if m.duo_connected() {
                content = content.child(self.duo_option(installer, cx));
            }
            content = content.child(self.developer_options(installer, cx));
        } else if m.device == rc2fx_installer::DeviceModel::Duo {
            content = content.child(paragraph(rc2fx_installer::DUO_WARNING, WARN));
        }
        content
    }

    pub(super) fn installer_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(installer) = self.installer.as_ref() else {
            return div().into_any_element();
        };
        div()
            .v_flex()
            .size_full()
            .relative()
            .font_family("Inter")
            .bg(onboarding::background())
            .child(onboarding::backdrop())
            .text_color(rgb(TEXT))
            .child(
                div()
                    .id("installer-scroll")
                    .track_scroll(&installer.controls_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_8()
                    .py_4()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.))
                            .mx_auto()
                            .child(self.installer_content(installer, cx)),
                    ),
            )
            .child(
                div().flex_shrink_0().px_8().pb_6().child(
                    div()
                        .w_full()
                        .max_w(px(760.))
                        .mx_auto()
                        .child(self.installer_actions(installer, cx)),
                ),
            )
            .into_any_element()
    }

    /// What to do: the choice of firmware before a run, the run's progress
    /// during it, and the result after.
    fn action_card(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        let m = &installer.model;
        let idle = m.run.is_none() && !m.finished;
        let (title, detail) = if m.finished && m.stock {
            (
                "Stock RØDE firmware is installed",
                "RC2FX is removed. The mixer may restart once more by itself.",
            )
        } else if m.finished {
            (
                "RC2FX is installed",
                "It starts by itself whenever the mixer turns on.",
            )
        } else if m.waiting_after_flash() {
            (
                "Waiting for your mixer",
                "Keep it powered on and connected while it finishes updating.",
            )
        } else if !idle {
            if m.stock {
                ("Flashing stock firmware", "")
            } else {
                ("Installing RC2FX", "")
            }
        } else if m.status.is_none() {
            ("Checking your mixer…", "")
        } else {
            match m.goal() {
                Goal::Reinstall => (
                    "Update RC2FX",
                    "Any RC2FX settings are saved first and carried over.",
                ),
                Goal::Setup => (
                    "Install RC2FX",
                    "RC2FX is built into official RØDE firmware and starts with the mixer.",
                ),
                Goal::Stock => (
                    "Restore stock firmware",
                    "Official RØDE firmware, unchanged. Your RC2FX settings stay on this computer.",
                ),
            }
        };
        let found = idle && m.goal() == Goal::Setup && m.usb().len() == 1;
        let ready = m.finished && !m.stock && m.run.is_none();
        let mut card = div().v_flex().gap_4()
            .when(!self.reduced_motion, |card| {
                card.child(
                    div()
                        .w_full()
                        .h(px(if idle { 96. } else if ready { 150. } else { 120. }))
                        .child(self.onboarding_signature.clone()),
                )
            })
            .child(
            div()
                .v_flex()
                .items_center()
                .text_center()
                .gap_4()
                .child(if found {
                    div().text_3xl().font_bold().child("Install RC2FX").into_any_element()
                } else if ready {
                    onboarding::headline("Clean signal.", "You're ready.").into_any_element()
                } else {
                    div().text_size(px(if idle { 32. } else { 40. })).font_bold().child(title).into_any_element()
                })
                .when(found, |d| {
                    d.child(div().max_w(px(540.)).text_size(px(17.))
                        .line_height(relative(1.55)).text_color(rgb(MUTED)).child(format!(
                            "Noise suppression, EQ and compression, running on your {}. Choose your firmware below.", m.device.label()
                        )))
                })
                .when(!found && !detail.is_empty(), |d| {
                    d.child(div().max_w(px(540.)).text_size(px(17.))
                        .line_height(relative(1.55)).text_color(rgb(MUTED)).child(detail))
                }),
        );
        if idle {
            if !m.releases.is_empty() {
                card = card.child(
                    div()
                        .h_flex()
                        .items_start()
                        .gap_3()
                        .child(self.mode_card(m, false, cx))
                        .child(self.mode_card(m, true, cx)),
                );
            }
        } else {
            card = card.child(self.timeline(installer));
        }
        card
    }

    /// One way to flash the mixer, with its own release menu. Choosing a
    /// release in either card also chooses that card.
    fn mode_card(&self, m: &Model, stock: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = m.stock == stock;
        let (title, detail) = match (stock, m.answering()) {
            (true, _) => ("Stock firmware", "Removes RC2FX"),
            (false, true) => ("RC2FX firmware", "Settings carried over"),
            (false, false) => ("RC2FX firmware", "Starts with the mixer"),
        };
        let choice = m.choice_for(stock);
        let releases: Vec<_> = m
            .releases_for(stock)
            .map(|(index, r)| {
                let chosen = choice
                    .as_ref()
                    .is_some_and(|(c, v)| c == &r.channel && v == &r.version);
                (index, format!("{} {}", r.version, r.channel), chosen)
            })
            .collect();
        let label = choice
            .map(|(channel, version)| format!("RØDE {version} {channel}"))
            .unwrap_or_else(|| "No release".into());
        let app = cx.entity().downgrade();
        let menu = Button::new(if stock {
            "installer-release-stock"
        } else {
            "installer-release-rc2fx"
        })
        .small()
        .label(label)
        .dropdown_caret(true)
        .disabled(m.busy.is_some() || releases.is_empty())
        .dropdown_menu(move |mut menu, _, _| {
            for (index, name, chosen) in &releases {
                let app = app.clone();
                let index = *index;
                menu = menu.item(PopupMenuItem::new(name.clone()).checked(*chosen).on_click(
                    move |_, _, cx| {
                        let _ = app.update(cx, |app, cx| {
                            app.installer_event(cx, |m| {
                                m.select(stock, index);
                                None
                            })
                        });
                    },
                ));
            }
            menu
        });
        div()
            .id(if stock {
                "installer-mode-stock"
            } else {
                "installer-mode-rc2fx"
            })
            .v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .p_4()
            .rounded_lg()
            .border_2()
            .cursor_pointer()
            .border_color(rgb(if selected { ACCENT } else { surface().line }))
            .bg(rgb(if selected {
                surface().selected
            } else {
                surface().raised
            }))
            .when(!selected, |c| {
                c.hover(|s| s.border_color(rgb(surface().line_strong)))
            })
            .child(div().font_semibold().child(title))
            .child(paragraph(detail, MUTED))
            .child(div().h_flex().pt_2().child(menu))
            .on_click(cx.listener(move |app, _, _, cx| {
                app.installer_event(cx, |m| {
                    if m.stock != stock {
                        m.set_stock(stock);
                    }
                    None
                })
            }))
    }

    /// The run's steps: finished ones as one line, the current one in full,
    /// later ones dimmed.
    fn timeline(&self, installer: &Installer) -> Div {
        let m = &installer.model;
        let mut list = div().v_flex().gap_1();
        for step in STEPS {
            let title = match step {
                Step::Start if m.stock => "Wait for the mixer",
                _ => step_title(step),
            };
            // A finished run did every step.
            let state = if m.finished && m.run.is_none() {
                StepState::Done
            } else {
                m.state(step)
            };
            // The current operation is the focus, not a second dashboard of
            // completed and future steps. Keep its actual message, errors and
            // measured progress. A finished run retains the final receipt.
            if matches!(state, StepState::Pending)
                || (matches!(state, StepState::Done)
                    && !(m.finished && m.run.is_none() && step == Step::Start))
            {
                continue;
            }
            let icon_cell = |child: AnyElement| {
                div()
                    .w(px(18.))
                    .h(px(20.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(child)
            };
            let row = match &state {
                StepState::Done => div()
                    .h_flex()
                    .items_start()
                    .gap_3()
                    .py_1()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(icon_cell(
                        Icon::new(IconName::Check)
                            .text_color(rgb(ACCENT))
                            .into_any_element(),
                    ))
                    .child(div().flex_1().min_w_0().child(self.done_text(m, step))),
                _ => {
                    let (line, color, marker) = match &state {
                        StepState::Failed(e) => (
                            e.clone(),
                            DANGER,
                            Icon::new(IconName::CircleX)
                                .text_color(rgb(DANGER))
                                .into_any_element(),
                        ),
                        StepState::Waiting(why) => (
                            why.clone(),
                            MUTED,
                            Spinner::new().color(rgb(ACCENT).into()).into_any_element(),
                        ),
                        _ => (
                            installer
                                .progress
                                .as_ref()
                                .map(progress_text)
                                .filter(|t| !t.is_empty())
                                .unwrap_or_else(|| "Working…".into()),
                            MUTED,
                            Spinner::new().color(rgb(ACCENT).into()).into_any_element(),
                        ),
                    };
                    let mut body = div()
                        .v_flex()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_semibold().child(title))
                        .child(paragraph(line, color));
                    if state == StepState::Running
                        && let Some(p) = installer.progress.as_ref().filter(|p| p.total > 0)
                    {
                        body =
                            body.child(ProgressBar::new("installer-progress").value(
                                (p.completed as f32 / p.total as f32 * 100.).clamp(0., 100.),
                            ));
                    }
                    div()
                        .h_flex()
                        .items_start()
                        .gap_3()
                        .my_2()
                        .py_3()
                        .child(icon_cell(marker))
                        .child(body)
                }
            };
            list = list.child(row);
        }
        list
    }

    fn done_text(&self, m: &Model, step: Step) -> String {
        match step {
            Step::Prepare => m
                .prepared()
                .map(|p| {
                    if p.stock {
                        return format!("Firmware ready: stock RØDE {} {}", p.version, p.channel);
                    }
                    let mut parts = vec!["RC2FX"];
                    if !p.settings.is_empty() {
                        parts.push("your settings");
                    }
                    if !p.key.is_empty() {
                        parts.push("SSH access");
                    }
                    let with = match parts.split_last() {
                        Some((last, [])) => (*last).to_owned(),
                        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
                        None => String::new(),
                    };
                    format!(
                        "Firmware ready: RØDE {} {} with {with}",
                        p.version, p.channel
                    )
                })
                .unwrap_or_else(|| "Firmware ready".into()),
            Step::Backup
                if m.run
                    .as_ref()
                    .or(m.last.as_ref())
                    .is_some_and(|r| r.backed_up) =>
            {
                "Settings saved".into()
            }
            Step::Backup => "No RC2FX settings to save".into(),
            Step::UsbMode => "In USB update mode".into(),
            Step::Flash => "Firmware sent to the mixer".into(),
            Step::Start if m.stock => "Back in normal mode".into(),
            Step::Start => "RC2FX running".into(),
        }
    }

    /// Administrator access on systems that need it for flashing: what it is
    /// for, and the one button that opens the system's password prompt.
    fn access_card(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        let asking = installer.access == Access::Asking;
        let (detail, color) = match &installer.access {
            Access::Asking => ("Answer your system's password prompt to continue.".to_owned(), MUTED),
            Access::Refused(why) => (format!("Access wasn't granted. {why}"), WARN),
            _ => (
                "Administrator access lets RC2FX switch the mixer into update mode and install firmware. Your password is handled by the system, not stored by RC2FX.".to_owned(),
                MUTED,
            ),
        };
        div()
            .v_flex()
            .gap_4()
            .p_6()
            .mt_4()
            .rounded_xl()
            .bg(rgb(surface().panel))
            .border_1()
            .border_color(rgb(surface().line))
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .child("Administrator access"),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(relative(1.5))
                    .text_color(rgb(color))
                    .child(detail),
            )
            .child(
                div().h_flex().justify_end().child(
                    Button::new("installer-grant-access")
                        .primary()
                        .large()
                        .h(px(52.))
                        .px(px(28.))
                        .rounded(px(26.))
                        .label(match &installer.access {
                            Access::Asking => "Waiting for access…",
                            Access::Refused(_) => "Try again",
                            _ => "Grant access",
                        })
                        .disabled(asking)
                        .on_click(cx.listener(|app, _, _, cx| app.grant_installer_access(cx))),
                ),
            )
    }

    fn duo_option(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        desk::card().w_full().v_flex().gap_3()
            .child(div().h_flex().items_center().gap_3()
                .child(div().flex_1().text_sm().font_semibold().child("Allow experimental Duo flashing"))
                .child(Switch::new("allow-experimental-duo")
                    .checked(installer.model.allow_duo)
                    .disabled(installer.busy() || installer.model.run.is_some() || installer.confirming)
                    .on_click(cx.listener(|app, value: &bool, _, cx| {
                        match connection::remember_duo(*value) {
                            Ok(()) => app.installer_event(cx, |m| m.set_allow_duo(*value)),
                            Err(e) => { app.message = format!("Could not save Duo preference: {e}"); cx.notify(); }
                        }
                    }))))
            .child(paragraph(rc2fx_installer::DUO_WARNING, WARN))
            .child(paragraph("Saved on this computer. Every Duo flash still requires risk confirmation. Switching this off does not uninstall an existing Duo runtime.", MUTED))
    }

    /// Options for working on RC2FX itself.
    fn developer_options(&self, installer: &Installer, cx: &mut Context<Self>) -> Stateful<Div> {
        let m = &installer.model;
        let small_viewport =
            !self.session.connected && installer.controls_scroll.bounds().size.height < px(500.);
        let card = desk::card().w_full().child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .v_flex()
                                .gap_1()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_semibold().child("SSH access"))
                                .child(paragraph(
                                    "Adds this computer's RC2FX key to the firmware, so you can log in to the mixer as root. Development commands use the same key.",
                                    MUTED,
                                ))
                                .child(
                                    div()
                                        .mt_1()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(surface().sunken))
                                        .text_xs()
                                        .font_family(Theme::global(cx).mono_font_family.clone())
                                        .text_color(rgb(TEXT))
                                        // The mixer doesn't report its network
                                        // address, so the command says where it goes.
                                        .child(format!(
                                            "ssh -i {} root@<mixer's IP>",
                                            installer.key.display()
                                        )),
                                )
                                .child(paragraph(
                                    "The mixer's IP address is on its screen under Settings › Network.",
                                    FAINT,
                                )),
                        )
                        .child(
                            Switch::new("installer-ssh")
                                .checked(m.ssh && !m.stock)
                                .disabled(m.stock || m.running() || m.busy.is_some())
                                .on_click(cx.listener(|app, value: &bool, _, cx| {
                                    let ssh = *value;
                                    app.installer_event(cx, |m| {
                                        m.set_ssh(ssh);
                                        None
                                    })
                                })),
                        ),
                );
        div()
            .id("installer-developer-section")
            .anchor_scroll(Some(installer.developer_anchor.clone()))
            .w_full()
            .pt_3()
            .border_t_1()
            .border_color(rgb(surface().line))
            .child(
                Collapsible::new()
                    .when(!small_viewport, |disclosure| {
                        disclosure.motion_id("installer-developer-options")
                    })
                    .open(installer.developer_open)
                    .child(
                        Button::new("installer-developer-toggle")
                            .ghost()
                            .w_full()
                            .justify_start()
                            .icon(if installer.developer_open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(if m.ssh {
                                "Developer options · SSH access on"
                            } else {
                                "Developer options"
                            })
                            .on_click(cx.listener(|app, _, window, cx| {
                                let standalone = !app.session.connected;
                                if let Some(installer) = app.installer.as_mut() {
                                    installer.developer_open = !installer.developer_open;
                                    let height = installer.controls_scroll.bounds().size.height;
                                    if standalone
                                        && installer.developer_open
                                        && height > px(0.)
                                        && height < px(500.)
                                    {
                                        installer.developer_anchor.scroll_to(window, cx);
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .content(div().pt_3().child(card)),
            )
    }

    /// The action card's footer: leaving or cancelling, and the next step.
    fn installer_actions(&self, installer: &Installer, cx: &mut Context<Self>) -> Div {
        if installer.needs_access() {
            return div()
                .h_flex()
                .justify_end()
                .pt_4()
                .border_t_1()
                .border_color(rgb(surface().line))
                .child(
                    Button::new("close-installer")
                        .ghost()
                        .label(if self.session.connected {
                            "Close"
                        } else {
                            "Exit installer"
                        })
                        .disabled(installer.busy() || installer.access == Access::Asking)
                        .on_click(cx.listener(|app, _, _, cx| app.close_installer(cx))),
                );
        }
        let m = &installer.model;
        let busy = installer.busy();
        let failed = m.run.as_ref().is_some_and(|r| r.failed.is_some());
        let finished = m.finished && m.run.is_none();
        let idle = m.run.is_none() && !finished;
        let note = if m.running() {
            "Keep the mixer on and connected."
        } else if idle && !m.privileged {
            "Grant administrator access to continue."
        } else if idle {
            "Takes a few minutes. Audio stops while the mixer updates."
        } else {
            ""
        };
        let footer = |buttons: Div| {
            div()
                .v_flex()
                .gap_3()
                .pt_4()
                .border_t_1()
                .border_color(rgb(surface().line))
                .when(!note.is_empty(), |footer| {
                    footer.child(div().text_xs().text_color(rgb(FAINT)).child(note))
                })
                .child(buttons)
        };
        let mut bar = div().h_flex().flex_wrap().justify_end().gap_2();
        if m.running() {
            bar = bar.when(m.can_cancel() || m.cancelling(), |bar| {
                bar.child(
                    Button::new("installer-cancel")
                        .label(if m.cancelling() {
                            "Cancelling…"
                        } else {
                            "Cancel"
                        })
                        .disabled(m.cancelling())
                        .on_click(cx.listener(|app, _, _, cx| {
                            if let Some(installer) = app.installer.as_mut()
                                && installer.model.cancel()
                            {
                                installer.runner.cancel();
                            }
                            cx.notify();
                        })),
                )
            });
            return footer(bar);
        }
        if failed {
            bar = bar.child(
                Button::new("installer-stop")
                    .ghost()
                    .label("Stop")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.installer_event(cx, |m| {
                            m.stop();
                            None
                        })
                    })),
            );
        } else if finished && (m.stock || self.session.connected) {
            bar = bar.child(
                Button::new("installer-again")
                    .ghost()
                    .label("Install again")
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.installer_event(cx, |m| {
                            m.start_over();
                            None
                        })
                    })),
            );
        } else {
            bar = bar.child(
                Button::new("close-installer")
                    .ghost()
                    .label(if finished {
                        "Done"
                    } else if self.session.connected {
                        "Close"
                    } else {
                        "Exit installer"
                    })
                    .disabled(busy)
                    .on_click(cx.listener(|app, _, _, cx| app.close_installer(cx))),
            );
        }
        enum Action {
            Close,
            Controls,
            Retry,
            Start,
        }
        // Inside Settings the mixer controls are already open.
        let (label, action) = if finished && (m.stock || self.session.connected) {
            ("Done", Action::Close)
        } else if finished {
            ("Open mixer controls", Action::Controls)
        } else if failed {
            ("Retry", Action::Retry)
        } else {
            let label = match m.goal() {
                _ if !m.needs_confirmation() => "Continue",
                Goal::Reinstall => "Update…",
                Goal::Setup => "Install…",
                Goal::Stock => "Flash stock firmware…",
            };
            (label, Action::Start)
        };
        let enabled = match action {
            Action::Close | Action::Controls | Action::Retry => !busy,
            Action::Start => m.can_start(),
        };
        footer(
            bar.child(
                Button::new("installer-primary")
                    .primary()
                    .large()
                    .h(px(52.))
                    .px(px(28.))
                    .font_semibold()
                    .rounded(px(26.))
                    .label(label)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |app, _, window, cx| match action {
                        Action::Close => app.close_installer(cx),
                        Action::Controls => app.open_mixer_controls(cx),
                        Action::Retry => app.start_installer(window, cx),
                        Action::Start => app.start_installer(window, cx),
                    })),
            ),
        )
    }
}

#[cfg(test)]
mod access_step_tests {
    use super::{Access, Model, flow::Run, needs_access};

    #[test]
    fn permission_is_required_only_until_granted() {
        let model = Model::new();
        for access in [
            Access::Needed,
            Access::Asking,
            Access::Refused("Dismissed".into()),
        ] {
            assert!(needs_access(&model, &access));
        }
        for access in [Access::Granted, Access::NotNeeded] {
            assert!(!needs_access(&model, &access));
        }
    }

    #[test]
    fn lost_access_never_hides_running_progress_or_a_completion_receipt() {
        let mut model = Model::new();
        model.run = Some(Run::default());
        assert!(!needs_access(&model, &Access::Needed));
        model.run.as_mut().unwrap().failed = Some("Access ended".into());
        assert!(needs_access(&model, &Access::Needed));
        assert!(!needs_access(&model, &Access::Granted));
        model.run = None;
        model.finished = true;
        assert!(!needs_access(&model, &Access::Needed));
    }
}
