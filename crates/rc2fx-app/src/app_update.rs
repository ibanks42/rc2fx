//! Checking GitHub for a newer desktop app. Notification only: the app never
//! downloads or installs anything, it links to the release page.
use super::*;
use crate::versions::AppStanding;
use crate::versions_dialog::RODE_NOTES_URL;
use gpui_kit::component::WindowExt;
use rc2fx_installer::app_update::{
    self, APP_HISTORY_URL, APP_RELEASES_URL, Changelog, ChangelogStatus, MIXER_HISTORY_URL,
    MixerUpdate, UpdateCheck, UpdateSnapshot,
};
use rc2fx_installer::{Engine, Reply, Request, mixer_update, mixer_update::MixerOffer};

#[derive(Default)]
pub(super) struct AppUpdate {
    checking: bool,
    /// The last result, and whether the user asked for it. A failed check
    /// at startup stays quiet; one the user asked for says what went wrong.
    last: Option<(UpdateSnapshot, bool)>,
    /// The latest authenticated mixer releases. The feed in `last` can't say
    /// what this app may install; only these can. `None` until a check
    /// answers.
    signed: Option<Vec<MixerOffer>>,
}

impl AppUpdate {
    fn result(&self) -> Option<&UpdateCheck> {
        self.last.as_ref()?.0.app.as_ref()
    }

    /// Where this app stands, for the Versions dialog.
    pub(super) fn standing(&self) -> AppStanding {
        if self.checking {
            return AppStanding::Checking;
        }
        match self.result() {
            Some(UpdateCheck::UpToDate { latest }) => AppStanding::Current {
                latest: latest.clone(),
            },
            Some(UpdateCheck::Available { version, .. }) => AppStanding::Update {
                version: version.clone(),
            },
            _ => AppStanding::Unknown,
        }
    }

    /// Compare a mixer with what this app can install and, once a check has
    /// answered, published. It needs no network, so it works before the first
    /// check.
    pub(super) fn mixer_update(
        &self,
        installed: Option<&str>,
        available: Option<&str>,
    ) -> MixerUpdate {
        match &self.last {
            Some((snapshot, _)) => snapshot.mixer_update(installed, available),
            None => UpdateSnapshot::unchecked().mixer_update(installed, available),
        }
    }

    pub(super) fn signed(&self) -> Option<&[MixerOffer]> {
        self.signed.as_deref()
    }

    pub(super) fn set_signed(&mut self, offers: Vec<MixerOffer>) {
        self.signed = Some(offers);
    }

    /// The status line under "Check for updates"; `None` shows nothing.
    pub(super) fn status(&self) -> Option<String> {
        if self.checking {
            return Some("Checking…".into());
        }
        let (snapshot, manual) = self.last.as_ref()?;
        match snapshot.app.as_ref()? {
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
        match self.result() {
            Some(UpdateCheck::Available { url, .. }) if safe_url(url).is_some() => Some(url),
            _ => None,
        }
    }

    /// The offered version and its changes, when an update is available.
    pub(super) fn offered(&self) -> Option<(&str, &Changelog)> {
        match self.result() {
            Some(UpdateCheck::Available {
                version, changelog, ..
            }) => Some((version, changelog)),
            _ => None,
        }
    }

    pub(super) fn checking(&self) -> bool {
        self.checking
    }
}

/// Ask the signed repository what this app could install. `None` when it
/// can't say; the last answer stands, and the installer can always ask again.
fn signed_mixers() -> Option<Vec<MixerOffer>> {
    if !mixer_update::configured() {
        return None;
    }
    // Its own folder: the engine holds its folder's lock for a whole request,
    // network included, and the installer page must not wait for this check.
    let work = installer::work_directory()?.join("update-check");
    match Engine::download_only(work).handle(Request::MixerReleases) {
        Ok(Reply::MixerReleases(offers)) => Some(offers),
        _ => None,
    }
}

/// What the changes view says about missing notes; `None` when complete.
pub(super) fn history_note(status: &ChangelogStatus) -> Option<String> {
    match status {
        ChangelogStatus::Complete => None,
        ChangelogStatus::Partial => Some(
            "Some release notes could not be loaded. The full update history is online.".into(),
        ),
        ChangelogStatus::Unavailable(error) => Some(format!(
            "Release notes could not be loaded ({error}). The full update history is online."
        )),
    }
}

/// Only this project's release pages and update history, and RØDE's notes,
/// are opened. A history link may name a version: `#v1.2.3`.
pub(super) fn safe_url(url: &str) -> Option<&str> {
    let release = url == APP_RELEASES_URL || url.starts_with(&format!("{APP_RELEASES_URL}/"));
    let history = [APP_HISTORY_URL, MIXER_HISTORY_URL].iter().any(|page| {
        url.strip_prefix(page).is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix("#v")
                    .is_some_and(|v| crate::versions::compare(v, v).is_some())
        })
    });
    (release || history || url == RODE_NOTES_URL).then_some(url)
}

pub(super) fn link(id: impl Into<ElementId>, label: &'static str, url: &str) -> Option<Button> {
    let url = safe_url(url)?.to_owned();
    Some(
        Button::new(id)
            .ghost()
            .label(label)
            .on_click(move |_, _, cx| cx.open_url(&url)),
    )
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
                .spawn(async { app_update::check_for_updates(crate::versions::APP_VERSION) })
                .await;
            let _ = this.update_in(cx, |app, window, cx| {
                app.app_update.checking = false;
                if let Some(UpdateCheck::Available { version, .. }) = &result.app {
                    // The Versions dialog has the link; this makes sure it is seen.
                    window.push_notification(
                        format!("RC2FX {version} is available. Settings > About > View versions."),
                        cx,
                    );
                }
                app.app_update.last = Some((result, manual));
                cx.notify();
            });
        })
        .detach();
        // Separate, so a slow download host never holds up the app's answer.
        cx.spawn(async move |this, cx| {
            let signed = cx
                .background_executor()
                .spawn(async { signed_mixers() })
                .await;
            let _ = this.update(cx, |app, cx| {
                if let Some(signed) = signed {
                    app.app_update.set_signed(signed);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APP_HISTORY_URL, APP_RELEASES_URL, AppUpdate, Changelog, ChangelogStatus,
        MIXER_HISTORY_URL, RODE_NOTES_URL, UpdateCheck, history_note, safe_url,
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
        assert!(safe_url(APP_HISTORY_URL).is_some());
        assert!(safe_url(&format!("{MIXER_HISTORY_URL}#v1.2.3")).is_some());
        assert!(safe_url(&format!("{MIXER_HISTORY_URL}#v1.2.3x")).is_none());
        assert!(safe_url(&format!("{APP_HISTORY_URL}.evil.example")).is_none());
        assert!(safe_url(RODE_NOTES_URL).is_some());
        assert!(safe_url("https://update.rode.com/other").is_none());
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
            signed: None,
            last: Some((result.into(), manual)),
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
            ..Default::default()
        };
        assert_eq!(a.status().as_deref(), Some("Checking…"));
        assert_eq!(AppUpdate::default().status(), None);
    }
}
