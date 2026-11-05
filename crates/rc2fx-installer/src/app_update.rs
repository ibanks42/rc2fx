//! What the official project has published: the latest RC2FX desktop app, the
//! mixer software it bundles, and the release notes leading to both.
//!
//! Unauthenticated HTTPS requests only. GitHub's latest release decides what
//! is published; the update feed on the documentation site supplies the
//! notes and which mixer software each app release bundles. Without a usable
//! feed, app notes come from GitHub's release list and the published mixer
//! software stays unknown. Nothing is downloaded or installed: the result is
//! versions, release notes and links.
//!
//! GitHub's latest release excludes drafts and prereleases; this module still
//! refuses anything that is not a published, stable `vMAJOR.MINOR.PATCH` app
//! release, such as a `mixer-v` tag left from before mixer runtimes were only
//! released inside the app.
//!
//! A feed entry newer than that release is ignored. That only blocks entries
//! above the latest release: that every lower entry was published is the
//! official generator's guarantee, from GitHub's release list, and is not
//! checked here.
//!
//! This says what exists, not what can be installed: what this app installs
//! comes from its own bundled payload alone.

use rc2fx_core::compare_versions;
use std::{
    cmp::Ordering,
    fmt,
    io::Read,
    time::{Duration, Instant},
};

/// The official repository, as `owner/name`. Fixed on purpose: a build from
/// a fork still checks the official releases.
pub const APP_REPOSITORY: &str = "ibanks42/rc2fx";
/// The repository's releases page.
pub const APP_RELEASES_URL: &str = "https://github.com/ibanks42/rc2fx/releases";
const LATEST_API: &str = "https://api.github.com/repos/ibanks42/rc2fx/releases/latest";
const LIST_API: &str = "https://api.github.com/repos/ibanks42/rc2fx/releases";
/// The update feed, generated from the same notes as the history pages.
pub const UPDATE_FEED_URL: &str = "https://ibanks42.github.io/rc2fx/updates.json";
/// Every app release's notes, on one page.
pub const APP_HISTORY_URL: &str = "https://ibanks42.github.io/rc2fx/updates/app";
/// Every mixer software release's notes, on one page.
pub const MIXER_HISTORY_URL: &str = "https://ibanks42.github.io/rc2fx/updates/mixer";
/// The only feed schema this build reads. New fields are ignored; a breaking
/// change gets a new URL.
const FEED_SCHEMA: u32 = 1;
const MAX_FEED_BODY: u64 = 4 * 1024 * 1024;
/// Per list; far above any real history.
const MAX_FEED_RELEASES: usize = 2000;
const TIMEOUT: Duration = Duration::from_secs(10);
/// A release reply is a few KiB; anything past this is refused.
const MAX_BODY: u64 = 1024 * 1024;
/// Release-list paging: at most this many pages of this many releases, each
/// page at most `MAX_LIST_BODY`, all within `NOTES_DEADLINE`.
const PER_PAGE: usize = 100;
const MAX_PAGES: u32 = 5;
const MAX_LIST_BODY: u64 = 4 * 1024 * 1024;
const NOTES_DEADLINE: Duration = Duration::from_secs(30);
/// One release's notes are cut here, at a character boundary.
const MAX_NOTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    /// No newer release. `latest` is the newest published app version, which
    /// can be older than a development build.
    UpToDate {
        latest: String,
    },
    /// A newer release, and the release page built from its validated tag.
    Available {
        version: String,
        url: String,
        changelog: Changelog,
    },
    /// The repository has no published app release yet.
    NoRelease,
    Failed(UpdateError),
}

/// The release notes between the installed version and the offered one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changelog {
    /// Every stable app release with installed < version <= offered that was
    /// found, newest first.
    pub entries: Vec<ReleaseNotes>,
    pub status: ChangelogStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseNotes {
    /// `0.2.0`.
    pub version: String,
    /// The release page, built from the validated tag.
    pub url: String,
    /// Markdown source, from the feed or GitHub's release body. Never render
    /// it as HTML.
    pub notes: String,
    /// The notes were longer than 64 KiB and were cut.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangelogStatus {
    /// Every release in the range is in `entries`.
    Complete,
    /// Some releases in the range may be missing (a paging limit or the
    /// deadline was reached, a later page failed, or the offered release was
    /// not listed). Link to [`APP_RELEASES_URL`] for the rest.
    Partial,
    /// The release list could not be read; `entries` is empty.
    Unavailable(UpdateError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    Offline,
    RateLimited,
    Other(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Offline => f.write_str("You appear to be offline"),
            UpdateError::RateLimited => f.write_str("GitHub rate limit reached, try again later"),
            UpdateError::Other(why) => write!(f, "Update check failed: {why}"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// What is known about published releases. [`UpdateSnapshot::mixer_update`]
/// answers for any mixer without another request, also before any check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateSnapshot {
    /// This app's check; `None` until one has finished.
    pub app: Option<UpdateCheck>,
    latest_mixer: Option<PublishedMixer>,
    history: Result<History, UpdateError>,
}

/// The mixer software bundled by the latest published app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedMixer {
    pub version: String,
    /// `None` when the feed does not record it.
    pub protocol: Option<u32>,
    /// The app release that bundles it.
    pub app_version: String,
}

/// How a mixer's software compares with what is bundled and published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixerUpdate {
    pub state: MixerUpdateState,
    /// The notes for installed < version <= the newest version `state`
    /// names, newest first; `None` when it names none. Complete only when
    /// the installed version and every version up to that one are in the
    /// known history with notes.
    pub changelog: Option<Changelog>,
}

/// Information only. Whether the installer may run is decided from the
/// bundled payload and the firmware it would flash, never from this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MixerUpdateState {
    /// The installed version is missing or not comparable, or nothing newer
    /// is bundled and the latest published version is unknown.
    Unknown,
    /// As new as the newest of the bundled and latest published versions.
    Current,
    /// Newer than the latest published version and any bundled one. Nothing
    /// is offered: that would be a downgrade.
    Newer,
    /// The bundled `version` is newer than the installed one. Says nothing
    /// about what is published when [`UpdateSnapshot::latest_mixer`] is
    /// `None`, and nothing about whether the installer may run.
    Bundled { version: String },
    /// The bundled `version` is newer than the installed one, and the app
    /// `app_version` carries the still newer `latest`.
    BundledThenApp {
        version: String,
        latest: String,
        app_version: String,
    },
    /// Nothing bundled is newer; the newer app `app_version` carries `latest`.
    AppUpdateRequired { latest: String, app_version: String },
}

impl From<UpdateCheck> for UpdateSnapshot {
    /// A check that learned nothing about mixer software.
    fn from(app: UpdateCheck) -> Self {
        let error = match &app {
            UpdateCheck::Failed(error) => error.clone(),
            _ => UpdateError::Other("the update feed was not read".into()),
        };
        Self {
            app: Some(app),
            latest_mixer: None,
            history: Err(error),
        }
    }
}

impl UpdateSnapshot {
    /// Before any check: only what the caller passes in is known.
    pub fn unchecked() -> Self {
        Self {
            app: None,
            latest_mixer: None,
            history: Err(UpdateError::Other("no update check has finished".into())),
        }
    }

    /// `None` when the latest published mixer software is unknown.
    pub fn latest_mixer(&self) -> Option<&PublishedMixer> {
        self.latest_mixer.as_ref()
    }

    /// Compare what the mixer reports with what this app bundles and what is
    /// published. A version that is missing or not comparable is unknown,
    /// never older or newer. No network: call it again whenever the mixer's
    /// reported version changes.
    pub fn mixer_update(&self, installed: Option<&str>, bundled: Option<&str>) -> MixerUpdate {
        let Some(installed) = installed.filter(|v| compare_versions(v, v).is_some()) else {
            return MixerUpdate {
                state: MixerUpdateState::Unknown,
                changelog: None,
            };
        };
        let bundled = bundled.filter(|v| compare_versions(v, v).is_some());
        let offered_bundled = bundled.filter(|v| is_newer(v, installed));
        let offered_latest = self
            .latest_mixer
            .as_ref()
            .filter(|l| is_newer(&l.version, installed));
        let state = match (offered_bundled, offered_latest) {
            (None, None) => match &self.latest_mixer {
                None => MixerUpdateState::Unknown,
                Some(latest)
                    if is_newer(installed, &latest.version)
                        && bundled.is_none_or(|b| is_newer(installed, b)) =>
                {
                    MixerUpdateState::Newer
                }
                Some(_) => MixerUpdateState::Current,
            },
            (Some(version), None) => MixerUpdateState::Bundled {
                version: version.to_owned(),
            },
            (None, Some(latest)) => MixerUpdateState::AppUpdateRequired {
                latest: latest.version.clone(),
                app_version: latest.app_version.clone(),
            },
            (Some(version), Some(latest)) if is_newer(&latest.version, version) => {
                MixerUpdateState::BundledThenApp {
                    version: version.to_owned(),
                    latest: latest.version.clone(),
                    app_version: latest.app_version.clone(),
                }
            }
            (Some(version), Some(_)) => MixerUpdateState::Bundled {
                version: version.to_owned(),
            },
        };
        let offered = match &state {
            MixerUpdateState::Unknown | MixerUpdateState::Current | MixerUpdateState::Newer => None,
            MixerUpdateState::Bundled { version } => Some(version),
            MixerUpdateState::BundledThenApp { latest, .. }
            | MixerUpdateState::AppUpdateRequired { latest, .. } => Some(latest),
        };
        let changelog = offered.map(|offered| match &self.history {
            Ok(history) => history.mixer_changelog(installed, offered),
            Err(error) => Changelog {
                entries: Vec::new(),
                status: ChangelogStatus::Unavailable(error.clone()),
            },
        });
        MixerUpdate { state, changelog }
    }
}

/// Ask whether a release newer than `current` (this app's version, e.g.
/// `0.1.0`) exists and what mixer software is published, and collect the
/// release notes since `current`. Blocking: up to ten seconds each for GitHub
/// and the feed, and about thirty more if the notes must come from GitHub's
/// release list. Call it off the UI thread.
pub fn check_for_updates(current: &str) -> UpdateSnapshot {
    snapshot_at(LATEST_API, LIST_API, UPDATE_FEED_URL, current)
}

fn snapshot_at(latest_url: &str, list_url: &str, feed_url: &str, current: &str) -> UpdateSnapshot {
    if compare_versions(current, current).is_none() {
        return UpdateCheck::Failed(UpdateError::Other(format!(
            "this app's version {current:?} is not a release version"
        )))
        .into();
    }
    let reply = match fetch(latest_url, MAX_BODY, TIMEOUT) {
        Ok(reply) => reply,
        Err(error) => return UpdateCheck::Failed(error).into(),
    };
    let app = evaluate(current, &reply);
    // The feed cannot say what is latest, so without GitHub's answer it is
    // not read.
    let latest = match &app {
        UpdateCheck::UpToDate { latest } => latest.clone(),
        UpdateCheck::Available { version, .. } => version.clone(),
        UpdateCheck::NoRelease | UpdateCheck::Failed(_) => return app.into(),
    };
    let history = fetch(feed_url, MAX_FEED_BODY, TIMEOUT).and_then(|r| History::parse(&r, &latest));
    let latest_mixer = history
        .as_ref()
        .ok()
        .and_then(|history| history.published_mixer(&latest));
    let app = match app {
        UpdateCheck::Available { version, url, .. } => {
            let changelog = history
                .as_ref()
                .ok()
                .map(|h| h.app_changelog(current, &version))
                .filter(|log| log.status == ChangelogStatus::Complete)
                .unwrap_or_else(|| github_changelog(list_url, current, &version));
            UpdateCheck::Available {
                version,
                url,
                changelog,
            }
        }
        other => other,
    };
    UpdateSnapshot {
        app: Some(app),
        latest_mixer,
        history,
    }
}

/// The notes since `current`, from GitHub's paged release list.
fn github_changelog(list_url: &str, current: &str, offered: &str) -> Changelog {
    let deadline = Instant::now() + NOTES_DEADLINE;
    changelog(current, offered, deadline, |page| {
        // No page may outlast the notes deadline.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(UpdateError::Other("the release notes took too long".into()));
        }
        fetch(
            &format!("{list_url}?per_page={PER_PAGE}&page={page}"),
            MAX_LIST_BODY,
            remaining.min(TIMEOUT),
        )
    })
}

fn is_newer(a: &str, b: &str) -> bool {
    compare_versions(a, b) == Some(Ordering::Greater)
}

/// `low` < `version` <= `high`.
fn in_range(version: &str, low: &str, high: &str) -> bool {
    is_newer(version, low) && !is_newer(version, high)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Feed {
    schema_version: u32,
    app: FeedList<FeedApp>,
    mixer: FeedList<FeedMixer>,
}

#[derive(serde::Deserialize)]
struct FeedList<T> {
    releases: Vec<T>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedMixer {
    version: String,
    #[serde(default)]
    notes_markdown: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedApp {
    version: String,
    #[serde(default)]
    notes_markdown: Option<String>,
    #[serde(default)]
    mixer_version: Option<String>,
    #[serde(default)]
    protocol_version: Option<u32>,
}

/// The feed, reduced to what is published.
#[derive(Clone, Debug, PartialEq, Eq)]
struct History {
    /// No newer than the latest published release, newest first.
    apps: Vec<FeedApp>,
    /// Each mixer version a listed app bundled, newest first.
    mixers: Vec<MixerEntry>,
    /// The latest published release is not listed, so what it bundles is
    /// unknown and a release before it may be missing too.
    stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MixerEntry {
    version: String,
    /// The first app release that bundled it.
    first_app: String,
    /// `None` when the feed has no notes for it.
    notes: Option<String>,
}

impl History {
    fn parse(reply: &Reply, latest: &str) -> Result<Self, UpdateError> {
        let bad = |why: String| UpdateError::Other(format!("the update feed {why}"));
        if reply.status != 200 {
            return Err(bad(format!("answered HTTP {}", reply.status)));
        }
        let feed: Feed = serde_json::from_slice(&reply.body)
            .map_err(|e| bad(format!("is not readable: {e}")))?;
        if feed.schema_version != FEED_SCHEMA {
            return Err(bad(format!("has schema {}", feed.schema_version)));
        }
        if feed.app.releases.len().max(feed.mixer.releases.len()) > MAX_FEED_RELEASES {
            return Err(bad("lists too many releases".into()));
        }
        let mut apps = feed.app.releases;
        let mut listed_mixers: Vec<&FeedMixer> = feed.mixer.releases.iter().collect();
        for app in &apps {
            let mixer_ok = app
                .mixer_version
                .as_ref()
                .is_none_or(|v| strict_version(v).is_some());
            if strict_version(&app.version).is_none()
                || !mixer_ok
                || app.protocol_version == Some(0)
            {
                return Err(bad(format!("has an invalid release {:?}", app.version)));
            }
        }
        if let Some(mixer) = listed_mixers
            .iter()
            .find(|m| strict_version(&m.version).is_none())
        {
            return Err(bad(format!("has an invalid release {:?}", mixer.version)));
        }
        let newest_first =
            |a: &str, b: &str| compare_versions(b, a).expect("feed versions compare");
        apps.sort_by(|a, b| newest_first(&a.version, &b.version));
        listed_mixers.sort_by(|a, b| newest_first(&a.version, &b.version));
        if apps.windows(2).any(|w| w[0].version == w[1].version)
            || listed_mixers
                .windows(2)
                .any(|w| w[0].version == w[1].version)
        {
            return Err(bad("lists a release twice".into()));
        }
        apps.retain(|app| !is_newer(&app.version, latest));
        let stale = apps.first().is_none_or(|app| app.version != latest);

        let mut mixers: Vec<MixerEntry> = Vec::new();
        for app in apps.iter().rev() {
            let Some(version) = &app.mixer_version else {
                continue;
            };
            let same = |v: &str| compare_versions(v, version) == Some(Ordering::Equal);
            if mixers.iter().any(|m| same(&m.version)) {
                continue;
            }
            mixers.push(MixerEntry {
                version: version.clone(),
                first_app: app.version.clone(),
                notes: listed_mixers
                    .iter()
                    .find(|m| same(&m.version))
                    .and_then(|m| m.notes_markdown.clone())
                    .filter(|notes| !notes.trim().is_empty()),
            });
        }
        mixers.sort_by(|a, b| {
            compare_versions(&b.version, &a.version).expect("feed versions compare")
        });
        Ok(Self {
            apps,
            mixers,
            stale,
        })
    }

    /// What the latest release bundles, when the feed lists that exact
    /// release and names it.
    fn published_mixer(&self, latest: &str) -> Option<PublishedMixer> {
        let app = self.apps.first().filter(|app| app.version == latest)?;
        Some(PublishedMixer {
            version: app.mixer_version.clone()?,
            protocol: app.protocol_version,
            app_version: app.version.clone(),
        })
    }

    /// Complete only when `current` and the offered release are listed and
    /// every release in the range has notes.
    fn app_changelog(&self, current: &str, offered: &str) -> Changelog {
        let in_feed: Vec<&FeedApp> = self
            .apps
            .iter()
            .filter(|app| in_range(&app.version, current, offered))
            .collect();
        let complete = self.apps.iter().any(|app| app.version == current)
            && in_feed.iter().any(|app| app.version == offered)
            && in_feed.iter().all(|app| has_notes(&app.notes_markdown));
        Changelog {
            entries: in_feed
                .iter()
                .map(|app| {
                    notes_entry(
                        &app.version,
                        &app.version,
                        app.notes_markdown.as_deref().unwrap_or_default(),
                    )
                })
                .collect(),
            status: if complete {
                ChangelogStatus::Complete
            } else {
                ChangelogStatus::Partial
            },
        }
    }

    /// Complete only when the installed and offered versions are listed,
    /// every version in the range has notes, and no release could hide
    /// another version: the feed is current and names the mixer software of
    /// every app release. An installed version that is not listed may
    /// predate the history, so what lies between is not known.
    fn mixer_changelog(&self, installed: &str, offered: &str) -> Changelog {
        let in_feed: Vec<&MixerEntry> = self
            .mixers
            .iter()
            .filter(|mixer| in_range(&mixer.version, installed, offered))
            .collect();
        let same = |a: &str, b: &str| compare_versions(a, b) == Some(Ordering::Equal);
        let complete = !self.stale
            && self.apps.iter().all(|app| app.mixer_version.is_some())
            && self.mixers.iter().any(|m| same(&m.version, installed))
            && in_feed.iter().any(|m| same(&m.version, offered))
            && in_feed.iter().all(|mixer| mixer.notes.is_some());
        Changelog {
            entries: in_feed
                .iter()
                .map(|mixer| {
                    notes_entry(
                        &mixer.version,
                        &mixer.first_app,
                        mixer.notes.as_deref().unwrap_or_default(),
                    )
                })
                .collect(),
            status: if complete {
                ChangelogStatus::Complete
            } else {
                ChangelogStatus::Partial
            },
        }
    }
}

fn has_notes(notes: &Option<String>) -> bool {
    notes.as_ref().is_some_and(|n| !n.trim().is_empty())
}

/// `version`'s notes, linking to the release page of `app_version`.
fn notes_entry(version: &str, app_version: &str, text: &str) -> ReleaseNotes {
    let (notes, truncated) = cut_notes(text);
    ReleaseNotes {
        version: version.to_owned(),
        url: release_url(app_version),
        notes,
        truncated,
    }
}

/// What the server answered, reduced to what the decision needs.
#[derive(Debug)]
struct Reply {
    status: u16,
    rate_limit_exhausted: bool,
    body: Vec<u8>,
}

fn fetch(url: &str, max_body: u64, timeout: Duration) -> Result<Reply, UpdateError> {
    if !url.starts_with("https://") && !cfg!(test) {
        return Err(UpdateError::Other("refusing a non-HTTPS URL".into()));
    }
    let client = reqwest::blocking::Client::builder()
        // Covers connecting, the reply and reading its body.
        .timeout(timeout)
        .connect_timeout(timeout)
        // Both hosts answer directly; a redirect elsewhere is not followed.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("rc2fx-update-check/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| UpdateError::Other(e.to_string()))?;
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(classify)?;
    let status = response.status().as_u16();
    let rate_limit_exhausted = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0");
    let mut body = Vec::new();
    response
        .take(max_body + 1)
        .read_to_end(&mut body)
        .map_err(|e| UpdateError::Other(format!("reading the reply: {e}")))?;
    if body.len() as u64 > max_body {
        return Err(UpdateError::Other("the reply is too large".into()));
    }
    Ok(Reply {
        status,
        rate_limit_exhausted,
        body,
    })
}

fn classify(error: reqwest::Error) -> UpdateError {
    if error.is_connect() || error.is_timeout() {
        UpdateError::Offline
    } else {
        UpdateError::Other(error.to_string())
    }
}

/// `v1.2.3` → `1.2.3`; anything else, including `mixer-v…`, is refused.
/// The same rule as the release script's STABLE pattern: three numbers,
/// none with a leading zero.
fn app_version(tag: &str) -> Option<&str> {
    strict_version(tag.strip_prefix('v')?)
}

fn strict_version(version: &str) -> Option<&str> {
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 9
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        }))
    .then_some(version)
}

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
}

/// A non-200 reply as an error: rate limits apart from everything else.
fn status_error(reply: &Reply) -> UpdateError {
    match reply.status {
        429 => UpdateError::RateLimited,
        403 if reply.rate_limit_exhausted => UpdateError::RateLimited,
        status => UpdateError::Other(format!("GitHub answered HTTP {status}")),
    }
}

fn evaluate(current: &str, reply: &Reply) -> UpdateCheck {
    let fail = |why: String| UpdateCheck::Failed(UpdateError::Other(why));
    match reply.status {
        200 => {}
        404 => return UpdateCheck::NoRelease,
        _ => return UpdateCheck::Failed(status_error(reply)),
    }
    let release: Release = match serde_json::from_slice(&reply.body) {
        Ok(release) => release,
        Err(e) => return fail(format!("GitHub's reply is not a release: {e}")),
    };
    if release.draft || release.prerelease {
        return fail("GitHub's latest release is not a published stable release".into());
    }
    let Some(latest) = app_version(&release.tag_name) else {
        return fail(format!(
            "the latest release {:?} is not an app release",
            release.tag_name
        ));
    };
    match compare_versions(latest, current) {
        Some(Ordering::Greater) => UpdateCheck::Available {
            version: latest.to_owned(),
            url: release_url(latest),
            changelog: Changelog {
                entries: Vec::new(),
                status: ChangelogStatus::Partial,
            },
        },
        Some(_) => UpdateCheck::UpToDate {
            latest: latest.to_owned(),
        },
        None => fail(format!("cannot compare {latest:?} with {current:?}")),
    }
}

fn release_url(version: &str) -> String {
    format!("{APP_RELEASES_URL}/tag/v{version}")
}

/// `text` cut to at most `MAX_NOTES` bytes at a character boundary.
fn cut_notes(text: &str) -> (String, bool) {
    if text.len() <= MAX_NOTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_NOTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

/// The notes of every stable app release with `current` < version <=
/// `offered`, newest first, from the paged release list `page(n)` (n from 1).
/// Complete only when the whole list was read within the limits and the
/// offered release was in it: the list is ordered by creation date, not by
/// version, so stopping early could miss a release.
fn changelog(
    current: &str,
    offered: &str,
    deadline: Instant,
    mut page: impl FnMut(u32) -> Result<Reply, UpdateError>,
) -> Changelog {
    let mut entries: Vec<ReleaseNotes> = Vec::new();
    let mut exhausted = false;
    let mut failure = None;
    for n in 1..=MAX_PAGES {
        if Instant::now() >= deadline {
            break;
        }
        let reply = match page(n) {
            Ok(reply) if reply.status == 200 => reply,
            Ok(reply) => {
                failure = Some(status_error(&reply));
                break;
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        };
        let releases: Vec<Release> = match serde_json::from_slice(&reply.body) {
            Ok(releases) => releases,
            Err(e) => {
                failure = Some(UpdateError::Other(format!(
                    "GitHub's release list is not readable: {e}"
                )));
                break;
            }
        };
        for release in &releases {
            if release.draft || release.prerelease {
                continue;
            }
            let Some(version) = app_version(&release.tag_name) else {
                continue;
            };
            let in_range = compare_versions(version, current) == Some(Ordering::Greater)
                && matches!(
                    compare_versions(version, offered),
                    Some(Ordering::Less | Ordering::Equal)
                );
            if !in_range || entries.iter().any(|e| e.version == version) {
                continue;
            }
            let (notes, truncated) = cut_notes(release.body.as_deref().unwrap_or_default());
            entries.push(ReleaseNotes {
                version: version.to_owned(),
                url: release_url(version),
                notes,
                truncated,
            });
        }
        if releases.len() < PER_PAGE {
            exhausted = true;
            break;
        }
    }
    entries.sort_by(|a, b| compare_versions(&b.version, &a.version).expect("app versions compare"));
    let status = match failure {
        Some(error) if entries.is_empty() => ChangelogStatus::Unavailable(error),
        _ if exhausted && entries.iter().any(|e| e.version == offered) => ChangelogStatus::Complete,
        _ => ChangelogStatus::Partial,
    };
    Changelog { entries, status }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    /// The check, with no feed to read.
    fn check_at(latest_url: &str, list_url: &str, current: &str) -> UpdateCheck {
        snapshot_at(latest_url, list_url, "http://127.0.0.1:9/no-feed", current)
            .app
            .unwrap()
    }

    fn ok(body: &str) -> Reply {
        Reply {
            status: 200,
            rate_limit_exhausted: false,
            body: body.as_bytes().to_vec(),
        }
    }

    fn release(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"html_url":"https://evil.example/"}}"#
        )
    }

    #[test]
    fn a_newer_stable_release_is_available_with_a_constructed_link() {
        let UpdateCheck::Available { version, url, .. } =
            evaluate("0.1.0", &ok(&release("v0.10.0")))
        else {
            panic!("a newer release is available");
        };
        assert_eq!(version, "0.10.0");
        assert_eq!(
            url,
            "https://github.com/ibanks42/rc2fx/releases/tag/v0.10.0"
        );
    }

    #[test]
    fn the_same_or_an_older_release_is_up_to_date() {
        for (current, tag) in [
            ("0.2.0", "v0.2.0"),
            ("0.3.0", "v0.2.9"),
            ("1.0.0", "v0.99.99"),
        ] {
            assert_eq!(
                evaluate(current, &ok(&release(tag))),
                UpdateCheck::UpToDate {
                    latest: tag[1..].into()
                },
                "{current} vs {tag}"
            );
        }
    }

    #[test]
    fn mixer_drafts_prereleases_and_odd_tags_are_never_offered() {
        for body in [
            release("mixer-v0.9.0"),
            release("0.9.0"),
            release("v0.9"),
            release("v0.9.0-rc1"),
            release("v0.9.0.1"),
            release("v0..9"),
            release("v01.2.3"),
            release("v1.02.3"),
            release("v1.2.00"),
            r#"{"tag_name":"v9.0.0","draft":true,"prerelease":false}"#.into(),
            r#"{"tag_name":"v9.0.0","draft":false,"prerelease":true}"#.into(),
            "not json".into(),
        ] {
            assert!(
                matches!(
                    evaluate("0.1.0", &ok(&body)),
                    UpdateCheck::Failed(UpdateError::Other(_))
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn no_release_rate_limits_and_errors_are_told_apart() {
        let reply = |status, exhausted| Reply {
            status,
            rate_limit_exhausted: exhausted,
            body: b"{}".to_vec(),
        };
        assert_eq!(
            evaluate("0.1.0", &reply(404, false)),
            UpdateCheck::NoRelease
        );
        for r in [reply(429, false), reply(403, true)] {
            assert_eq!(
                evaluate("0.1.0", &r),
                UpdateCheck::Failed(UpdateError::RateLimited)
            );
        }
        for r in [reply(403, false), reply(500, false), reply(301, false)] {
            assert!(matches!(
                evaluate("0.1.0", &r),
                UpdateCheck::Failed(UpdateError::Other(_))
            ));
        }
    }

    #[test]
    fn an_unreleasable_current_version_fails_without_a_request() {
        assert!(matches!(
            check_at(
                "http://127.0.0.1:9/never-contacted",
                "http://127.0.0.1:9/never-contacted",
                "dev-build"
            ),
            UpdateCheck::Failed(UpdateError::Other(_))
        ));
    }

    fn listed(tag: &str, body: &str) -> String {
        format!(r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"body":"{body}"}}"#)
    }

    fn page(items: &[String]) -> Result<Reply, UpdateError> {
        Ok(ok(&format!("[{}]", items.join(","))))
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn the_changelog_covers_exactly_the_range_newest_first() {
        let items = vec![
            listed("v0.4.0", "Four"),
            listed("mixer-v0.3.5", "Mixer"),
            listed("v0.2.0", "Two"),
            r#"{"tag_name":"v0.3.1","draft":true,"prerelease":false,"body":"Draft"}"#.into(),
            r#"{"tag_name":"v0.3.2","draft":false,"prerelease":true,"body":"Pre"}"#.into(),
            listed("v0.10.0", "Ten, created later than the offered one"),
            listed("v0.3.0", "Three"),
            listed("v0.1.0", "Installed"),
            listed("v0.4.0", "Duplicate"),
        ];
        let log = changelog("0.1.0", "0.4.0", later(), |_| page(&items));
        assert_eq!(log.status, ChangelogStatus::Complete);
        let versions: Vec<_> = log.entries.iter().map(|e| e.version.as_str()).collect();
        assert_eq!(versions, ["0.4.0", "0.3.0", "0.2.0"]);
        assert_eq!(log.entries[0].notes, "Four");
        assert_eq!(
            log.entries[2].url,
            "https://github.com/ibanks42/rc2fx/releases/tag/v0.2.0"
        );
    }

    #[test]
    fn a_range_that_is_not_fully_read_is_partial_never_complete() {
        // A full page every time: the paging limit is reached.
        let full: Vec<String> = (0..PER_PAGE)
            .map(|i| listed(&format!("v0.0.{i}"), ""))
            .collect();
        let mut first = full.clone();
        first[0] = listed("v0.2.0", "Two");
        let log = changelog("0.1.0", "0.2.0", later(), |n| {
            page(if n == 1 { &first } else { &full })
        });
        assert_eq!(log.status, ChangelogStatus::Partial);
        assert_eq!(log.entries.len(), 1);
        // The offered release is not in the list.
        let log = changelog("0.1.0", "0.3.0", later(), |_| page(&[listed("v0.2.0", "")]));
        assert_eq!(log.status, ChangelogStatus::Partial);
        // A later page fails after some notes were read.
        let log = changelog("0.1.0", "0.2.0", later(), |n| {
            if n == 1 {
                page(&first)
            } else {
                Err(UpdateError::Offline)
            }
        });
        assert_eq!(log.status, ChangelogStatus::Partial);
        // The deadline has passed before the first page.
        let log = changelog("0.1.0", "0.2.0", Instant::now(), |_| page(&first));
        assert_eq!(log.status, ChangelogStatus::Partial);
        assert!(log.entries.is_empty());
    }

    #[test]
    fn an_unreadable_list_is_unavailable_with_its_reason() {
        let log = changelog("0.1.0", "0.2.0", later(), |_| Err(UpdateError::Offline));
        assert_eq!(
            log.status,
            ChangelogStatus::Unavailable(UpdateError::Offline)
        );
        let log = changelog("0.1.0", "0.2.0", later(), |_| {
            Ok(Reply {
                status: 429,
                rate_limit_exhausted: false,
                body: Vec::new(),
            })
        });
        assert_eq!(
            log.status,
            ChangelogStatus::Unavailable(UpdateError::RateLimited)
        );
        let log = changelog("0.1.0", "0.2.0", later(), |_| Ok(ok("{}")));
        assert!(matches!(
            log.status,
            ChangelogStatus::Unavailable(UpdateError::Other(_))
        ));
    }

    #[test]
    fn long_notes_are_cut_at_a_character_boundary() {
        let text = "é".repeat(MAX_NOTES);
        let (cut, truncated) = cut_notes(&text);
        assert!(truncated && cut.len() <= MAX_NOTES && text.starts_with(&cut));
        assert_eq!(cut_notes("short"), ("short".to_owned(), false));
    }

    /// `(app, mixer)` rows, then `(mixer, notes)` rows, as a feed.
    fn feed(apps: &[(&str, Option<&str>)], mixers: &[(&str, Option<&str>)]) -> String {
        let quoted = |v: &Option<&str>| match v {
            Some(v) => format!("\"{v}\""),
            None => "null".into(),
        };
        let apps: Vec<String> = apps
            .iter()
            .map(|(version, mixer)| {
                format!(
                    r#"{{"version":"{version}","date":"2026-01-01","notesMarkdown":"App {version}","releaseUrl":"https://evil.example/","mixerVersion":{},"protocolVersion":1,"future":true}}"#,
                    quoted(mixer)
                )
            })
            .collect();
        let mixers: Vec<String> = mixers
            .iter()
            .map(|(version, notes)| {
                format!(
                    r#"{{"version":"{version}","date":null,"notesMarkdown":{}}}"#,
                    quoted(notes)
                )
            })
            .collect();
        format!(
            r#"{{"schemaVersion":1,"app":{{"latest":null,"releases":[{}]}},"mixer":{{"releases":[{}]}},"rode":{{}}}}"#,
            apps.join(","),
            mixers.join(",")
        )
    }

    fn history(body: &str, latest: &str) -> History {
        History::parse(&ok(body), latest).unwrap()
    }

    /// A snapshot whose latest published app is `latest`.
    fn snapshot(body: &str, latest: &str) -> UpdateSnapshot {
        let history = history(body, latest);
        UpdateSnapshot {
            app: Some(UpdateCheck::UpToDate {
                latest: latest.into(),
            }),
            latest_mixer: history.published_mixer(latest),
            history: Ok(history),
        }
    }

    fn versions(log: &Changelog) -> Vec<&str> {
        log.entries.iter().map(|e| e.version.as_str()).collect()
    }

    /// Apps 1.0.0 to 1.3.0, the mixer changing at 1.1.0 and 1.3.0.
    fn full_feed() -> String {
        feed(
            &[
                ("1.3.0", Some("1.2.0")),
                ("1.2.0", Some("1.1.0")),
                ("1.1.0", Some("1.1.0")),
                ("1.0.0", Some("1.0.0")),
            ],
            &[
                ("1.2.0", Some("Mixer two")),
                ("1.1.0", Some("Mixer one")),
                ("1.0.0", Some("Baseline")),
            ],
        )
    }

    #[test]
    fn an_unusable_feed_is_refused() {
        for body in [
            "not json".to_owned(),
            r#"{"schemaVersion":2,"app":{"releases":[]},"mixer":{"releases":[]}}"#.to_owned(),
            feed(&[("v1.0.0", None)], &[]),
            feed(&[("1.0", None)], &[]),
            feed(&[("1.0.0", Some("latest"))], &[]),
            feed(&[("1.0.0", None), ("1.0.0", None)], &[]),
            feed(&[], &[("1.0.0-beta", None)]),
            // Comparable, but not MAJOR.MINOR.PATCH.
            feed(&[("1.0.0", Some("1.0"))], &[]),
            feed(&[("1.0.0", Some("1.0.0.0"))], &[]),
            feed(&[("1.0.0", Some("01.0.0"))], &[]),
            feed(&[], &[("1.0", None)]),
            feed(&[], &[("1.0.0.0", None)]),
            feed(&[], &[("01.0.0", None)]),
            // A mixer listed twice, in any order, with or without conflicting notes.
            feed(&[], &[("1.0.0", Some("A")), ("1.0.0", Some("A"))]),
            feed(
                &[],
                &[("1.0.0", Some("A")), ("1.1.0", None), ("1.0.0", Some("B"))],
            ),
            feed(&[], &[("1.0.0", None), ("1.0.0", Some("B"))]),
            feed(&[("1.0.0", None)], &[])
                .replace(r#""protocolVersion":1"#, r#""protocolVersion":0"#),
        ] {
            assert!(History::parse(&ok(&body), "1.0.0").is_err(), "{body}");
        }
        let reply = Reply {
            status: 404,
            rate_limit_exhausted: false,
            body: Vec::new(),
        };
        assert!(History::parse(&reply, "1.0.0").is_err());
        let many = vec![("1.0.0", None); MAX_FEED_RELEASES + 1];
        assert!(History::parse(&ok(&feed(&many, &[])), "1.0.0").is_err());
    }

    #[test]
    fn the_latest_mixer_is_what_the_exact_latest_app_bundles() {
        let latest = snapshot(&full_feed(), "1.3.0");
        assert_eq!(
            latest.latest_mixer(),
            Some(&PublishedMixer {
                version: "1.2.0".into(),
                protocol: Some(1),
                app_version: "1.3.0".into(),
            })
        );
        // The feed has not caught up, or its row does not say.
        assert_eq!(snapshot(&full_feed(), "1.4.0").latest_mixer(), None);
        let unmapped = feed(&[("1.3.0", None), ("1.2.0", Some("1.1.0"))], &[]);
        assert_eq!(snapshot(&unmapped, "1.3.0").latest_mixer(), None);
    }

    #[test]
    fn entries_above_the_latest_release_are_ignored() {
        let early = snapshot(&full_feed(), "1.2.0");
        assert_eq!(early.latest_mixer().unwrap().version, "1.1.0");
        let update = early.mixer_update(Some("1.0.0"), Some("1.0.0"));
        assert_eq!(
            update.state,
            MixerUpdateState::AppUpdateRequired {
                latest: "1.1.0".into(),
                app_version: "1.2.0".into(),
            }
        );
        // Mixer 1.2.0 is listed only by the app above the latest release.
        assert_eq!(versions(&update.changelog.unwrap()), ["1.1.0"]);
        let log = history(&full_feed(), "1.2.0").app_changelog("1.0.0", "1.2.0");
        assert_eq!(versions(&log), ["1.2.0", "1.1.0"]);
    }

    #[test]
    fn app_notes_cover_skipped_releases_cumulatively() {
        let log = history(&full_feed(), "1.3.0").app_changelog("1.0.0", "1.3.0");
        assert_eq!(log.status, ChangelogStatus::Complete);
        assert_eq!(versions(&log), ["1.3.0", "1.2.0", "1.1.0"]);
        assert_eq!(log.entries[0].notes, "App 1.3.0");
        assert_eq!(
            log.entries[0].url,
            "https://github.com/ibanks42/rc2fx/releases/tag/v1.3.0"
        );
    }

    #[test]
    fn app_notes_that_cannot_be_proved_complete_are_partial() {
        let history = history(&full_feed(), "1.4.0");
        // The offered release is not listed.
        assert_eq!(
            history.app_changelog("1.0.0", "1.4.0").status,
            ChangelogStatus::Partial
        );
        // The running version is outside the listed history.
        assert_eq!(
            history.app_changelog("0.9.0", "1.3.0").status,
            ChangelogStatus::Partial
        );
        let missing = feed(&[("1.1.0", None), ("1.0.0", None)], &[])
            .replace(r#""notesMarkdown":"App 1.1.0""#, r#""notesMarkdown":null"#);
        assert_eq!(
            self::history(&missing, "1.1.0")
                .app_changelog("1.0.0", "1.1.0")
                .status,
            ChangelogStatus::Partial
        );
    }

    #[test]
    fn a_mixer_version_repeated_across_apps_appears_once_under_its_first_app() {
        let update = snapshot(&full_feed(), "1.3.0").mixer_update(Some("1.0.0"), Some("1.0.0"));
        let log = update.changelog.unwrap();
        assert_eq!(log.status, ChangelogStatus::Complete);
        assert_eq!(versions(&log), ["1.2.0", "1.1.0"]);
        assert_eq!(log.entries[1].notes, "Mixer one");
        assert_eq!(
            log.entries[1].url,
            "https://github.com/ibanks42/rc2fx/releases/tag/v1.1.0"
        );
    }

    #[test]
    fn mixer_notes_nobody_published_are_dropped() {
        let body = feed(
            &[("1.1.0", Some("1.1.0")), ("1.0.0", Some("1.0.0"))],
            &[
                ("1.0.5", Some("Never in an app")),
                ("1.1.0", Some("One")),
                ("1.0.0", Some("Baseline")),
            ],
        );
        let log = snapshot(&body, "1.1.0")
            .mixer_update(Some("1.0.0"), None)
            .changelog
            .unwrap();
        assert_eq!(log.status, ChangelogStatus::Complete);
        assert_eq!(versions(&log), ["1.1.0"]);
    }

    #[test]
    fn mixer_history_that_could_hide_a_release_is_partial() {
        let partial = |body: &str, latest: &str, installed: &str, bundled: Option<&str>| {
            let log = snapshot(body, latest)
                .mixer_update(Some(installed), bundled)
                .changelog
                .unwrap();
            assert_eq!(log.status, ChangelogStatus::Partial, "{installed} {body}");
            log
        };
        // An intermediate app's mixer software is unknown.
        let unmapped = feed(
            &[
                ("1.2.0", Some("1.2.0")),
                ("1.1.0", None),
                ("1.0.0", Some("1.0.0")),
            ],
            &[("1.2.0", Some("Two")), ("1.0.0", Some("Baseline"))],
        );
        assert_eq!(
            versions(&partial(&unmapped, "1.2.0", "1.0.0", None)),
            ["1.2.0"]
        );
        // An intermediate version has no notes, in either way of saying so.
        let no_notes = feed(
            &[
                ("1.2.0", Some("1.2.0")),
                ("1.1.0", Some("1.1.0")),
                ("1.0.0", Some("1.0.0")),
            ],
            &[
                ("1.2.0", Some("Two")),
                ("1.1.0", None),
                ("1.0.0", Some("Baseline")),
            ],
        );
        let log = partial(&no_notes, "1.2.0", "1.0.0", None);
        assert_eq!(versions(&log), ["1.2.0", "1.1.0"]);
        assert_eq!(log.entries[1].notes, "");
        let absent = feed(
            &[("1.1.0", Some("1.1.0")), ("1.0.0", Some("1.0.0"))],
            &[("1.0.0", Some("Baseline"))],
        );
        assert_eq!(
            versions(&partial(&absent, "1.1.0", "1.0.0", None)),
            ["1.1.0"]
        );
        // The installed version is outside the known history.
        assert_eq!(
            versions(&partial(&full_feed(), "1.3.0", "0.9.0", None)),
            ["1.2.0", "1.1.0", "1.0.0"]
        );
        assert_eq!(
            versions(&partial(&full_feed(), "1.3.0", "1.0.5", None)),
            ["1.2.0", "1.1.0"]
        );
        // A bundled version newer than the public feed has no notes there.
        let log = partial(&full_feed(), "1.3.0", "1.0.0", Some("1.5.0"));
        assert_eq!(versions(&log), ["1.2.0", "1.1.0"]);
    }

    #[test]
    fn a_stale_feed_keeps_old_notes_but_leaves_the_published_mixer_unknown() {
        let stale = snapshot(&full_feed(), "1.4.0");
        let update = stale.mixer_update(Some("1.0.0"), Some("1.2.0"));
        assert_eq!(
            update.state,
            MixerUpdateState::Bundled {
                version: "1.2.0".into()
            }
        );
        let log = update.changelog.unwrap();
        assert_eq!(log.status, ChangelogStatus::Partial);
        assert_eq!(versions(&log), ["1.2.0", "1.1.0"]);
    }

    #[test]
    fn mixer_states_compare_installed_bundled_and_latest() {
        use MixerUpdateState::*;
        let known = snapshot(&full_feed(), "1.3.0"); // latest mixer 1.2.0
        let bundled = |v: &str| Bundled { version: v.into() };
        let needs_app = AppUpdateRequired {
            latest: "1.2.0".into(),
            app_version: "1.3.0".into(),
        };
        for (installed, local, state) in [
            ("1.2.0", Some("1.2.0"), Current),
            ("1.2.0", None, Current),
            ("1.2.0", Some("1.1.0"), Current),
            ("1.3.0", Some("1.3.0"), Current),
            ("1.3.0", Some("1.2.0"), Newer),
            ("1.3.0", None, Newer),
            ("1.3.0", Some("not-a-version"), Newer),
            ("1.1.0", Some("1.2.0"), bundled("1.2.0")),
            ("1.1.0", Some("1.3.0"), bundled("1.3.0")),
            ("1.2.0", Some("1.3.0"), bundled("1.3.0")),
            ("1.1.0", Some("1.1.0"), needs_app.clone()),
            ("1.1.0", Some("1.0.0"), needs_app.clone()),
            ("1.1.0", None, needs_app.clone()),
            (
                "1.0.0",
                Some("1.1.0"),
                BundledThenApp {
                    version: "1.1.0".into(),
                    latest: "1.2.0".into(),
                    app_version: "1.3.0".into(),
                },
            ),
        ] {
            assert_eq!(
                known.mixer_update(Some(installed), local).state,
                state,
                "{installed} {local:?}"
            );
        }
    }

    #[test]
    fn a_missing_version_is_unknown_and_nothing_is_offered() {
        let known = snapshot(&full_feed(), "1.3.0");
        for installed in [None, Some("dev"), Some("1.2.0-beta"), Some("")] {
            assert_eq!(
                known.mixer_update(installed, Some("1.2.0")),
                MixerUpdate {
                    state: MixerUpdateState::Unknown,
                    changelog: None,
                }
            );
        }
        for up_to_date in ["1.2.0", "9.0.0"] {
            assert_eq!(
                known.mixer_update(Some(up_to_date), None).changelog,
                None,
                "{up_to_date}"
            );
        }
    }

    #[test]
    fn without_a_check_only_local_versions_are_compared() {
        let failure = UpdateSnapshot::from(UpdateCheck::Failed(UpdateError::Offline));
        for snapshot in [UpdateSnapshot::unchecked(), failure.clone()] {
            assert_eq!(snapshot.latest_mixer(), None);
            let update = snapshot.mixer_update(Some("1.0.0"), Some("1.1.0"));
            assert_eq!(
                update.state,
                MixerUpdateState::Bundled {
                    version: "1.1.0".into()
                }
            );
            let log = update.changelog.unwrap();
            assert!(log.entries.is_empty());
            assert!(matches!(log.status, ChangelogStatus::Unavailable(_)));
            // Not behind what is bundled, but the published version is unknown.
            for installed in ["1.1.0", "1.2.0"] {
                assert_eq!(
                    snapshot.mixer_update(Some(installed), Some("1.1.0")).state,
                    MixerUpdateState::Unknown
                );
            }
        }
        assert_eq!(UpdateSnapshot::unchecked().app, None);
        let log = failure
            .mixer_update(Some("1.0.0"), Some("1.1.0"))
            .changelog
            .unwrap();
        assert_eq!(
            log.status,
            ChangelogStatus::Unavailable(UpdateError::Offline)
        );
    }

    /// Serve one canned HTTP reply on a local port.
    fn serve(status: &str, headers: &str, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let reply = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            stream.write_all(reply.as_bytes()).unwrap();
        });
        format!("http://{address}/repos/x/y/releases/latest")
    }

    #[test]
    fn real_http_replies_are_classified() {
        let url = serve("200 OK", "", release("v0.2.0"));
        let list = serve(
            "200 OK",
            "",
            format!(
                "[{},{}]",
                listed("v0.2.0", "Second"),
                listed("mixer-v0.3.0", "x")
            ),
        );
        let list = list
            .trim_end_matches("/repos/x/y/releases/latest")
            .to_owned()
            + "/releases";
        let UpdateCheck::Available { changelog, .. } = check_at(&url, &list, "0.1.0") else {
            panic!("a newer release is available");
        };
        assert_eq!(changelog.status, ChangelogStatus::Complete);
        assert_eq!(changelog.entries.len(), 1);
        assert_eq!(changelog.entries[0].notes, "Second");
        let url = serve("404 Not Found", "", r#"{"message":"Not Found"}"#.into());
        assert_eq!(
            check_at(&url, "http://127.0.0.1:9/unused", "0.1.0"),
            UpdateCheck::NoRelease
        );
        let url = serve(
            "403 Forbidden",
            "X-RateLimit-Remaining: 0\r\n",
            r#"{"message":"API rate limit exceeded"}"#.into(),
        );
        assert_eq!(
            check_at(&url, "http://127.0.0.1:9/unused", "0.1.0"),
            UpdateCheck::Failed(UpdateError::RateLimited)
        );
        let url = serve("200 OK", "", "x".repeat(MAX_BODY as usize + 10));
        assert!(matches!(
            check_at(&url, "http://127.0.0.1:9/unused", "0.1.0"),
            UpdateCheck::Failed(UpdateError::Other(_))
        ));
    }

    #[test]
    fn a_feed_supplies_the_notes_and_github_stays_the_authority() {
        let list_never_read = "http://127.0.0.1:9/unused";
        let snapshot = snapshot_at(
            &serve("200 OK", "", release("v1.3.0")),
            list_never_read,
            &serve("200 OK", "", full_feed()),
            "1.2.0",
        );
        let Some(UpdateCheck::Available { changelog, .. }) = &snapshot.app else {
            panic!("a newer release is available");
        };
        assert_eq!(changelog.status, ChangelogStatus::Complete);
        assert_eq!(versions(changelog), ["1.3.0"]);
        assert_eq!(snapshot.latest_mixer().unwrap().version, "1.2.0");

        // An up-to-date app still learns about the mixer.
        let snapshot = snapshot_at(
            &serve("200 OK", "", release("v1.3.0")),
            list_never_read,
            &serve("200 OK", "", full_feed()),
            "1.3.0",
        );
        assert!(matches!(snapshot.app, Some(UpdateCheck::UpToDate { .. })));
        assert_eq!(snapshot.latest_mixer().unwrap().version, "1.2.0");

        // GitHub's answer is missing, so the feed is not even asked.
        let snapshot = snapshot_at(
            &serve("500 Oops", "", String::new()),
            list_never_read,
            "http://127.0.0.1:9/unused",
            "1.0.0",
        );
        assert!(matches!(snapshot.app, Some(UpdateCheck::Failed(_))));
        assert_eq!(snapshot.latest_mixer(), None);
    }

    #[test]
    fn app_notes_fall_back_to_github_without_a_usable_feed() {
        let list_url = |url: String| {
            url.trim_end_matches("/repos/x/y/releases/latest")
                .to_owned()
                + "/releases"
        };
        let github = || {
            list_url(serve(
                "200 OK",
                "",
                format!("[{}]", listed("v1.4.0", "From GitHub")),
            ))
        };
        // Missing, and a feed that has not caught up with the release.
        for feed in [
            serve("404 Not Found", "", String::new()),
            serve("200 OK", "", full_feed()),
        ] {
            let snapshot = snapshot_at(
                &serve("200 OK", "", release("v1.4.0")),
                &github(),
                &feed,
                "1.3.0",
            );
            let Some(UpdateCheck::Available { changelog, .. }) = &snapshot.app else {
                panic!("a newer release is available");
            };
            assert_eq!(changelog.status, ChangelogStatus::Complete);
            assert_eq!(changelog.entries[0].notes, "From GitHub");
            assert_eq!(snapshot.latest_mixer(), None);
        }
        // The prose line is no source of a mixer identity.
        let body = r#"{"tag_name":"v1.4.0","body":"Bundled RC2FX mixer software: 9.9.9."}"#;
        let snapshot = snapshot_at(
            &serve("200 OK", "", body.into()),
            &github(),
            &serve("404 Not Found", "", String::new()),
            "1.3.0",
        );
        assert_eq!(snapshot.latest_mixer(), None);
    }

    #[test]
    fn an_oversized_feed_is_refused() {
        let snapshot = snapshot_at(
            &serve("200 OK", "", release("v1.3.0")),
            "http://127.0.0.1:9/unused",
            &serve("200 OK", "", " ".repeat(MAX_FEED_BODY as usize + 10)),
            "1.3.0",
        );
        assert_eq!(snapshot.latest_mixer(), None);
        assert!(matches!(
            snapshot
                .mixer_update(Some("1.0.0"), Some("1.1.0"))
                .changelog
                .unwrap()
                .status,
            ChangelogStatus::Unavailable(UpdateError::Other(_))
        ));
    }

    #[test]
    fn nothing_listening_reads_as_offline() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // The listener is dropped, so the connection is refused.
        assert_eq!(
            check_at(
                &format!("http://127.0.0.1:{port}/"),
                "http://127.0.0.1:9/unused",
                "0.1.0"
            ),
            UpdateCheck::Failed(UpdateError::Offline)
        );
    }

    #[test]
    fn every_endpoint_and_link_is_on_the_official_repository() {
        let api = format!("https://api.github.com/repos/{APP_REPOSITORY}/releases");
        assert_eq!(LIST_API, api);
        assert_eq!(LATEST_API, format!("{api}/latest"));
        assert_eq!(
            APP_RELEASES_URL,
            format!("https://github.com/{APP_REPOSITORY}/releases")
        );
        assert!(release_url("1.2.3").starts_with(&format!("{APP_RELEASES_URL}/tag/")));
        let site = "https://ibanks42.github.io/rc2fx";
        assert_eq!(UPDATE_FEED_URL, format!("{site}/updates.json"));
        assert_eq!(APP_HISTORY_URL, format!("{site}/updates/app"));
        assert_eq!(MIXER_HISTORY_URL, format!("{site}/updates/mixer"));
    }
}
