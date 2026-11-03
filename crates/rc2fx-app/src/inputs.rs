use super::*;

#[derive(Default)]
pub(super) struct ShowInputs {
    pub inventory: Option<StockInventory>,
    pub selected: Option<String>,
    pub pending: bool,
    pub checked: Option<Instant>,
    pub error: Option<String>,
}

impl ShowInputs {
    // Filter on every access, including inventory cached before this UI policy.
    fn visible_sources(&self) -> impl Iterator<Item = &StockSource> {
        self.inventory
            .iter()
            .flat_map(|inventory| &inventory.inputs)
            .filter(|source| source.source_id != 11)
    }

    fn receive(&mut self, result: std::result::Result<StockInventory, String>) -> bool {
        self.pending = false;
        match result {
            Ok(inventory) if inventory.available => {
                self.error = None;
                self.inventory = Some(inventory);
                true
            }
            result => {
                self.error = Some(match result {
                    Ok(inventory) => inventory.message,
                    Err(error) => error,
                });
                // Preserve the last confirmed names/selection, with an explicit
                // stale label. A failed read does not prove assignments changed.
                false
            }
        }
    }
}

impl Rc2FxApp {
    pub(super) fn refresh_inputs(&mut self) {
        if self.session.connected
            && self.stream_connected
            && !self.inputs.pending
            && self.pending.is_none()
            && !self.presets.busy()
            && self.routing_pending.is_none()
            && self
                .inputs
                .checked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.inputs.checked = Some(Instant::now());
            self.inputs.pending = self.command(Command::Inputs {
                generation: self.generation,
            });
        }
    }

    pub(super) fn receive_inputs(
        &mut self,
        result: std::result::Result<StockInventory, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inputs.receive(result)
            && let Some(id) = self.inputs.selected.clone()
        {
            let source = self.show_sources().find(|s| s.id == id).cloned();
            if let Some(source) = source {
                if let Some(index) = self
                    .session
                    .draft
                    .iter()
                    .position(|t| t.id == source.target_id)
                    && self.selected != index
                {
                    self.selected = index;
                    self.rebuild_editors(window, cx);
                }
            } else {
                self.inputs.selected = None;
            }
        }
    }

    pub(super) fn show_sources(&self) -> impl Iterator<Item = &StockSource> {
        self.inputs.visible_sources()
    }

    pub(super) fn target_name(&self, target: &Target) -> String {
        self.show_sources()
            .find(|s| s.target_id == target.id)
            .map(|s| target_layout::display_name(&s.name))
            .or_else(|| target_layout::output_label(&target.id).map(str::to_owned))
            .unwrap_or_else(|| target_layout::display_name(&target.name))
    }

    pub(super) fn unprocessed_source(&self) -> Option<&StockSource> {
        self.inputs
            .selected
            .as_ref()
            .and_then(|id| self.show_sources().find(|s| &s.id == id))
            .filter(|s| !self.session.draft.iter().any(|t| t.id == s.target_id))
    }

    /// A show source with no RC2FX path: its name and fader, then why it
    /// can't be processed.
    pub(super) fn unprocessed_panel(&self, source: &StockSource) -> impl IntoElement {
        let note = if self.inputs.error.is_some() {
            format!(
                "{} · last confirmed show; refresh unavailable",
                assignment_label(source)
            )
        } else {
            format!("{} · saved on your RØDECaster", assignment_label(source))
        };
        div()
            .pt(px(22.))
            .child(
                div()
                    .text_size(px(38.))
                    .line_height(relative(1.))
                    .font_bold()
                    .child(target_layout::display_name(&source.name)),
            )
            .child(div().mt(px(10.)).text_size(px(13.5)).text_color(rgb(FAINT)).child(note))
            .child(
                div()
                    .mt(px(28.))
                    .py(px(44.))
                    .border_t_1()
                    .border_color(rgb(surface().line))
                    .v_flex()
                    .items_center()
                    .text_center()
                    .child(div().text_size(px(24.)).font_bold().child("No RC2FX path for this input yet."))
                    .child(
                        div()
                            .mt(px(10.))
                            .max_w(px(480.))
                            .text_size(px(14.))
                            .line_height(relative(1.55))
                            .text_color(rgb(MUTED))
                            .child(if source.source_id == 11 {
                                "SMART Pads audio is mixed internally by the RØDECaster. RC2FX doesn't have an independent processing path for it yet."
                            } else {
                                "It's listed in your RØDE show, but its RC2FX processing path isn't installed. This listing doesn't mean a device is connected or a call is active."
                            }),
                    ),
            )
    }
}

fn assignment_label(source: &StockSource) -> String {
    source.fader.map_or_else(
        || "Not assigned to a fader".into(),
        |f| format!("Fader {f}"),
    )
}

pub(super) fn source_detail(source: &StockSource, note: Option<&str>) -> String {
    let fader = source
        .fader
        .map_or_else(|| "Unassigned".to_owned(), |f| format!("Fader {f}"));
    match note {
        Some(note) if !source.target_id.is_empty() => format!("{fader} · {note}"),
        _ => fader,
    }
}

#[cfg(test)]
mod tests {
    use super::ShowInputs;
    use rc2fx_core::{StockInventory, StockSource};

    fn inventory(name: &str) -> StockInventory {
        StockInventory {
            available: true,
            show_name: "Show".into(),
            revision: "a".repeat(64),
            message: String::new(),
            inputs: vec![StockSource {
                id: "input-3".into(),
                source_id: 3,
                fader: Some(1),
                name: name.into(),
                color: "fffdba0e".into(),
                target_id: "mic-1".into(),
            }],
        }
    }

    #[test]
    fn assigned_pads_are_hidden_even_in_old_cached_inventory() {
        let mut cached = inventory("Mic 4");
        let mut pads = cached.inputs[0].clone();
        pads.id = "input-11".into();
        pads.source_id = 11;
        pads.name = "SMART Pads".into();
        pads.target_id.clear();
        pads.fader = Some(7);
        cached.inputs.push(pads);
        let mut inputs = ShowInputs {
            inventory: Some(cached.clone()),
            selected: Some("input-11".into()),
            ..ShowInputs::default()
        };
        assert_eq!(inputs.visible_sources().count(), 1);
        assert!(!inputs.visible_sources().any(|s| s.source_id == 11));
        assert!(inputs.receive(Ok(cached)));
        assert!(!inputs.visible_sources().any(|s| s.id == "input-11"));
        assert_eq!(inputs.inventory.as_ref().unwrap().inputs.len(), 2);
    }

    #[test]
    fn unassigned_sources_do_not_look_like_live_or_allocated_paths() {
        let mut source = inventory("CallMe 1").inputs.remove(0);
        source.fader = None;
        source.target_id.clear();
        assert_eq!(
            super::source_detail(&source, Some("Not routed")),
            "Unassigned"
        );
        assert_eq!(super::assignment_label(&source), "Not assigned to a fader");
        source.target_id = "callme-1".into();
        assert_eq!(
            super::source_detail(&source, Some("Not routed")),
            "Unassigned · Not routed"
        );
    }

    #[test]
    fn failed_refresh_retains_last_confirmed_assignment_until_success() {
        let mut inputs = ShowInputs::default();
        assert!(inputs.receive(Ok(inventory("Mic 4"))));
        inputs.selected = Some("input-3".into());
        inputs.pending = true;
        assert!(!inputs.receive(Err("connection closed".into())));
        assert!(!inputs.pending);
        assert_eq!(inputs.inventory.as_ref().unwrap().inputs[0].name, "Mic 4");
        assert_eq!(inputs.selected.as_deref(), Some("input-3"));
        assert_eq!(inputs.error.as_deref(), Some("connection closed"));
        let unavailable = StockInventory {
            available: false,
            inputs: vec![],
            revision: String::new(),
            message: "saved show unavailable".into(),
            show_name: String::new(),
        };
        assert!(!inputs.receive(Ok(unavailable)));
        assert_eq!(inputs.inventory.as_ref().unwrap().inputs[0].name, "Mic 4");
        assert!(inputs.receive(Ok(inventory("Renamed Mic"))));
        assert_eq!(
            inputs.inventory.as_ref().unwrap().inputs[0].name,
            "Renamed Mic"
        );
        assert!(inputs.error.is_none());
        // Explicit connection changes still construct a fresh cache.
        assert!(ShowInputs::default().inventory.is_none());
    }
}
