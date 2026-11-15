//! Authenticate complete mixer payloads before exposing their contents to the installer.
use crate::{Error, Result, embed::Envelope, runtime::Bundle};
use serde::{Deserialize, Serialize};
use sigstore_tuf::{TargetFile, Targets, Updater};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

const MAX_PAYLOAD: u64 = (256 << 20) + 256;
const MAX_CATALOG: u64 = 2 << 20;
static CATALOG: std::sync::Mutex<Option<VerifiedCatalog>> = std::sync::Mutex::new(None);

#[derive(Clone, Debug)]
pub struct VerifiedCatalog {
    catalog: crate::Catalog,
    bytes: Vec<u8>,
    root_version: u64,
    targets_version: u64,
    expires: jiff::Timestamp,
}

impl VerifiedCatalog {
    pub fn catalog(&self) -> Result<&crate::Catalog> {
        if jiff::Timestamp::now() >= self.expires {
            return Err(error(
                "firmware catalog authorization expired; check for updates again",
            ));
        }
        Ok(&self.catalog)
    }
}

pub(crate) fn cached_catalog() -> Result<crate::Catalog> {
    let cache = CATALOG
        .lock()
        .map_err(|_| error("catalog cache unavailable"))?;
    Ok(cache
        .as_ref()
        .ok_or_else(|| error("check signed firmware releases before installing"))?
        .catalog()?
        .clone())
}

pub(crate) fn cached_catalog_bytes() -> Result<Vec<u8>> {
    let cache = CATALOG
        .lock()
        .map_err(|_| error("catalog cache unavailable"))?;
    let verified = cache
        .as_ref()
        .ok_or_else(|| error("check signed firmware releases before inspecting firmware"))?;
    verified.catalog()?;
    Ok(verified.bytes.clone())
}

pub(crate) async fn refresh_catalog(repository: &MixerRepository) -> Result<()> {
    let catalog = repository.catalog().await?;
    let mut cache = CATALOG
        .lock()
        .map_err(|_| error("catalog cache unavailable"))?;
    if let Some(previous) = cache.as_ref() {
        let old = (previous.root_version, previous.targets_version);
        let new = (catalog.root_version, catalog.targets_version);
        if new < old || (new == old && previous.bytes != catalog.bytes) {
            return Err(error(
                "firmware catalog is older than the latest verified check or changed under the same metadata version",
            ));
        }
    }
    *cache = Some(catalog);
    Ok(())
}
const TRUSTED_ROOT: &[u8] = include_bytes!("../../../config/mixer-update-root.json");
const METADATA_HOST: &str = "raw.githubusercontent.com";
const TARGETS_HOST: &str = "github.com";
const METADATA_URL: &str = "https://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/";
const TARGETS_URL: &str = "https://github.com/ibanks42/rc2fx/releases/download/";

/// An exact signed offer, safe to pass between the app and helper. The helper
/// still retrieves trusted metadata and checks every field before accepting it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MixerOffer {
    pub target: String,
    pub sha256: String,
    pub length: u64,
    pub metadata: MixerTarget,
}

/// A release's signed metadata exactly as published, for a mixer that checks
/// it itself (an in-place update). Each role is its file's text: the snapshot
/// and timestamp sign the bytes of the files they list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawMetadata {
    /// Every root, oldest first: the mixer follows a key rotation from the
    /// root it was built with.
    pub root: Vec<String>,
    pub timestamp: String,
    pub snapshot: String,
    pub targets: String,
}

const RAW_METADATA_LIMIT: u64 = 1 << 20;
const MAX_ROOTS: u64 = 8;

/// Fetch the published role files byte for byte. The mixer verifies them
/// against its own trusted root; this only makes sure they belong together,
/// retrying when a release was published between two reads.
pub fn raw_metadata() -> Result<RawMetadata> {
    #[cfg(debug_assertions)]
    if let Some(folder) = dev_repository() {
        return assemble(|name| {
            match std::fs::read_to_string(folder.join("metadata").join(name)) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        });
    }
    let base = Url::parse(METADATA_URL).map_err(error)?;
    check_repository_url(&base, METADATA_HOST)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let fetch = |name: &str| -> Result<Option<String>> {
        use std::io::Read;
        let response = client.get(base.join(name).map_err(error)?).send()?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        response
            .error_for_status()?
            .take(RAW_METADATA_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > RAW_METADATA_LIMIT {
            return Err(error(format!("signed metadata {name} is too large")));
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| error(format!("signed metadata {name} is not text")))
    };
    assemble(fetch)
}

/// The role files `fetch` returns, checked to belong together.
fn assemble(fetch: impl Fn(&str) -> Result<Option<String>>) -> Result<RawMetadata> {
    let required =
        |name: &str| fetch(name)?.ok_or_else(|| error(format!("signed metadata has no {name}")));
    let mut root = Vec::new();
    for version in 1..=MAX_ROOTS {
        match fetch(&format!("{version}.root.json"))? {
            Some(text) => root.push(text),
            None => break,
        }
    }
    if root.is_empty() {
        return Err(error("signed metadata has no root"));
    }
    for _ in 0..3 {
        let timestamp = required("timestamp.json")?;
        let snapshot = required("snapshot.json")?;
        let targets = required("targets.json")?;
        if lists(&timestamp, "snapshot.json", &snapshot)
            && lists(&snapshot, "targets.json", &targets)
        {
            return Ok(RawMetadata {
                root,
                timestamp,
                snapshot,
                targets,
            });
        }
    }
    Err(error(
        "signed metadata kept changing while it was read; try again",
    ))
}

/// Whether `listing` names `file` with the length and SHA-256 (where given)
/// of `content`, as TUF's timestamp and snapshot do.
fn lists(listing: &str, name: &str, content: &str) -> bool {
    let Ok(listing) = serde_json::from_str::<serde_json::Value>(listing) else {
        return false;
    };
    let Some(meta) = listing.pointer(&format!(
        "/signed/meta/{}",
        name.replace('~', "~0").replace('/', "~1")
    )) else {
        return false;
    };
    let length_ok = meta
        .get("length")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|length| length == content.len() as u64);
    let hash_ok = meta
        .pointer("/hashes/sha256")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|hash| hash.eq_ignore_ascii_case(&crate::files::sum(content.as_bytes())));
    length_ok && hash_ok
}

/// The two addresses updates may come from, over plain HTTPS.
fn check_repository_url(url: &Url, host: &str) -> Result<()> {
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(error(
            "update repository URLs must use the official HTTPS hosts",
        ));
    }
    Ok(())
}

pub fn configured() -> bool {
    TRUSTED_ROOT.trim_ascii() != b"null"
}

/// Debug builds only: `RC2FX_DEV_MIXER_REPOSITORY=FOLDER` reads mixer
/// releases from a local folder laid out as published (`FOLDER/metadata/`
/// holds the role files, `FOLDER/mixer-vVERSION/` the targets). It is for
/// testing an app and mixer built with a throwaway trust root; anything
/// signed for the official root verifies the same as it would from GitHub.
#[cfg(debug_assertions)]
fn dev_repository() -> Option<std::path::PathBuf> {
    std::env::var_os("RC2FX_DEV_MIXER_REPOSITORY").map(std::path::PathBuf::from)
}

pub(crate) async fn official(work: &Path) -> Result<MixerRepository> {
    if !configured() {
        return Err(error("this app has no configured mixer release trust root"));
    }
    #[cfg(debug_assertions)]
    if let Some(folder) = dev_repository() {
        // Its own floors, so a test root never meets the official one's.
        return MixerRepository::open(
            crate::tuf::Folders {
                metadata: folder.join("metadata"),
                targets: folder,
            },
            TRUSTED_ROOT,
            &work.join("mixer-metadata-dev"),
        )
        .await;
    }
    let state = metadata_directory(work)?;
    MixerRepository::load(
        TRUSTED_ROOT,
        Url::parse(METADATA_URL).map_err(error)?,
        Url::parse(TARGETS_URL).map_err(error)?,
        &state,
    )
    .await
}

pub(crate) fn metadata_directory(work: &Path) -> Result<std::path::PathBuf> {
    // The Linux helper must not let a desktop-controlled symlink redirect the
    // metadata store's writes as root. Its rollback state is independent and root-owned.
    if let Some(path) = helper_directory("mixer-updates")? {
        return Ok(path);
    }
    Ok(work.join("mixer-metadata"))
}

/// `name` under `/var/lib/rc2fx` when running as the privileged Linux helper:
/// state the desktop user cannot change. `None` everywhere else.
#[cfg(target_os = "linux")]
pub(crate) fn helper_directory(name: &str) -> Result<Option<std::path::PathBuf>> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(None);
    }
    let mut path = std::path::PathBuf::from("/");
    for part in ["var", "lib", "rc2fx", name] {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            }
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(error(
                "helper metadata directory must be a root-owned directory, not writable by other users",
            ));
        }
    }
    Ok(Some(path))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn helper_directory(_: &str) -> Result<Option<std::path::PathBuf>> {
    Ok(None)
}

#[cfg(test)]
mod tests;

/// Compatibility claims covered by the TUF targets signature.
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
    /// Derive signing metadata from the same complete release payload the
    /// installer validates. Publishers cannot advertise unqualified profiles.
    pub fn for_payload(payload: &Envelope, minimum_app_version: &str) -> Result<Self> {
        let bundle = payload.bundle().map_err(error)?;
        let release =
            rc2fx_core::MixerRelease::parse(&bundle["mixer-release.json"]).map_err(error)?;
        let catalog = crate::embed::parse_catalog(&bundle["firmware-profiles.json"])?;
        let metadata = Self {
            version: release.version,
            payload_format: 1,
            protocol_version: release.protocol_version,
            minimum_app_version: minimum_app_version.to_owned(),
            firmware_profiles: catalog
                .profiles
                .iter()
                .filter(|p| crate::firmware::supported(p))
                .map(|p| p.id.clone())
                .collect(),
        };
        metadata.validate()?;
        MixerRepository::verify_payload(
            payload.as_bytes(),
            metadata.clone(),
            &metadata.firmware_profiles[0],
            None,
        )?;
        Ok(metadata)
    }

    pub fn require_compatible(
        &self,
        app_version: &str,
        protocol: u32,
        profile: &str,
    ) -> Result<()> {
        self.validate()?;
        if self.payload_format != 1
            || self.protocol_version != protocol
            || !self.firmware_profiles.iter().any(|p| p == profile)
            || !matches!(
                rc2fx_core::compare_versions(app_version, &self.minimum_app_version),
                Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
            )
        {
            return Err(error(
                "mixer release is incompatible with this app or firmware",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if rc2fx_core::compare_versions(&self.version, &self.version).is_none()
            || rc2fx_core::compare_versions(&self.minimum_app_version, &self.minimum_app_version)
                .is_none()
            || self.protocol_version == 0
            || self.firmware_profiles.is_empty()
            || self.firmware_profiles.iter().any(|p| p.is_empty())
            || self
                .firmware_profiles
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.firmware_profiles.len()
        {
            return Err(error("invalid signed mixer compatibility metadata"));
        }
        Ok(())
    }
}

/// Owned, authenticated bytes. No public constructor accepts an unchecked bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedMixerPayload {
    pub(crate) bundle: Arc<Bundle>,
    bytes: Arc<Vec<u8>>,
    metadata: MixerTarget,
}

impl VerifiedMixerPayload {
    pub fn metadata(&self) -> &MixerTarget {
        &self.metadata
    }
}

/// A repository authenticated against a caller-supplied, previously trusted root.
/// The desktop caller must supply its compiled-in root, never a downloaded root.
pub struct MixerRepository {
    updater: Updater,
}

fn error(e: impl std::fmt::Display) -> Error {
    Error(format!("Mixer update verification: {e}"))
}

impl crate::Engine {
    pub(crate) fn mixer_operation<T>(
        &self,
        operation: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let cancellation = async {
                loop {
                    if self.cancelled() {
                        return Err(Error(crate::CANCELLED.into()));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            };
            tokio::select! {
                result = tokio::time::timeout(std::time::Duration::from_secs(1800), operation) =>
                    result.map_err(|_| error("download timed out"))?,
                result = cancellation => result,
            }
        })
    }
}

impl MixerRepository {
    pub async fn catalog(&self) -> Result<VerifiedCatalog> {
        self.require_current()?;
        let newest = self
            .offers()?
            .into_iter()
            .next()
            .ok_or_else(|| error("no signed mixer releases"))?;
        let candidates: Vec<_> = self
            .targets()?
            .targets
            .iter()
            .filter(|(_, t)| {
                t.custom
                    .as_ref()
                    .and_then(|custom| custom.get("rc2fxCatalog"))
                    .and_then(serde_json::Value::as_str)
                    == Some(newest.metadata.version.as_str())
            })
            .collect();
        if candidates.len() != 1 {
            return Err(error(
                "release must identify exactly one signed firmware catalog",
            ));
        }
        let (name, target) = candidates[0];
        if target.length == 0 || target.length > MAX_CATALOG {
            return Err(error("firmware catalog exceeds size limit"));
        }
        let bytes = self.read(name, target).await?;
        if bytes.len() as u64 != target.length {
            return Err(error("firmware catalog length mismatch"));
        }
        let catalog = crate::embed::parse_catalog(&bytes)?;
        let mut supported: Vec<_> = catalog
            .profiles
            .iter()
            .filter(|p| crate::firmware::supported(p))
            .map(|p| p.id.clone())
            .collect();
        let mut expected = newest.metadata.firmware_profiles;
        supported.sort();
        expected.sort();
        if supported != expected {
            return Err(error(
                "signed firmware catalog disagrees with mixer compatibility",
            ));
        }
        let expires = self.expiries()?.into_iter().min().unwrap();
        Ok(VerifiedCatalog {
            catalog,
            bytes,
            expires,
            root_version: self.updater.trusted().root().version,
            targets_version: self.targets()?.version,
        })
    }
    /// The verified top-level targets.
    fn targets(&self) -> Result<&Targets> {
        self.updater
            .trusted()
            .targets_role("targets")
            .ok_or_else(|| error("signed mixer metadata is incomplete"))
    }
    /// When each of the four verified roles expires.
    fn expiries(&self) -> Result<[jiff::Timestamp; 4]> {
        let trusted = self.updater.trusted();
        let (Some(timestamp), Some(snapshot)) = (trusted.timestamp(), trusted.snapshot()) else {
            return Err(error("signed mixer metadata is incomplete"));
        };
        Ok([
            trusted.root().expires,
            timestamp.expires,
            snapshot.expires,
            self.targets()?.expires,
        ])
    }
    /// The target `name`, downloaded and checked against its signed length
    /// and hash before any byte is returned.
    async fn read(&self, name: &str, target: &TargetFile) -> Result<Vec<u8>> {
        self.updater
            .download_target(target, name)
            .await
            .map_err(error)
    }
    /// The signed release target `name`.
    fn target(&self, name: &str) -> Result<&TargetFile> {
        self.targets()?
            .target(name)
            .ok_or_else(|| error("target disappeared"))
    }
    fn require_current(&self) -> Result<()> {
        let now = jiff::Timestamp::now();
        if self.expiries()?.into_iter().any(|expires| expires <= now) {
            return Err(error(
                "signed mixer metadata expired; refresh before installation",
            ));
        }
        // RC2FX only publishes top-level targets. Avoid extending cached-target
        // trust to delegated roles without checking each delegation's lifetime.
        if self
            .targets()?
            .delegations
            .as_ref()
            .is_some_and(|d| !d.roles.is_empty())
        {
            return Err(error("delegated mixer targets are not supported"));
        }
        Ok(())
    }
    pub async fn accept_cached(
        &self,
        offer: &MixerOffer,
        app_version: &str,
        protocol: u32,
        profile: &str,
        cache: &Path,
    ) -> Result<VerifiedMixerPayload> {
        if !self.offers()?.contains(offer) {
            return Err(error(
                "the selected mixer release changed; refresh and review it again",
            ));
        }
        offer
            .metadata
            .require_compatible(app_version, protocol, profile)?;
        std::fs::create_dir_all(cache)?;
        let path = cache.join(format!("{}.payload", offer.sha256));
        match std::fs::File::open(&path) {
            Ok(file) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(MAX_PAYLOAD + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 == offer.length && crate::files::sum(&bytes) == offer.sha256 {
                    return Self::verify_payload(
                        &bytes,
                        offer.metadata.clone(),
                        profile,
                        Some(app_version),
                    );
                }
                // A damaged cache is not another install source. Re-fetch only
                // the exact same signed offer and atomically replace the cache.
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let payload = self.accept(offer, app_version, protocol, profile).await?;
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(cache)?;
        file.write_all(&payload.bytes)?;
        file.as_file().sync_all()?;
        file.persist(&path).map_err(error)?;
        Ok(payload)
    }
    /// The signed payload of the release a mixer runs, cached in `cache`, for
    /// the in-place update to send only what changed since. Nothing from it
    /// is installed: the mixer rebuilds the new release from its own files
    /// and checks the result against the new release's signature. `None`
    /// when no signed release has that version (a development build).
    pub async fn base_payload(&self, version: &str, cache: &Path) -> Result<Option<PathBuf>> {
        self.require_current()?;
        let Some((name, _)) = self
            .releases()?
            .into_iter()
            .find(|(_, metadata)| metadata.version == version)
        else {
            return Ok(None);
        };
        let signed = self.target(&name)?;
        let sha256 =
            crate::tuf::sha256(signed).ok_or_else(|| error("signed release has no SHA-256"))?;
        let length = signed.length;
        std::fs::create_dir_all(cache)?;
        let path = cache.join(format!("{sha256}.payload"));
        if std::fs::read(&path)
            .is_ok_and(|bytes| bytes.len() as u64 == length && crate::files::sum(&bytes) == sha256)
        {
            return Ok(Some(path));
        }
        // read checks the signed hash before it hands over the bytes.
        let bytes = self.read(&name, signed).await?;
        if bytes.len() as u64 != length {
            return Err(error("payload length differs from signed metadata"));
        }
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new_in(cache)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(&path).map_err(error)?;
        Ok(Some(path))
    }

    pub fn offers(&self) -> Result<Vec<MixerOffer>> {
        self.require_current()?;
        let mut offers = self
            .releases()?
            .into_iter()
            .map(|(name, metadata)| {
                let target = self.target(&name)?;
                Ok(MixerOffer {
                    sha256: crate::tuf::sha256(target)
                        .ok_or_else(|| error("signed release has no SHA-256"))?,
                    length: target.length,
                    target: name,
                    metadata,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        offers.sort_by(|a, b| {
            rc2fx_core::compare_versions(&b.metadata.version, &a.metadata.version)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(offers)
    }

    pub async fn accept(
        &self,
        offer: &MixerOffer,
        app_version: &str,
        protocol: u32,
        profile: &str,
    ) -> Result<VerifiedMixerPayload> {
        if !self.offers()?.contains(offer) {
            return Err(error(
                "the selected mixer release changed; refresh and review it again",
            ));
        }
        self.download(&offer.target, app_version, protocol, profile)
            .await
    }

    pub async fn load(
        trusted_root: &[u8],
        metadata_url: Url,
        targets_url: Url,
        datastore: &Path,
    ) -> Result<Self> {
        check_repository_url(&metadata_url, METADATA_HOST)?;
        check_repository_url(&targets_url, TARGETS_HOST)?;
        let repository = crate::tuf::http(
            &metadata_url,
            &targets_url,
            std::time::Duration::from_secs(1800),
        )
        .map_err(error)?;
        Self::open(repository, trusted_root, datastore).await
    }

    /// Refresh `repository`'s metadata from `trusted_root`, through the
    /// verified state in `datastore`: the newest versions seen there are the
    /// floor, so older metadata is refused. Every role must be unexpired.
    async fn open(
        repository: impl sigstore_tuf::Repository + 'static,
        trusted_root: &[u8],
        datastore: &Path,
    ) -> Result<Self> {
        std::fs::create_dir_all(datastore)?;
        let mut updater = Updater::new(repository, trusted_root)
            .map_err(error)?
            .with_config(crate::tuf::config(MAX_PAYLOAD))
            .with_store(crate::tuf::MetadataOnly(sigstore_tuf::FileStore::new(
                datastore,
            )));
        updater
            .refresh(jiff::Timestamp::now())
            .await
            .map_err(error)?;
        crate::tuf::distinct_keys(updater.trusted().root()).map_err(error)?;
        Ok(Self { updater })
    }

    pub fn releases(&self) -> Result<Vec<(String, MixerTarget)>> {
        self.targets()?
            .targets
            .iter()
            .filter_map(|(name, target)| {
                let custom = target.custom.as_ref()?;
                custom.get("rc2fxMixer").map(|metadata| {
                    let metadata: MixerTarget =
                        serde_json::from_value(metadata.clone()).map_err(error)?;
                    metadata.validate()?;
                    if target.length == 0 || target.length > MAX_PAYLOAD {
                        return Err(error("payload size is outside the supported limit"));
                    }
                    Ok((name.clone(), metadata))
                })
            })
            .collect()
    }

    pub async fn download(
        &self,
        name: &str,
        app_version: &str,
        protocol_version: u32,
        firmware_profile: &str,
    ) -> Result<VerifiedMixerPayload> {
        let metadata = self
            .releases()?
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, m)| m)
            .ok_or_else(|| error("target is not a mixer release"))?;
        metadata.require_compatible(app_version, protocol_version, firmware_profile)?;
        let target = self.target(name)?;
        // No consumer sees the bytes until their signed hash has been checked.
        let bytes = self.read(name, target).await?;
        let expected_length = target.length;
        if bytes.len() as u64 != expected_length {
            return Err(error("payload length differs from signed metadata"));
        }
        Self::verify_payload(&bytes, metadata, firmware_profile, Some(app_version))
    }

    fn verify_payload(
        bytes: &[u8],
        metadata: MixerTarget,
        firmware_profile: &str,
        app_version: Option<&str>,
    ) -> Result<VerifiedMixerPayload> {
        let envelope = Envelope::standalone(bytes).map_err(error)?;
        let bundle = envelope.bundle().map_err(error)?;
        if crate::embed::format::build_kind(&bundle["rc2fx.sh"]).map_err(error)? != "release" {
            return Err(error("downloaded mixer payload is not a release build"));
        }
        let release =
            rc2fx_core::MixerRelease::parse(&bundle["mixer-release.json"]).map_err(error)?;
        if release.version != metadata.version
            || release.protocol_version != metadata.protocol_version
        {
            return Err(error("signed metadata disagrees with the payload release"));
        }
        let catalog = crate::embed::parse_catalog(&bundle["firmware-profiles.json"])?;
        if let Some(app) = app_version
            && let Some(profile) = catalog.profiles.iter().find(|p| p.id == firmware_profile)
            && !crate::firmware::app_supports(profile, app)
        {
            return Err(error(format!(
                "firmware {} requires app {} or newer; update the app first",
                profile.version, profile.minimum_app_version
            )));
        }
        let mut actual: Vec<_> = catalog
            .profiles
            .iter()
            .filter(|p| crate::firmware::supported(p))
            .map(|p| p.id.clone())
            .collect();
        let mut declared = metadata.firmware_profiles.clone();
        actual.sort();
        declared.sort();
        if actual != declared {
            return Err(error(
                "signed firmware compatibility differs from the payload catalog",
            ));
        }
        if !catalog
            .profiles
            .iter()
            .any(|p| p.id == firmware_profile && crate::firmware::supported(p))
        {
            return Err(error(
                "payload does not support the selected firmware profile",
            ));
        }
        Ok(VerifiedMixerPayload {
            bundle: Arc::new(bundle),
            bytes: Arc::new(bytes.to_vec()),
            metadata,
        })
    }
}
