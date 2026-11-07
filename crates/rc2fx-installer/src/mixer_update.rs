//! Authenticate complete mixer payloads before exposing their contents to the installer.
use crate::{Error, Result, embed::Envelope, runtime::Bundle};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tough::{IntoVec, Repository, RepositoryLoader, TargetName};
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
const METADATA_URL: &str = "https://github.com/ibanks42/rc2fx/releases/download/mixer-metadata/";
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

pub fn configured() -> bool {
    TRUSTED_ROOT.trim_ascii() != b"null"
}

pub(crate) async fn official(work: &Path) -> Result<MixerRepository> {
    if !configured() {
        return Err(error("this app has no configured mixer release trust root"));
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
    // The Linux helper must not let a desktop-controlled symlink redirect TUF's
    // writes as root. Its rollback state is independent and root-owned.
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
    repository: Repository,
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
            .repository
            .all_targets()
            .filter(|(_, t)| {
                t.custom
                    .get("rc2fxCatalog")
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
        let bytes = self
            .repository
            .read_target(name)
            .await
            .map_err(error)?
            .ok_or_else(|| error("firmware catalog disappeared"))?
            .into_vec()
            .await
            .map_err(error)?;
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
        let expires = [
            self.repository.root().signed.expires,
            self.repository.targets().signed.expires,
            self.repository.snapshot().signed.expires,
            self.repository.timestamp().signed.expires,
        ]
        .into_iter()
        .min()
        .unwrap();
        Ok(VerifiedCatalog {
            catalog,
            bytes,
            expires,
            root_version: self.repository.root().signed.version.get(),
            targets_version: self.repository.targets().signed.version.get(),
        })
    }
    fn require_current(&self) -> Result<()> {
        let now = jiff::Timestamp::now();
        if [
            self.repository.root().signed.expires,
            self.repository.timestamp().signed.expires,
            self.repository.snapshot().signed.expires,
            self.repository.targets().signed.expires,
        ]
        .into_iter()
        .any(|expires| expires <= now)
        {
            return Err(error(
                "signed mixer metadata expired; refresh before installation",
            ));
        }
        // RC2FX only publishes top-level targets. Avoid extending cached-target
        // trust to delegated roles without checking each delegation's lifetime.
        if self
            .repository
            .targets()
            .signed
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
    pub fn offers(&self) -> Result<Vec<MixerOffer>> {
        self.require_current()?;
        let mut offers = self
            .releases()?
            .into_iter()
            .map(|(name, metadata)| {
                let target_name = TargetName::new(&name).map_err(error)?;
                let target = self
                    .repository
                    .targets()
                    .signed
                    .find_target(&target_name, false)
                    .map_err(error)?;
                Ok(MixerOffer {
                    target: name,
                    sha256: target
                        .hashes
                        .sha256
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                    length: target.length,
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
        if metadata_url.scheme() != "https" || targets_url.scheme() != "https" {
            return Err(error("update repository URLs must use HTTPS"));
        }
        std::fs::create_dir_all(datastore)?;
        let repository = RepositoryLoader::new(&trusted_root, metadata_url, targets_url)
            .datastore(datastore)
            .transport(
                tough::HttpTransportBuilder::new()
                    .timeout(std::time::Duration::from_secs(1800))
                    .build(),
            )
            .expiration_enforcement(tough::ExpirationEnforcement::Safe)
            .load()
            .await
            .map_err(error)?;
        Ok(Self { repository })
    }

    pub fn releases(&self) -> Result<Vec<(String, MixerTarget)>> {
        self.repository
            .all_targets()
            .filter_map(|(name, target)| {
                target.custom.get("rc2fxMixer").map(|metadata| {
                    let metadata: MixerTarget =
                        serde_json::from_value(metadata.clone()).map_err(error)?;
                    metadata.validate()?;
                    if target.length == 0 || target.length > MAX_PAYLOAD {
                        return Err(error("payload size is outside the supported limit"));
                    }
                    Ok((name.raw().to_owned(), metadata))
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
        let target = TargetName::new(name).map_err(error)?;
        // No consumer sees stream bytes until EOF has verified the target hash.
        let bytes = self
            .repository
            .read_target(&target)
            .await
            .map_err(error)?
            .ok_or_else(|| error("target disappeared"))?
            .into_vec()
            .await
            .map_err(error)?;
        let expected_length = self
            .repository
            .targets()
            .signed
            .find_target(&target, false)
            .map_err(error)?
            .length;
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
