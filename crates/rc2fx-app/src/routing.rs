use super::*;

impl Rc2FxApp {
    fn routing_ready(&self, enabled: bool) -> bool {
        if !self.session.connected
            || !self.stream_connected
            || self.routing_pending.is_some()
            || self.transition_busy()
        {
            return false;
        }
        let Some(saved) = self.session.saved.as_ref() else {
            return false;
        };
        if !saved.engine.mode.has_processing_controls() {
            return false;
        }
        if enabled {
            saved.can_enable_processing()
                // Stopped DSP has no meters. A live event connection and the
                // service's explicit recovery eligibility gate this request.
                && self.engine_seen.is_some()
                && !self.session.dirty()
                && !self.session.conflict
                && self.pending.is_none()
                && !self.presets.writing()
                && !self.file_busy
        } else {
            saved.can_disable_processing()
        }
    }

    fn change_routing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.routing_ready(enabled) {
            return;
        }
        match self.session.begin_routing(enabled) {
            Ok(pending) => {
                if self.command(Command::Routing {
                    generation: self.generation,
                    request: pending.request.clone(),
                }) {
                    self.routing_pending = Some(pending);
                    self.message = if enabled {
                        "Starting mixer processing…"
                    } else {
                        "Returning to stock audio…"
                    }
                    .into();
                }
            }
            Err(error) => self.message = error.to_string(),
        }
        cx.notify();
    }

    pub(super) fn routing_reply(&mut self, result: std::result::Result<DeviceState, String>) {
        let Some(pending) = self.routing_pending.take() else {
            return;
        };
        let previous = self.session.saved.as_ref().map(|s| s.engine.generation);
        match result.and_then(|state| {
            self.session
                .acknowledge_routing(&pending, state)
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => {
                self.message = "Processing request accepted • waiting for mixer status".into()
            }
            Err(error) => {
                self.message = format!(
                    "{error} Checking mixer status; the request will not be retried automatically."
                )
            }
        }
        if previous != self.session.saved.as_ref().map(|s| s.engine.generation) {
            self.engine_seen = None;
            self.meter = None;
            self.meter_progress.clear();
            self.levels.clear();
        }
        self.command(Command::Refresh {
            generation: self.generation,
        });
    }

    /// Settings' Processing row: RC2FX's processing on the mixer, on or off
    /// for every processed channel at once. None when the mixer has no
    /// processing controls.
    pub(super) fn processing_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        let saved = self
            .session
            .saved
            .as_ref()
            .filter(|s| s.engine.mode.has_processing_controls())?;
        let phase = saved.routing.status;
        let enabled = matches!(phase, RoutingPhase::Active | RoutingPhase::Starting)
            || (saved.engine.mode == EngineMode::Native
                && phase == RoutingPhase::Error
                && saved.can_disable_processing());
        let ready = self.routing_ready(!enabled);
        let label = if !self.stream_connected {
            "Connection interrupted"
        } else if self.transition_busy() {
            "Changing processing…"
        } else if self.routing_pending.is_some() {
            "Updating…"
        } else if (self.pending.is_some() || self.presets.writing())
            && matches!(phase, RoutingPhase::Bypassing | RoutingPhase::Starting)
        {
            "Applying effects…"
        } else {
            match phase {
                RoutingPhase::Active => "Enabled",
                RoutingPhase::Bypassed => "Disabled · stock audio",
                RoutingPhase::Starting => "Starting processing…",
                RoutingPhase::Bypassing => "Restoring stock audio…",
                RoutingPhase::Error => "Needs attention",
                RoutingPhase::Unmanaged => "Unavailable",
            }
        };
        let mut hint = "Enable RC2FX effects processing.".to_owned();
        if !self.stream_connected {
            hint.push_str(" Waiting for the mixer connection.");
        } else if phase == RoutingPhase::Bypassed && !ready {
            hint.push_str(if self.session.dirty() || self.session.conflict {
                " Apply or reload your changes before enabling processing."
            } else {
                " Waiting for the mixer to finish saving and become ready."
            });
        } else if matches!(phase, RoutingPhase::Error | RoutingPhase::Unmanaged) {
            hint.push(' ');
            hint.push_str(&saved.routing.message);
        }
        // The switch says on or off; only other states get words.
        let steady = self.stream_connected && matches!(label, "Enabled" | "Disabled · stock audio");
        let mut switch = ui::toggle("rc2fx-processing", enabled, false, !ready);
        if ready {
            switch =
                switch.on_click(cx.listener(move |app, _, _, cx| app.change_routing(!enabled, cx)));
        }
        Some(settings::row(
            settings::labelled("Processing", hint),
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .when(!steady, |v| {
                    v.child(div().text_size(px(13.)).text_color(rgb(WARN)).child(label))
                })
                .child(switch),
        ))
    }
}
