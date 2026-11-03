use super::*;
use rc2fx_transport::{PresetAction, PresetError, PresetReply};

enum Pending {
    List,
    Create,
    Load {
        revision: u64,
        original: Vec<Target>,
    },
    Delete,
}

pub(super) struct PresetLibrary {
    pub open: bool,
    items: Vec<PresetSummary>,
    name: Entity<InputState>,
    pending: Option<Pending>,
    confirm_delete: Option<String>,
}

impl PresetLibrary {
    pub fn new(window: &mut Window, cx: &mut Context<Rc2FxApp>) -> Self {
        Self {
            open: false,
            items: Vec::new(),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Preset name")),
            pending: None,
            confirm_delete: None,
        }
    }
    pub fn reset(&mut self) {
        self.items.clear();
        self.pending = None;
        self.confirm_delete = None;
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn loading_revision(&self) -> Option<u64> {
        match self.pending {
            Some(Pending::Load { revision, .. }) => Some(revision),
            _ => None,
        }
    }
    pub fn writing(&self) -> bool {
        self.busy() && !matches!(self.pending, Some(Pending::List))
    }
}

impl Rc2FxApp {
    fn presets_ready(&self) -> bool {
        self.session.connected
            && self.stream_connected
            && self.pending.is_none()
            && !self.presets.busy()
            && !self.file_busy
            && !self.transition_busy()
    }

    pub(super) fn list_presets(&mut self) {
        if !self.session.connected || self.presets.busy() {
            return;
        }
        if self.command(Command::Preset {
            generation: self.generation,
            action: PresetAction::List,
        }) {
            self.presets.pending = Some(Pending::List);
        }
    }

    fn create_preset(&mut self, cx: &mut Context<Self>) {
        if !self.presets_ready() || self.session.dirty() || self.session.conflict {
            return;
        }
        let name = self.presets.name.read(cx).value().trim().to_owned();
        if name.is_empty() || name.len() > 128 {
            self.message = "Enter a preset name of 1–128 bytes.".into();
            cx.notify();
            return;
        }
        let Some(saved) = &self.session.saved else {
            return;
        };
        let expected_revision = saved.config.revision;
        if self.command(Command::Preset {
            generation: self.generation,
            action: PresetAction::Create {
                name,
                expected_revision,
            },
        }) {
            self.presets.pending = Some(Pending::Create);
            self.message = "Saving a preset of the mixer’s current settings…".into();
        }
        cx.notify();
    }

    fn load_preset(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.presets_ready() || self.session.dirty() || self.session.conflict {
            return;
        }
        let Some(saved) = &self.session.saved else {
            return;
        };
        let revision = saved.config.revision;
        let original = self.session.draft.clone();
        if self.command(Command::Preset {
            generation: self.generation,
            action: PresetAction::Apply {
                id,
                expected_revision: revision,
            },
        }) {
            self.presets.pending = Some(Pending::Load { revision, original });
            self.message = "Loading preset on the mixer…".into();
        }
        cx.notify();
    }

    fn delete_preset(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.presets_ready() || self.presets.confirm_delete.as_ref() != Some(&id) {
            return;
        }
        if self.command(Command::Preset {
            generation: self.generation,
            action: PresetAction::Delete { id },
        }) {
            self.presets.pending = Some(Pending::Delete);
            self.message = "Deleting preset…".into();
        }
        cx.notify();
    }

    pub(super) fn preset_reply(
        &mut self,
        result: std::result::Result<PresetReply, PresetError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.presets.pending.take() else {
            return;
        };
        let was_loading = matches!(pending, Pending::Load { .. });
        let previous = self.session.draft.clone();
        match (pending, result) {
            (_, Err(error)) => {
                self.message = error.message;
                if error.conflict || was_loading {
                    self.session.conflict = true;
                    self.awaiting_refresh |= self.command(Command::Refresh {
                        generation: self.generation,
                    });
                }
            }
            (Pending::List, Ok(PresetReply::List(items))) => self.presets.items = items,
            (Pending::Create, Ok(PresetReply::Created(preset))) => {
                self.message = format!("Preset ‘{}’ saved on the mixer", preset.name);
                self.presets
                    .name
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.list_presets();
            }
            (Pending::Delete, Ok(PresetReply::Deleted(id))) => {
                self.presets.items.retain(|p| p.id != id);
                self.presets.confirm_delete = None;
                self.message = "Preset deleted • current mixer settings are unchanged".into();
            }
            (Pending::Load { revision, original }, Ok(PresetReply::Applied(value))) => {
                let previous_generation = self.session.saved.as_ref().map(|s| s.engine.generation);
                let result = DeviceState::from_json(&value.to_string())
                    .and_then(|state| self.session.acknowledge_preset(revision, &original, state));
                if previous_generation != self.session.saved.as_ref().map(|s| s.engine.generation) {
                    self.engine_seen = None;
                    self.meter = None;
                    self.meter_progress.clear();
                    self.levels.clear();
                }
                match result {
                    Ok(()) if self.session.conflict => self.message = "Preset loaded. Your newer edits were kept on top; save them or discard them.".into(),
                    Ok(()) => self.message = "Preset saved to mixer • waiting for applied telemetry".into(),
                    Err(error) => {
                        self.message = error.to_string();
                        self.session.conflict = true;
                    }
                }
                // A stream event can precede the request's reply, including a later
                // controller's write. Refresh after resolving our snapshot.
                self.awaiting_refresh |= self.command(Command::Refresh {
                    generation: self.generation,
                });
            }
            _ => {
                self.message = "Unexpected preset response; reload mixer settings.".into();
                self.session.conflict = true;
                self.awaiting_refresh |= self.command(Command::Refresh {
                    generation: self.generation,
                });
            }
        }
        if previous != self.session.draft {
            self.selected = self.selected.min(self.session.draft.len() - 1);
            self.import = None;
            self.rebuild_editors(window, cx);
        }
    }

    /// Presets saved on the mixer: save the current settings, then load or
    /// delete each one.
    pub(super) fn preset_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = self.presets_ready();
        let can_snapshot = ready && !self.session.dirty() && !self.session.conflict;
        let refresh = Button::new("refresh-presets")
            .ghost()
            .small()
            .icon(IconName::RefreshCw)
            .tooltip("Refresh")
            .disabled(!self.session.connected || self.presets.busy())
            .on_click(cx.listener(|app, _, _, cx| {
                app.list_presets();
                cx.notify();
            }));
        let muted = |text: &'static str| {
            div()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(text)
        };
        if !self.session.connected {
            return settings::section("PRESETS", refresh, div().child(muted("Not connected.")));
        }
        let mut body = div().child(settings::row(
            div().child(Input::new(&self.presets.name)),
            Button::new("create-preset")
                .label("Save preset")
                .disabled(!can_snapshot || self.presets.items.len() >= 128)
                .on_click(cx.listener(|app, _, _, cx| app.create_preset(cx))),
        ));
        if self.session.dirty() || self.session.conflict {
            body = body.child(
                div()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(WARN))
                    .border_b_1()
                    .border_color(rgb(surface().line))
                    .child("Apply or reload your edits to save or load presets."),
            );
        }
        let mut list = div()
            .id("preset-list")
            .v_flex()
            .max_h(px(320.))
            .overflow_y_scroll();
        if self.presets.items.is_empty() {
            list = list.child(muted(if self.presets.busy() {
                "Loading…"
            } else {
                "No presets yet."
            }));
        }
        let last = self.presets.items.len().saturating_sub(1);
        for (index, preset) in self.presets.items.iter().enumerate() {
            let load_id = preset.id.clone();
            let delete_id = preset.id.clone();
            let confirm = self.presets.confirm_delete.as_ref() == Some(&preset.id);
            let mut actions = div().h_flex().gap_1().items_center();
            if confirm {
                actions = actions
                    .child(
                        Button::new(("cancel-delete", index))
                            .ghost()
                            .label("Cancel")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.presets.confirm_delete = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("delete-preset", index))
                            .danger()
                            .label("Delete")
                            .disabled(!ready)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.delete_preset(delete_id.clone(), cx)
                            })),
                    );
            } else {
                actions = actions
                    .child(
                        Button::new(("load-preset", index))
                            .label("Load")
                            .disabled(!can_snapshot)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.load_preset(load_id.clone(), cx)
                            })),
                    )
                    .child(
                        Button::new(("delete-preset", index))
                            .ghost()
                            .icon(IconName::Delete)
                            .tooltip("Delete")
                            .disabled(!ready)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.presets.confirm_delete = Some(delete_id.clone());
                                cx.notify();
                            })),
                    );
            }
            list = list.child(
                settings::row(preset.name.clone(), actions).when(index == last, |r| r.border_b_0()),
            );
        }
        settings::section("PRESETS", refresh, body.child(list))
    }
}
