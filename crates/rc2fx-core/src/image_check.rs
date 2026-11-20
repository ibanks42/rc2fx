//! Root filesystem integrity check (`rc2fx.sh image-check`), exposed via
//! `GET api/v1/image-check`.
//!
//! The installer writes a SHA-256 manifest of RØDE's files into the image. On
//! initial boots, the launcher verifies the root filesystem against that
//! manifest, restores any damaged file from the inactive root partition if its
//! hash matches, and writes the `key=value` result to `/opt/rc2fx/data/image-check`.

use serde::{Deserialize, Serialize};

/// Max file paths included in a report; overflow is counted in `unlisted`.
pub const MAX_LISTED: usize = 32;
const MAX_RECORD: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageStatus {
    /// No manifest in this image (pre-check install or in-place update).
    None,
    /// Manifest exists, but verification hasn't completed yet.
    Pending,
    /// All manifest files matched.
    Ok,
    /// Damaged files were restored from the inactive root; a reboot is needed
    /// since RØDE's boot already loaded the damaged copies.
    Repaired,
    /// At least one file is damaged and couldn't be restored.
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
    /// Count of repaired/damaged files omitted due to [`MAX_LISTED`].
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
    /// Parses the on-disk record against the current image's manifest digest.
    /// Records from an older manifest are treated as `Pending`.
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
