//! Offline qualification tooling. Never extracts files, prepares an image,
//! opens USB, or grants runtime support to an experimental board.
use crate::{Result, bail, files, firmware};
use rc2fx_firmware::{Catalog, Model};
use serde::Serialize;
use std::{collections::HashMap, io::Read, path::Path};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub model: Model,
    pub profile: String,
    pub archive_sha256: String,
    pub executable_verified: bool,
    pub runtime_qualified: bool,
    pub notes: String,
}

/// Verify an official archive against the built-in pins, model/board evidence,
/// and the already-qualified executable ABI. A successful result for Duo is
/// only offline evidence: `runtime_qualified` stays false.
pub fn inspect_archive(model: Model, archive: &Path) -> Result<Inspection> {
    let bytes = crate::embed::catalog_bytes()?;
    let catalog = Catalog::parse(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)?;
    inspect_archive_with_catalog(model, archive, &catalog)
}

/// Offline tooling can explicitly provide a catalog instead of downloading it.
pub fn inspect_archive_with_catalog(
    model: Model,
    archive: &Path,
    catalog: &Catalog,
) -> Result<Inspection> {
    if std::fs::metadata(archive)?.len() > crate::MAX_DOWNLOAD {
        bail!("firmware exceeds size limit");
    }
    let (sha, md5) = files::hash_both(archive, |_, _| {})?;
    let release = catalog
        .profiles
        .iter()
        .find(|p| p.model == model.id() && p.archive_sha256 == sha && p.archive_md5 == md5)
        .ok_or(
            "archive is not pinned for the requested model (wrong model or uninspected release)",
        )?;
    let members = read_members(archive, model)?;
    let text = |name: &str| -> Result<&str> {
        std::str::from_utf8(&members[name]).map_err(|e| crate::Error(e.to_string()))
    };
    if Model::from_board(text("etc/hostname")?, text("boot/extlinux/extlinux.conf")?)? != model {
        bail!("archive board identity differs from the requested model");
    }
    let profile = catalog.inspect_for_model(
        model,
        &members["usr/bin/rc_audio_mixer"],
        &members["usr/bin/audio-start.sh"],
    )?;
    if profile.id != release.id {
        bail!("archive and executable belong to different releases");
    }
    Ok(Inspection {
        model,
        profile: profile.id.clone(),
        archive_sha256: sha,
        executable_verified: true,
        runtime_qualified: model.runtime_enabled() && profile.runtime.is_some(),
        notes: profile.notes.clone(),
    })
}

/// Recheck board evidence in a prepared archive as well. Cache metadata alone
/// must never be enough to turn a Pro II image into a Duo image.
pub(crate) fn verify_archive_model(model: Model, archive: &Path) -> Result<()> {
    let members = read_members(archive, model)?;
    let hostname = std::str::from_utf8(&members["etc/hostname"]).map_err(|e| e.to_string())?;
    let extlinux =
        std::str::from_utf8(&members["boot/extlinux/extlinux.conf"]).map_err(|e| e.to_string())?;
    if Model::from_board(hostname, extlinux)? != model {
        bail!("prepared archive belongs to a different model");
    }
    Ok(())
}

fn read_members(archive: &Path, model: Model) -> Result<HashMap<String, Vec<u8>>> {
    let required = [
        "etc/hostname",
        "boot/extlinux/extlinux.conf",
        "usr/bin/rc_audio_mixer",
        "usr/bin/audio-start.sh",
        &model.board_dtb()[1..],
    ];
    let mut members = HashMap::new();
    firmware::walk_archive(archive, 2 << 30, false, |entry| {
        let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let name = firmware::normalized(&path);
        if required.contains(&name) {
            let limit = if name == "usr/bin/rc_audio_mixer" {
                firmware::MAX_MIXER_MEMBER
            } else {
                firmware::MAX_SMALL_MEMBER
            };
            if members.contains_key(name)
                || !entry.header().entry_type().is_file()
                || entry.size() > limit
            {
                bail!("duplicate, nonregular or oversized identity member {name}");
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            members.insert(name.to_owned(), bytes);
        }
        Ok(())
    })?;
    if required.iter().any(|name| !members.contains_key(*name)) {
        bail!("archive is missing required model identity or executable members");
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_archive_is_rejected_without_extracting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("unknown.tar.gz");
        std::fs::write(&file, b"not firmware").unwrap();
        let catalog =
            Catalog::parse(include_str!("../../../config/firmware-profiles.json")).unwrap();
        for model in Model::ALL {
            assert!(
                inspect_archive_with_catalog(model, &file, &catalog)
                    .unwrap_err()
                    .0
                    .contains("not pinned")
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    #[ignore = "set RC2FX_DUO_ARCHIVE to an inspected official Duo archive; no device needed"]
    fn pinned_duo_archive_passes_offline_only_and_fails_as_pro_ii() {
        let file = std::env::var("RC2FX_DUO_ARCHIVE").unwrap();
        let report = inspect_archive(Model::Duo, Path::new(&file)).unwrap();
        assert!(
            ["rodecaster-duo-1.7.6", "rodecaster-duo-1.7.7"].contains(&report.profile.as_str())
        );
        assert!(report.executable_verified);
        assert!(!report.runtime_qualified);
        assert!(inspect_archive(Model::ProIi, Path::new(&file)).is_err());
    }
}
