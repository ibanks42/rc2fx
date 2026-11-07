mod app_update;
mod connection;
mod desk;
mod dev_page;
mod developer;
mod diagnostics;
mod draft_files;
mod editing;
mod editor;
mod eq_controls;
mod events;
mod graph;
mod home;
mod inputs;
mod installer;
mod interchange;
mod level_history;
mod licenses;
mod line_overlay;
mod live_line;
mod onboarding;
mod presets;
mod restart;
mod routing;
mod selection;
mod settings;
mod state_updates;
mod target_layout;
mod telemetry;
mod telemetry_view;
mod ui;
mod usb;
mod versions;
mod versions_dialog;

use desk::surface;
use gpui_kit::component::{
    button::*,
    input::{Input, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    *,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
// GPUI's test-support feature also exports an attribute named `test`.
#[cfg(test)]
use core::prelude::v1::test;
use rc2fx_core::*;
use rc2fx_transport::{Command, EventKind, Transport};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

// Text, accent and problem colors are shared by every appearance; the
// surfaces come from `surface()` (desk.rs). Cyan and violet mark processing;
// amber and red mark problems.
const TEXT: u32 = 0xf2f4f7;
const MUTED: u32 = 0x939cad;
const FAINT: u32 = 0x737c91;
const ACCENT: u32 = 0x5ce1e6;
const ACCENT_HOVER: u32 = 0x7ee8ec;
const ACCENT_ACTIVE: u32 = 0x3fc9cf;
const VIOLET: u32 = 0xa78bfa;
const WARN: u32 = 0xf5c451;
const DANGER: u32 = 0xf17e7e;
const DANGER_BG: u32 = 0x3a2030;
/// Primary actions are white pills.
const PRIMARY_HOVER: u32 = 0xffffff;
const PRIMARY_ACTIVE: u32 = 0xd9dde4;

/// The USB link to the mixer, or why there isn't one.
enum Backend {
    Live(Transport),
    Unavailable(String),
}

impl Backend {
    fn new() -> (Self, String) {
        match Transport::new() {
            Ok(transport) => (
                Self::Live(transport),
                "Connect to load your mixer’s channels and settings".into(),
            ),
            Err(error) => (Self::Unavailable(error.clone()), error),
        }
    }

    fn send(&mut self, command: Command) -> std::result::Result<(), String> {
        match self {
            Self::Live(transport) => transport.send(command),
            Self::Unavailable(error) => Err(error.clone()),
        }
    }

    fn try_recv(&mut self) -> Option<rc2fx_transport::Event> {
        match self {
            Self::Live(transport) => transport.try_recv(),
            Self::Unavailable(_) => None,
        }
    }
}

struct Rc2FxApp {
    session: Session,
    transport: Backend,
    generation: u64,
    /// The USB serial of the mixer the session belongs to.
    connected_mixer: Option<String>,
    /// What the connected mixer reported; `None` is unknown.
    device_versions: Option<rc2fx_core::DeviceVersions>,
    app_update: app_update::AppUpdate,
    /// Shows developer controls in Settings; grants nothing by itself.
    developer_mode: bool,
    /// The page `RC2FX_DEV_PAGE` asked for, until it has been opened.
    dev_page: Option<dev_page::DevPage>,
    ssh: developer::Ssh,
    /// The address typed for the login command. Kept for this run of the app,
    /// across reconnects; the pinned host key rejects a wrong mixer.
    ssh_address: Entity<InputState>,
    /// The RC2FX mixer software this app installs, read at start.
    bundled_mixer_software: Option<String>,
    selected: usize,
    inputs: inputs::ShowInputs,
    effect: Effect,
    band: usize,
    channel: Channel,
    editor_epoch: u64,
    import: Option<interchange::ImportDraft>,
    import_error: Option<String>,
    file_busy: bool,
    closing_prompt: bool,
    editors: Vec<editor::ParameterEditor>,
    /// The control whose value is being typed, if any.
    editing_value: Option<editor::Parameter>,
    /// A save was refused for an older revision; it is sent again once the
    /// mixer's current settings arrive (`settle_conflict`).
    resave: bool,
    /// Waiting for the refresh that follows a refused save.
    awaiting_refresh: bool,
    /// Settings shows its Diagnostics page (telemetry, events, files).
    diagnostics_open: bool,
    /// The main column's scroll position, shared by the pages shown in it.
    content_scroll: ScrollHandle,
    /// The Linux USB permission screen's request (connection::usb_access).
    usb_rule: connection::UsbRule,
    /// Its "Show the exact file" disclosure.
    usb_rule_open: bool,
    /// Which Copy button just copied, briefly.
    copied: Option<&'static str>,
    /// Its read-only boxes: the rule's exact text, and the undo command.
    usb_rule_text: Entity<gpui_kit::component::input::TextareaState>,
    usb_undo_text: Entity<InputState>,
    subscriptions: Vec<Subscription>,
    pending: Option<PendingApply>,
    routing_pending: Option<PendingRouting>,
    restart_pending: Option<PendingRestart>,
    selection_pending: Option<PendingSelection>,
    /// This app asked for the current path change, so its failure is
    /// ours to report. A failure the mixer reports on its own (while
    /// shutting down for an update, say) is not.
    selection_asked: bool,
    settings_open: bool,
    /// Home is the page in front (below settings and the installer).
    home_open: bool,
    /// The installer is the page in front (it may keep running behind
    /// another page).
    installer_page: bool,
    telemetry_open: bool,
    /// The Telemetry section's reset is waiting for confirmation.
    confirm_reset: bool,
    /// The mixer's recent events, shown by the events window.
    event_log: Entity<events::EventLog>,
    events_window: Option<AnyWindowHandle>,
    /// When the open events window last asked for events, and whether that
    /// request is still out.
    events_polled: Option<Instant>,
    events_pending: bool,
    reset_pending: bool,
    diagnostics_pending: bool,
    presets: presets::PresetLibrary,
    connecting: bool,
    auto_connect: bool,
    /// The connection was dropped for a flash and resumes when it is done.
    paused_for_flash: bool,
    reconnect_at: Option<Instant>,
    stream_connected: bool,
    stream_error: Option<String>,
    usb: usb::Observation,
    message: String,
    meter: Option<Meter>,
    meter_progress: HashMap<String, telemetry::Progress>,
    /// Recent output peaks per processed path, for the live line.
    levels: level_history::LevelHistories,
    /// Counts this view's renders; Home's line slot is claimed per render
    /// (line_overlay.rs).
    render_generation: u64,
    line_slots: line_overlay::SharedSlots,
    onboarding_signature: Entity<onboarding::Signature>,
    /// The window's size and scale at this view's last render.
    layout: line_overlay::Layout,
    /// Cached system policy, from GPUI/Base's platform motion integration.
    /// This is not an RC2FX preference; old persisted values are ignored.
    reduced_motion: bool,
    system_motion_checked: Instant,
    engine_seen: Option<Instant>,
    graph_bounds: Rc<Cell<Bounds<Pixels>>>,
    eq_curve: RefCell<Option<graph::EqCurve>>,
    dragging_band: bool,
    was_fresh: bool,
    /// What Home last drew that can change with time alone.
    home_key: Option<(String, String, Vec<bool>)>,
    installer: Option<installer::Installer>,
    /// Keeps keyboard focus in the window so Esc returns to the overview.
    focus: FocusHandle,
    /// The last `message` considered for a toast (`toast_message`).
    toasted: String,
}

impl Rc2FxApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (transport, message) = Backend::new();
        let owner = cx.weak_entity();
        let onboarding_signature = cx.new(|cx| onboarding::Signature::new(owner, window, cx));
        let mut app = Self {
            session: Session::default(),
            transport,
            generation: 0,
            connected_mixer: None,
            device_versions: None,
            app_update: Default::default(),
            developer_mode: connection::remembered_developer(),
            dev_page: dev_page::DevPage::from_env(),
            ssh: Default::default(),
            ssh_address: developer::address_input(window, cx),
            bundled_mixer_software: versions::bundled_mixer_software(),
            selected: 0,
            inputs: inputs::ShowInputs::default(),
            effect: Effect::Suppression,
            band: 0,
            channel: Channel::Left,
            editor_epoch: 0,
            import: None,
            import_error: None,
            file_busy: false,
            closing_prompt: false,
            editors: Vec::new(),
            editing_value: None,
            resave: false,
            awaiting_refresh: false,
            diagnostics_open: false,
            content_scroll: ScrollHandle::new(),
            usb_rule: connection::UsbRule::Idle,
            usb_rule_open: false,
            copied: None,
            usb_rule_text: cx.new(|cx| {
                gpui_kit::component::input::TextareaState::new(window, cx)
                    .default_value(rc2fx_installer::helper::USB_RULE)
            }),
            usb_undo_text: cx
                .new(|cx| InputState::new(window, cx).default_value(connection::USB_UNDO)),
            subscriptions: Vec::new(),
            pending: None,
            routing_pending: None,
            restart_pending: None,
            selection_pending: None,
            selection_asked: false,
            settings_open: false,
            home_open: true,
            installer_page: false,
            telemetry_open: connection::remembered_telemetry(),
            confirm_reset: false,
            event_log: cx.new(|_| events::EventLog::new()),
            events_window: None,
            events_polled: None,
            events_pending: false,
            reset_pending: false,
            diagnostics_pending: false,
            presets: presets::PresetLibrary::new(window, cx),
            connecting: false,
            auto_connect: false,
            paused_for_flash: false,
            reconnect_at: None,
            stream_connected: false,
            stream_error: None,
            usb: usb::Observation::default(),
            message,
            meter: None,
            meter_progress: HashMap::new(),
            levels: Default::default(),
            render_generation: 0,
            line_slots: Default::default(),
            onboarding_signature,
            layout: (window.viewport_size(), window.scale_factor()),
            reduced_motion: cx.reduce_motion(),
            system_motion_checked: Instant::now(),
            engine_seen: None,
            graph_bounds: Rc::new(Cell::new(Bounds::default())),
            eq_curve: RefCell::new(None),
            dragging_band: false,
            was_fresh: false,
            home_key: None,
            installer: None,
            focus: cx.focus_handle(),
            toasted: String::new(),
        };
        window.focus(&app.focus, cx);
        app.connect(cx);
        // Notification only, in the background; a failure here stays quiet.
        app.check_app_update(false, window, cx);
        app.open_dev_page(window, cx);
        let view = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            view.update(cx, |app, cx| {
                let close = app.confirm_close(window, cx);
                if close {
                    app.close_events(cx);
                }
                close
            })
            .unwrap_or(true)
        });
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if this
                    .update_in(cx, |app, window, cx| app.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        app
    }

    fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.file_busy {
            self.message = "Finish or cancel the file dialog before closing.".into();
            cx.notify();
            return false;
        }
        if let Some(verdict) = self.confirm_close_installer(window, cx) {
            return verdict;
        }
        if !self.session.any_dirty()
            && self.pending.is_none()
            && !self.presets.writing()
            && self.routing_pending.is_none()
            && !self.transition_busy()
        {
            return true;
        }
        if self.closing_prompt {
            return false;
        }
        self.closing_prompt = true;
        let detail = if self.pending.is_some()
            || self.presets.writing()
            || self.routing_pending.is_some()
            || self.transition_busy()
        {
            "A change already sent to the mixer may still finish. Any remaining local edits will be discarded."
        } else {
            "Your local edits have not been saved to the mixer."
        };
        let discard = cx.weak_entity();
        let close = cx.weak_entity();
        ui::confirm(
            window,
            cx,
            ui::Confirm {
                eyebrow: None,
                title: "Close without saving your edits?".into(),
                detail: detail.into(),
                cancel: Some("Keep editing"),
                action: "Discard and close".into(),
            },
            move |window, cx| {
                let _ = discard.update(cx, |app, cx| {
                    app.closing_prompt = false;
                    app.close_events(cx);
                });
                window.remove_window();
            },
            move |_, cx| {
                let _ = close.update(cx, |app, cx| {
                    app.closing_prompt = false;
                    cx.notify();
                });
            },
        );
        false
    }

    fn target(&self) -> &Target {
        &self.session.draft[self.selected.min(self.session.draft.len() - 1)]
    }
    fn target_mut(&mut self) -> &mut Target {
        let i = self.selected.min(self.session.draft.len() - 1);
        &mut self.session.draft[i]
    }

    fn command(&mut self, command: Command) -> bool {
        match self.transport.send(command) {
            Ok(()) => true,
            Err(e) => {
                self.message = e;
                false
            }
        }
    }

    /// Connect to the mixer on USB 1. Until connected, or until Disconnect,
    /// the app keeps trying every few seconds.
    fn connect(&mut self, cx: &mut Context<Self>) {
        self.begin_connection(false, cx);
    }

    fn begin_connection(&mut self, background_retry: bool, cx: &mut Context<Self>) {
        // Only the scheduled retry path may retain a previous observation.
        // An explicit connection starts a new lineage, even with a timer pending.
        self.auto_connect = true;
        self.home_open = true;
        self.reconnect_at = None;
        self.generation += 1;
        self.diagnostics_pending = false;
        self.reset_events(cx);
        self.session.disconnect();
        self.device_versions = None;
        self.ssh.clear();
        self.usb.clear();
        self.inputs = inputs::ShowInputs::default();
        self.pending = None;
        self.resave = false;
        self.awaiting_refresh = false;
        self.routing_pending = None;
        self.restart_pending = None;
        self.selection_pending = None;
        self.presets.reset();
        self.meter = None;
        self.meter_progress.clear();
        self.levels.clear();
        self.engine_seen = None;
        self.stream_connected = false;
        self.stream_error = None;
        self.connecting = self.command(Command::Connect {
            generation: self.generation,
        });
        if self.connecting && !background_retry {
            self.message.clear();
        }
        cx.notify();
    }

    fn disconnect(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.diagnostics_pending = false;
        self.reset_events(cx);
        self.auto_connect = false;
        self.reconnect_at = None;
        self.generation += 1;
        self.command(Command::Disconnect {
            generation: self.generation,
        });
        self.session.disconnect();
        self.device_versions = None;
        self.ssh.clear();
        self.usb.clear();
        self.inputs = inputs::ShowInputs::default();
        self.pending = None;
        self.resave = false;
        self.awaiting_refresh = false;
        self.routing_pending = None;
        self.restart_pending = None;
        self.selection_pending = None;
        self.presets.reset();
        self.connecting = false;
        self.stream_connected = false;
        self.stream_error = None;
        self.meter = None;
        self.meter_progress.clear();
        self.levels.clear();
        self.engine_seen = None;
        self.message = "Disconnected • mixer processing is independent of this app".into();
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some()
            || self.presets.busy()
            || self.file_busy
            || !self.stream_connected
            || self.transition_busy()
        {
            return;
        }
        match self.session.begin_apply() {
            Ok(pending) => {
                if self.command(Command::Apply {
                    generation: self.generation,
                    update: pending.request.clone(),
                }) {
                    self.pending = Some(pending);
                    self.message = "Saving settings to mixer…".into();
                }
            }
            Err(error) => self.message = error.to_string(),
        }
        cx.notify();
    }

    fn receive(
        &mut self,
        value: serde_json::Value,
        acknowledged: bool,
        snapshot: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_draft = self.session.draft.clone();
        let previous_generation = self.session.saved.as_ref().map(|s| s.engine.generation);
        let state = match DeviceState::from_json(&value.to_string()) {
            Ok(state) => state,
            Err(error) => {
                self.message = format!(
                    "Device state rejected: {error}. This app requires a matching controller with schema {} and explicit processing-path selection.",
                    SCHEMA_VERSION
                );
                if acknowledged {
                    self.pending = None;
                    self.session.conflict = true;
                    self.awaiting_refresh |= self.command(Command::Refresh {
                        generation: self.generation,
                    });
                }
                return;
            }
        };
        let mut runtime_observed = false;
        if acknowledged {
            if let Some(pending) = self.pending.take() {
                match self.session.acknowledge(&pending, state) {
                    Ok(()) => {
                        self.message = "Saved to mixer • waiting for applied telemetry".into()
                    }
                    Err(error) => {
                        self.message = error.to_string();
                        self.session.conflict = true;
                        self.awaiting_refresh |= self.command(Command::Refresh {
                            generation: self.generation,
                        });
                    }
                }
            }
        } else {
            let preserve_snapshot_runtime = snapshot
                && self.stream_connected
                && self
                    .engine_seen
                    .is_some_and(|seen| seen.elapsed() < Duration::from_secs(30));
            let received = state_updates::receive(
                &mut self.session,
                state,
                self.pending.as_ref(),
                self.presets.loading_revision(),
                snapshot,
                preserve_snapshot_runtime,
            );
            let received = received.map(|received| {
                runtime_observed = received.runtime_observed;
                received.update
            });
            match received {
                Ok(RemoteUpdate::Stale) => return,
                Ok(RemoteUpdate::Conflict)=>self.message="Mixer settings changed elsewhere. Keep your draft or reload the mixer settings.".into(),
                Ok(RemoteUpdate::Loaded)=>self.message="Mixer settings loaded".into(),
                Ok(_)=>{},Err(error)=>{
                    self.message=error.to_string();
                    return;
                },
            }
        }
        if runtime_observed {
            self.engine_seen = Some(Instant::now());
        } else if previous_generation != self.session.saved.as_ref().map(|s| s.engine.generation) {
            // A settings ACK is not an observation of live audio. A different
            // engine lifetime cannot inherit the prior generation's timestamp.
            self.engine_seen = None;
        }
        if previous_generation != self.session.saved.as_ref().map(|s| s.engine.generation)
            || self
                .session
                .saved
                .as_ref()
                .is_some_and(|s| s.engine.status != EnginePhase::Running)
        {
            self.meter = None;
            self.meter_progress.clear();
            self.levels.clear();
        }
        self.selected = self.selected.min(self.session.draft.len() - 1);
        if self.session.draft != previous_draft || self.editors.is_empty() {
            self.rebuild_editors(window, cx);
        }
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Base follows Linux portal changes itself. Its native macOS/Windows
        // readers need reapplying to notice settings changed while we're open.
        // On Linux this reuses the existing subscription; it starts no new one.
        if self.system_motion_checked.elapsed() >= Duration::from_secs(1) {
            gpui_kit::base::apply_system_reduce_motion(cx);
            self.system_motion_checked = Instant::now();
        }
        let mut changed = self.reduced_motion != cx.reduce_motion();
        self.reduced_motion = cx.reduce_motion();
        let mut meter_arrived = false;
        for _ in 0..64 {
            let Some(event) = self.transport.try_recv() else {
                break;
            };
            if event.generation != self.generation {
                continue;
            }
            changed = true;
            match event.kind {
                EventKind::Selection(result) => self.selection_reply(result),
                EventKind::RestartEngine(result) => self.restart_reply(result),
                EventKind::Inputs(result) => self.receive_inputs(result, window, cx),
                EventKind::Diagnostics(result) => self.save_diagnostics(result, window, cx),
                EventKind::Settings(result) => {
                    self.installer_event(cx, |m| m.settings_read(result))
                }
                EventKind::Routing(result) => self.routing_reply(result),
                // A failed read leaves the versions unknown; it is not an error
                // worth interrupting the user for.
                EventKind::Version(result) => self.device_versions = result.ok().flatten(),
                EventKind::Ssh { id, result } => self.receive_ssh(id, result, cx),
                EventKind::Connected { mixer, state } => {
                    self.connecting = false;
                    self.message.clear();
                    // Edits carry over to the same mixer, not to another one.
                    if self.connected_mixer.as_ref() != Some(&mixer) {
                        let draft = self
                            .session
                            .dirty()
                            .then(|| std::mem::take(&mut self.session.draft));
                        let selection_draft = self
                            .session
                            .selection_dirty()
                            .then(|| self.session.selection_draft.clone());
                        self.session = Session::default();
                        if let Some(draft) = draft {
                            self.session.draft = draft;
                        }
                        if let Some(selection_draft) = selection_draft {
                            self.session.selection_draft = selection_draft;
                        }
                    }
                    self.connected_mixer = Some(mixer);
                    self.receive(state, false, false, window, cx);
                    self.open_dev_page(window, cx);
                    // Versions change only across a restart: read once per connection.
                    self.command(Command::Version {
                        generation: self.generation,
                    });
                    if self.presets.open {
                        self.list_presets();
                    }
                }
                EventKind::Preset(result) => self.preset_reply(result, window, cx),
                EventKind::Applied(state) => self.receive(state, true, false, window, cx),
                EventKind::State(state) => self.receive(state, false, false, window, cx),
                EventKind::Refreshed(state) => {
                    self.receive(state, false, true, window, cx);
                    self.awaiting_refresh = false;
                }
                EventKind::Engine(value) => match serde_json::from_value::<EngineStatus>(value)
                    .ok()
                    .filter(|engine| engine.validate().is_ok())
                {
                    Some(engine) => {
                        let reset = self
                            .session
                            .saved
                            .as_ref()
                            .is_none_or(|s| s.engine.generation != engine.generation)
                            || engine.status != EnginePhase::Running;
                        if self.session.receive_engine(engine) {
                            if reset {
                                self.meter = None;
                                self.meter_progress.clear();
                                self.levels.clear();
                            }
                            self.engine_seen = Some(Instant::now());
                        }
                    }
                    None => {
                        self.message = "Invalid engine status received".into();
                        self.engine_seen = None;
                        self.meter = None;
                    }
                },
                EventKind::Meter(value) => {
                    if let Ok(meter) = serde_json::from_value::<Meter>(value)
                        && self.session.saved.as_ref().is_some_and(|state| {
                            telemetry::receive_meter(
                                state,
                                &meter,
                                &mut self.meter_progress,
                                &mut self.engine_seen,
                                Instant::now(),
                            )
                        })
                    {
                        self.levels.receive(
                            Instant::now(),
                            meter.targets.iter().map(|t| (t.id.as_str(), t.output_peak)),
                        );
                        meter_arrived = true;
                        self.meter = Some(meter);
                    }
                }
                EventKind::Usb(value) => self.usb.receive(value, Instant::now()),
                EventKind::ResetDiagnostics(result) => self.reset_reply(result, window, cx),
                EventKind::DiagnosticsEvents(result) => self.events_reply(result, cx),
                EventKind::StreamStatus { connected, message } => {
                    self.stream_connected = connected;
                    self.stream_error = if connected { None } else { Some(message) };
                    if connected {
                        // The mixer may have been flashed or restarted meanwhile.
                        self.command(Command::Version {
                            generation: self.generation,
                        });
                        self.ssh.clear();
                        self.read_ssh();
                    } else {
                        // Never show a pre-restart observation as current.
                        self.device_versions = None;
                        self.ssh.clear();
                        // A refresh asked for before the loss won't arrive.
                        self.awaiting_refresh = false;
                        self.usb.clear();
                        // Durable device generations remain ordered across
                        // reconnect. Preserve the observed high-water mark;
                        // only the new stream can reconcile uncertain writes.
                        self.routing_pending = None;
                        self.restart_pending = None;
                        self.selection_pending = None;
                        self.meter = None;
                        self.meter_progress.clear();
                        self.levels.clear();
                        self.engine_seen = None;
                    }
                }
                // Only this app writes the mixer's settings, so a refused
                // save means our revision was behind our own last change:
                // read the current settings, keep the edits, save again.
                EventKind::Conflict => {
                    self.pending = None;
                    self.session.conflict = true;
                    self.resave = true;
                    self.awaiting_refresh = self.command(Command::Refresh {
                        generation: self.generation,
                    });
                }
                EventKind::Failure(message) => {
                    self.pending = None;
                    self.connecting = false;
                    self.message = message;
                    if !self.session.connected && self.auto_connect {
                        self.reconnect_at = Some(Instant::now() + Duration::from_secs(3));
                    }
                }
            }
        }
        if self.session.conflict && !self.awaiting_refresh {
            changed = true;
            self.settle_conflict(cx);
        }
        let fresh = self.fresh_meter().is_some();
        changed |= self.usb.expire(Instant::now());
        changed |= self.poll_installer(cx);
        if self
            .session
            .saved
            .as_ref()
            .is_some_and(|s| s.selection.status == SelectionPhase::Idle)
        {
            self.selection_asked = false;
        }
        // While the mixer is being flashed its controls and their errors mean
        // nothing: hand the window to the installer. Once flashed, look for
        // the mixer again; reconnecting is how the installer sees RC2FX back.
        if self.installer_flashing() {
            if self.session.connected || self.auto_connect {
                self.disconnect(window, cx);
                self.message = "The mixer is updating.".into();
                self.paused_for_flash = true;
            }
        } else if std::mem::take(&mut self.paused_for_flash) {
            // The pre-flash connection result belongs to an earlier device
            // state. Start a new observation after the expected USB transition.
            self.message.clear();
            self.auto_connect = true;
            self.reconnect_at = Some(Instant::now());
        }
        self.refresh_inputs();
        self.events_tick(cx);
        // Freshness and warnings expire with time, not events: when meters
        // stop while the connection stays up, nothing else would redraw
        // Home. Compared on this existing 50 ms tick; no extra frames are
        // scheduled, and a redraw happens only when what Home shows changes.
        let home_key = (self.session.connected && self.home_open).then(|| self.home_key());
        // The overview's strips show every reading as it arrives.
        let overview = home_key.is_some() && !self.settings_open && !self.installer_page;
        if changed
            || fresh != self.was_fresh
            || home_key != self.home_key
            || (overview && meter_arrived)
        {
            cx.notify();
        }
        self.toast_message(window, cx);
        self.was_fresh = fresh;
        self.home_key = home_key;
        if !self.session.connected
            && !self.connecting
            && self.auto_connect
            && self.reconnect_at.is_some_and(|t| Instant::now() >= t)
        {
            self.begin_connection(true, cx);
        }
    }

    fn fresh_meter(&self) -> Option<(&TargetMeter, bool)> {
        self.fresh_meter_for(&self.target().id)
    }

    fn fresh_meter_for(&self, target_id: &str) -> Option<(&TargetMeter, bool)> {
        if !self.session.connected || !self.stream_connected {
            return None;
        }
        telemetry::fresh_meter(
            self.session.saved.as_ref()?,
            self.meter.as_ref()?,
            &self.meter_progress,
            target_id,
            self.engine_seen,
            Instant::now(),
        )
    }

    /// What the mixer reports for the selected path: the latency it reports
    /// there, and the lookahead it runs there. Both None while offline or
    /// before anything is applied, so the editor states relative costs only.
    fn reported_lookahead(&self) -> editor::Reported {
        let id = self.target().id.clone();
        let live_lookahead = self
            .session
            .saved
            .as_ref()
            .and_then(|state| state.config.targets.iter().find(|target| target.id == id))
            .map(|target| target.suppression.lookahead);
        let latency_samples = self
            .fresh_meter_for(&id)
            .map(|(meter, _)| meter.latency_samples)
            .filter(|samples| *samples > 0);
        editor::Reported {
            latency_samples,
            live_lookahead,
        }
    }

    /// The connection, path-change, restart and engine states that outrank
    /// everything else, shared by the status line and Home.
    fn system_status(&self) -> Option<(&'static str, u32)> {
        Some(if self.connecting {
            ("Connecting", WARN)
        } else if !self.session.connected {
            ("Offline draft", MUTED)
        } else if !self.stream_connected {
            ("Connection interrupted", WARN)
        } else if self.selection_busy() {
            ("Changing processing paths", WARN)
        } else if self.restart_busy() {
            (self.processing_transition_label(), WARN)
        } else if self.selection_failed() {
            ("Processing path change failed", WARN)
        } else if self
            .session
            .saved
            .as_ref()
            .and_then(telemetry::engine_error)
            .is_some()
        {
            (telemetry::EngineErrorNotice::TITLE, WARN)
        } else {
            return None;
        })
    }

    /// Audio routing that isn't active, where the engine has processing
    /// controls. Shared by the status line and Home.
    fn routing_status(&self) -> Option<(&'static str, u32)> {
        let state = self.session.saved.as_ref()?;
        if !state.engine.mode.has_processing_controls() {
            return None;
        }
        Some(match state.routing.status {
            RoutingPhase::Active => return None,
            RoutingPhase::Unmanaged => ("Processing not routed", MUTED),
            RoutingPhase::Starting => ("Connecting audio", WARN),
            RoutingPhase::Bypassing => ("Restoring stock audio", WARN),
            RoutingPhase::Bypassed => ("RC2FX audio off · settings saved", WARN),
            RoutingPhase::Error => ("Audio routing needs attention", WARN),
        })
    }

    fn status(&self) -> (&'static str, u32) {
        if let Some(status) = self.system_status() {
            status
        } else if self.session.conflict {
            ("Reload needed", WARN)
        } else if self.pending.is_some() || self.presets.writing() {
            ("Saving", WARN)
        } else if self.session.any_dirty() {
            ("Unsaved changes", WARN)
        } else if self
            .session
            .saved
            .as_ref()
            .is_some_and(DeviceState::stock_selected)
        {
            ("Stock audio · no processing paths", MUTED)
        } else if self
            .session
            .saved
            .as_ref()
            .is_some_and(DeviceState::can_enable_processing)
        {
            ("Processing disabled · settings saved", MUTED)
        } else if self.unprocessed_source().is_some() {
            ("Selected input has no RC2FX processing", MUTED)
        } else if self.session.saved.as_ref().is_some_and(|state| {
            !state
                .selection
                .active_target_ids
                .contains(&self.target().id)
        }) {
            ("Selected path is inactive · effects saved", MUTED)
        } else if let Some(status) = self.routing_status() {
            status
        } else if let Some(warning) = self.audio_warning() {
            (warning.label(), WARN)
        } else if self.target().bypass {
            ("Selected channel effects bypassed", MUTED)
        } else if self.session.is_applied() && self.fresh_meter().is_some() {
            if self.meter.as_ref().is_some_and(|meter| meter.synthetic) {
                ("Applied · simulation", ACCENT)
            } else {
                ("Applied · DSP advancing", ACCENT)
            }
        } else {
            ("Saved · audio unconfirmed", MUTED)
        }
    }

    fn audio_warning(&self) -> Option<telemetry::Warning> {
        self.audio_warning_for(&self.target().id)
    }

    fn audio_warning_for(&self, target_id: &str) -> Option<telemetry::Warning> {
        let (meter, synthetic) = self.fresh_meter_for(target_id)?;
        let state = self.session.saved.as_ref()?;
        if synthetic || state.routing.status != RoutingPhase::Active {
            return None;
        }
        let suppression_active = state.config.revision == state.engine.applied_revision
            && state.selection.active_target_ids.contains(&meter.id)
            && state.config.targets.iter().any(|target| {
                target.id == meter.id
                    && !target.bypass
                    && target.suppression.enabled
                    && target.suppression.wet > 0.
            });
        self.meter_progress
            .get(&meter.id)?
            .warning_for(Instant::now(), suppression_active)
    }
}

impl Rc2FxApp {
    fn apply_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("apply")
            .primary()
            .h(px(38.))
            .px(px(18.))
            .rounded(px(19.))
            .font_semibold()
            .label("Save to mixer")
            .disabled(
                !self.session.connected
                    || !self.stream_connected
                    || self.pending.is_some()
                    || self.presets.busy()
                    || self.file_busy
                    || self.transition_busy()
                    || self.session.conflict
                    || !self.session.dirty(),
            )
            .on_click(cx.listener(|app, _, _, cx| app.apply(cx)))
    }

    /// Edits not yet on the mixer: a floating bar over the
    /// page with where the edits are, Discard and Save to mixer.
    fn draft_bar(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.pending.is_some()
            || self.presets.busy()
            || self.file_busy
            || self.transition_busy();
        let changed: Vec<String> = self
            .session
            .saved
            .as_ref()
            .map(|saved| {
                self.session
                    .draft
                    .iter()
                    .filter(|t| saved.config.targets.iter().find(|s| s.id == t.id) != Some(*t))
                    .map(|t| self.target_name(t))
                    .collect()
            })
            .unwrap_or_default();
        let text = if self.pending.is_some() {
            div().child("Saving changes to the mixer…")
        } else if self.session.conflict {
            div().child("Reload the mixer's settings to continue.")
        } else {
            let place = match changed.len() {
                0 => String::new(),
                1..=3 => format!(" on {}", changed.join(", ")),
                n => format!(" on {n} channels"),
            };
            div()
                .h_flex()
                .child(
                    div()
                        .font_medium()
                        .text_color(rgb(TEXT))
                        .child("Unsaved edits"),
                )
                .child(format!("{place} · not on the mixer yet"))
        };
        div()
            .h_flex()
            .items_center()
            .gap(px(14.))
            .pl(px(20.))
            .pr(px(8.))
            .py(px(8.))
            .rounded_full()
            .bg(rgb(0x1a1d27))
            .border_1()
            .border_color(rgb(surface().line_strong))
            .shadow_lg()
            .text_size(px(13.5))
            .text_color(rgb(MUTED))
            .whitespace_nowrap()
            .child(text)
            .child(
                Button::new("discard-draft")
                    .ghost()
                    .h(px(38.))
                    .px(px(16.))
                    .rounded(px(19.))
                    .label("Discard")
                    .disabled(busy || self.session.conflict || self.session.saved.is_none())
                    .on_click(cx.listener(|app, _, window, cx| {
                        app.session.reload_saved();
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    })),
            )
            .child(self.apply_button(cx))
    }

    /// The channel list: processed
    /// channels first, each with its live line and effects; then the rest by
    /// kind; and the filter at the foot.
    fn channel_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = !self.home_open && !self.settings_open && !self.installer_page;
        let sources: Vec<_> = self.show_sources().collect();
        let source_of = |id: &str| sources.iter().find(|s| s.target_id == id);
        let mut list = div()
            .id("target-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.))
            .pt(px(6.))
            .pb(px(12.));
        // Processed channels.
        let processed = self.processed_indexes();
        list = list.child(nav_heading("Processing", Some(processed.len())));
        for &index in &processed {
            let target = &self.session.draft[index];
            let selected = editing && self.inputs.selected.is_none() && index == self.selected
                || editing
                    && self
                        .inputs
                        .selected
                        .as_ref()
                        .is_some_and(|id| source_of(&target.id).is_some_and(|s| &s.id == id));
            list = list.child(self.processed_row(index, selected, cx));
        }
        // The rest, by kind: show sources in the show's order, then the
        // mixer's other paths.
        let mut groups: Vec<(target_layout::Group, Vec<AnyElement>)> = target_layout::Group::ALL
            .iter()
            .map(|g| (*g, Vec::new()))
            .collect();
        let mut push = |group: target_layout::Group, row: AnyElement| {
            if let Some((_, rows)) = groups.iter_mut().find(|(g, _)| *g == group) {
                rows.push(row);
            }
        };
        for source in &sources {
            let index = self
                .session
                .draft
                .iter()
                .position(|t| t.id == source.target_id);
            if index.is_some_and(|i| processed.contains(&i)) {
                continue;
            }
            let selected = editing
                && (self.inputs.selected.as_ref() == Some(&source.id)
                    || (self.inputs.selected.is_none() && index == Some(self.selected)));
            if source.fader.is_none() {
                continue;
            }
            let group = match index.map(|i| self.session.draft[i].kind) {
                Some(TargetKind::Mic) => target_layout::Group::Microphones,
                _ => target_layout::Group::Sources,
            };
            let id = source.id.clone();
            push(
                group,
                quiet_row(
                    ("source", source.source_id as usize),
                    target_layout::display_name(&source.name),
                    inputs::source_detail(source, None),
                    selected,
                )
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.inputs.selected = Some(id.clone());
                    app.home_open = false;
                    if let Some(index) = index {
                        app.selected = index;
                    }
                    app.band = 0;
                    app.channel = Channel::Left;
                    app.import = None;
                    app.import_error = None;
                    app.set_settings_open(false);
                    app.rebuild_editors(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        for (index, target) in self.session.draft.iter().enumerate() {
            if processed.contains(&index) || source_of(&target.id).is_some() {
                continue;
            }
            let group = target_layout::Group::of(target);
            let selected = editing && self.inputs.selected.is_none() && index == self.selected;
            let outputs = matches!(
                group,
                target_layout::Group::Headphones | target_layout::Group::Outputs
            );
            if !outputs {
                continue;
            }
            push(
                group,
                quiet_row(
                    ("target", index),
                    self.target_name(target),
                    String::new(),
                    selected,
                )
                .on_click(
                    cx.listener(move |app, _, window, cx| app.open_channel(index, window, cx)),
                )
                .into_any_element(),
            );
        }
        for (group, rows) in groups {
            if rows.is_empty() {
                continue;
            }
            list = list.child(nav_heading(group.label(), None)).children(rows);
        }
        if let Some(error) = &self.inputs.error {
            list = list.child(
                div()
                    .px(px(12.))
                    .pt_2()
                    .text_size(px(12.))
                    .text_color(rgb(WARN))
                    .child(format!(
                        "Last confirmed show · refresh unavailable: {error}"
                    )),
            );
        } else if self.inputs.inventory.is_none() && self.session.connected {
            list = list.child(
                div()
                    .px(px(12.))
                    .pt_2()
                    .text_size(px(12.))
                    .text_color(rgb(FAINT))
                    .child("Loading show inputs…"),
            );
        }
        let overview = !self.settings_open && !self.installer_page && self.home_open;
        let settings = self.settings_open || self.installer_page;
        div()
            .v_flex()
            .w(px(248.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(rgb(surface().line))
            .child(
                div().px(px(12.)).pt(px(14.)).child(
                    page_row(
                        "overview",
                        gpui_kit::assets::IconName::LayoutDashboard,
                        "Overview",
                        overview,
                    )
                    .on_click(cx.listener(|app, _, _, cx| app.open_overview(cx))),
                ),
            )
            .child(list)
            .child(
                div()
                    .px(px(12.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(rgb(surface().line))
                    .child(
                        page_row(
                            "settings",
                            gpui_kit::assets::IconName::Settings,
                            "Settings",
                            settings,
                        )
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.set_settings_open(true);
                            cx.notify();
                        })),
                    ),
            )
    }

    /// Shows a new outcome in `message` (an error, a saved file, a loaded
    /// preset) as a toast. The footer no longer carries it; progress steps
    /// ("Saving…", "… waiting for …") aren't worth interrupting for. Before a
    /// connection the connection screen shows `message` itself.
    fn toast_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message == self.toasted {
            return;
        }
        self.toasted = self.message.clone();
        if self.session.connected && !self.message.is_empty() && !progress_message(&self.message) {
            window.push_notification(self.message.clone(), cx);
        }
    }

    /// The overview of every channel, from the sidebar or Esc.
    fn open_overview(&mut self, cx: &mut Context<Self>) {
        self.home_open = true;
        self.set_settings_open(false);
        cx.notify();
    }

    /// The window's foot: a problem worth reading (the connection or its USB link, unsaved
    /// edits, an engine fault) or else how many channels are processed.
    /// Routine progress messages aren't shown; empty when there's nothing.
    fn footer(&self) -> Option<Div> {
        let (status, color) = self.status();
        let text = if let Some(error) = &self.stream_error {
            Some((error.clone(), WARN))
        } else if self.usb.needs_attention() {
            Some((self.usb.label().to_owned(), WARN))
        } else if color != ACCENT && color != MUTED {
            Some((status.to_owned(), color))
        } else {
            None
        };
        let row = div()
            .h_flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(30.))
            .px(px(20.))
            .py(px(6.))
            .text_size(px(12.5))
            .border_t_1()
            .border_color(rgb(surface().line));
        if let Some((text, color)) = text {
            return Some(
                row.text_color(rgb(color))
                    .child(
                        div()
                            .size(px(7.))
                            .rounded_full()
                            .flex_shrink_0()
                            .bg(rgb(color)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(text)),
            );
        }
        let processed = self.processed_indexes().len();
        (processed > 0).then(|| {
            row.text_color(rgb(MUTED))
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(ui::gradient()),
                )
                .child(format!(
                    "{processed} channel{} processed",
                    if processed == 1 { "" } else { "s" }
                ))
        })
    }

    /// A processed channel: its name, its first effects (or what is wrong),
    /// its live line, and a dot while it has edits not on the mixer.
    fn processed_row(
        &self,
        index: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let target = &self.session.draft[index];
        let on: Vec<&str> = desk::visible_effects(target)
            .into_iter()
            .filter(|e| target.effect_enabled(*e))
            .map(desk::short_effect)
            .collect();
        let (detail, problem) = match self.path_note(&target.id) {
            Some(note) => (note.to_owned(), true),
            None if target.bypass => ("Bypassed".to_owned(), false),
            None if on.is_empty() => ("No effects on".to_owned(), false),
            None if on.len() > 2 => (format!("{} +{}", on[..2].join(" · "), on.len() - 2), false),
            None => (on.join(" · "), false),
        };
        let edited = self
            .session
            .saved
            .as_ref()
            .and_then(|s| s.config.targets.iter().find(|t| t.id == target.id))
            .is_some_and(|saved| saved != target);
        let fresh = self.fresh_meter_for(&target.id).is_some();
        div()
            .id(("processed", index))
            .relative()
            .h_flex()
            .items_center()
            .gap(px(11.))
            .h(px(48.))
            .px(px(12.))
            .rounded(px(10.))
            .cursor_pointer()
            .text_size(px(14.))
            .text_color(rgb(if selected { TEXT } else { MUTED }))
            .when(selected, |r| r.bg(hsla(0., 0., 1., 0.07)))
            .when(!selected, |r| {
                r.hover(|s| s.bg(hsla(0., 0., 1., 0.035)).text_color(rgb(TEXT)))
            })
            .child(ui::dot(true, 7.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(self.target_name(target)))
                    .child(
                        div()
                            .mt(px(1.))
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(rgb(if problem { WARN } else { FAINT }))
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .w(px(46.))
                    .h(px(18.))
                    .flex_shrink_0()
                    .when(!self.reduced_motion, |d| {
                        d.child(self.indicator(
                            line_overlay::Source::Path(target.id.clone()),
                            fresh,
                            live_line::MINI,
                        ))
                    }),
            )
            .when(edited, |r| {
                r.child(
                    div()
                        .absolute()
                        .right(px(5.))
                        .top(px(21.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(rgb(TEXT)),
                )
            })
            .on_click(cx.listener(move |app, _, window, cx| app.open_channel(index, window, cx)))
    }

    /// The channel's name and tags, its Processing switch, and one line on
    /// where its effects apply.
    fn channel_header(&self, target: &Target, cx: &mut Context<Self>) -> impl IntoElement {
        let tag = |text: SharedString, color: u32| {
            div()
                .h(px(24.))
                .px(px(9.))
                .flex_shrink_0()
                .h_flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(surface().line_strong))
                .text_size(px(12.))
                .text_color(rgb(color))
                .child(text)
        };
        let fader = self
            .show_sources()
            .find(|s| s.target_id == target.id)
            .and_then(|s| s.fader);
        let processed = self.processed(&target.id);
        let id = target.id.clone();
        let mut switch = ui::toggle(
            "channel-processing",
            processed,
            false,
            !self.can_switch_path(&target.id),
        );
        if self.can_switch_path(&target.id) {
            switch = switch
                .on_click(cx.listener(move |app, _, _, cx| app.switch_path(&id, !processed, cx)));
        }
        div().pt(px(22.)).child(
            div()
                .h_flex()
                .items_end()
                .gap(px(16.))
                .child(
                    div()
                        .text_size(px(38.))
                        .line_height(relative(1.))
                        .font_bold()
                        .child(self.target_name(target)),
                )
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .pb(px(4.))
                        .child(tag(
                            if target.channels == 1 {
                                "Mono".into()
                            } else {
                                "Stereo".into()
                            },
                            MUTED,
                        ))
                        .when_some(fader, |d, f| {
                            d.child(tag(format!("Fader {f}").into(), MUTED))
                        })
                        .when_some(self.path_note(&target.id), |d, note| {
                            d.child(tag(note.into(), WARN))
                        })
                        .when(self.selection_busy(), |d| {
                            d.child(tag("Changing…".into(), WARN))
                        }),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_3()
                        .pb(px(3.))
                        .text_size(px(14.))
                        .text_color(rgb(MUTED))
                        .child("Processing")
                        .child(switch),
                ),
        )
    }

    fn engine_error_card(&self, cx: &mut Context<Self>) -> Option<Div> {
        let notice = self
            .session
            .saved
            .as_ref()
            .and_then(telemetry::engine_error)?;
        Some(
            desk::card()
                .border_color(rgb(WARN))
                .child(
                    div()
                        .font_semibold()
                        .text_color(rgb(WARN))
                        .child(telemetry::EngineErrorNotice::TITLE),
                )
                .child(div().text_sm().child(notice.message.to_owned()))
                .child(div().text_sm().text_color(rgb(MUTED)).child(format!(
                    "Last reported recovery status: {}",
                    notice.recovery_detail
                )))
                .child(
                    div().child(
                        Button::new("export-error-diagnostics")
                            .label("Export diagnostics…")
                            .disabled(
                                self.diagnostics_pending
                                    || self.file_busy
                                    || !self.session.connected,
                            )
                            .on_click(cx.listener(|app, _, _, cx| app.request_diagnostics(cx))),
                    ),
                ),
        )
    }

    /// Puts local edits back on top of the mixer's current settings when
    /// the revision moved under them. Only this app writes the mixer's
    /// settings (over USB 1), so that move is always one of our own changes
    /// and never a choice for the user. A refused save is sent again once.
    fn settle_conflict(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() || self.presets.writing() {
            return;
        }
        match self.session.rebase_edits() {
            Ok(()) => {
                if std::mem::take(&mut self.resave) {
                    self.apply(cx);
                }
            }
            Err(error) => {
                // The edits no longer fit the mixer's channels: the notice
                // below offers the one way on.
                self.resave = false;
                self.message = error.to_string();
            }
        }
    }

    /// Only when local edits can't be placed on the mixer's settings.
    fn conflict_card(&self, cx: &mut Context<Self>) -> Div {
        desk::card()
            .border_color(rgb(WARN))
            .child(
                div()
                    .font_semibold()
                    .text_color(rgb(WARN))
                    .child("Your edits don't fit the mixer's settings"),
            )
            .child(
                div().text_sm().text_color(rgb(MUTED)).child(
                    "They name channels the mixer no longer has. Reload the mixer's settings to continue editing.",
                ),
            )
            .child(
                div().child(
                    Button::new("reload")
                        .primary()
                        .rounded(px(18.))
                        .px(px(16.))
                        .label("Reload mixer settings")
                        .on_click(cx.listener(|app, _, window, cx| {
                            app.session.reload_saved();
                            app.rebuild_editors(window, cx);
                            cx.notify();
                        })),
                ),
            )
    }
}

/// A sidebar heading, with a count when given.
fn nav_heading(label: &'static str, count: Option<usize>) -> Div {
    div()
        .h_flex()
        .items_center()
        .px(px(12.))
        .pt(px(18.))
        .pb(px(8.))
        .text_size(px(11.))
        .font_semibold()
        .text_color(rgb(FAINT))
        .child(label.to_uppercase())
        .child(div().flex_1())
        .when_some(count, |d, n| {
            d.child(
                div()
                    .font_normal()
                    .text_color(rgb(ui::DIM))
                    .child(format!("{n}")),
            )
        })
}

/// Whether `message` reports a step still under way rather than an outcome.
fn progress_message(message: &str) -> bool {
    message.ends_with('…') || message.contains("waiting for") || message.starts_with("Requesting")
}

/// A page in the sidebar (Overview, Settings): an icon and its name.
fn page_row(
    id: &'static str,
    icon: gpui_kit::assets::IconName,
    label: &'static str,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap(px(11.))
        .h(px(40.))
        .px(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .text_size(px(14.))
        .font_medium()
        .text_color(rgb(TEXT))
        .when(selected, |r| r.bg(hsla(0., 0., 1., 0.07)))
        .when(!selected, |r| r.hover(|s| s.bg(hsla(0., 0., 1., 0.035))))
        .child(
            Icon::new(icon)
                .size(px(16.))
                .text_color(rgb(if selected { TEXT } else { MUTED })),
        )
        .child(label)
}

/// An unprocessed channel in the sidebar: a hollow dot, its name and where
/// it sits (its fader).
fn quiet_row(
    id: impl Into<ElementId>,
    name: String,
    meta: String,
    selected: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap(px(11.))
        .h(px(40.))
        .px(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .text_size(px(14.))
        .text_color(rgb(if selected { TEXT } else { MUTED }))
        .when(selected, |r| r.bg(hsla(0., 0., 1., 0.07)))
        .when(!selected, |r| {
            r.hover(|s| s.bg(hsla(0., 0., 1., 0.035)).text_color(rgb(TEXT)))
        })
        .child(ui::dot(false, 7.))
        .child(div().flex_1().min_w_0().truncate().child(name))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(rgb(ui::DIM))
                .child(meta),
        )
}

impl Rc2FxApp {
    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_generation += 1;
        self.layout = (window.viewport_size(), window.scale_factor());
        line_overlay::count(&line_overlay::APP_RENDERS);
        // This render's claims replace the last render's (line_overlay.rs).
        line_overlay::begin(&self.line_slots, self.render_generation, self.layout);
        // With a connection the installer lives in Settings instead.
        if self.installer.is_some() && !self.session.connected {
            return self.installer_screen(cx);
        }
        if !self.session.connected {
            return self.connection_screen(cx).into_any_element();
        }
        let target = self.target().clone();
        let mut content = div()
            .id("content-scroll")
            .track_scroll(&self.content_scroll)
            .v_flex()
            .gap_4()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px(px(48.))
            .pt(px(30.))
            .pb(px(120.));
        // Anything that needs a decision comes before editing.
        if let Some(card) = self.engine_error_card(cx) {
            content = content.child(card);
        }
        content = content.child(self.restart_panel(cx));
        if self.session.conflict {
            content = content.child(self.conflict_card(cx));
        }
        if let Some(card) = self.selection_notice(cx) {
            content = content.child(card);
        }
        if self.installer_page
            && let Some(installer) = self.installer.as_ref()
        {
            content = content.child(
                div()
                    .v_flex()
                    .gap_6()
                    .w_full()
                    .max_w(px(900.))
                    .mx_auto()
                    .pb_6()
                    .child(div().text_2xl().font_semibold().child("Firmware"))
                    .child(self.installer_body(installer, cx)),
            );
        } else if self.settings_open {
            content = content.child(self.settings_page(cx));
        } else if self.home_open {
            content = content.child(self.home_page(cx));
        } else if let Some(source) = self.unprocessed_source() {
            content = content.child(self.unprocessed_panel(source));
        } else {
            // Every channel shows its whole editor; the header's Processing
            // switch is what turns it on.
            content = content
                .child(self.channel_header(&target, cx))
                .child(self.flow_lane(cx))
                .child(self.render_editor(cx));
        }
        let footer = self.footer();
        let drafting = self.session.dirty() || self.pending.is_some();
        div()
            .id("app")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|app, event: &KeyDownEvent, _, cx| {
                let away = !app.home_open || app.settings_open || app.installer_page;
                if event.keystroke.key == "escape" && away {
                    app.open_overview(cx);
                }
            }))
            .v_flex()
            .size_full()
            .font_family("Inter")
            .bg(rgb(surface().bg))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.channel_nav(cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(content)
                            .when(drafting, |page| {
                                page.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom(px(22.))
                                        .h_flex()
                                        .justify_center()
                                        .child(self.draft_bar(cx)),
                                )
                            }),
                    ),
            )
            .children(footer)
            .into_any_element()
    }
}

impl Render for Rc2FxApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_page(window, cx)
    }
}

/// Native application's entry point.
pub fn run() {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .get(1)
        .is_some_and(|a| a == rc2fx_installer::helper::USB_RULE_FLAG)
    {
        // Started through pkexec by the USB permission screen: write the
        // embedded rule, reload udev, exit. No window, no other input.
        if let Err(error) = rc2fx_installer::helper::install_usb_rule() {
            eprintln!("rc2fx: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args
        .get(1)
        .is_some_and(|a| a == rc2fx_installer::helper::FLAG)
    {
        // The privileged installer process: no window, JSON lines on stdio.
        let (Some(root), Some(work)) = (args.get(2), args.get(3)) else {
            std::process::exit(2);
        };
        if let Err(error) = rc2fx_installer::helper::serve(root.into(), work.into()) {
            eprintln!("rc2fx installer helper: {error}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(debug_assertions)]
    if args
        .get(1)
        .is_some_and(|a| a == telemetry_view::preview::FLAG)
    {
        match args.get(2).and_then(|a| a.to_str()) {
            Some("events") => events::preview::run(),
            other => telemetry_view::preview::run(other != Some("old")),
        }
        return;
    }
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            desk::set_appearance(connection::remembered_appearance(), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1320.), px(860.)),
                    cx,
                ))),
                window_min_size: Some(size(px(960.), px(620.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("RC2FX".into()),
                    ..Default::default()
                }),
                ..ui::window_options()
            };
            // Home's animated indicators wrap the same app in a shell
            // (line_overlay.rs); RC2FX_ANIMATED_LINE=0 opens the window
            // exactly as before, with still indicators.
            if line_overlay::enabled() {
                gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| line_overlay::Shell::new(window, cx))
                })
                .map(|_| ())
            } else {
                gpui_kit::open_window(options, cx, |window, cx| {
                    cx.new(|cx| Rc2FxApp::new(window, cx))
                })
                .map(|_| ())
            }
            .expect("Open native RC2FX window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod message_tests {
    use super::progress_message;

    #[test]
    fn progress_steps_are_not_toasted_but_outcomes_are() {
        for step in [
            "Saving settings to mixer…",
            "Saved to mixer • waiting for applied telemetry",
            "Path change accepted • waiting for the mixer's result",
            "Requesting processing paths. Local effect edits are kept; saved effects will be used.",
            "Loading preset on the mixer…",
            "Collecting mixer diagnostics…",
        ] {
            assert!(progress_message(step), "{step}");
        }
        for outcome in [
            "Could not import EQ: Unsupported filter type: Allpass",
            "Saved settings file to /tmp/a.json",
            "Preset ‘Podcast’ saved on the mixer",
            "Enter a preset name of 1–128 bytes.",
            "Mixer settings changed elsewhere. Keep your draft or reload the mixer settings.",
        ] {
            assert!(!progress_message(outcome), "{outcome}");
        }
    }
}
