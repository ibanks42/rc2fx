//! TUF verification for in-place mixer updates using the same embedded trust
//! root and role metadata (`root`, `timestamp`, `snapshot`, `targets`) as the
//! desktop app. Everything is verified in memory before anything is staged.
//!
//! Because the mixer has no trusted RTC, expiry checks run against [`NO_CLOCK`]
//! (Unix epoch) and replay/downgrade protection is enforced by persisting the
//! highest verified metadata versions beside the saved settings. The desktop
//! app still enforces metadata expiry before offering an update.

use super::install::{StoredState, sum};
use serde::{Deserialize, Serialize};
use sigstore_tuf::{Root, TrustedMetadataSet};
use std::{collections::HashSet, fs, path::Path};

/// Fixed epoch timestamp used for TUF expiry checks (mixer has no trusted RTC).
const NO_CLOCK: jiff::Timestamp = jiff::Timestamp::UNIX_EPOCH;
const MAX_ROOTS: usize = 8;

/// Embedded TUF root shared with the desktop app.
pub const TRUSTED_ROOT: &[u8] = include_bytes!("../../../../config/mixer-update-root.json");

/// The compatibility claims the publisher signs for one mixer release. Kept
/// in step with `MixerTarget` in `desktop/installer/mixer_update.go`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MixerTarget {
    pub version: String,
    pub payload_format: u32,
    pub protocol_version: u32,
    pub minimum_app_version: String,
    pub firmware_profiles: Vec<String>,
}

impl MixerTarget {
    fn validate(&self) -> Result<(), String> {
        if rc2fx_core::compare_versions(&self.version, &self.version).is_none()
            || rc2fx_core::compare_versions(&self.minimum_app_version, &self.minimum_app_version)
                .is_none()
            || self.payload_format != 1
            || self.protocol_version == 0
            || self.firmware_profiles.is_empty()
            || self.firmware_profiles.iter().any(|p| p.is_empty())
            || self.firmware_profiles.iter().collect::<HashSet<_>>().len()
                != self.firmware_profiles.len()
        {
            return Err("the signed mixer compatibility metadata is invalid".into());
        }
        Ok(())
    }
}

/// The four role documents the desktop downloaded.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataInput {
    /// The root documents, oldest first: one, or the chain after a key
    /// rotation. Each role is its file's exact text, because the snapshot
    /// and timestamp sign the bytes of the files they list; reformatted JSON
    /// would no longer match.
    pub root: Vec<String>,
    pub timestamp: String,
    pub snapshot: String,
    pub targets: String,
}

/// What the request claims the uploaded file is.
#[derive(Clone, Debug)]
pub struct ReleaseRequest {
    pub version: String,
    pub size: u64,
    pub sha256: String,
    pub allow_downgrade: bool,
}

/// A signed release that this mixer may install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRelease {
    pub version: String,
    pub metadata: MixerTarget,
    pub metadata_versions: StoredState,
    pub from: Option<String>,
}

/// Verify the metadata and the uploaded file. Nothing is written.
pub fn verify(
    trusted_root: &[u8],
    metadata: &MetadataInput,
    request: &ReleaseRequest,
    payload: &Path,
    profile: Option<&str>,
    running: Option<&str>,
    prior: &StoredState,
) -> Result<VerifiedRelease, String> {
    let trusted = trusted_metadata(trusted_root, metadata)?;
    let (Some(timestamp), Some(snapshot), Some(signed_targets)) = (
        trusted.timestamp(),
        trusted.snapshot(),
        trusted.targets_role("targets"),
    ) else {
        return Err("the signed mixer metadata is incomplete".into());
    };

    // RC2FX publishes top-level targets only. Delegated roles would extend
    // trust beyond the root this build carries.
    if signed_targets
        .delegations
        .as_ref()
        .is_some_and(|d| !d.roles.is_empty())
    {
        return Err("signed metadata uses delegated targets, which are not supported".into());
    }

    let candidates = signed_targets.targets.values().filter_map(|target| {
        let custom = target.custom.as_ref()?.get("rc2fxMixer")?;
        Some((
            target,
            serde_json::from_value::<MixerTarget>(custom.clone()),
        ))
    });
    let mut found = None;
    for (target, claims) in candidates {
        let claims = claims.map_err(|e| format!("signed mixer compatibility is invalid: {e}"))?;
        if claims.version == request.version {
            if found.is_some() {
                return Err("signed metadata names two releases with the same version".into());
            }
            found = Some((target, claims));
        }
    }
    let (target, claims) =
        found.ok_or_else(|| "signed metadata does not offer this release version".to_owned())?;
    claims.validate()?;
    if target.length != request.size {
        return Err("the uploaded size differs from the signed release".into());
    }
    if target.length == 0 || target.length > super::MAX_PAYLOAD {
        return Err("the signed release size is outside the supported range".into());
    }
    let requested = decode_hex(&request.sha256)
        .ok_or_else(|| "the request's sha256 is not 64 hexadecimal characters".to_owned())?;
    // Compare all 32 bytes of the signed SHA-256 digest.
    let signed = target.hashes.get("sha256").and_then(|hex| decode_hex(hex));
    if signed.as_deref() != Some(requested.as_slice()) {
        return Err("the request's sha256 is not the signed release's".into());
    }

    // The uploaded file is the only copy the mixer trusts, and only after this.
    let file_size = fs::metadata(payload)
        .map_err(|e| format!("cannot read the uploaded file: {e}"))?
        .len();
    if file_size != target.length {
        return Err(format!(
            "the uploaded file is {file_size} bytes; the signed release is {}",
            target.length
        ));
    }
    let digest = super::install::sum_file(payload)
        .map_err(|e| format!("cannot read the uploaded file: {e}"))?;
    if digest != request.sha256.to_ascii_lowercase() {
        return Err("the uploaded file does not match the request's sha256".into());
    }

    let profile = profile.ok_or_else(|| {
        "this mixer's firmware profile is unknown, so no release can be qualified for it".to_owned()
    })?;
    if !claims.firmware_profiles.iter().any(|p| p == profile) {
        return Err(format!(
            "the signed release does not support firmware profile {profile}"
        ));
    }

    // Releases only move forward unless the request asks for a downgrade.
    let floor = match (prior.installed_version.as_deref(), running) {
        (Some(stored), Some(now)) => match rc2fx_core::compare_versions(stored, now) {
            Some(std::cmp::Ordering::Less) => Some(now),
            Some(_) => Some(stored),
            None => Some(now),
        },
        (Some(stored), None) => Some(stored),
        (None, Some(now)) => Some(now),
        (None, None) => None,
    };
    if let Some(floor) = floor {
        match rc2fx_core::compare_versions(&claims.version, floor) {
            Some(std::cmp::Ordering::Equal) => {
                return Err("that release is already installed".into());
            }
            Some(std::cmp::Ordering::Less) if !request.allow_downgrade => {
                return Err(
                    "that release is older than the installed one; a downgrade must be requested explicitly"
                        .into(),
                );
            }
            Some(_) => {}
            None => return Err("the signed release version cannot be compared".into()),
        }
    }

    let root = trusted.root().version;
    let targets = signed_targets.version;
    let snapshot = snapshot.version;
    let timestamp = timestamp.version;
    // Hash the exact raw targets bytes; re-serializing parsed JSON isn't deterministic.
    let targets_digest = sum(metadata.targets.as_bytes());
    if root < prior.root_version
        || targets < prior.targets_version
        || snapshot < prior.snapshot_version
        || timestamp < prior.timestamp_version
    {
        return Err("signed metadata is older than the newest set this mixer verified".into());
    }
    if targets == prior.targets_version
        && !prior.targets_digest.is_empty()
        && targets_digest != prior.targets_digest
    {
        return Err("signed targets changed without a new version".into());
    }

    Ok(VerifiedRelease {
        version: claims.version.clone(),
        metadata: claims,
        metadata_versions: StoredState {
            schema: 1,
            installed_version: prior.installed_version.clone(),
            root_version: root.max(prior.root_version),
            targets_version: targets.max(prior.targets_version),
            snapshot_version: snapshot.max(prior.snapshot_version),
            timestamp_version: timestamp.max(prior.timestamp_version),
            targets_digest: if targets >= prior.targets_version {
                targets_digest
            } else {
                prior.targets_digest.clone()
            },
        },
        from: floor.map(str::to_owned),
    })
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.len() != 64 {
        return None;
    }
    let mut out = Vec::with_capacity(32);
    let mut chars = text.bytes();
    while let (Some(high), Some(low)) = (chars.next(), chars.next()) {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        out.push(digit(high)? << 4 | digit(low)?);
    }
    Some(out)
}

/// The metadata, verified from `trusted_root`: the root chain (each root
/// signed by the one before it and by itself), then timestamp, snapshot and
/// targets, each signed by its role and pinned by the one above it.
fn trusted_metadata(
    trusted_root: &[u8],
    metadata: &MetadataInput,
) -> Result<TrustedMetadataSet, String> {
    let roots = root_chain(metadata)?;
    let refused = |e: sigstore_tuf::Error| format!("the signed mixer metadata was refused: {e}");
    let mut trusted = TrustedMetadataSet::from_root(trusted_root).map_err(refused)?;
    distinct_keys(trusted.root())?;
    // Roots at or below the trusted version are already known; the chain
    // ends at the first version the request does not carry.
    while let Some((_, next)) = roots
        .iter()
        .find(|(version, _)| *version == trusted.root().version + 1)
    {
        trusted.update_root(next.as_bytes()).map_err(refused)?;
        distinct_keys(trusted.root())?;
    }
    trusted
        .update_timestamp(metadata.timestamp.as_bytes(), NO_CLOCK)
        .map_err(refused)?;
    trusted
        .update_snapshot(metadata.snapshot.as_bytes(), NO_CLOCK)
        .map_err(refused)?;
    trusted
        .update_targets(metadata.targets.as_bytes(), NO_CLOCK)
        .map_err(refused)?;
    Ok(trusted)
}

/// The request's roots by version. Each role is its file's exact text,
/// because signatures cover the documents as signed.
fn root_chain(metadata: &MetadataInput) -> Result<Vec<(u64, &str)>, String> {
    if metadata.root.is_empty() || metadata.root.len() > MAX_ROOTS {
        return Err("the request must carry one to eight root documents".into());
    }
    let mut seen: Vec<(u64, &str)> = Vec::new();
    for root in &metadata.root {
        // Parsed only to order the chain; the library verifies the text.
        let version = serde_json::from_str::<serde_json::Value>(root)
            .ok()
            .and_then(|root| root.get("signed")?.get("version")?.as_u64())
            .filter(|version| *version >= 1)
            .ok_or_else(|| "a root document has no version".to_owned())?;
        match seen.iter().find(|(v, _)| *v == version) {
            Some((_, previous)) if *previous != root.as_str() => {
                return Err("two different roots share one version".into());
            }
            Some(_) => {}
            None => seen.push((version, root)),
        }
    }
    Ok(seen)
}

/// Rejects any root that lists the same public key under multiple key IDs for
/// a role (prevents a single key from satisfying a threshold > 1).
fn distinct_keys(root: &Root) -> Result<(), String> {
    for (role, keys) in &root.roles {
        let mut material = HashSet::new();
        for id in &keys.keyids {
            if let Some(key) = root.keys.get(id)
                && !material.insert((&key.keytype, key.keyval.public.trim()))
            {
                return Err(format!(
                    "the signed root lists one {role} key under two IDs"
                ));
            }
        }
    }
    Ok(())
}

/// The profile ids a desktop installer considers qualified, mirrored so the
/// signed compatibility list must agree with the payload's own catalog.
pub fn qualified(profile: &rc2fx_firmware::Profile) -> bool {
    let Some(runtime) = &profile.runtime else {
        return false;
    };
    let Ok(model) = rc2fx_firmware::Model::parse(&profile.model) else {
        return false;
    };
    runtime.callback_abi == "jack-process-v1"
        && runtime.route_layout == model.route_layout()
        && runtime.source_enum == model.source_enum()
        && runtime.job_lifetime.abi == "capture-values-v1"
        && runtime.sample_rate == 48000
        && runtime.block_frames == 112
}
