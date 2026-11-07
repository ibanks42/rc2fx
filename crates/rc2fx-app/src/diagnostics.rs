use super::*;
use std::{path::PathBuf, time::SystemTime};

impl Rc2FxApp {
    pub(super) fn request_diagnostics(&mut self, cx: &mut Context<Self>) {
        if self.diagnostics_pending || self.file_busy || !self.session.connected {
            return;
        }
        self.diagnostics_pending = self.command(Command::Diagnostics {
            generation: self.generation,
        });
        if self.diagnostics_pending {
            self.message = "Collecting mixer diagnostics…".into();
        }
        cx.notify();
    }
    pub(super) fn save_diagnostics(
        &mut self,
        result: std::result::Result<serde_json::Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diagnostics_pending = false;
        if self.file_busy {
            self.message = "Finish the open file dialog, then export diagnostics again.".into();
            return;
        }
        let text = match result
            .and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string()))
        {
            Ok(text) => text,
            Err(error) => {
                self.message = format!("Could not collect diagnostics: {error}");
                return;
            }
        };
        let directory = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let picker = cx.prompt_for_new_path(&directory, Some("rc2fx-diagnostics.json"));
        self.file_busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result: std::result::Result<Option<PathBuf>, String> = async {
                let Some(path) = picker
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                let destination = path.clone();
                cx.background_executor()
                    .spawn(async move { interchange::write_export(&destination, &text) })
                    .await?;
                Ok(Some(path))
            }
            .await;
            let _ = this.update_in(cx, |app, _, cx| {
                app.file_busy = false;
                match result {
                    Ok(Some(path)) => {
                        app.message = format!("Saved diagnostics to {}", path.display())
                    }
                    Ok(None) => app.message = "Diagnostic export canceled".into(),
                    Err(error) => app.message = format!("Could not save diagnostics: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Rc2FxApp {
    /// Opens the Diagnostic events window, or brings it forward.
    pub(super) fn open_events(&mut self, cx: &mut Context<Self>) {
        let reopened = self.events_window.is_some();
        self.events_window = events::open(
            self.events_window,
            self.event_log.clone(),
            Some(cx.weak_entity()),
            cx,
        );
        if !reopened {
            // A fresh window reads from the start of what the mixer keeps.
            self.event_log.update(cx, |log, _| log.clear());
            self.events_polled = None;
        }
    }

    /// The Telemetry section's header controls: when its counters start,
    /// the events window and the reset.
    pub(super) fn telemetry_aside(&self, cx: &mut Context<Self>) -> Div {
        let connected = self.session.connected && self.stream_connected;
        let log = self.event_log.read(cx);
        let (since, epoch) = (log.since, log.epoch);
        let app = cx.weak_entity();
        let set_confirm = move |value: bool| {
            let app = app.clone();
            move |cx: &mut App| {
                let _ = app.update(cx, |app, cx| {
                    app.confirm_reset = value;
                    cx.notify();
                });
            }
        };
        let app = cx.weak_entity();
        div()
            .h_flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child(format!("Live · {}", events::since_label(since, epoch))),
            )
            .when(!self.confirm_reset, |aside| {
                aside.child(
                    Button::new("show-events")
                        .ghost()
                        .small()
                        .label("Show events…")
                        .on_click(cx.listener(|app, _, _, cx| app.open_events(cx))),
                )
            })
            .child(events::reset_control(
                "telemetry-reset",
                self.confirm_reset,
                connected && !self.reset_pending,
                set_confirm(true),
                set_confirm(false),
                move |cx| {
                    let _ = app.update(cx, |app, cx| app.reset_diagnostics(cx));
                },
            ))
    }
}

impl Rc2FxApp {
    /// Asks the mixer to restart its counters and event history.
    pub(super) fn reset_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.confirm_reset = false;
        self.event_log.update(cx, |log, cx| {
            log.confirm_reset = false;
            cx.notify();
        });
        if self.session.connected && !self.reset_pending {
            self.reset_pending = self.command(Command::ResetDiagnostics {
                generation: self.generation,
            });
            if self.reset_pending {
                self.message = "Resetting mixer diagnostics…".into();
            }
        }
        cx.notify();
    }

    pub(super) fn reset_reply(
        &mut self,
        result: std::result::Result<serde_json::Value, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_pending = false;
        match result {
            Ok(state) => {
                self.message = "Mixer diagnostics reset".into();
                self.event_log.update(cx, |log, cx| {
                    log.since = Some(SystemTime::now());
                    cx.notify();
                });
                // Read the reset entry, and whatever followed it, straight away.
                self.events_polled = None;
                self.receive(state, false, false, window, cx);
            }
            Err(error) => self.message = format!("Could not reset diagnostics: {error}"),
        }
    }

    pub(super) fn events_reply(
        &mut self,
        result: std::result::Result<DiagnosticsEvents, String>,
        cx: &mut Context<Self>,
    ) {
        self.events_pending = false;
        let received = SystemTime::now();
        let caught_up = result.as_ref().ok().map(|page| !page.more);
        let names: HashMap<String, String> = self
            .session
            .draft
            .iter()
            .map(|target| (target.id.clone(), self.target_name(target)))
            .collect();
        self.event_log.update(cx, |log, cx| {
            match result {
                Ok(page) => {
                    log.error = None;
                    if page.more {
                        // Catch up without waiting for the next interval.
                        self.events_polled = None;
                    }
                    log.receive_page(page, received, |id| {
                        names.get(id).cloned().unwrap_or_else(|| id.to_owned())
                    });
                }
                Err(error) => log.error = Some(format!("Could not read events: {error}")),
            }
            cx.notify();
        });
        match caught_up {
            Some(true) => {
                let log = self.event_log.read(cx);
                self.startup
                    .read_events(|from, until| log.xruns_between(from, until));
            }
            Some(false) => {}
            None => self.startup.give_up(),
        }
    }

    /// Called from the main poll: keeps the events window's view of the
    /// connection current and, while it is open, reads new events.
    pub(super) fn events_tick(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.events_window
            && handle.update(cx, |_, _, _| ()).is_err()
        {
            // Closed by the user: stop reading.
            self.events_window = None;
            self.events_pending = false;
        }
        let connected = self.session.connected && self.stream_connected;
        let period_ns = self.session.saved.as_ref().and_then(|state| {
            (state.engine.sample_rate > 0).then(|| {
                u64::from(state.engine.block_size) * 1_000_000_000
                    / u64::from(state.engine.sample_rate)
            })
        });
        if self.event_log.read(cx).period_ns != period_ns {
            self.event_log
                .update(cx, |log, _| log.period_ns = period_ns);
        }
        let epoch = self.meter.as_ref().map(|meter| meter.diagnostics_epoch);
        let log = self.event_log.read(cx);
        if log.connected != connected || epoch.is_some_and(|epoch| epoch != log.epoch) {
            self.event_log.update(cx, |log, cx| {
                log.connected = connected;
                if let Some(epoch) = epoch
                    && epoch != log.epoch
                {
                    // A reset this app did not just make or read about
                    // (another client's) restarts the counters at a time
                    // the app does not know.
                    if log.since.is_none_or(|at| {
                        SystemTime::now().duration_since(at).unwrap_or_default()
                            > Duration::from_secs(5)
                    }) {
                        log.since = None;
                    }
                    log.epoch = epoch;
                }
                cx.notify();
            });
        }
        // Telemetry reads the log once, to place faults from before this app
        // connected.
        let startup = self.telemetry_open && self.startup.wants_events();
        if (self.events_window.is_none() && !startup)
            || !connected
            || self.events_pending
            || self
                .events_polled
                .is_some_and(|at| at.elapsed() < events::POLL)
        {
            return;
        }
        self.events_polled = Some(Instant::now());
        let after = self.event_log.read(cx).next_seq.unwrap_or(0);
        self.events_pending = self.command(Command::DiagnosticsEvents {
            generation: self.generation,
            after,
        });
    }

    /// A new connection or none: forget the old mixer's events and any
    /// request whose reply will never arrive.
    pub(super) fn reset_events(&mut self, cx: &mut Context<Self>) {
        self.events_pending = false;
        self.events_polled = None;
        self.reset_pending = false;
        self.confirm_reset = false;
        self.event_log.update(cx, |log, cx| {
            log.clear();
            log.since = None;
            log.epoch = 0;
            log.confirm_reset = false;
            cx.notify();
        });
    }
}
