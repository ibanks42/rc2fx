//! Sign app release metadata: a TUF repository of its own, beside the
//! mixer's, with the same root and release keys. Every asset of one published
//! app release becomes a target `v{VERSION}/{ASSET}`, so the app can check a
//! downloaded update package before applying it.
use crate::{Result, sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tough::{
    editor::RepositoryEditor,
    key_source::{KeySource, LocalKeySource},
    schema::{RoleType, Root, Signed},
};

/// Covers the other assets; it is how they are checked, not something the
/// app downloads.
const SUMS: &str = "SHA256SUMS";

const SIGN_USAGE: &str = "usage: cargo xtask app-sign ROOT ASSETS VERSION OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY";
const REFRESH_USAGE: &str =
    "usage: cargo xtask app-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY";

/// Sign every asset in ASSETS, a downloaded copy of the published release
/// VERSION. Its SHA256SUMS must list exactly the other files, with their
/// hashes, so nothing is signed that the release did not publish.
pub fn sign(args: &[&str]) -> Result {
    let [
        root_file,
        assets,
        version,
        out,
        sequence,
        expires,
        targets_key,
        snapshot_key,
        timestamp_key,
    ] = args
    else {
        return Err(SIGN_USAGE.into());
    };
    if !stable(version) {
        return Err(format!("{version:?} is not a stable app version"));
    }
    let assets = checked_assets(Path::new(assets))?;
    let sequence: std::num::NonZeroU64 =
        sequence.parse().map_err(|_| "sequence must be positive")?;
    let expires: jiff::Timestamp = expires
        .parse()
        .map_err(|e| format!("invalid expiry: {e}"))?;
    if expires <= jiff::Timestamp::now() {
        return Err("metadata expiry must be in the future".into());
    }
    let out = Path::new(out);
    if out.exists() {
        return Err("metadata output already exists; use a new directory".into());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let root: Signed<Root> =
            serde_json::from_slice(&fs::read(root_file).map_err(|e| format!("{root_file}: {e}"))?)
                .map_err(|e| e.to_string())?;
        root.signed.verify_role(&root).map_err(|e| e.to_string())?;
        if root.signed.consistent_snapshot {
            return Err("GitHub release target paths require consistent_snapshot=false".into());
        }
        // As for mixer releases: targets stay authorized for as long as the
        // root trusts their key, and only discovery metadata needs renewal.
        let targets_expires = root.signed.expires;
        if expires >= targets_expires {
            return Err("discovery metadata must expire before the targets and root".into());
        }
        let keys = release_keys(
            &root.signed,
            [
                (RoleType::Targets, targets_key),
                (RoleType::Snapshot, snapshot_key),
                (RoleType::Timestamp, timestamp_key),
            ],
        )
        .await?;
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
        for (name, path) in &assets {
            let (_, mut target) = RepositoryEditor::build_target(path)
                .await
                .map_err(|e| e.to_string())?;
            target
                .custom
                .insert("version".into(), version.to_string().into());
            editor
                .add_target(format!("v{version}/{name}"), target)
                .map_err(|e| e.to_string())?;
        }
        let signed = editor.sign(&keys).await.map_err(|e| e.to_string())?;
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
            "Signed app {version} metadata ({} assets) sequence {sequence}; expires {expires}",
            assets.len()
        );
        Ok(())
    })
}

/// Renew app snapshot and timestamp metadata. The mixer's renewal checks
/// nothing specific to the mixer, so the same code serves both repositories.
pub fn refresh(args: &[&str]) -> Result {
    if args.len() != 7 {
        return Err(REFRESH_USAGE.into());
    }
    crate::mixer_refresh::run(args)
}

/// A stable `MAJOR.MINOR.PATCH`, as app tags are.
fn stable(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
}

/// A name that stays one path segment, both in a target name and on disk.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// The assets in `dir` by name, each one checked against SHA256SUMS, which
/// must list exactly them.
fn checked_assets(dir: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let sums = fs::read_to_string(dir.join(SUMS))
        .map_err(|e| format!("{}: {e}", dir.join(SUMS).display()))?;
    let mut listed = BTreeMap::new();
    for line in sums.lines() {
        let (hash, name) = line
            .split_once("  ")
            .ok_or_else(|| format!("malformed {SUMS} line {line:?}"))?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || !plain(name) {
            return Err(format!("malformed {SUMS} line {line:?}"));
        }
        if name == SUMS || listed.insert(name.to_owned(), hash.to_owned()).is_some() {
            return Err(format!("{SUMS} lists {name} more than once"));
        }
    }
    let mut assets = BTreeMap::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|n| format!("asset name {n:?} is not UTF-8"))?;
        if name == SUMS {
            continue;
        }
        // Not followed: a link could sign bytes from outside the release.
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err(format!("{name} is not a regular file"));
        }
        let expected = listed
            .remove(&name)
            .ok_or_else(|| format!("{name} is not listed in {SUMS}"))?;
        let bytes = fs::read(entry.path()).map_err(|e| e.to_string())?;
        if !sha256(&bytes).eq_ignore_ascii_case(&expected) {
            return Err(format!("{name} does not match {SUMS}"));
        }
        assets.insert(name, entry.path());
    }
    if let Some(name) = listed.keys().next() {
        return Err(format!("{SUMS} lists {name}, which is missing"));
    }
    if assets.is_empty() {
        return Err("the release has no assets to sign".into());
    }
    Ok(assets)
}

/// Load release-role keys, refusing a root key or a key the root does not
/// authorize for its role.
async fn release_keys(
    root: &Root,
    roles: [(RoleType, &str); 3],
) -> Result<Vec<Box<dyn KeySource>>> {
    let mut keys: Vec<Box<dyn KeySource>> = Vec::new();
    for (role, path) in roles {
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
            .roles
            .get(&RoleType::Root)
            .ok_or("root role missing")?
            .keyids
            .contains(&id)
        {
            return Err("root private keys must never be supplied to release signing".into());
        }
        if !root
            .roles
            .get(&role)
            .is_some_and(|r| r.keyids.contains(&id))
        {
            return Err(format!("key is not authorized for {role:?}"));
        }
        keys.push(Box::new(key));
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
    use std::{collections::HashMap, num::NonZeroU64};
    use tough::{
        editor::signed::SignedRole,
        schema::{KeyHolder, RoleKeys},
    };

    /// A root with one disposable key per role, as `ROLE.pk8` in `dir`.
    fn root(dir: &Path) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut public = HashMap::new();
                let mut roles = HashMap::new();
                let mut root_key: Vec<Box<dyn KeySource>> = Vec::new();
                for role in [
                    RoleType::Root,
                    RoleType::Targets,
                    RoleType::Snapshot,
                    RoleType::Timestamp,
                ] {
                    let path = dir.join(format!("{role}.pk8"));
                    fs::write(
                        &path,
                        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                            .unwrap()
                            .as_ref(),
                    )
                    .unwrap();
                    let key = LocalKeySource { path };
                    let value = key.as_sign().await.unwrap().tuf_key();
                    let id = value.key_id().unwrap();
                    public.insert(id.clone(), value);
                    let keys = RoleKeys {
                        keyids: vec![id],
                        threshold: NonZeroU64::new(1).unwrap(),
                        _extra: HashMap::new(),
                    };
                    roles.insert(role, keys);
                    if role == RoleType::Root {
                        root_key.push(Box::new(key));
                    }
                }
                let root = Root {
                    spec_version: "1.0.0".into(),
                    consistent_snapshot: false,
                    version: NonZeroU64::new(1).unwrap(),
                    expires: "2099-01-01T00:00:00Z".parse().unwrap(),
                    keys: public,
                    roles,
                    _extra: HashMap::new(),
                };
                let signed = SignedRole::new(
                    root.clone(),
                    &KeyHolder::Root(root),
                    &root_key,
                    &SystemRandom::new(),
                )
                .await
                .unwrap();
                fs::write(dir.join("root.json"), signed.buffer()).unwrap();
            });
    }

    /// A downloaded release: two assets and their SHA256SUMS.
    fn release(dir: &Path) -> PathBuf {
        let assets = dir.join("assets");
        fs::create_dir_all(&assets).unwrap();
        let mut sums = String::new();
        for (name, bytes) in [
            ("io.github.ibanks42.rc2fx-1.2.3-full.nupkg", &b"package"[..]),
            ("rc2fx-linux-x64.AppImage", b"appimage"),
        ] {
            fs::write(assets.join(name), bytes).unwrap();
            sums += &format!("{}  {name}\n", sha256(bytes));
        }
        fs::write(assets.join(SUMS), sums).unwrap();
        assets
    }

    fn sign_into(dir: &Path, out: &str, version: &str, targets_key: &str) -> Result {
        let expiry =
            (jiff::Timestamp::now() + std::time::Duration::from_secs(7 * 86400)).to_string();
        let path = |name: &str| dir.join(name).display().to_string();
        let args = [
            path("root.json"),
            path("assets"),
            version.into(),
            path(out),
            "2".into(),
            expiry,
            path(targets_key),
            path("snapshot.pk8"),
            path("timestamp.pk8"),
        ];
        sign(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn every_asset_but_the_sums_becomes_a_versioned_target() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        release(dir.path());
        sign_into(dir.path(), "signed", "1.2.3", "targets.pk8").unwrap();
        let repository = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(
                tough::RepositoryLoader::new(
                    &fs::read(dir.path().join("root.json")).unwrap(),
                    format!("file://{}/signed/", dir.path().display())
                        .parse()
                        .unwrap(),
                    format!("file://{}/", dir.path().display()).parse().unwrap(),
                )
                .load(),
            )
            .unwrap();
        let targets = &repository.targets().signed;
        let mut names: Vec<_> = targets.targets.keys().map(|n| n.raw().to_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "v1.2.3/io.github.ibanks42.rc2fx-1.2.3-full.nupkg",
                "v1.2.3/rc2fx-linux-x64.AppImage"
            ]
        );
        for target in targets.targets.values() {
            assert_eq!(target.custom["version"], "1.2.3");
        }
        let appimage =
            &targets.targets[&tough::TargetName::new("v1.2.3/rc2fx-linux-x64.AppImage").unwrap()];
        assert_eq!(appimage.length, 8);
        assert_eq!(
            sha256(b"appimage"),
            appimage
                .hashes
                .sha256
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        assert_eq!(
            repository.targets().signed.expires,
            repository.root().signed.expires
        );
        assert!(
            sign_into(dir.path(), "signed", "1.2.3", "targets.pk8")
                .unwrap_err()
                .contains("already exists")
        );
    }

    #[test]
    fn signing_refuses_assets_the_sums_do_not_vouch_for() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        let assets = release(dir.path());
        let refused = |out: &str| sign_into(dir.path(), out, "1.2.3", "targets.pk8").unwrap_err();
        fs::write(assets.join("rc2fx-linux-x64.AppImage"), b"replaced").unwrap();
        assert!(refused("a").contains("does not match"));
        fs::write(assets.join("rc2fx-linux-x64.AppImage"), b"appimage").unwrap();
        fs::write(assets.join("extra.zip"), b"extra").unwrap();
        assert!(refused("b").contains("not listed"));
        fs::remove_file(assets.join("extra.zip")).unwrap();
        fs::remove_file(assets.join("rc2fx-linux-x64.AppImage")).unwrap();
        assert!(refused("c").contains("missing"));
        fs::remove_file(assets.join(SUMS)).unwrap();
        assert!(refused("d").contains(SUMS));
        for out in ["a", "b", "c", "d"] {
            assert!(!dir.path().join(out).exists());
        }
    }

    #[test]
    fn signing_refuses_root_or_misassigned_keys_and_unstable_versions() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        release(dir.path());
        assert!(
            sign_into(dir.path(), "a", "1.2.3", "root.pk8")
                .unwrap_err()
                .contains("root private keys")
        );
        assert!(
            sign_into(dir.path(), "b", "1.2.3", "snapshot.pk8")
                .unwrap_err()
                .contains("not authorized")
        );
        for version in ["1.2", "1.2.3-rc1", "01.2.3", "v1.2.3", "1.2.3/x"] {
            assert!(
                sign_into(dir.path(), "c", version, "targets.pk8")
                    .unwrap_err()
                    .contains("not a stable app version")
            );
        }
    }

    #[test]
    fn renewal_keeps_the_signed_app_targets() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path());
        release(dir.path());
        sign_into(dir.path(), "signed", "1.2.3", "targets.pk8").unwrap();
        let expiry =
            (jiff::Timestamp::now() + std::time::Duration::from_secs(7 * 86400)).to_string();
        let path = |name: &str| dir.path().join(name).display().to_string();
        let args = [
            path("root.json"),
            path("signed"),
            path("renewed"),
            "3".into(),
            expiry,
            path("snapshot.pk8"),
            path("timestamp.pk8"),
        ];
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        assert!(refresh(&args[..6]).unwrap_err().contains("app-refresh"));
        refresh(&args).unwrap();
        assert_eq!(
            fs::read(dir.path().join("signed/targets.json")).unwrap(),
            fs::read(dir.path().join("renewed/targets.json")).unwrap()
        );
    }

    #[test]
    fn asset_names_stay_one_path_segment() {
        for name in [
            "rc2fx-linux-x64.AppImage",
            "releases.linux.json",
            "io.github.ibanks42.rc2fx-1.2.3-full.nupkg",
        ] {
            assert!(plain(name), "{name}");
        }
        for name in ["", ".hidden", "a/b", "a\\b", "..", "a b"] {
            assert!(!plain(name), "{name}");
        }
    }
}
