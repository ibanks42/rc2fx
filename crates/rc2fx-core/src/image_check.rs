//! The mixer's check of the files a flash unpacked (`rc2fx.sh image-check`),
//! as `GET api/v1/image-check` reports it.
//!
//! The installer lists RØDE's files with their SHA-256 in the image. On the
//! first boots the launcher compares the root against that list, restores a
//! damaged file from the other root partition when that copy is identical,
//! and records the outcome as `key=value` lines in `/opt/rc2fx/data/image-check`.

use serde::{Deserialize, Serialize};

/// How many file names a report carries; the rest are counted, not listed.
pub const MAX_LISTED: usize = 32;
/// The largest record read.
const MAX_RECORD: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageStatus {
    /// The image has no file list (a mixer installed before the check, or
    /// after an in-place update): there is nothing to check.
    None,
    /// The list is there and the check has not finished yet.
    Pending,
    /// Every listed file matches.
    Ok,
    /// Damaged files were restored; RØDE's boot already used the damaged
    /// copies, so the mixer needs a restart.
    Repaired,
    /// At least one listed file is damaged and could not be restored.
    Damaged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImageCheck {
    pub status: ImageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repaired: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub damaged: Vec<String>,
    /// Files beyond [`MAX_LISTED`] in either list.
    #[serde(skip_serializing_if = "is_zero")]
    pub unlisted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Default for ImageCheck {
    fn default() -> Self {
        Self {
            status: ImageStatus::None,
            checked: None,
            repaired: Vec::new(),
            damaged: Vec::new(),
            unlisted: 0,
        }
    }
}

impl ImageCheck {
    /// What the mixer reports from its record, given whether the image has a
    /// file list. A record written for an older list is still pending.
    pub fn from_record(record: Option<&str>, manifest_digest: Option<&str>) -> Self {
        let Some(digest) = manifest_digest else {
            return Self::default();
        };
        let pending = Self {
            status: ImageStatus::Pending,
            ..Self::default()
        };
        let Some(text) = record else {
            return pending;
        };
        let text = &text[..text.len().min(MAX_RECORD)];
        let mut check = pending.clone();
        let mut status = None;
        let mut manifest = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let list = match key {
                "status" => {
                    status = match value {
                        "ok" => Some(ImageStatus::Ok),
                        "repaired" => Some(ImageStatus::Repaired),
                        "damaged" => Some(ImageStatus::Damaged),
                        _ => None,
                    };
                    continue;
                }
                "manifest" => {
                    manifest = Some(value);
                    continue;
                }
                "checked" => {
                    check.checked = Some(value.chars().take(64).collect());
                    continue;
                }
                "repaired" => &mut check.repaired,
                "damaged" => &mut check.damaged,
                _ => continue,
            };
            if list.len() < MAX_LISTED {
                list.push(value.chars().take(256).collect());
            } else {
                check.unlisted += 1;
            }
        }
        match status {
            Some(status) if manifest == Some(digest) => Self { status, ..check },
            _ => pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "schema=1\nstatus=repaired\nmanifest=abc\nchecked=2026-11-15T22:10:00Z\nrepaired=usr/bin/rode-init-update-env.sh\n";

    #[test]
    fn a_record_for_this_image_is_reported() {
        let check = ImageCheck::from_record(Some(RECORD), Some("abc"));
        assert_eq!(check.status, ImageStatus::Repaired);
        assert_eq!(check.repaired, ["usr/bin/rode-init-update-env.sh"]);
        assert_eq!(check.checked.as_deref(), Some("2026-11-15T22:10:00Z"));
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["status"], "repaired");
        assert!(json.get("damaged").is_none());
    }

    #[test]
    fn no_list_no_record_or_a_stale_record() {
        assert_eq!(
            ImageCheck::from_record(Some(RECORD), None).status,
            ImageStatus::None
        );
        assert_eq!(
            ImageCheck::from_record(None, Some("abc")).status,
            ImageStatus::Pending
        );
        assert_eq!(
            ImageCheck::from_record(Some(RECORD), Some("other")).status,
            ImageStatus::Pending
        );
        assert_eq!(
            ImageCheck::from_record(Some("status=bogus\nmanifest=abc\n"), Some("abc")).status,
            ImageStatus::Pending
        );
    }

    #[test]
    fn long_lists_are_capped() {
        let mut record = String::from("status=damaged\nmanifest=abc\n");
        for n in 0..MAX_LISTED + 3 {
            record.push_str(&format!("damaged=file{n}\n"));
        }
        let check = ImageCheck::from_record(Some(&record), Some("abc"));
        assert_eq!(check.damaged.len(), MAX_LISTED);
        assert_eq!(check.unlisted, 3);
        // An older app reading a newer field set still parses.
        let parsed: ImageCheck = serde_json::from_str(r#"{"status":"ok","later":1}"#).unwrap();
        assert_eq!(parsed.status, ImageStatus::Ok);
    }
}
