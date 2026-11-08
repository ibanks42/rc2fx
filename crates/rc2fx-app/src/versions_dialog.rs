//! The Versions section of Settings, and the changelog that What's new opens.
//!
//! Notes come from the network, so they are treated as untrusted: raw HTML and
//! image syntax are escaped, images never load, and links open only when
//! `app_update::safe_url` allows them.
use super::*;
use crate::app_update::{history_note, link, safe_url};
use crate::settings::section;
use crate::versions::{AppStanding, Line, Overview, Part, Tone};
use gpui_kit::base::text::{TextView, TextViewStyle};
use gpui_kit::component::{
    WindowExt,
    dialog::{Dialog, DialogFooter},
};
use rc2fx_installer::app_update::{APP_HISTORY_URL, Changelog, MIXER_HISTORY_URL, ReleaseNotes};
use std::sync::Arc;

/// RØDE's published notes for the Pro II and the Duo.
pub(super) const RODE_NOTES_URL: &str = "https://update.rode.com/notes/rcp2/";

fn tone_color(tone: Tone) -> u32 {
    match tone {
        Tone::Unknown => FAINT,
        Tone::Quiet => MUTED,
        Tone::Update => ACCENT,
        Tone::Warn => WARN,
        Tone::Bad => DANGER,
    }
}

/// A status: a small dot and its words, in the status's color.
pub(super) fn flag(text: impl Into<SharedString>, tone: Tone) -> Div {
    let color = rgb(tone_color(tone));
    div()
        .h_flex()
        .items_center()
        .gap(px(6.))
        .flex_shrink_0()
        .text_size(px(13.))
        .font_medium()
        .whitespace_nowrap()
        .text_color(color)
        .child(div().size(px(7.)).rounded_full().bg(color))
        .child(text.into())
}

/// The shell every RC2FX dialog shares.
fn styled(dialog: Dialog, width: f32) -> Dialog {
    dialog
        .w(px(width))
        .rounded(px(20.))
        .p_6()
        .bg(rgb(surface().panel))
        .border_1()
        .border_color(rgb(surface().line_strong))
        .text_color(rgb(TEXT))
        .font_family("Inter")
}

/// A button in the Versions section.
fn pill(button: Button) -> Button {
    button.h(px(34.)).px(px(16.)).rounded(px(17.))
}

fn large(button: Button) -> Button {
    button.large().h(px(44.)).px_5().rounded(px(22.))
}

/// What the changes dialog shows for one part.
struct Changes {
    /// A small line above the title.
    eyebrow: String,
    title: String,
    /// Each entry's heading starts with this.
    entry_prefix: &'static str,
    changelog: Option<Changelog>,
    /// Said above the notes instead of the changelog's own status.
    note: Option<String>,
    /// Entries newer than this need an app update first.
    available: Option<String>,
    links: Vec<(&'static str, String)>,
}

/// One version's heading, marking notes this app cannot install.
fn entry_heading(prefix: &str, entry: &ReleaseNotes, available: Option<&str>) -> String {
    let later = available.is_some_and(|available| {
        crate::versions::compare(&entry.version, available) == Some(std::cmp::Ordering::Greater)
    });
    if later {
        format!("{prefix} {} · needs an app update", entry.version)
    } else {
        format!("{prefix} {}", entry.version)
    }
}

/// Escape what could reach the renderer as raw HTML or as an image. Text in
/// fenced blocks and code spans is literal already, so it stays as written.
/// A misjudged fence only leaves a tag to the renderer, which still loads no
/// images and opens no unapproved links.
pub(super) fn safe_markdown(notes: &str) -> String {
    let mut out = String::with_capacity(notes.len());
    let mut fence: Option<(char, usize)> = None;
    for line in notes.split_inclusive('\n') {
        let text = line.trim_start();
        let marker = text.chars().next().filter(|c| matches!(c, '`' | '~'));
        let run = marker.map_or(0, |m| text.chars().take_while(|c| *c == m).count());
        match fence {
            Some((open, length)) => {
                out.push_str(line);
                if marker == Some(open) && run >= length {
                    fence = None;
                }
            }
            None if run >= 3 => {
                out.push_str(line);
                fence = marker.map(|m| (m, run));
            }
            None => escape_line(line, &mut out),
        }
    }
    out
}

fn escape_line(line: &str, out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                out.extend(&chars[i..(i + 2).min(chars.len())]);
                i += 2;
            }
            '`' => {
                let run = chars[i..].iter().take_while(|c| **c == '`').count();
                let close = (i + run..chars.len()).find(|&j| {
                    chars[j..].iter().take_while(|c| **c == '`').count() == run
                        && chars[j - 1] != '`'
                });
                let end = close.map_or(i + run, |j| j + run);
                out.extend(&chars[i..end]);
                i = end;
            }
            '<' => {
                out.push_str("\\<");
                i += 1;
            }
            '!' if chars.get(i + 1) == Some(&'[') => {
                out.push_str("\\!");
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
}

/// An image that decodes to nothing: no note can make the app fetch one.
fn no_image(_: &SharedUri) -> ImageSource {
    ImageSource::Image(Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new())))
}

fn notes_view(id: usize, notes: SharedString, style: TextViewStyle) -> TextView {
    TextView::markdown(("change-notes", id), notes)
        .style(style)
        .selectable(true)
        .image_source(no_image)
        .on_link_click(|url, _, _, cx| {
            if let Some(url) = safe_url(url) {
                cx.open_url(url);
            }
        })
}

fn open_dialog(spec: Changes, window: &mut Window, cx: &mut App) {
    let changelog = spec.changelog.clone();
    let status_note = changelog.as_ref().and_then(|c| history_note(&c.status));
    let note = spec.note.clone().or(status_note);
    let entries: Vec<(String, SharedString, bool, bool)> = changelog
        .iter()
        .flat_map(|c| c.entries.iter())
        .map(|entry| {
            (
                entry_heading(spec.entry_prefix, entry, spec.available.as_deref()),
                SharedString::from(safe_markdown(&entry.notes)),
                entry.notes.trim().is_empty(),
                entry.truncated,
            )
        })
        .collect();
    window.open_dialog(cx, move |dialog, _, cx| {
        let style = TextViewStyle::from_theme(&gpui_kit::base::Theme::global(cx))
            .with_foreground(rgb(MUTED).into())
            .with_link(rgb(ACCENT).into())
            .with_code_background(rgb(surface().raised).into());
        let mut body = div()
            .id("changes")
            .v_flex()
            .gap_5()
            .max_h(px(420.))
            .overflow_y_scroll()
            .p(px(18.))
            .rounded(px(12.))
            .bg(rgb(surface().sunken))
            .border_1()
            .border_color(rgb(surface().line))
            .text_size(px(13.5));
        if let Some(note) = &note {
            body = body.child(div().text_color(rgb(FAINT)).child(note.clone()));
        }
        for (i, (heading, notes, empty, truncated)) in entries.iter().enumerate() {
            let mut entry = div().v_flex().gap_1().child(
                div()
                    .font_semibold()
                    .text_size(px(15.))
                    .child(heading.clone()),
            );
            entry = if *empty {
                entry.child(
                    div()
                        .text_color(rgb(MUTED))
                        .child("No notes for this release."),
                )
            } else {
                entry.child(notes_view(i, notes.clone(), style.clone()))
            };
            body = body.child(entry.when(*truncated, |d| {
                d.child(
                    div()
                        .text_color(rgb(FAINT))
                        .child("Notes shortened; the update history has the rest."),
                )
            }));
        }
        let mut footer = DialogFooter::new().pt_5();
        for (i, (label, url)) in spec.links.iter().enumerate() {
            if let Some(button) = link(("change-link", i), label, url) {
                footer = footer.child(large(button).border_1());
            }
        }
        footer = footer.child(
            gpui_kit::component::dialog::DialogClose::new().trigger(|button| {
                button
                    .primary()
                    .large()
                    .h(px(44.))
                    .px_5()
                    .rounded(px(22.))
                    .font_semibold()
                    .label("Close")
            }),
        );
        styled(dialog, 720.)
            .title(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .child(spec.eyebrow.clone()),
                    )
                    .child(div().text_xl().font_semibold().child(spec.title.clone())),
            )
            .child(body)
            .footer(footer)
    });
}

impl Rc2FxApp {
    fn version_inputs(&self) -> (Option<String>, Option<String>, Option<String>) {
        let device = self.device_versions.as_ref();
        (
            device.and_then(|d| d.runtime.clone()),
            device.and_then(|d| d.firmware.clone()),
            crate::versions::newest_supported_firmware(
                device.and_then(|d| d.firmware_profile.as_deref()),
            ),
        )
    }

    /// The newest mixer software this app could install right now.
    fn available_mixer_software(&self) -> Option<String> {
        crate::versions::newest_available(
            self.app_update.signed().unwrap_or_default(),
            self.bundled_mixer_software.as_deref(),
        )
    }

    /// The versions as Settings and the Versions dialog show them.
    pub(super) fn versions_overview(&self) -> Overview {
        let (runtime, firmware, newest_firmware) = self.version_inputs();
        let available = self.available_mixer_software();
        let available = available.as_deref();
        let mixer = self.app_update.mixer_update(runtime.as_deref(), available);
        let needs_app = crate::versions::needs_newer_app(
            self.app_update.signed().unwrap_or_default(),
            available,
        );
        crate::versions::overview(
            &crate::versions::Inputs {
                app: crate::versions::APP_VERSION,
                installed_runtime: runtime.as_deref(),
                available_runtime: available,
                signed_known: self.app_update.signed().is_some(),
                needs_app: needs_app.as_ref().map(|(v, m)| (v.as_str(), m.as_str())),
                installed_firmware: firmware.as_deref(),
                newest_supported_firmware: newest_firmware.as_deref(),
                rode_firmware: self.app_update.rode_firmware(
                    self.device_versions
                        .as_ref()
                        .and_then(|d| d.firmware_profile.as_deref()),
                ),
                device_protocol: self.device_versions.as_ref().and_then(|d| d.protocol),
            },
            &self.app_update.standing(),
            &mixer.state,
        )
    }

    /// The Versions section: what matters first, then each part.
    pub(super) fn versions_section(&self, cx: &mut Context<Self>) -> Div {
        let overview = self.versions_overview();
        let lead = overview.lead.clone();
        let release = self.app_update.release_url().map(str::to_owned);
        let mut actions = div().h_flex().flex_wrap().items_center().gap_2();
        let has_release = release.is_some();
        if let Some(url) = release {
            actions = actions.child(
                pill(Button::new("versions-download").primary())
                    .font_semibold()
                    .label("Download")
                    .icon(gpui_kit::assets::IconName::ExternalLink)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        if overview.offer_installer {
            // Only opens the installer; flashing still needs its confirmation.
            let button = if has_release {
                pill(Button::new("versions-installer").ghost().border_1())
                    .border_color(rgb(surface().line_strong))
            } else {
                pill(Button::new("versions-installer").primary()).font_semibold()
            };
            actions = actions.child(
                button
                    .label("Open installer")
                    .on_click(cx.listener(|app, _, _, cx| app.toggle_installer_page(cx))),
            );
        }
        if let Some(part) = lead.changes {
            actions = actions.child(self.whats_new(("versions-changes", 0usize), part, cx));
        }
        let acting = has_release || overview.offer_installer || lead.changes.is_some();
        let tiles = overview.lines.iter().map(|line| {
            let elsewhere = line.read_changes && lead.changes != Some(line.part);
            self.version_tile(line, elsewhere, cx)
        });
        let tiles = div().h_flex().items_stretch().gap_2().children(tiles);
        let failure = self
            .app_update
            .status()
            .filter(|_| self.app_update.standing() == AppStanding::Unknown);
        let body = div()
            .v_flex()
            .gap_3()
            .pt_3()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(ui::SUBHEADING_TEXT))
                            .font_semibold()
                            .child(lead.title),
                    )
                    .when_some(lead.detail, |d, detail| {
                        d.child(div().text_sm().text_color(rgb(MUTED)).child(detail))
                    }),
            )
            .when(acting, |d| d.child(actions))
            .child(tiles)
            .children(overview.details.iter().map(|note| {
                div()
                    .text_xs()
                    .line_height(relative(1.45))
                    .text_color(rgb(MUTED))
                    .child(note.clone())
            }))
            .when_some(failure, |d, failure| {
                d.child(div().text_xs().text_color(rgb(FAINT)).child(failure))
            });
        let check = Button::new("versions-check")
            .ghost()
            .small()
            .label("Check for updates")
            .disabled(self.app_update.checking())
            .on_click(cx.listener(|app, _, window, cx| app.check_app_update(true, window, cx)));
        section("Versions", check, body)
    }

    fn whats_new(
        &self,
        id: impl Into<ElementId>,
        part: Part,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        pill(Button::new(id).ghost().border_1())
            .border_color(rgb(surface().line_strong))
            .label("What's new")
            .on_click(cx.listener(move |app, _, window, cx| app.open_changes(part, window, cx)))
    }

    /// One part: its name, version and standing.
    fn version_tile(&self, line: &Line, whats_new: bool, cx: &mut Context<Self>) -> Div {
        let version = if line.tone == Tone::Update {
            format!("{} → {}", line.installed, line.available)
        } else {
            line.installed.clone()
        };
        div()
            .v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .p_3()
            .rounded_lg()
            .bg(rgb(surface().raised))
            .child(div().text_xs().text_color(rgb(MUTED)).child(line.name))
            .child(
                div()
                    .text_size(px(ui::SUBHEADING_TEXT))
                    .font_semibold()
                    .child(version),
            )
            .child(flag(line.status, line.tone))
            .when(whats_new, |tile| {
                tile.child(div().h_flex().pt_1().child(self.whats_new(
                    ("versions-changes", line.part as usize + 1),
                    line.part,
                    cx,
                )))
            })
    }

    fn open_changes(&mut self, part: Part, window: &mut Window, cx: &mut Context<Self>) {
        let (runtime, firmware, newest_firmware) = self.version_inputs();
        let unknown = "Unknown".to_owned();
        let spec = match part {
            Part::App => {
                let Some((version, changelog)) = self.app_update.offered() else {
                    return;
                };
                let mut links = vec![("All app updates", format!("{APP_HISTORY_URL}#v{version}"))];
                if let Some(url) = self.app_update.release_url() {
                    links.push(("Download", url.to_owned()));
                }
                Changes {
                    eyebrow: format!("RC2FX {version} is available"),
                    title: format!("What's new since {}", crate::versions::APP_VERSION),
                    entry_prefix: "RC2FX",
                    changelog: Some(changelog.clone()),
                    note: None,
                    available: None,
                    links,
                }
            }
            Part::Mixer => {
                let available = self.available_mixer_software();
                let update = self
                    .app_update
                    .mixer_update(runtime.as_deref(), available.as_deref());
                let newest = update
                    .changelog
                    .as_ref()
                    .and_then(|c| c.entries.first())
                    .map(|e| e.version.clone())
                    .or_else(|| available.clone())
                    .unwrap_or_else(|| unknown.clone());
                Changes {
                    eyebrow: format!("Mixer software {newest}"),
                    title: format!("What's new since {}", runtime.as_ref().unwrap_or(&unknown)),
                    entry_prefix: "Mixer software",
                    changelog: update.changelog,
                    note: None,
                    available,
                    links: vec![(
                        "All mixer updates",
                        format!("{MIXER_HISTORY_URL}#v{newest}"),
                    )],
                }
            }
            Part::Firmware => Changes {
                eyebrow: format!(
                    "You have {}. Nothing changes until you install it.",
                    firmware.as_ref().unwrap_or(&unknown)
                ),
                title: format!(
                    "RØDE firmware {}",
                    newest_firmware.as_ref().unwrap_or(&unknown)
                ),
                entry_prefix: "Version",
                changelog: None,
                note: Some("RØDE publishes the notes for this firmware on its website.".into()),
                available: None,
                links: vec![("RØDE's release notes", RODE_NOTES_URL.to_owned())],
            },
        };
        open_dialog(spec, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{entry_heading, safe_markdown};
    use rc2fx_installer::app_update::{APP_RELEASES_URL, ReleaseNotes};

    #[test]
    fn notes_cannot_carry_html_or_images() {
        assert_eq!(
            safe_markdown("<img src=\"https://evil.example/x.png\">"),
            "\\<img src=\"https://evil.example/x.png\">"
        );
        assert_eq!(
            safe_markdown("a ![pixel](https://evil.example/x.png) b"),
            "a \\![pixel](https://evil.example/x.png) b"
        );
        assert_eq!(
            safe_markdown("<script>x</script>"),
            "\\<script>x\\</script>"
        );
    }

    #[test]
    fn ordinary_markdown_is_unchanged() {
        let notes = "### Features\n\n- *(app)* Show **bold** and [a link](https://example.com)\n";
        assert_eq!(safe_markdown(notes), notes);
    }

    #[test]
    fn code_stays_literal_and_escapes_are_kept() {
        assert_eq!(safe_markdown("use `<T>` here <b>"), "use `<T>` here \\<b>");
        assert_eq!(
            safe_markdown("```\n<div>\n```\n<div>"),
            "```\n<div>\n```\n\\<div>"
        );
        assert_eq!(safe_markdown("already \\<safe"), "already \\<safe");
        assert_eq!(safe_markdown("a lone ` <b>"), "a lone ` \\<b>");
    }

    #[test]
    fn versions_this_app_cannot_install_are_marked() {
        let entry = |version: &str| ReleaseNotes {
            version: version.into(),
            url: format!("{APP_RELEASES_URL}/tag/v{version}"),
            notes: String::new(),
            truncated: false,
        };
        assert_eq!(
            entry_heading("Mixer software", &entry("1.1.0"), Some("1.0.0")),
            "Mixer software 1.1.0 · needs an app update"
        );
        assert_eq!(
            entry_heading("Mixer software", &entry("1.0.0"), Some("1.0.0")),
            "Mixer software 1.0.0"
        );
        assert_eq!(entry_heading("RC2FX", &entry("1.1.0"), None), "RC2FX 1.1.0");
    }
}
