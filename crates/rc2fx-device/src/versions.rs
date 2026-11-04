//! What `GET api/v1/version` reports: the installed RC2FX mixer release and
//! the RØDE firmware the launcher validated. Read once when the controller
//! starts; neither can change without a restart.

use rc2fx_core::{DeviceVersions, MixerRelease};
use std::{io::Read, path::Path};

/// The largest file read for either input; both are a few KiB at most.
const MAX_BYTES: u64 = 256 * 1024;

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut data))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() as u64 > MAX_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    Ok(data)
}

/// The versions this controller reports. `release` is the installed
/// `mixer-release.json`; `profile` is the id the launcher selected after
/// `rc2fx-firmware check-device` verified the stock mixer against `catalog`.
/// A profile for another model is not reported. Anything that cannot be read
/// or checked stays unknown, and the reason is returned for the log.
pub fn read(
    release: Option<&Path>,
    catalog: Option<&Path>,
    profile: Option<&str>,
    model: rc2fx_firmware::Model,
) -> (DeviceVersions, Vec<String>) {
    let mut versions = DeviceVersions::default();
    let mut problems = Vec::new();
    if let Some(path) = release {
        match read_limited(path).and_then(|data| MixerRelease::parse(&data).map_err(|e| e.0)) {
            Ok(release) => {
                versions.runtime = Some(release.version);
                versions.protocol = Some(release.protocol_version);
            }
            Err(why) => problems.push(format!("mixer release unknown: {why}")),
        }
    }
    match (catalog, profile) {
        (Some(path), Some(id)) => {
            let found = read_limited(path)
                .and_then(|data| String::from_utf8(data).map_err(|e| e.to_string()))
                .and_then(|text| rc2fx_firmware::Catalog::parse(&text))
                .and_then(|catalog| {
                    catalog
                        .profiles
                        .into_iter()
                        .find(|p| p.id == id)
                        .ok_or_else(|| format!("profile {id:?} is not in the catalog"))
                })
                .and_then(|p| {
                    if p.model == model.id() {
                        Ok(p)
                    } else {
                        Err(format!(
                            "profile {id:?} is for {}, not {}",
                            p.model,
                            model.id()
                        ))
                    }
                });
            match found {
                Ok(p) => {
                    versions.firmware = Some(p.version);
                    versions.firmware_profile = Some(p.id);
                }
                Err(why) => problems.push(format!("firmware version unknown: {why}")),
            }
        }
        (None, None) => {}
        _ => problems.push("firmware version unknown: catalog and profile go together".into()),
    }
    (versions, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_firmware::Model;

    const CATALOG: &str = include_str!("../../../config/firmware-profiles.json");

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir(name: &str) -> Dir {
        let path =
            std::env::temp_dir().join(format!("rc2fx-versions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }
    fn first_pro_ii() -> rc2fx_firmware::Profile {
        rc2fx_firmware::Catalog::parse(CATALOG)
            .unwrap()
            .profiles
            .into_iter()
            .find(|p| p.model == Model::ProIi.id())
            .unwrap()
    }

    #[test]
    fn reports_the_installed_release_and_the_validated_profile() {
        let d = dir("ok");
        let release = d.0.join("mixer-release.json");
        let catalog = d.0.join("firmware-profiles.json");
        std::fs::write(
            &release,
            br#"{"schemaVersion":1,"version":"0.3.1","protocolVersion":1}"#,
        )
        .unwrap();
        std::fs::write(&catalog, CATALOG).unwrap();
        let profile = first_pro_ii();
        let (v, problems) = read(
            Some(&release),
            Some(&catalog),
            Some(&profile.id),
            Model::ProIi,
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(v.runtime.as_deref(), Some("0.3.1"));
        assert_eq!(v.protocol, Some(1));
        assert_eq!(v.firmware.as_deref(), Some(profile.version.as_str()));
        assert_eq!(v.firmware_profile.as_deref(), Some(profile.id.as_str()));
    }

    #[test]
    fn an_older_install_without_a_manifest_stays_unknown() {
        let d = dir("old");
        let (v, problems) = read(
            Some(&d.0.join("mixer-release.json")),
            None,
            None,
            Model::ProIi,
        );
        assert_eq!(v, DeviceVersions::default());
        assert_eq!(problems.len(), 1);
        let (v, problems) = read(None, None, None, Model::ProIi);
        assert_eq!(v, DeviceVersions::default());
        assert!(problems.is_empty());
    }

    #[test]
    fn an_invalid_manifest_or_unknown_profile_is_never_guessed() {
        let d = dir("bad");
        let release = d.0.join("mixer-release.json");
        let catalog = d.0.join("firmware-profiles.json");
        std::fs::write(
            &release,
            br#"{"schemaVersion":2,"version":"9.9.9","protocolVersion":1}"#,
        )
        .unwrap();
        std::fs::write(&catalog, CATALOG).unwrap();
        let (v, problems) = read(
            Some(&release),
            Some(&catalog),
            Some("no-such"),
            Model::ProIi,
        );
        assert_eq!(v, DeviceVersions::default());
        assert_eq!(problems.len(), 2, "{problems:?}");
        // A Pro II profile on a Duo is not reported.
        let profile = first_pro_ii();
        let (v, _) = read(None, Some(&catalog), Some(&profile.id), Model::Duo);
        assert_eq!(v.firmware, None);
    }
}
