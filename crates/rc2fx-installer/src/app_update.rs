//! Whether a newer RC2FX desktop app has been released on GitHub.
//!
//! Unauthenticated HTTPS requests to GitHub's releases API for the official
//! repository: the latest release, then, when it is newer, the release notes
//! of every app version between this one and it. Nothing is downloaded or
//! installed: the result is versions, release notes and links. GitHub's
//! latest release excludes drafts and prereleases; this module still refuses
//! anything that is not a published, stable `vMAJOR.MINOR.PATCH` app release,
//! such as a `mixer-v` tag left from before mixer runtimes were only released
//! inside the app.

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
    /// The release body as GitHub stores it (Markdown source). Show it as
    /// plain text; it is never HTML to render.
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

/// Ask GitHub whether a release newer than `current` (this app's version,
/// e.g. `0.1.0`) exists and, if so, collect the release notes since
/// `current`. Blocking: about ten seconds for the check and at most about
/// thirty more for the notes. Call it off the UI thread.
pub fn check_for_app_update(current: &str) -> UpdateCheck {
    check_at(LATEST_API, LIST_API, current)
}

fn check_at(latest_url: &str, list_url: &str, current: &str) -> UpdateCheck {
    if compare_versions(current, current).is_none() {
        return UpdateCheck::Failed(UpdateError::Other(format!(
            "this app's version {current:?} is not a release version"
        )));
    }
    let offered = match fetch(latest_url, MAX_BODY, TIMEOUT) {
        Ok(reply) => evaluate(current, &reply),
        Err(error) => return UpdateCheck::Failed(error),
    };
    let UpdateCheck::Available { version, url, .. } = offered else {
        return offered;
    };
    let deadline = Instant::now() + NOTES_DEADLINE;
    let changelog = changelog(current, &version, deadline, |page| {
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
    });
    UpdateCheck::Available {
        version,
        url,
        changelog,
    }
}

/// What GitHub answered, reduced to what the decision needs.
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
        // The API answers directly; a redirect elsewhere is not followed.
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
        .map_err(|e| UpdateError::Other(format!("reading GitHub's reply: {e}")))?;
    if body.len() as u64 > max_body {
        return Err(UpdateError::Other("GitHub's reply is too large".into()));
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
    let version = tag.strip_prefix('v')?;
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
    }
}
