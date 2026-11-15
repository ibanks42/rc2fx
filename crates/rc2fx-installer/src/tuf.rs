//! What the two signed repositories (mixer releases and app releases) share:
//! the HTTPS transport, the metadata store and a check of each trusted root.
//! Verification itself is sigstore-tuf's, the same library the mixer uses.
use sigstore_tuf::{FileStore, HttpRepository, MetadataStore, Root, UpdaterConfig};
use std::time::Duration;
use url::Url;

/// An HTTPS repository with metadata under `metadata` and targets under
/// `targets`. A stalled connection fails after a minute; `total` bounds the
/// whole of any one request.
pub(crate) fn http(
    metadata: &Url,
    targets: &Url,
    total: Duration,
) -> Result<HttpRepository, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .timeout(total)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(HttpRepository::new(metadata.as_str())
        .and_then(|repository| repository.with_targets_base(targets.as_str()))
        .map_err(|e| e.to_string())?
        .with_http_client(client))
}

/// Limits for a repository whose largest target is `max_target` bytes.
pub(crate) fn config(max_target: u64) -> UpdaterConfig {
    let mut config = UpdaterConfig::default();
    config.target_max_length = max_target;
    config
}

/// The metadata store for a repository's rollback floors: verified role
/// files only. The library would also keep a copy of every downloaded
/// target here; the mixer payloads have their own cache.
pub(crate) struct MetadataOnly(pub(crate) FileStore);

impl MetadataStore for MetadataOnly {
    fn load(&self, name: &str) -> sigstore_tuf::Result<Option<Vec<u8>>> {
        if name.starts_with("targets/") {
            return Ok(None);
        }
        self.0.load(name)
    }

    fn store(&self, name: &str, bytes: &[u8]) -> sigstore_tuf::Result<()> {
        if name.starts_with("targets/") {
            return Ok(());
        }
        self.0.store(name, bytes)
    }
}

/// Refuse a root that lists one key under two IDs for a role. The library
/// counts signatures by key ID, so such a root could meet a threshold of two
/// with a single key. RC2FX's roles need one signature each today; this keeps
/// a future threshold honest. The mixer makes the same check.
pub(crate) fn distinct_keys(root: &Root) -> Result<(), String> {
    for (role, keys) in &root.roles {
        let mut material = std::collections::HashSet::new();
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

/// The SHA-256 a target is signed with, lowercase.
pub(crate) fn sha256(target: &sigstore_tuf::TargetFile) -> Option<String> {
    target
        .hashes
        .get("sha256")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|hex| hex.to_ascii_lowercase())
}

/// A repository in two local folders, for tests and debug builds' local
/// release folder: what the official hosts serve, laid out the same way.
#[cfg(any(test, debug_assertions))]
pub(crate) struct Folders {
    pub(crate) metadata: std::path::PathBuf,
    pub(crate) targets: std::path::PathBuf,
}

#[cfg(any(test, debug_assertions))]
impl Folders {
    fn read(
        base: &std::path::Path,
        name: &str,
        max_length: u64,
    ) -> sigstore_tuf::Result<Option<Vec<u8>>> {
        match std::fs::read(base.join(name)) {
            Ok(bytes) if bytes.len() as u64 > max_length => Err(sigstore_tuf::Error::transport(
                format!("{name} exceeds {max_length} bytes"),
            )),
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(sigstore_tuf::Error::io(name.to_owned(), e)),
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl sigstore_tuf::Repository for Folders {
    fn fetch_metadata<'a>(
        &'a self,
        name: &'a str,
        max_length: u64,
    ) -> sigstore_tuf::transport::FetchFuture<'a> {
        Box::pin(async move { Self::read(&self.metadata, name, max_length) })
    }

    fn fetch_target<'a>(
        &'a self,
        path: &'a str,
        max_length: u64,
    ) -> sigstore_tuf::transport::FetchFuture<'a> {
        Box::pin(async move { Self::read(&self.targets, path, max_length) })
    }
}
