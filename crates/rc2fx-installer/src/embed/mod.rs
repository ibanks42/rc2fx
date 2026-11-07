//! The installer payload this binary was built with, and where a payload
//! comes from: the executable itself (the desktop app) or a folder (developer
//! tools and tests).
//!
//! The desktop app embeds the payload at build time — `build.rs` checks it with
//! exactly the rules a folder's payload passes — so installing needs no
//! runtime, catalog or script files beside it. See [`format`] for the payload's
//! layout and the envelope a binary carries.

pub(crate) mod catalog;
pub(crate) mod format;
pub use catalog::{Catalog, JobLifetime, Profile, RuntimeProfile};
pub use format::Envelope;

use crate::{
    Error, Result, Service, files,
    runtime::{self, Bundle},
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::OnceLock,
};

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// The payload this build embeds, as `build.rs` recorded it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Identity {
    /// `embedded` or `folder`.
    pub kind: String,
    /// What `rc2fx.sh` in this payload says: `release` or `development`.
    pub build: String,
    /// SHA-256 of the payload's `firmware-profiles.json`.
    pub catalog: String,
    /// The RC2FX mixer release version from the payload's `mixer-release.json`.
    pub release: String,
    /// The digest the prepared firmware records as its RC2FX runtime.
    pub runtime: String,
    pub files: Vec<FilePin>,
    pub profiles: Vec<ProfilePin>,
}

/// One payload file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FilePin {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

/// One firmware profile the payload's catalog pins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProfilePin {
    pub id: String,
    pub model: String,
    pub version: String,
    pub mixer_sha256: String,
    pub launcher_sha256: String,
    pub archive_sha256: String,
    pub supported: bool,
}

struct Payload {
    bundle: Bundle,
    identity: Identity,
}

static PAYLOAD: OnceLock<std::result::Result<Payload, String>> = OnceLock::new();

fn payload() -> Result<&'static Payload> {
    match PAYLOAD.get_or_init(load) {
        Ok(payload) => Ok(payload),
        Err(why) => Err(Error(why.clone())),
    }
}

fn load() -> std::result::Result<Payload, String> {
    if !PRESENT {
        return Err(MISSING.to_owned());
    }
    // The blob is checked when it is used, not only when it was built: the
    // envelope's framing, every file's checksum, SHA256SUMS, the ARM64 ELFs,
    // the catalog and the build line.
    let bundle = Envelope::find(BLOB)?.bundle()?;
    let identity = identity_of("embedded", &bundle)?;
    Ok(Payload { bundle, identity })
}

pub(crate) fn identity_of(kind: &str, bundle: &Bundle) -> std::result::Result<Identity, String> {
    let catalog = bundle
        .get("firmware-profiles.json")
        .ok_or("the payload has no firmware catalog")?;
    let catalog: Catalog = serde_json::from_slice(catalog)
        .map_err(|e| format!("the payload catalog is not JSON: {e}"))?;
    let script = bundle
        .get("rc2fx.sh")
        .ok_or("the payload has no rc2fx.sh")?;
    Ok(Identity {
        kind: kind.to_owned(),
        build: format::build_kind(script)?.to_owned(),
        catalog: files::sum(bundle.get("firmware-profiles.json").expect("checked above")),
        release: release_of(bundle).map_err(|e| e.0)?.version,
        runtime: runtime::digest(bundle),
        files: format::PAYLOAD_NAMES
            .iter()
            .map(|name| {
                let data = bundle.get(*name).map(Vec::as_slice).unwrap_or_default();
                FilePin {
                    name: (*name).to_owned(),
                    sha256: files::sum(data),
                    size: data.len() as u64,
                }
            })
            .collect(),
        profiles: catalog
            .profiles
            .iter()
            .map(|p| ProfilePin {
                id: p.id.clone(),
                model: p.model.clone(),
                version: p.version.clone(),
                mixer_sha256: p.mixer_sha.clone(),
                launcher_sha256: p.launcher_sha.clone(),
                archive_sha256: p.archive_sha.clone(),
                supported: crate::firmware::supported(p),
            })
            .collect(),
    })
}

/// The embedded payload's catalog (as a bundle the runtime checks).
pub(crate) fn embedded_bundle() -> Result<Cow<'static, Bundle>> {
    Ok(Cow::Borrowed(&payload()?.bundle))
}

pub(crate) fn catalog_bytes() -> Result<Vec<u8>> {
    if PRESENT {
        return Ok(payload()?.bundle["firmware-profiles.json"].clone());
    }
    crate::mixer_update::cached_catalog_bytes()
}

/// Whether RC2FX's state (as the app's USB connection reads it) says this
/// payload's runtime is ready, the way the app asks about a folder's payload.
pub fn service(state: &[u8]) -> Service {
    if !PRESENT {
        return match Source::Download.catalog().ok().and_then(|c|
            c.profiles.into_iter().find(crate::firmware::supported).and_then(|p| p.runtime)) {
            Some(profile) => match runtime::readiness_error(state, &profile) {
                why if why.is_empty() => Service::Ready,
                detail => Service::Starting { detail },
            },
            None => Service::Starting { detail: "No qualified runtime profile".into() },
        };
    }
    match PAYLOAD.get_or_init(load) {
        Ok(payload) => runtime::service(&payload.bundle, state),
        // Fail closed: a payload that cannot be read says why the runtime is
        // not ready. It never passes as ready by judging against an empty
        // profile.
        Err(why) => Service::Starting {
            detail: why.clone(),
        },
    }
}

/// The identity of the payload this binary serves.
pub fn identity() -> Result<Identity> {
    Ok(payload()?.identity.clone())
}

fn release_of(bundle: &Bundle) -> Result<rc2fx_core::MixerRelease> {
    let data = bundle
        .get(format::RELEASE_MANIFEST)
        .ok_or("the payload has no mixer-release.json")?;
    rc2fx_core::MixerRelease::parse(data).map_err(|e| Error(e.0))
}

/// The RC2FX mixer release this binary bundles. An error when the build
/// embeds no payload.
pub fn bundled_release() -> Result<rc2fx_core::MixerRelease> {
    release_of(&payload()?.bundle)
}

/// The newest RØDE firmware version the bundled catalog supports for the
/// same mixer model as `firmware_profile` (the `firmwareProfile` the device
/// reports), compared numerically. `None` when the build embeds no payload,
/// the profile is not in the bundled catalog, or nothing is supported for
/// that model.
pub fn newest_supported_firmware(firmware_profile: &str) -> Option<String> {
    let source = if PRESENT { Source::Embedded } else { Source::Download };
    let catalog = source.catalog().ok()?;
    newest_supported(&catalog, firmware_profile)
}

fn newest_supported(catalog: &Catalog, firmware_profile: &str) -> Option<String> {
    let model = &catalog
        .profiles
        .iter()
        .find(|p| p.id == firmware_profile)?
        .model;
    catalog
        .profiles
        .iter()
        .filter(|p| &p.model == model && crate::firmware::supported(p)
            && crate::firmware::app_supports(p, env!("CARGO_PKG_VERSION")))
        .map(|p| p.version.as_str())
        .filter(|v| rc2fx_core::compare_versions(v, v).is_some())
        .max_by(|a, b| rc2fx_core::compare_versions(a, b).expect("both versions are comparable"))
        .map(str::to_owned)
}

/// Where an installer's payload and catalog come from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Source {
    /// Download-only desktop before a signed payload has been selected.
    Download,
    /// A release folder or a source checkout: `runtime/` (or `target/mixer`),
    /// `xtask/mixer` and `config/`. Developer tools and tests only.
    Folder(PathBuf),
    /// The payload compiled into this executable.
    #[cfg_attr(not(feature = "embedded"), allow(dead_code))]
    Embedded,
    /// Authenticated remote payload, held in memory throughout preparation.
    Verified(crate::mixer_update::VerifiedMixerPayload),
}

impl Source {
    pub(crate) fn folder(root: impl Into<PathBuf>) -> Self {
        Source::Folder(root.into())
    }

    pub(crate) fn bundle(&self) -> Result<Cow<'static, Bundle>> {
        match self {
            Source::Download => Err(Error("Select and verify a mixer software download before preparing firmware".into())),
            Source::Folder(root) => Ok(Cow::Owned(runtime::bundle_from_disk(root)?)),
            Source::Embedded => embedded_bundle(),
            Source::Verified(payload) => Ok(Cow::Owned((*payload.bundle).clone())),
        }
    }

    pub(crate) fn catalog(&self) -> Result<Catalog> {
        match self {
            Source::Download => crate::mixer_update::cached_catalog(),
            Source::Folder(root) => catalog_from_disk(root),
            Source::Embedded | Source::Verified(_) => {
                let bundle = self.bundle()?;
                let data = bundle
                    .get("firmware-profiles.json")
                    .ok_or("the runtime has no firmware catalog")?;
                parse_catalog(data)
            }
        }
    }

    pub(crate) fn service(&self, state: &[u8]) -> Service {
        match self.bundle() {
            Ok(bundle) => runtime::service(&bundle, state),
            // Fail closed, as the embedded payload does.
            Err(e) => Service::Starting { detail: e.0 },
        }
    }
}

fn catalog_from_disk(root: &Path) -> Result<Catalog> {
    parse_catalog(&std::fs::read(
        root.join("config").join("firmware-profiles.json"),
    )?)
}

/// What a valid catalog is, for every source: the strict rules the mixer's own
/// firmware tool applies, and versions the installer can compare. Nothing may
/// reach the installer through a looser reading of the same file.
pub(crate) fn parse_catalog(data: &[u8]) -> Result<Catalog> {
    let text = std::str::from_utf8(data).map_err(|e| Error(e.to_string()))?;
    rc2fx_firmware::Catalog::parse(text).map_err(Error)?;
    let catalog: Catalog = serde_json::from_str(text)?;
    let comparable = |v: &str| rc2fx_core::compare_versions(v, v).is_some();
    if catalog
        .profiles
        .iter()
        .any(|p| !comparable(&p.version) || !comparable(&p.minimum_app_version))
    {
        return Err(Error("invalid firmware catalog profile version".into()));
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_that_cannot_be_read_says_why_and_is_never_ready() {
        // Whatever this build embeds, a source that cannot produce a catalog
        // must not claim readiness.
        let missing = Source::folder("/nonexistent/rc2fx-installer");
        let state = br#"{"config":{"revision":1},"engine":{"mode":"native","status":"running","appliedRevision":1,"sampleRate":48000,"blockSize":112,"audioAdvancing":true}}"#;
        match missing.service(state) {
            Service::Starting { detail } => assert!(!detail.is_empty(), "a reason is given"),
            Service::Ready => panic!("a missing payload claimed readiness"),
        }
    }

    #[test]
    fn the_newest_supported_firmware_is_per_model_and_numeric() {
        let mut catalog: Catalog =
            serde_json::from_str(include_str!("../../../../config/firmware-profiles.json"))
                .unwrap();
        let pro = rc2fx_firmware::Model::ProIi;
        let reported = catalog
            .profiles
            .iter()
            .find(|p| p.model == pro.id())
            .unwrap()
            .id
            .clone();
        let newest = newest_supported(&catalog, &reported).expect("a supported Pro II profile");
        for p in catalog.profiles.iter().filter(|p| p.model == pro.id()) {
            if crate::firmware::supported(p) {
                assert_ne!(
                    rc2fx_core::compare_versions(&p.version, &newest),
                    Some(std::cmp::Ordering::Greater)
                );
            }
        }
        // 1.7.10 is newer than 1.7.9, though it sorts lower as text; an
        // unsupported profile never counts, nor does another model's.
        let template = catalog
            .profiles
            .iter()
            .find(|p| p.model == pro.id() && crate::firmware::supported(p))
            .unwrap()
            .clone();
        assert_eq!(newest_supported(&catalog, "no-such-profile"), None);
        catalog.profiles = vec![
            Profile {
                version: "1.7.9".into(),
                ..template.clone()
            },
            Profile {
                version: "1.7.10".into(),
                ..template.clone()
            },
            Profile {
                version: "9.0.0".into(),
                runtime: None,
                ..template.clone()
            },
            Profile {
                version: "1.8-beta".into(),
                ..template.clone()
            },
            Profile {
                id: "duo-newer".into(),
                version: "8.0.0".into(),
                model: rc2fx_firmware::Model::Duo.id().into(),
                ..template.clone()
            },
        ];
        // Every Pro II entry above shares the template's id.
        assert_eq!(
            newest_supported(&catalog, &template.id).as_deref(),
            Some("1.7.10")
        );
        // The Duo entry carries a Pro II runtime, so it is not supported and
        // its higher version never counts for either model.
        assert_eq!(newest_supported(&catalog, "duo-newer"), None);
    }

    #[test]
    fn the_bundled_release_is_the_source_manifest() {
        if !PRESENT {
            assert!(bundled_release().is_err());
            assert_eq!(newest_supported_firmware("rodecaster-pro-ii-1.7.7"), None);
            return;
        }
        let source = rc2fx_core::MixerRelease::parse(include_bytes!(
            "../../../../config/mixer-release.json"
        ))
        .unwrap();
        assert_eq!(bundled_release().unwrap(), source);
        assert_eq!(identity().unwrap().release, source.version);
    }

    #[test]
    fn the_embedded_identity_matches_the_payload_it_carries() {
        if !PRESENT {
            eprintln!("this build embeds no payload; identity is reported as unavailable");
            assert!(identity().is_err());
            return;
        }
        let identity = identity().expect("the embedded payload is usable");
        assert_eq!(identity.kind, "embedded");
        assert!(
            identity.build == format::RELEASE || identity.build == format::DEVELOPMENT,
            "build kind {:?}",
            identity.build
        );
        assert_eq!(identity.files.len(), format::PAYLOAD_NAMES.len());
        for (pin, name) in identity.files.iter().zip(format::PAYLOAD_NAMES) {
            assert_eq!(pin.name, name);
            assert_eq!(pin.sha256.len(), 64);
            assert!(pin.size > 0);
        }
        assert!(identity.profiles.iter().any(|p| p.supported));
        assert!(!identity.catalog.is_empty());
        assert!(!identity.runtime.is_empty());
    }

    #[test]
    fn the_embedded_payload_builds_the_image_its_identity_names() {
        if !PRESENT {
            return;
        }
        let identity = identity().unwrap();
        let bundle = embedded_bundle().unwrap();
        let catalog = Source::Embedded.catalog().unwrap();
        let profile = catalog
            .profiles
            .iter()
            .find(|p| crate::firmware::supported(p))
            .expect("a supported profile");
        // What `prepare` embeds, from the same bytes it verified.
        let (entries, runtime) = crate::runtime::image_entries(&bundle, profile, None)
            .expect("the payload builds an image");
        assert_eq!(runtime, identity.runtime, "the prepared runtime digest");
        let paths: Vec<&str> = entries.iter().map(|e| e.path()).collect();
        for name in format::PAYLOAD_NAMES {
            assert!(
                paths.contains(&format!("opt/rc2fx/{name}").as_str()),
                "{name} is not in the image"
            );
        }
        for shim in ["usr/sbin/rc_audio_mixer", "usr/sbin/rc_gui_juce"] {
            assert!(paths.contains(&shim), "{shim} is not in the image");
        }
        // And the same digest is what a prepared record has to carry.
        assert_eq!(
            crate::runtime::digest(&bundle),
            identity.runtime,
            "one runtime identity everywhere"
        );
    }
}
