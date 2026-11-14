//! TUF verification of an in-place update, on the mixer, with the same signed
//! metadata and the same trust root the desktop app uses.
//!
//! The desktop sends the role documents it downloaded (`root`, `timestamp`,
//! `snapshot`, `targets`); the mixer has its own embedded trusted root and
//! verifies the chain from that, so a compromised app or a hostile computer
//! on USB 1 cannot authorize a release. Key rotation works when the app sends
//! the intermediate roots as well (see `docs/developer/mixer-releases.md`).
//!
//! The mixer has no trusted clock, so metadata expiry is not enforced here
//! (tough's `ExpirationEnforcement::Unsafe`): the mixer cannot tell a stale
//! timestamp from a fresh one, and refusing everything on an unset clock
//! would make the feature unusable. Instead, the highest verified versions
//! are persisted beside the settings and an older metadata set or release is
//! refused: replays are blocked by the recorder, not the calendar. The
//! desktop still enforces expiry before it offers anything.

use super::install::{StoredState, sum};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::Path};
use tough::{ExpirationEnforcement, FilesystemTransport, RepositoryLoader};
use url::Url;

/// The trust anchor compiled into this controller. The same file the desktop
/// embeds; a release's metadata must chain to it.
pub const TRUSTED_ROOT: &[u8] = include_bytes!("../../../../config/mixer-update-root.json");

/// The compatibility claims the publisher signs for one mixer release. Kept
/// in step with `rc2fx-installer`'s `MixerTarget`.
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
    pub target: String,
    pub version: String,
    pub metadata: MixerTarget,
    pub metadata_versions: StoredState,
    pub from: Option<String>,
}

/// Verify the metadata and the uploaded file. Nothing outside `work` is read
/// or written.
#[allow(clippy::too_many_arguments)] // Each input is a distinct fact under test.
pub fn verify(
    trusted_root: &[u8],
    metadata: &MetadataInput,
    request: &ReleaseRequest,
    payload: &Path,
    profile: Option<&str>,
    running: Option<&str>,
    prior: &StoredState,
    work: &Path,
) -> Result<VerifiedRelease, String> {
    let dir = work.join("metadata");
    fs::create_dir_all(&dir).map_err(|e| format!("cannot stage metadata: {e}"))?;
    write_metadata(&dir, metadata)?;
    let datastore = work.join("tuf-state");
    fs::create_dir_all(&datastore).map_err(|e| format!("cannot create the TUF state: {e}"))?;
    let base = Url::from_directory_path(&dir)
        .map_err(|_| "the metadata directory is not a usable path".to_owned())?;
    let repository = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot verify signed metadata: {e}"))?
        .block_on(async {
            RepositoryLoader::new(&trusted_root, base.clone(), base)
                .transport(FilesystemTransport)
                .datastore(datastore.clone())
                .expiration_enforcement(ExpirationEnforcement::Unsafe)
                .load()
                .await
        })
        .map_err(|e| format!("the signed mixer metadata was refused: {e}"))?;

    // RC2FX publishes top-level targets only. Delegated roles would extend
    // trust beyond the root this build carries.
    if repository
        .targets()
        .signed
        .delegations
        .as_ref()
        .is_some_and(|d| !d.roles.is_empty())
    {
        return Err("signed metadata uses delegated targets, which are not supported".into());
    }

    let candidates = repository.all_targets().filter_map(|(name, target)| {
        let custom = target.custom.get("rc2fxMixer")?;
        Some((
            name.raw().to_owned(),
            target,
            serde_json::from_value::<MixerTarget>(custom.clone()),
        ))
    });
    let mut found = None;
    for (name, target, claims) in candidates {
        let claims = claims.map_err(|e| format!("signed mixer compatibility is invalid: {e}"))?;
        if claims.version == request.version {
            if found.is_some() {
                return Err("signed metadata names two releases with the same version".into());
            }
            found = Some((name, target, claims));
        }
    }
    let (target_name, target, claims) =
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
    // The signed target carries one SHA-256 digest; compare all 32 bytes.
    if !target
        .hashes
        .sha256
        .iter()
        .copied()
        .eq(requested.iter().copied())
    {
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

    let root = repository.root().signed.version.get();
    let targets = repository.targets().signed.version.get();
    let snapshot = repository.snapshot().signed.version.get();
    let timestamp = repository.timestamp().signed.version.get();
    let targets_digest = sum(&serde_json::to_vec(&repository.targets().signed)
        .map_err(|e| format!("cannot record the signed targets: {e}"))?);
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
        target: target_name,
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

/// Write the role documents into a directory tough can read. The roots are
/// named `<version>.root.json`, which is how TUF fetches a rotation chain.
fn write_metadata(dir: &Path, metadata: &MetadataInput) -> Result<(), String> {
    if metadata.root.is_empty() || metadata.root.len() > 8 {
        return Err("the request must carry one to eight root documents".into());
    }
    let mut seen: Vec<(u64, Vec<u8>)> = Vec::new();
    for root in &metadata.root {
        // Parsed only to name the file; the original bytes are what's written.
        let version = serde_json::from_str::<serde_json::Value>(root)
            .ok()
            .and_then(|root| root.get("signed")?.get("version")?.as_u64())
            .filter(|version| *version >= 1)
            .ok_or_else(|| "a root document has no version".to_owned())?;
        let bytes = root.as_bytes().to_vec();
        if let Some((_, previous)) = seen.iter().find(|(v, _)| *v == version) {
            if previous != &bytes {
                return Err("two different roots share one version".into());
            }
            continue;
        }
        write(&dir.join(format!("{version}.root.json")), &bytes)?;
        seen.push((version, bytes));
    }
    for (name, document) in [
        ("timestamp.json", &metadata.timestamp),
        ("snapshot.json", &metadata.snapshot),
        ("targets.json", &metadata.targets),
    ] {
        if !serde_json::from_str::<serde_json::Value>(document).is_ok_and(|d| d.is_object()) {
            return Err(format!("{name} must be a signed role document"));
        }
        write(&dir.join(name), document.as_bytes())?;
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("cannot stage {}: {e}", path.display()))
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
