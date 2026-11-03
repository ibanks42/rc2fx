use super::*;
use rc2fx_core::easyeffects::{self, ImportOptions, ImportResult, Pipeline};
use std::result::Result;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone)]
struct ImportTarget {
    id: String,
    channels: u8,
    eq: EqSettings,
    generation: u64,
}
impl ImportTarget {
    fn capture(target: &Target, generation: u64) -> Self {
        Self {
            id: target.id.clone(),
            channels: target.channels,
            eq: target.eq.clone(),
            generation,
        }
    }
    fn matches(&self, target: &Target, generation: u64) -> bool {
        self.generation == generation
            && self.id == target.id
            && self.channels == target.channels
            && self.eq == target.eq
    }
}

pub(super) struct ImportDraft {
    target: ImportTarget,
    text: String,
    name: String,
    options: ImportOptions,
    stage: ImportResult,
}

pub(super) fn read_preset(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Choose an EQ preset file".into());
    }
    let mut bytes = Vec::new();
    file.take(easyeffects::MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > easyeffects::MAX_FILE_BYTES {
        return Err("Preset file exceeds 4 MiB".into());
    }
    decode_text(&bytes)
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if !bytes.len().is_multiple_of(2) {
            return Err("Truncated UTF-16 preset".into());
        }
        let little = bytes[0] == 0xff;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| "Invalid UTF-16 preset".into())
    } else {
        std::str::from_utf8(bytes)
            .map(|s| s.trim_start_matches('\u{feff}').to_owned())
            .map_err(|_| "Preset must be UTF-8 or UTF-16 text".into())
    }
}

fn parse_import(text: &str, options: &ImportOptions) -> Result<ImportResult, String> {
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with('{') {
        easyeffects::import(text, options).map_err(|e| e.to_string())
    } else {
        let eq = rc2fx_core::apo::import(text).map_err(|e| e.to_string())?;
        let source = easyeffects::CandidateSummary {
            key: "Equalizer APO / AutoEQ".into(),
            pipeline: Pipeline::Output,
            instance: "parametric text".into(),
            band_count: eq.bands.len(),
            channels_differ: false,
            error: None,
        };
        Ok(ImportResult::Ready {
            eq,
            source,
            other_effects: 0,
        })
    }
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) fn write_export(path: &Path, text: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("Choose a file location")?;
    let mut temporary = None;
    for _ in 0..32 {
        let name = format!(
            ".rc2fx-export-{}-{}.tmp",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let candidate = parent.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let (temporary, mut file) = temporary.ok_or("Could not create export file")?;
    let result = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("Writing export: {e}"));
    drop(file);
    let result = result
        .and_then(|_| fs::rename(&temporary, path).map_err(|e| format!("Replacing export: {e}")));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

impl Rc2FxApp {
    fn stage_import(&mut self, target: ImportTarget, name: String, text: String) {
        let options = ImportOptions {
            stereo: target.channels == 2,
            ..Default::default()
        };
        match parse_import(&text, &options) {
            Ok(stage) => {
                self.import_error = None;
                self.message =
                    format!("Review {name} below Import EQ, then replace this target’s EQ.");
                self.import = Some(ImportDraft {
                    target,
                    text,
                    name,
                    options,
                    stage,
                });
            }
            Err(error) => {
                self.import = None;
                self.message = format!("Could not import EQ: {error}");
                self.import_error = Some(self.message.clone());
            }
        }
    }

    pub(super) fn paste_eq(&mut self, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        self.import = None;
        self.import_error = None;
        self.dragging_band = false;
        match cx.read_from_clipboard().and_then(|item| item.text()) {
            Some(text) => self.stage_import(
                ImportTarget::capture(self.target(), self.generation),
                "clipboard".into(),
                text,
            ),
            None => {
                self.import_error =
                    Some("Copy APO text or EasyEffects JSON, then click Paste.".into())
            }
        }
        cx.notify();
    }

    pub(super) fn import_eq(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        self.import = None;
        self.import_error = None;
        self.dragging_band = false;
        self.file_busy = true;
        let target = ImportTarget::capture(self.target(), self.generation);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import EQ: APO text or EasyEffects JSON".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<(String, String)>, String> = async {
                let selected = picker.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
                let Some(path) = selected.and_then(|mut paths| paths.pop()) else { return Ok(None); };
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let text = cx.background_executor().spawn(async move { read_preset(&path) }).await?;
                Ok(Some((name, text)))
            }.await;
            let _ = this.update_in(cx, |app, _, cx| {
                app.file_busy = false;
                match result {
                    Ok(Some((name, text))) if target.matches(app.target(), app.generation) => {
                        app.stage_import(target, name, text);
                    }
                    Ok(Some(_)) => app.import_error = Some("The target or EQ changed while the file was opening. Import it again for the current EQ.".into()),
                    Ok(None) => {},
                    Err(error) => app.import_error = Some(format!("Could not import EQ: {error}")),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn choose_import(
        &mut self,
        selected: Option<String>,
        channel: Option<Channel>,
        cx: &mut Context<Self>,
    ) {
        if let Some(import) = &mut self.import {
            if let Some(selected) = selected {
                import.options.selected = Some(selected);
            }
            import.options.channel = channel;
            match parse_import(&import.text, &import.options) {
                Ok(stage) => import.stage = stage,
                Err(error) => self.import_error = Some(error),
            }
        }
        cx.notify();
    }

    fn finish_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(import) = self.import.take() else {
            return;
        };
        if !import.target.matches(self.target(), self.generation) {
            self.import_error = Some("This EQ changed after the import started. Import again to replace its current settings.".into());
        } else if let ImportResult::Ready { eq, .. } = import.stage {
            self.target_mut().eq = eq;
            self.channel = Channel::Left;
            self.band = 0;
            self.message = format!(
                "Imported {} into {}. Use Save to mixer to save it to the mixer.",
                import.name,
                self.target_name(self.target())
            );
            self.rebuild_editors(window, cx);
        }
        cx.notify();
    }

    pub(super) fn import_panel(&self, import: &ImportDraft, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div()
            .v_flex()
            .gap_3()
            .p_3()
            .rounded_md()
            .bg(rgb(surface().raised))
            .child(
                div()
                    .font_semibold()
                    .child(format!("Import {}", import.name)),
            );
        match &import.stage {
            ImportResult::ChooseEqualizer { candidates } => {
                panel = panel.child("Choose the equalizer to import:");
                for (index, candidate) in candidates.iter().enumerate() {
                    let key = candidate.key.clone();
                    panel = panel.child(
                        Button::new(("import-candidate", index))
                            .label(format!(
                                "{} · {} bands",
                                candidate.key, candidate.band_count
                            ))
                            .disabled(candidate.error.is_some())
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.choose_import(Some(key.clone()), None, cx)
                            })),
                    );
                    if let Some(error) = &candidate.error {
                        panel = panel.child(div().text_sm().child(error.clone()));
                    }
                }
            }
            ImportResult::ChooseChannel { source } => {
                panel = panel.child("This preset has different left and right settings. Choose one for this mono input:");
                for (index, channel, label) in [
                    (0usize, Channel::Left, "Left"),
                    (1, Channel::Right, "Right"),
                ] {
                    let key = source.key.clone();
                    panel = panel.child(
                        Button::new(("import-channel", index))
                            .label(label)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.choose_import(Some(key.clone()), Some(channel), cx)
                            })),
                    );
                }
            }
            ImportResult::Ready {
                eq,
                source,
                other_effects,
            } => {
                panel = panel.child(format!(
                    "{} · {} left / {} right bands · input {:+.1} dB · output {:+.1} dB",
                    source.key,
                    eq.bands.len(),
                    eq.channel(Channel::Right).len(),
                    eq.input_gain_db,
                    eq.output_gain_db
                ));
                if *other_effects > 0 {
                    panel = panel.child(
                        "Only the equalizer is imported; the preset’s other effects are excluded.",
                    );
                }
                panel = panel.child(
                    Button::new("import-confirm")
                        .primary()
                        .label(format!("Replace {} EQ", self.target_name(self.target())))
                        .on_click(cx.listener(|app, _, window, cx| app.finish_import(window, cx))),
                );
            }
        }
        panel
            .child(
                Button::new("import-cancel")
                    .ghost()
                    .label("Cancel import")
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.import = None;
                        app.import_error = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(super) fn export_eq(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_busy {
            return;
        }
        let target = self.target();
        let pipeline = if target.kind == TargetKind::Mic {
            Pipeline::Input
        } else {
            Pipeline::Output
        };
        let text = match easyeffects::export(&target.eq, pipeline) {
            Ok(text) => text,
            Err(e) => {
                self.message = e.to_string();
                cx.notify();
                return;
            }
        };
        let name = format!("{}-eq.json", target.id);
        let directory = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let picker = cx.prompt_for_new_path(&directory, Some(&name));
        self.file_busy = true;
        self.dragging_band = false;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Option<PathBuf>, String> = async {
                let path = picker
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| e.to_string())?;
                let Some(path) = path else {
                    return Ok(None);
                };
                let output = path.clone();
                cx.background_executor()
                    .spawn(async move { write_export(&output, &text) })
                    .await?;
                Ok(Some(path))
            }
            .await;
            let _ = this.update_in(cx, |app, _, cx| {
                app.file_busy = false;
                match result {
                    Ok(Some(path)) => app.message = format!("Exported EQ to {}", path.display()),
                    Ok(None) => {}
                    Err(error) => app.message = format!("Could not export EQ: {error}"),
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
    use super::{
        ImportTarget, TEMP_SEQUENCE, decode_text, parse_import, read_preset, write_export,
    };
    use rc2fx_core::{
        Channel, Config,
        easyeffects::{self, ImportOptions, ImportResult, Pipeline},
    };
    use std::{fs, sync::atomic::Ordering};
    #[test]
    fn import_detects_autoeq_text_and_native_easyeffects_without_a_format_picker() {
        let options = ImportOptions {
            stereo: true,
            ..Default::default()
        };
        let apo = include_str!("../../../fixtures/apo/sennheiser-hd-6xx.txt");
        let ImportResult::Ready { eq, .. } = parse_import(apo, &options).unwrap() else {
            panic!()
        };
        assert_eq!(eq.bands.len(), 10);
        assert_eq!(eq.input_gain_db, -8.46);
        assert_eq!(eq.bands[0].frequency, 105.);
        let json = include_str!("../../../fixtures/easyeffects/output-split-different.json");
        let ImportResult::Ready { eq, .. } = parse_import(json, &options).unwrap() else {
            panic!()
        };
        assert!(eq.right_bands.is_some());
        let bytes: Vec<_> = [0xff, 0xfe]
            .into_iter()
            .chain(apo.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode_text(&bytes).unwrap(), apo);
        assert!(
            parse_import("Filter 1: ON mystery", &options)
                .unwrap_err()
                .contains("APO line 1")
        );
        assert!(decode_text(&[0xff, 0xfe, 1]).is_err());
    }
    #[test]
    fn import_cannot_replace_another_target_or_a_newer_edit() {
        let mut target = Config::default().targets[4].clone();
        let captured = ImportTarget::capture(&target, 4);
        assert!(captured.matches(&target, 4));
        assert!(!captured.matches(&target, 5));
        let mut other = target.clone();
        other.id = "different".into();
        assert!(!captured.matches(&other, 4));
        target.eq.add_band(Channel::Left).unwrap();
        assert!(!captured.matches(&target, 4));
    }
    #[test]
    fn native_file_roundtrip_preserves_split_eq_and_replaces_export_atomically() {
        let directory = std::env::temp_dir().join(format!(
            "rc2fx-eq-test-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("eq.json");
        let text = include_str!("../../../fixtures/easyeffects/output-split-different.json");
        write_export(&path, text).unwrap();
        let ImportResult::Ready { eq, .. } = easyeffects::import(
            &read_preset(&path).unwrap(),
            &ImportOptions {
                stereo: true,
                ..Default::default()
            },
        )
        .unwrap() else {
            panic!("expected split EQ")
        };
        assert!(eq.right_bands.is_some());
        let exported = easyeffects::export(&eq, Pipeline::Output).unwrap();
        write_export(&path, &exported).unwrap();
        assert_eq!(read_preset(&path).unwrap(), exported);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(&path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
