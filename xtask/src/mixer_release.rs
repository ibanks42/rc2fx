//! Package the exact standalone bytes that the signed update repository serves.
use crate::{Result, package, root, sha256};
use rc2fx_installer::embed::Envelope;
use std::{fs, path::Path};

#[cfg(test)]
#[path = "mixer_release_tests.rs"]
mod tests;

pub fn package(destination: &Path) -> Result {
    // Create a new directory: never replace an already packaged release.
    let envelope = Envelope::expected(&root(), true)?;
    let checked = Envelope::standalone(envelope.as_bytes())?;
    let bundle = checked.bundle()?;
    let release: serde_json::Value =
        serde_json::from_slice(&bundle["mixer-release.json"]).map_err(|e| e.to_string())?;
    let version = release["version"].as_str().ok_or("missing mixer version")?;
    let name = format!("rc2fx-mixer-{version}.payload");
    let parent = destination.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if destination.exists() {
        return Err(format!(
            "{} already exists; use a fresh output directory",
            destination.display()
        ));
    }
    let staging = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
    fs::write(staging.path().join(&name), checked.as_bytes()).map_err(|e| e.to_string())?;
    fs::write(
        staging.path().join("firmware-profiles.json"),
        &bundle["firmware-profiles.json"],
    )
    .map_err(|e| e.to_string())?;
    let compatibility =
        fs::read(root().join("config/mixer-compatibility.json")).map_err(|e| e.to_string())?;
    fs::write(
        staging.path().join("mixer-compatibility.json"),
        compatibility,
    )
    .map_err(|e| e.to_string())?;
    package::write_licenses(&staging.path().join("THIRD-PARTY-NOTICES.html"))?;
    let notices =
        fs::read(staging.path().join("THIRD-PARTY-NOTICES.html")).map_err(|e| e.to_string())?;
    fs::write(
        staging.path().join("SHA256SUMS"),
        format!(
            "{}  {name}\n{}  THIRD-PARTY-NOTICES.html\n{}  firmware-profiles.json\n",
            sha256(checked.as_bytes()),
            sha256(&notices),
            sha256(&bundle["firmware-profiles.json"])
        ),
    )
    .map_err(|e| e.to_string())?;
    fs::rename(staging.path(), destination).map_err(|e| e.to_string())?;
    println!(
        "Packaged mixer {version}: {} (unsigned; signing is required before distribution)",
        destination.display()
    );
    Ok(())
}

/// Sign only a fully packaged, checked release. Key creation and root changes
/// are deliberately separate from this operation and from the build worker.
pub fn sign(args: &[&str]) -> Result {
    let [
        root_file,
        package,
        out,
        sequence,
        expires,
        targets_key,
        snapshot_key,
        timestamp_key,
    ] = args
    else {
        return Err("usage: cargo xtask mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY".into());
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        use std::num::NonZeroU64;
        use tough::{
            editor::RepositoryEditor,
            key_source::{KeySource, LocalKeySource},
            schema::{RoleType, Root, Signed},
        };
        let sequence: NonZeroU64 = sequence.parse().map_err(|_| "sequence must be positive")?;
        let expires: jiff::Timestamp = expires
            .parse()
            .map_err(|e| format!("invalid expiry: {e}"))?;
        if expires <= jiff::Timestamp::now() {
            return Err("metadata expiry must be in the future".into());
        }
        let root_bytes = fs::read(root_file).map_err(|e| e.to_string())?;
        let root: Signed<Root> = serde_json::from_slice(&root_bytes).map_err(|e| e.to_string())?;
        root.signed.verify_role(&root).map_err(|e| e.to_string())?;
        // A release stays authorized for as long as the root trusts its key, so
        // nothing needs signing with the targets key between releases.
        let targets_expires = root.signed.expires;
        if expires >= targets_expires {
            return Err("discovery metadata must expire before the targets and root".into());
        }
        if root.signed.consistent_snapshot {
            return Err("GitHub release target paths require consistent_snapshot=false".into());
        }
        let mut keys: Vec<Box<dyn KeySource>> = Vec::new();
        for (role, path) in [
            (RoleType::Targets, targets_key),
            (RoleType::Snapshot, snapshot_key),
            (RoleType::Timestamp, timestamp_key),
        ] {
            let key = LocalKeySource {
                path: Path::new(path).to_owned(),
            };
            let id = key
                .as_sign()
                .await
                .map_err(|e| e.to_string())?
                .tuf_key()
                .key_id()
                .map_err(|e| e.to_string())?;
            if root
                .signed
                .roles
                .get(&RoleType::Root)
                .ok_or("root role missing")?
                .keyids
                .contains(&id)
            {
                return Err("root private keys must never be supplied to release signing".into());
            }
            if !root
                .signed
                .roles
                .get(&role)
                .is_some_and(|r| r.keyids.contains(&id))
            {
                return Err(format!("key is not authorized for {role:?}"));
            }
            keys.push(Box::new(key));
        }
        let package = Path::new(package);
        let payloads: Vec<_> = fs::read_dir(package)
            .map_err(|e| e.to_string())?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "payload"))
            .collect();
        if payloads.len() != 1 {
            return Err("package must contain exactly one mixer payload".into());
        }
        let bytes = fs::read(&payloads[0]).map_err(|e| e.to_string())?;
        let bundle = Envelope::standalone(&bytes)?.bundle()?;
        let release = rc2fx_installer::MixerRelease::parse(&bundle["mixer-release.json"])
            .map_err(|e| e.to_string())?;
        let compatibility: serde_json::Value = serde_json::from_slice(
            &fs::read(package.join("mixer-compatibility.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let minimum_app = compatibility["minimumAppVersion"]
            .as_str()
            .ok_or("minimumAppVersion is required in mixer-compatibility.json")?;
        let metadata = rc2fx_installer::mixer_update::MixerTarget::for_payload(
            &Envelope::standalone(&bytes)?,
            minimum_app,
        )
        .map_err(|e| e.to_string())?;
        let mut editor = RepositoryEditor::new(root_file)
            .await
            .map_err(|e| e.to_string())?;
        editor
            .targets_version(sequence)
            .map_err(|e| e.to_string())?
            .targets_expires(targets_expires)
            .map_err(|e| e.to_string())?
            .snapshot_version(sequence)
            .snapshot_expires(expires)
            .timestamp_version(sequence)
            .timestamp_expires(expires);
        if fs::read(package.join("firmware-profiles.json")).map_err(|e| e.to_string())?
            != bundle["firmware-profiles.json"]
        {
            return Err("standalone catalog differs from the mixer payload".into());
        }
        for (path, mixer) in [
            (&payloads[0], true),
            (&package.join("THIRD-PARTY-NOTICES.html"), false),
            (&package.join("firmware-profiles.json"), false),
        ] {
            let (name, mut target) = RepositoryEditor::build_target(path)
                .await
                .map_err(|e| e.to_string())?;
            if mixer {
                target.custom.insert(
                    "rc2fxMixer".into(),
                    serde_json::to_value(&metadata).map_err(|e| e.to_string())?,
                );
            }
            if path
                .file_name()
                .is_some_and(|n| n == "firmware-profiles.json")
            {
                target
                    .custom
                    .insert("rc2fxCatalog".into(), release.version.clone().into());
            }
            let name = format!("mixer-v{}/{}", release.version, name.raw());
            editor.add_target(name, target).map_err(|e| e.to_string())?;
        }
        let signed = editor.sign(&keys).await.map_err(|e| e.to_string())?;
        let out = Path::new(out);
        if out.exists() {
            return Err("metadata output already exists; use a new directory".into());
        }
        let parent = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let stage = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
        signed
            .write(stage.path())
            .await
            .map_err(|e| e.to_string())?;
        fs::rename(stage.path(), out).map_err(|e| e.to_string())?;
        println!(
            "Signed mixer {} metadata sequence {sequence}; expires {expires}",
            release.version
        );
        Ok(())
    })
}
