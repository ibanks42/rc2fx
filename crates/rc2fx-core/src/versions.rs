//! Release identities: the RC2FX mixer runtime's own version, kept apart from
//! the desktop app's version and from RØDE's firmware version.
//!
//! The mixer runtime ships `mixer-release.json` (see `config/`), installed as
//! `/opt/rc2fx/mixer-release.json`. The device reports what it actually has
//! installed through `GET api/v1/version` as [`DeviceVersions`]. A runtime
//! that predates the endpoint answers 404, and its versions stay unknown.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// The control protocol this build speaks over USB 1. Bump it when a desktop
/// app built against an older protocol could no longer drive the mixer.
pub const MIXER_PROTOCOL_VERSION: u32 = 1;

/// The only `mixer-release.json` schema this build reads.
pub const MIXER_RELEASE_SCHEMA: u32 = 1;

/// The largest manifest accepted, far above any real one.
const MAX_RELEASE_BYTES: usize = 4 * 1024;

/// `mixer-release.json`: the RC2FX mixer runtime's release identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MixerRelease {
    pub schema_version: u32,
    pub version: String,
    pub protocol_version: u32,
}

impl MixerRelease {
    /// Parse and check a manifest: strict JSON, schema 1, a dotted numeric
    /// version and a non-zero protocol.
    pub fn parse(data: &[u8]) -> crate::Result<Self> {
        let text = std::str::from_utf8(data)
            .map_err(|_| crate::CoreError("mixer-release.json is not UTF-8".into()))?;
        let release: Self = crate::from_json_limited(text, MAX_RELEASE_BYTES, 16)?;
        if release.schema_version != MIXER_RELEASE_SCHEMA {
            return Err(crate::CoreError(format!(
                "unsupported mixer-release.json schema {}",
                release.schema_version
            )));
        }
        if parse_version(&release.version).is_none() {
            return Err(crate::CoreError(format!(
                "mixer-release.json version {:?} is not dotted numeric",
                release.version
            )));
        }
        if release.protocol_version == 0 {
            return Err(crate::CoreError(
                "mixer-release.json protocolVersion must be at least 1".into(),
            ));
        }
        Ok(release)
    }
}

/// What the mixer reports from `GET api/v1/version`. Every field is optional:
/// a field the runtime cannot establish is absent, never guessed. Unknown
/// fields are accepted so an older app can read a newer mixer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceVersions {
    /// The installed RC2FX mixer release version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The installed runtime's control protocol version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<u32>,
    /// RØDE's firmware version, from the profile the launcher validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// The validated firmware profile's id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware_profile: Option<String>,
}

fn parse_version(text: &str) -> Option<Vec<u64>> {
    if text.is_empty() {
        return None;
    }
    text.split('.')
        .map(|part| {
            if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect()
}

/// Compare dotted numeric versions (`1.7.7`, `0.1`). Missing trailing parts
/// count as zero, so `1.7` equals `1.7.0`. Anything else, including
/// pre-release suffixes, is not comparable and gives `None`.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parse_version(a)?, parse_version(b)?);
    let width = a.len().max(b.len());
    let part = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    Some(
        (0..width)
            .map(|i| part(&a, i).cmp(&part(&b, i)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_not_lexically() {
        assert_eq!(compare_versions("1.7.10", "1.7.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.1.0", "0.2"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.7", "1.7.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("2", "10"), Some(Ordering::Less));
    }

    #[test]
    fn unparseable_versions_are_not_comparable() {
        for bad in ["", "1..2", "1.2-rc1", "v1.2", " 1.2", "1.2.", "1234567890"] {
            assert_eq!(compare_versions(bad, "1.0"), None, "{bad:?}");
            assert_eq!(compare_versions("1.0", bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_shipped_manifest_parses_and_matches_this_protocol() {
        let release = MixerRelease::parse(include_bytes!("../../../config/mixer-release.json"))
            .expect("config/mixer-release.json is valid");
        assert_eq!(release.protocol_version, MIXER_PROTOCOL_VERSION);
    }

    #[test]
    fn manifests_are_checked() {
        let ok = br#"{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}"#;
        assert_eq!(MixerRelease::parse(ok).unwrap().version, "0.1.0");
        for bad in [
            &br#"{"schemaVersion":2,"version":"0.1.0","protocolVersion":1}"#[..],
            br#"{"schemaVersion":1,"version":"latest","protocolVersion":1}"#,
            br#"{"schemaVersion":1,"version":"0.1.0","protocolVersion":0}"#,
            br#"{"schemaVersion":1,"version":"0.1.0","protocolVersion":1,"x":1}"#,
            br#"{"schemaVersion":1,"version":"0.1.0","version":"0.2.0","protocolVersion":1}"#,
            br#"{"schemaVersion":1,"version":"0.1.0"}"#,
        ] {
            assert!(
                MixerRelease::parse(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn device_versions_omit_what_is_unknown_and_tolerate_new_fields() {
        assert_eq!(
            serde_json::to_string(&DeviceVersions::default()).unwrap(),
            "{}"
        );
        let newer: DeviceVersions = serde_json::from_str(
            r#"{"runtime":"0.2.0","protocol":2,"firmware":"1.7.7","firmwareProfile":"rodecaster-pro-ii-1.7.7","future":true}"#,
        )
        .unwrap();
        assert_eq!(newer.runtime.as_deref(), Some("0.2.0"));
        assert_eq!(
            newer.firmware_profile.as_deref(),
            Some("rodecaster-pro-ii-1.7.7")
        );
    }
}
