//! Check downloaded app updates against signed release metadata.
//!
//! App releases have their own TUF repository, beside the mixer's: the same
//! trust root and role keys, but separate metadata on the `app-metadata`
//! branch. Every asset of a release is a target named `v{VERSION}/{ASSET}`
//! whose custom `version` field repeats the version. Velopack's own feed
//! says which package to fetch; this says whether the bytes fetched
//! are the ones that were signed.
//!
//! Each check starts from the trusted root with no saved state. Both
//! repositories share one root, so mixer metadata served at the app's
//! address verifies; saved state would let such a replay raise the app's
//! rollback floor and freeze its updates. Without it a replay can only offer
//! older metadata, which is acceptable here: the app only checks versions
//! newer than itself, which metadata from before their release cannot name,
//! and expiry (every role must be unexpired; app metadata lasts 7 days)
//! bounds how long stale metadata can hide a newer release.
//!
//! Verification is sigstore-tuf's, the library the mixer releases use too.
use sigstore_tuf::{TargetFile, Updater};
use std::{io::Read, path::Path, time::Duration};
use url::Url;

const TRUSTED_ROOT: &[u8] = include_bytes!("../../../config/mixer-update-root.json");
const METADATA_HOST: &str = "raw.githubusercontent.com";
const TARGETS_HOST: &str = "github.com";
const METADATA_URL: &str = "https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/";
const TARGETS_URL: &str = "https://github.com/ibanks42/rc2fx/releases/download/";
/// Metadata only: the package itself is already on disk.
const TIMEOUT: Duration = Duration::from_secs(120);

const UNSIGNED: &str = "This update has not been signed yet. Try again later.";
/// Signing lists only the latest release, so an older download disappears
/// from the signed list once a newer release is signed.
const SUPERSEDED: &str = "A newer update has replaced this one. Download the update again.";
const MISMATCH: &str = "The downloaded update does not match its signed release and was discarded.";

/// Why a package was not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The check could not be made (network, metadata, timeout): the file
    /// may still be good, so keep it and try again.
    Unavailable(String),
    /// The signed release says this file must not be applied: unsigned,
    /// different bytes, a bad name or version, or unreadable. Discard it.
    Rejected(String),
}

impl VerifyError {
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(message) | Self::Rejected(message) => message,
        }
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for VerifyError {}

/// False in a checkout without a trust root: there is nothing to check
/// updates against, so callers must not offer them.
pub fn configured() -> bool {
    TRUSTED_ROOT.trim_ascii() != b"null"
}

/// Check that `file` is exactly the signed asset `asset` of app `version`.
/// Blocking: it fetches the app metadata on its own runtime, so call it from
/// a background thread. A file whose bytes do not match is removed, so
/// nothing can apply it afterwards. The metadata is kept only in memory, for
/// this check; `work`, the app's working folder, is not written.
pub fn verify_update_package(
    _work: &Path,
    version: &str,
    asset: &str,
    file: &Path,
) -> Result<(), VerifyError> {
    let unavailable = |e: String| VerifyError::Unavailable(e);
    if !configured() {
        return Err(unavailable(
            "This build has no trust root for signed app updates.".into(),
        ));
    }
    let metadata = Url::parse(METADATA_URL).map_err(|e| unavailable(e.to_string()))?;
    let targets = Url::parse(TARGETS_URL).map_err(|e| unavailable(e.to_string()))?;
    check_repository_url(&metadata, METADATA_HOST).map_err(unavailable)?;
    check_repository_url(&targets, TARGETS_HOST).map_err(unavailable)?;
    let repository = crate::tuf::http(&metadata, &targets, TIMEOUT).map_err(unavailable)?;
    verify(TRUSTED_ROOT, repository, version, asset, file)
}

/// `repository` is the official one, or local files in tests.
fn verify(
    root: &[u8],
    repository: impl sigstore_tuf::Repository + 'static,
    version: &str,
    asset: &str,
    file: &Path,
) -> Result<(), VerifyError> {
    let rejected = |e: String| VerifyError::Rejected(e);
    if rc2fx_core::compare_versions(version, version).is_none()
        || asset.is_empty()
        || asset.contains(['/', '\\'])
        || asset.starts_with('.')
    {
        return Err(rejected(format!(
            "{asset} {version} is not an app update package."
        )));
    }
    let name = format!("v{version}/{asset}");
    let (target, newest) =
        signed_target(root, repository, &name).map_err(VerifyError::Unavailable)?;
    let Some(target) = target else {
        let superseded = newest.is_some_and(|newest| {
            rc2fx_core::compare_versions(&newest, version) == Some(std::cmp::Ordering::Greater)
        });
        return Err(rejected(
            if superseded { SUPERSEDED } else { UNSIGNED }.into(),
        ));
    };
    if version_of(&target) != Some(version) {
        return Err(rejected(
            "The signed release information does not describe this update.".into(),
        ));
    }
    let matches = Some(
        digest(file).map_err(|e| rejected(format!("Could not read the downloaded update: {e}")))?,
    ) == crate::tuf::sha256(&target).map(|sha256| (target.length, sha256));
    if !matches {
        // Best effort: the caller discards the update either way.
        let _ = std::fs::remove_file(file);
        return Err(rejected(MISMATCH.into()));
    }
    Ok(())
}

/// The target `name` from freshly verified metadata, or `None` when no
/// release with that name has been signed, and the newest version signed.
fn signed_target(
    root: &[u8],
    repository: impl sigstore_tuf::Repository + 'static,
    name: &str,
) -> Result<(Option<TargetFile>, Option<String>), String> {
    let unavailable = |e: &dyn std::fmt::Display| {
        format!("Could not check this update against its signed release: {e}. Try again later.")
    };
    // No store: every check starts from the trusted root (see the module
    // comment).
    let mut updater = Updater::new(repository, root).map_err(|e| unavailable(&e))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| unavailable(&e))?;
    // Expired metadata could be a replay that hides a newer release.
    let refreshed = runtime.block_on(async {
        tokio::time::timeout(TIMEOUT, updater.refresh(jiff::Timestamp::now())).await
    });
    refreshed
        .map_err(|_| unavailable(&"timed out"))?
        .map_err(|e| unavailable(&e))?;
    crate::tuf::distinct_keys(updater.trusted().root()).map_err(|e| unavailable(&e))?;
    let targets = updater
        .trusted()
        .targets_role("targets")
        .ok_or_else(|| unavailable(&"the signed metadata is incomplete"))?;
    // Only top-level targets are published. A delegation would extend trust
    // to roles whose lifetimes nothing here checks.
    if targets
        .delegations
        .as_ref()
        .is_some_and(|d| !d.roles.is_empty())
    {
        return Err(unavailable(&"delegated app targets are not supported"));
    }
    let newest = targets
        .targets
        .values()
        .filter_map(version_of)
        .fold(None::<&str>, |newest, v| match newest {
            Some(n) if rc2fx_core::compare_versions(v, n) != Some(std::cmp::Ordering::Greater) => {
                Some(n)
            }
            _ => Some(v),
        })
        .map(str::to_owned);
    Ok((targets.target(name).cloned(), newest))
}

/// The version a signed asset says it belongs to.
fn version_of(target: &TargetFile) -> Option<&str> {
    target.custom.as_ref()?.get("version")?.as_str()
}

/// Length and SHA-256 of a file, read once in pieces.
fn digest(file: &Path) -> std::io::Result<(u64, String)> {
    use sha2::Digest;
    let mut file = std::fs::File::open(file)?;
    let mut hash = sha2::Sha256::new();
    let mut length = 0;
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            let sum = hash.finalize();
            return Ok((length, sum.iter().map(|b| format!("{b:02x}")).collect()));
        }
        length += read as u64;
        hash.update(&buffer[..read]);
    }
}

/// The two addresses app metadata may come from, over plain HTTPS.
fn check_repository_url(url: &Url, host: &str) -> Result<(), String> {
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("App update repository URLs must use the official HTTPS hosts.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
