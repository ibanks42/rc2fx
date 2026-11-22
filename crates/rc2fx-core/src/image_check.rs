//! Root filesystem integrity check (`rc2fx.sh image-check`), exposed via
//! `GET api/v1/image-check`.
//!
//! The installer writes a SHA-256 manifest of RØDE's files into the image. On
//! initial boots, the launcher verifies the root filesystem against that
//! manifest, restores any damaged file from the inactive root partition if its
//! hash matches, and writes the `key=value` result to `/opt/rc2fx/data/image-check`.

pub use crate::schema_gen::{ImageCheck, ImageStatus, MAX_LISTED};

const MAX_RECORD: usize = 64 * 1024;

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
