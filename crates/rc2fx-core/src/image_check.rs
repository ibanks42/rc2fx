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
