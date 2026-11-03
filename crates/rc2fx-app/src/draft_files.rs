use super::*;
use std::{path::PathBuf, result::Result};

#[derive(Clone)]
struct DraftSnapshot {
    generation: u64,
    targets: Vec<Target>,
    selected_targets: Vec<String>,
    saved: Option<Config>,
}
impl DraftSnapshot {
    fn capture(session: &Session, generation: u64) -> Self {
        Self {
            generation,
            targets: session.draft.clone(),
            selected_targets: session.selection_draft.clone(),
            saved: session.saved.as_ref().map(|s| s.config.clone()),
        }
    }
    fn matches(&self, session: &Session, generation: u64) -> bool {
        self.generation == generation
            && self.targets == session.draft
            && self.selected_targets == session.selection_draft
            && self.saved.as_ref() == session.saved.as_ref().map(|s| &s.config)
    }
}

fn imported_targets(config: Config, live: Option<&Config>) -> Result<Vec<Target>, String> {
    config.validate().map_err(|e| e.to_string())?;
    let Some(live) = live else {
        return Ok(config.targets);
    };
    if live.targets.len() != config.targets.len() {
        return Err("This file uses a different set of mixer channels.".into());
    }
    // Preserve the live engine's channel order. File revision numbers never
    // replace the session's revision used for a subsequent Apply request.
    live.targets
        .iter()
        .map(|target| {
            config
                .targets
                .iter()
                .find(|t| {
                    t.id == target.id && t.kind == target.kind && t.channels == target.channels
                })
                .cloned()
                .ok_or_else(|| format!("This file does not match mixer channel {}.", target.name))
        })
        .collect()
}

impl Rc2FxApp {
    fn install_draft(
        &mut self,
        snapshot: DraftSnapshot,
        targets: Vec<Target>,
        selected_targets: Vec<String>,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_busy = false;
        if !snapshot.matches(&self.session, self.generation) {
            self.message = "Settings changed while the file was opening. Open it again to replace the current draft.".into();
        } else {
            self.session.draft = targets;
            self.session.selection_draft = selected_targets;
            self.selected = self.selected.min(self.session.draft.len() - 1);
            self.channel = Channel::Left;
            self.band = 0;
            self.import = None;
            self.rebuild_editors(window, cx);
            self.message = format!(
                "Opened {name}. Apply saves effects; Change processing paths separately activates the path selection."
            );
        }
        cx.notify();
    }

    pub(super) fn open_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_busy
            || self.pending.is_some()
            || self.presets.writing()
            || self.connecting
            || self.transition_busy()
        {
            return;
        }
        let snapshot = DraftSnapshot::capture(&self.session, self.generation);
        self.file_busy = true;
        self.dragging_band = false;
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open RC2FX settings".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<(String, Config)>, String> = async {
                let paths = picker.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
                let Some(path) = paths.and_then(|mut p| p.pop()) else { return Ok(None); };
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let config = cx.background_executor().spawn(async move {
                    Config::from_json(&interchange::read_preset(&path)?).map_err(|e| e.to_string())
                }).await?;
                Ok(Some((name, config)))
            }.await;
            let _ = this.update_in(cx, |app, window, cx| {
                let parsed = result.and_then(|value| value.map(|(name, config)| {
                    let live = app.session.saved.as_ref().filter(|s| app.session.connected && s.engine.mode.has_processing_controls()).map(|s| &s.config);
                    let selected_targets = if let Some(live) = live {
                        live.canonical_selection(&config.selected_targets).map_err(|e| e.to_string())?
                    } else { config.canonical_selection(&config.selected_targets).map_err(|e| e.to_string())? };
                    imported_targets(config, live).map(|targets| (name, targets, selected_targets))
                }).transpose());
                match parsed {
                    Ok(Some((name, targets, selected_targets))) if snapshot.matches(&app.session, app.generation) => {
                        if app.session.any_dirty() {
                            let detail = format!("Open {name} with {} chains. This doesn't change the mixer's saved settings.", targets.len());
                            let open = cx.weak_entity();
                            let close = cx.weak_entity();
                            let opened = Rc::new(Cell::new(false));
                            let was_opened = opened.clone();
                            ui::confirm(window, cx, ui::Confirm {
                                eyebrow: None,
                                title: "Replace your unsaved draft?".into(),
                                detail,
                                cancel: Some("Keep editing"),
                                action: "Discard and open".into(),
                            }, move |window, cx| {
                                opened.set(true);
                                let (targets, selected_targets, name) = (targets.clone(), selected_targets.clone(), name.clone());
                                let _ = open.update(cx, |app, cx| app.install_draft(snapshot.clone(), targets, selected_targets, name, window, cx));
                            }, move |_, cx| {
                                if !was_opened.get() {
                                    let _ = close.update(cx, |app, cx| { app.file_busy = false; cx.notify(); });
                                }
                            });
                        } else { app.install_draft(snapshot, targets, selected_targets, name, window, cx); }
                    }
                    Ok(Some(_)) => { app.file_busy = false; app.message = "The draft or connection changed. Open the file again for the current session.".into(); }
                    Ok(None) => app.file_busy = false,
                    Err(error) => { app.file_busy = false; app.message = format!("Could not open settings: {error}"); }
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn save_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        let config = Config {
            schema_version: SCHEMA_VERSION,
            revision: 1,
            selected_targets: self.session.selection_draft.clone(),
            targets: self.session.draft.clone(),
        };
        let text = match config
            .validate()
            .map_err(|e| e.to_string())
            .and_then(|()| serde_json::to_string_pretty(&config).map_err(|e| e.to_string()))
        {
            Ok(text) => text,
            Err(error) => {
                self.message = error;
                cx.notify();
                return;
            }
        };
        let directory = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let picker = cx.prompt_for_new_path(&directory, Some("rc2fx-settings.json"));
        self.file_busy = true;
        self.dragging_band = false;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<PathBuf>, String> = async {
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
                        app.message = format!("Saved settings file to {}", path.display())
                    }
                    Ok(None) => {}
                    Err(error) => app.message = format!("Could not save settings: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{DraftSnapshot, imported_targets};
    use rc2fx_core::{Config, Session};
    #[test]
    fn import_preserves_live_channel_order_and_refuses_different_hardware() {
        let live = Config::default();
        let mut file = live.clone();
        file.revision = 999;
        file.targets[0].gain.db = -9.;
        file.targets.reverse();
        let imported = imported_targets(file.clone(), Some(&live)).unwrap();
        assert_eq!(imported[0].id, live.targets[0].id);
        assert_eq!(imported[0].gain.db, -9.);
        file.targets.pop();
        assert!(imported_targets(file.clone(), Some(&live)).is_err());
        assert!(imported_targets(file, None).is_ok());
    }
    #[test]
    fn file_selection_cannot_erase_later_edits_or_another_connection() {
        let mut session = Session::default();
        let captured = DraftSnapshot::capture(&session, 3);
        assert!(captured.matches(&session, 3));
        assert!(!captured.matches(&session, 4));
        session.draft[0].gain.db = -9.;
        assert!(!captured.matches(&session, 3));
        let captured = DraftSnapshot::capture(&session, 3);
        session.selection_draft.push("mic-1".into());
        assert!(!captured.matches(&session, 3));
    }
}
