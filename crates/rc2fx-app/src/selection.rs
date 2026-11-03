use super::*;

impl Rc2FxApp {
    pub(super) fn selection_busy(&self) -> bool {
        self.selection_pending.is_some()
            || self
                .session
                .saved
                .as_ref()
                .is_some_and(|s| s.selection.status == SelectionPhase::Changing)
    }
    pub(super) fn transition_busy(&self) -> bool {
        self.restart_busy() || self.selection_busy()
    }
    fn selection_ready(&self) -> bool {
        self.session.connected
            && self.stream_connected
            && self.engine_seen.is_some()
            && !self.transition_busy()
            && self.pending.is_none()
            && self.routing_pending.is_none()
            && !self.presets.writing()
            && !self.file_busy
            && !self.session.conflict
            && (self.session.selection_dirty()
                || self
                    .session
                    .saved
                    .as_ref()
                    .is_some_and(|s| s.selection.status == SelectionPhase::Error))
            && self.session.begin_selection().is_ok()
    }
    fn change_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selection_ready() {
            return;
        }
        match self.session.begin_selection() {
            Ok(pending) => {
                if self.command(Command::Selection {
                    generation: self.generation,
                    request: pending.request.clone(),
                }) {
                    self.selection_pending = Some(pending);
                    self.selection_asked = true;
                    self.meter = None;
                    self.meter_progress.clear();
                    self.levels.clear();
                    self.message = "Requesting processing paths. Local effect edits are kept; saved effects will be used.".into();
                }
            }
            Err(error) => self.message = error.to_string(),
        }
        cx.notify();
    }
    pub(super) fn selection_reply(&mut self, result: std::result::Result<DeviceState, String>) {
        let previous_generation = self.session.saved.as_ref().map(|s| s.engine.generation);
        let Some(pending) = self.selection_pending.take() else {
            return;
        };
        match result.and_then(|state| {
            self.session
                .acknowledge_selection(&pending, state)
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => self.message = "Path change accepted • waiting for the mixer's result".into(),
            Err(error) => {
                self.message = format!("{error} Checking mixer state; no automatic retry.")
            }
        }
        if previous_generation != self.session.saved.as_ref().map(|s| s.engine.generation) {
            self.engine_seen = None;
        }
        self.meter = None;
        self.meter_progress.clear();
        self.levels.clear();
        self.command(Command::Refresh {
            generation: self.generation,
        });
    }

    /// A path change this app asked for failed.
    pub(super) fn selection_failed(&self) -> bool {
        self.selection_asked
            && self
                .session
                .saved
                .as_ref()
                .is_some_and(|s| s.selection.status == SelectionPhase::Error)
    }

    /// Processing is enabled for this path on the mixer.
    pub(super) fn processed(&self, id: &str) -> bool {
        self.session
            .saved
            .as_ref()
            .is_some_and(|state| state.config.is_selected(id))
    }

    /// What is wrong with a processed path, if anything. Unprocessed paths
    /// need no note: they are drawn differently.
    pub(super) fn path_note(&self, id: &str) -> Option<&'static str> {
        if self.session.connected && !self.stream_connected {
            return Some("Unconfirmed");
        }
        let state = self
            .session
            .saved
            .as_ref()
            .filter(|_| self.session.connected)?;
        if !state.config.is_selected(id) {
            return None;
        }
        if state.selection.active_target_ids.iter().any(|s| s == id) {
            (state.routing.status != RoutingPhase::Active
                || state.engine.status != EnginePhase::Running)
                .then_some("Not routed")
        } else if state.engine.mode == EngineMode::Native
            && state.routing.status == RoutingPhase::Bypassed
        {
            Some("RC2FX off")
        } else {
            Some("Not running")
        }
    }

    /// Whether this path's processing can be switched now.
    pub(super) fn can_switch_path(&self, id: &str) -> bool {
        self.session.saved.as_ref().is_some_and(|state| {
            state.selection.allowed && state.selection.qualified_target_ids.iter().any(|q| q == id)
        }) && self.session.connected
            && self.stream_connected
            && self.engine_seen.is_some()
            && !self.transition_busy()
            && self.pending.is_none()
            && self.routing_pending.is_none()
            && !self.presets.writing()
            && !self.file_busy
            && !self.session.conflict
    }

    /// Turn processing for one path on or off on the mixer right away. Other
    /// paths keep what the mixer has; effect edits stay in the draft.
    pub(super) fn switch_path(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if !self.can_switch_path(id) {
            return;
        }
        let Some(saved) = &self.session.saved else {
            return;
        };
        let mut selected = saved.config.selected_targets.clone();
        selected.retain(|v| v != id);
        if on {
            selected.push(id.to_owned());
        }
        match saved.config.canonical_selection(&selected) {
            Ok(ids) => {
                self.session.selection_draft = ids;
                self.change_selection(cx);
            }
            Err(error) => {
                self.message = error.to_string();
                cx.notify();
            }
        }
    }

    /// Shown when the path selection needs a decision: a settings file asked
    /// for other paths, or the last change failed.
    pub(super) fn selection_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let state = self.session.saved.as_ref()?;
        let failed = self.selection_failed();
        if self.selection_busy() || !(self.session.selection_dirty() || failed) {
            return None;
        }
        let changed = changed_paths(
            &state.config.selected_targets,
            &self.session.selection_draft,
        );
        let names: Vec<_> = changed
            .iter()
            .map(|id| {
                state
                    .config
                    .targets
                    .iter()
                    .find(|t| t.id == *id)
                    .map_or_else(|| (*id).to_owned(), |t| self.target_name(t))
            })
            .collect();
        let (title, detail) = if self.session.selection_dirty() {
            (
                "Processed channels differ from the mixer",
                format!("Changes: {}", names.join(", ")),
            )
        } else {
            (
                "Changing processed channels failed",
                state.selection.message.clone(),
            )
        };
        let mut card = desk::card()
            .border_color(rgb(WARN))
            .child(div().font_semibold().text_color(rgb(WARN)).child(title))
            .child(div().text_sm().text_color(rgb(MUTED)).child(detail));
        if let Err(error) = state
            .selection
            .check_capacity(&state.config, &self.session.selection_draft)
        {
            card = card.child(
                div()
                    .text_sm()
                    .text_color(rgb(WARN))
                    .child(error.to_string()),
            );
        }
        Some(
            card.child(
                div()
                    .h_flex()
                    .gap_2()
                    .child(
                        Button::new("apply-path-selection")
                            .primary()
                            .label(if self.session.selection_dirty() {
                                "Use these channels"
                            } else {
                                "Retry"
                            })
                            .disabled(!self.selection_ready())
                            .on_click(cx.listener(|app, _, _, cx| app.change_selection(cx))),
                    )
                    .child(
                        Button::new("reset-path-selection")
                            .ghost()
                            .label("Keep the mixer's")
                            .disabled(!self.session.selection_dirty() || self.transition_busy())
                            .on_click(cx.listener(|app, _, _, cx| {
                                if let Some(saved) = &app.session.saved {
                                    app.session.selection_draft =
                                        saved.config.selected_targets.clone();
                                }
                                cx.notify();
                            })),
                    ),
            ),
        )
    }
}

fn changed_paths<'a>(saved: &'a [String], draft: &'a [String]) -> Vec<&'a str> {
    saved
        .iter()
        .filter(|id| !draft.contains(id))
        .chain(draft.iter().filter(|id| !saved.contains(id)))
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::changed_paths;

    #[test]
    fn pending_changes_include_other_channels_when_the_editor_switches() {
        let saved = vec!["mic-1".into(), "usb-main".into()];
        let draft = vec!["mic-1".into(), "headphones-1".into()];
        assert_eq!(
            changed_paths(&saved, &draft),
            vec!["usb-main", "headphones-1"]
        );
        assert!(changed_paths(&saved, &saved).is_empty());
    }
}
