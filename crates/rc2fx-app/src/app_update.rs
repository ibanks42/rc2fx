//! Checking GitHub for a newer desktop app. Notification only: the app never
//! downloads or installs anything, it links to the release page.
use super::*;
use gpui_kit::component::{WindowExt, dialog::DialogFooter};
use rc2fx_installer::app_update::{
    self, APP_RELEASES_URL, Changelog, ChangelogStatus, UpdateCheck,
};

#[derive(Default)]
pub(super) struct AppUpdate {
    checking: bool,
    /// The last result, and whether the user asked for it. A failed check
    /// at startup stays quiet; one the user asked for says what went wrong.
    last: Option<(UpdateCheck, bool)>,
}

impl AppUpdate {
    /// The status line under "Check for updates"; `None` shows nothing.
    pub(super) fn status(&self) -> Option<String> {
        if self.checking {
            return Some("Checking…".into());
        }
        let (result, manual) = self.last.as_ref()?;
        match result {
            UpdateCheck::UpToDate { latest } => {
                Some(format!("Up to date (latest release {latest})"))
            }
            UpdateCheck::Available { version, .. } => Some(format!("RC2FX {version} is available")),
            UpdateCheck::NoRelease => manual.then(|| "No release published yet".into()),
            UpdateCheck::Failed(error) => manual.then(|| error.to_string()),
        }
    }

    /// The release page to offer, only for a GitHub page of this project.
    pub(super) fn release_url(&self) -> Option<&str> {
        match &self.last {
            Some((UpdateCheck::Available { url, .. }, _)) if safe_url(url).is_some() => Some(url),
            _ => None,
        }
    }

    /// The offered version and its changes, when an update is available.
    pub(super) fn offered(&self) -> Option<(&str, &Changelog)> {
        match &self.last {
            Some((
                UpdateCheck::Available {
                    version, changelog, ..
                },
                _,
            )) => Some((version, changelog)),
            _ => None,
        }
    }

    pub(super) fn checking(&self) -> bool {
        self.checking
    }
}

/// What the changes view says about missing notes; `None` when complete.
pub(super) fn history_note(status: &ChangelogStatus) -> Option<String> {
    match status {
        ChangelogStatus::Complete => None,
        ChangelogStatus::Partial => Some(
            "Some release notes could not be loaded. All releases are listed on GitHub.".into(),
        ),
        ChangelogStatus::Unavailable(error) => Some(format!(
            "Release notes could not be loaded ({error}). All releases are listed on GitHub."
        )),
    }
}

/// Only GitHub pages of this project are opened.
fn safe_url(url: &str) -> Option<&str> {
    (url == APP_RELEASES_URL || url.starts_with(&format!("{APP_RELEASES_URL}/"))).then_some(url)
}

fn link(id: impl Into<ElementId>, label: &'static str, url: &str) -> Option<Button> {
    let url = safe_url(url)?.to_owned();
    Some(
        Button::new(id)
            .ghost()
            .small()
            .label(label)
            .on_click(move |_, _, cx| cx.open_url(&url)),
    )
}

/// The changes between this app and the offered release, newest first.
/// Notes are shown as plain text, line by line; nothing is rendered as HTML.
pub(super) fn open_changes(
    version: String,
    url: String,
    changelog: Changelog,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, _| {
        let mut list = div()
            .id("app-changes")
            .v_flex()
            .gap_4()
            .max_h(px(460.))
            .overflow_y_scroll();
        if let Some(note) = history_note(&changelog.status) {
            list = list.child(
                div()
                    .v_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(rgb(FAINT))
                    .child(note)
                    .children(link("all-releases", "All releases", APP_RELEASES_URL)),
            );
        }
        for (i, entry) in changelog.entries.iter().enumerate() {
            list = list.child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .font_semibold()
                                    .child(format!("RC2FX {}", entry.version)),
                            )
                            .children(link(("release", i), "View release", &entry.url)),
                    )
                    .child(
                        div()
                            .v_flex()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .children(entry.notes.lines().map(|line| {
                                div()
                                    .min_h(px(16.))
                                    .whitespace_normal()
                                    .child(line.to_owned())
                            }))
                            .when(entry.notes.trim().is_empty(), |d| {
                                d.child("No notes for this release.")
                            })
                            .when(entry.truncated, |d| {
                                d.child(
                                    div()
                                        .text_color(rgb(FAINT))
                                        .child("Notes shortened; the release page has the rest."),
                                )
                            }),
                    ),
            );
        }
        let mut footer = DialogFooter::new().pt_5().child(
            gpui_kit::component::dialog::DialogClose::new().trigger(|b| {
                b.ghost()
                    .large()
                    .h(px(44.))
                    .px_5()
                    .rounded(px(22.))
                    .label("Close")
            }),
        );
        if let Some(view) = link("offered-release", "View release", &url) {
            footer = footer.child(view.large().h(px(44.)).px_5());
        }
        dialog
            .w(px(720.))
            .rounded(px(20.))
            .p_6()
            .bg(rgb(surface().panel))
            .border_1()
            .border_color(rgb(surface().line_strong))
            .text_color(rgb(TEXT))
            .font_family("Inter")
            .title(
                div()
                    .v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                        "You have {}. Nothing is downloaded or installed.",
                        crate::versions::APP_VERSION
                    )))
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .child(format!("RC2FX {version} is available")),
                    ),
            )
            .child(list)
            .footer(footer)
    });
}

impl Rc2FxApp {
    /// Check in the background; the UI never waits on the network.
    pub(super) fn check_app_update(
        &mut self,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.app_update.checking {
            return;
        }
        self.app_update.checking = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { app_update::check_for_app_update(crate::versions::APP_VERSION) })
                .await;
            let _ = this.update_in(cx, |app, window, cx| {
                app.app_update.checking = false;
                if let UpdateCheck::Available { version, .. } = &result {
                    // Settings shows the link; this makes sure it is seen.
                    window.push_notification(
                        format!(
                            "RC2FX {version} is available. Settings > Versions > Read changes."
                        ),
                        cx,
                    );
                }
                app.app_update.last = Some((result, manual));
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APP_RELEASES_URL, AppUpdate, Changelog, ChangelogStatus, UpdateCheck, history_note,
        safe_url,
    };
    use rc2fx_installer::app_update::{ReleaseNotes, UpdateError};

    fn changelog(status: ChangelogStatus) -> Changelog {
        Changelog {
            entries: vec![ReleaseNotes {
                version: "0.2.0".into(),
                url: format!("{APP_RELEASES_URL}/tag/v0.2.0"),
                notes: "- Faster meters".into(),
                truncated: false,
            }],
            status,
        }
    }

    #[test]
    fn incomplete_history_is_never_presented_as_complete() {
        assert_eq!(history_note(&ChangelogStatus::Complete), None);
        assert!(
            history_note(&ChangelogStatus::Partial)
                .unwrap()
                .contains("could not be loaded")
        );
        let n = history_note(&ChangelogStatus::Unavailable(UpdateError::Offline)).unwrap();
        assert!(n.contains("could not be loaded") && n.contains(&UpdateError::Offline.to_string()));
    }

    #[test]
    fn only_project_release_pages_open() {
        assert!(safe_url(APP_RELEASES_URL).is_some());
        assert!(safe_url(&format!("{APP_RELEASES_URL}/tag/v0.2.0")).is_some());
        assert!(safe_url("https://github.com/ibanks42/rc2fx/releasesX").is_none());
        assert!(safe_url("https://evil.example/").is_none());
        assert!(safe_url("javascript:alert(1)").is_none());
    }

    #[test]
    fn the_offered_changelog_is_exposed() {
        let a = after(
            UpdateCheck::Available {
                version: "0.2.0".into(),
                url: format!("{APP_RELEASES_URL}/tag/v0.2.0"),
                changelog: changelog(ChangelogStatus::Complete),
            },
            false,
        );
        let (v, c) = a.offered().unwrap();
        assert_eq!((v, c.entries.len()), ("0.2.0", 1));
        assert!(after(UpdateCheck::NoRelease, true).offered().is_none());
    }

    fn after(result: UpdateCheck, manual: bool) -> AppUpdate {
        AppUpdate {
            checking: false,
            last: Some((result, manual)),
        }
    }

    #[test]
    fn startup_failures_are_quiet_and_manual_ones_readable() {
        for error in [UpdateError::Offline, UpdateError::RateLimited] {
            assert_eq!(
                after(UpdateCheck::Failed(error.clone()), false).status(),
                None
            );
            assert_eq!(
                after(UpdateCheck::Failed(error.clone()), true).status(),
                Some(error.to_string())
            );
        }
        assert_eq!(after(UpdateCheck::NoRelease, false).status(), None);
        assert_eq!(
            after(UpdateCheck::NoRelease, true).status().as_deref(),
            Some("No release published yet")
        );
    }

    #[test]
    fn only_an_available_release_on_this_project_is_linked() {
        let ok = format!("{APP_RELEASES_URL}/tag/v0.2.0");
        let a = after(
            UpdateCheck::Available {
                version: "0.2.0".into(),
                url: ok.clone(),
                changelog: changelog(ChangelogStatus::Complete),
            },
            false,
        );
        assert_eq!(a.release_url(), Some(ok.as_str()));
        assert_eq!(a.status().as_deref(), Some("RC2FX 0.2.0 is available"));
        let elsewhere = after(
            UpdateCheck::Available {
                version: "0.2.0".into(),
                url: "https://example.com/rc2fx".into(),
                changelog: changelog(ChangelogStatus::Partial),
            },
            true,
        );
        assert_eq!(elsewhere.release_url(), None);
        let current = after(
            UpdateCheck::UpToDate {
                latest: "0.1.0".into(),
            },
            false,
        );
        assert_eq!(current.release_url(), None);
        assert!(current.status().unwrap().contains("Up to date"));
    }

    #[test]
    fn checking_is_shown_while_running() {
        let a = AppUpdate {
            checking: true,
            last: None,
        };
        assert_eq!(a.status().as_deref(), Some("Checking…"));
        assert_eq!(AppUpdate::default().status(), None);
    }
}
