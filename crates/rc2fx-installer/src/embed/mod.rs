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
    collections::HashMap,
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

fn identity_of(kind: &str, bundle: &Bundle) -> std::result::Result<Identity, String> {
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

/// Whether RC2FX's state (as the app's USB connection reads it) says this
/// payload's runtime is ready, the way the app asks about a folder's payload.
pub fn service(state: &[u8]) -> Service {
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

/// Where an installer's payload and catalog come from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Source {
    /// A release folder or a source checkout: `runtime/` (or `target/mixer`),
    /// `xtask/mixer` and `config/`. Developer tools and tests only.
    Folder(PathBuf),
    /// The payload compiled into this executable.
    #[cfg_attr(not(feature = "embedded"), allow(dead_code))]
    Embedded,
}

impl Source {
    pub(crate) fn folder(root: impl Into<PathBuf>) -> Self {
        Source::Folder(root.into())
    }

    pub(crate) fn bundle(&self) -> Result<Cow<'static, Bundle>> {
        match self {
            Source::Folder(root) => Ok(Cow::Owned(runtime::bundle_from_disk(root)?)),
            Source::Embedded => embedded_bundle(),
        }
    }

    pub(crate) fn catalog(&self) -> Result<Catalog> {
        match self {
            Source::Folder(root) => catalog_from_disk(root),
            Source::Embedded => {
                let bundle = self.bundle()?;
                let data = bundle
                    .get("firmware-profiles.json")
                    .ok_or("the runtime has no firmware catalog")?;
                let c: Catalog = serde_json::from_slice(data)?;
                if c.schema != 1 {
                    return Err(Error("unsupported firmware catalog".into()));
                }
                Ok(c)
            }
        }
    }

    /// `.rc2fx-mixer.conf` is developer tooling: an embedded build has no
    /// folder to read it from and uses its portable defaults.
    pub(crate) fn config(&self) -> HashMap<String, String> {
        match self {
            Source::Folder(root) => crate::mixer_config(root),
            Source::Embedded => HashMap::new(),
        }
    }

    pub(crate) fn ssh_key(&self) -> PathBuf {
        match self {
            Source::Folder(root) => crate::ssh_key(root),
            Source::Embedded => crate::default_key(),
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
    let data = std::fs::read(root.join("config").join("firmware-profiles.json"))?;
    let c: Catalog = serde_json::from_slice(&data)?;
    if c.schema != 1 {
        return Err(Error("unsupported firmware catalog".into()));
    }
    Ok(c)
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
