//! Renew timestamp/snapshot signatures without access to a targets signing key.
use crate::Result;
use std::{fs, path::Path};
use tough::{
    editor::signed::SignedRole,
    key_source::{KeySource, LocalKeySource},
    schema::{KeyHolder, RoleType, Root, Signed, Snapshot, Targets, Timestamp},
};

pub fn run(args: &[&str]) -> Result {
    let [
        root,
        input,
        output,
        sequence,
        expires,
        snapshot_key,
        timestamp_key,
    ] = args
    else {
        return Err("usage: cargo xtask mixer-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY".into());
    };
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?.block_on(async {
        let root: Signed<Root> = serde_json::from_slice(&fs::read(root).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        root.signed.verify_role(&root).map_err(|e| e.to_string())?;
        if root.signed.consistent_snapshot { return Err("expected fixed GitHub metadata names".into()); }
        let input = Path::new(input);
        let targets_bytes = fs::read(input.join("targets.json")).map_err(|e| e.to_string())?;
        let targets: Signed<Targets> = serde_json::from_slice(&targets_bytes).map_err(|e| e.to_string())?;
        root.signed.verify_role(&targets).map_err(|e| e.to_string())?;
        let snapshot_bytes = fs::read(input.join("snapshot.json")).map_err(|e| e.to_string())?;
        let mut snapshot: Signed<Snapshot> = serde_json::from_slice(&snapshot_bytes).map_err(|e| e.to_string())?;
        root.signed.verify_role(&snapshot).map_err(|e| e.to_string())?;
        let mut timestamp: Signed<Timestamp> = serde_json::from_slice(&fs::read(input.join("timestamp.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        root.signed.verify_role(&timestamp).map_err(|e| e.to_string())?;
        // Verify the old chain as well as individual signatures; do not splice
        // targets from one approved snapshot into another.
        check_pin(snapshot.signed.meta.get("targets.json"), &targets_bytes, targets.signed.version)?;
        check_pin(timestamp.signed.meta.get("snapshot.json"), &snapshot_bytes, snapshot.signed.version)?;
        if snapshot.signed.meta.len() != 1 || targets.signed.delegations.as_ref().is_some_and(|d| !d.roles.is_empty()) {
            return Err("delegated metadata is not supported for renewal".into());
        }
        let sequence: std::num::NonZeroU64 = sequence.parse().map_err(|_| "invalid sequence")?;
        let expires: jiff::Timestamp = expires.parse().map_err(|e| format!("invalid expiry: {e}"))?;
        if sequence <= snapshot.signed.version || sequence <= timestamp.signed.version {
            return Err("renewal sequence must increase".into());
        }
        if expires <= jiff::Timestamp::now() || expires >= targets.signed.expires || expires >= root.signed.expires {
            return Err("renewal must expire in the future and before the existing targets and root; reauthorize targets offline if needed".into());
        }
        let mut keys: Vec<Box<dyn KeySource>> = Vec::new();
        for (role, path) in [(RoleType::Snapshot, snapshot_key), (RoleType::Timestamp, timestamp_key)] {
            let source = LocalKeySource { path: Path::new(path).into() };
            let id = source.as_sign().await.map_err(|e| e.to_string())?.tuf_key().key_id().map_err(|e| e.to_string())?;
            for forbidden in [RoleType::Root, RoleType::Targets] {
                if root.signed.roles.get(&forbidden).is_some_and(|r| r.keyids.contains(&id)) {
                    return Err("renewal keys must not authorize root or targets".into());
                }
            }
            if !root.signed.roles.get(&role).is_some_and(|r| r.keyids.contains(&id)) {
                return Err(format!("key is not authorized for {role:?}"));
            }
            keys.push(Box::new(source));
        }
        snapshot.signed.version = sequence;
        snapshot.signed.expires = expires;
        let holder = KeyHolder::Root(root.signed);
        let snapshot = SignedRole::new(snapshot.signed, &holder, &keys, &aws_lc_rs::rand::SystemRandom::new()).await.map_err(|e| e.to_string())?;
        timestamp.signed.version = sequence;
        timestamp.signed.expires = expires;
        let pin = serde_json::json!({"version": sequence, "length": snapshot.buffer().len(), "hashes": {"sha256": crate::sha256(snapshot.buffer())}});
        timestamp.signed.meta = std::collections::HashMap::from([("snapshot.json".into(), serde_json::from_value(pin).map_err(|e| e.to_string())?)]);
        let timestamp = SignedRole::new(timestamp.signed, &holder, &keys, &aws_lc_rs::rand::SystemRandom::new()).await.map_err(|e| e.to_string())?;
        let output = Path::new(output);
        if output.exists() { return Err("renewal output already exists".into()); }
        let parent = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let stage = tempfile::tempdir_in(parent).map_err(|e| e.to_string())?;
        fs::write(stage.path().join("targets.json"), targets_bytes).map_err(|e| e.to_string())?;
        fs::write(stage.path().join("snapshot.json"), snapshot.buffer()).map_err(|e| e.to_string())?;
        fs::write(stage.path().join("timestamp.json"), timestamp.buffer()).map_err(|e| e.to_string())?;
        fs::rename(stage.path(), output).map_err(|e| e.to_string())?;
        Ok(())
    })
}

fn check_pin(
    pin: Option<&tough::schema::Metafile>,
    bytes: &[u8],
    version: std::num::NonZeroU64,
) -> Result {
    let pin = pin.ok_or("metadata chain is incomplete")?;
    use sha2::Digest;
    let hash = sha2::Sha256::digest(bytes);
    if pin.version != version
        || pin.length != Some(bytes.len() as u64)
        || pin
            .hashes
            .as_ref()
            .is_none_or(|h| h.sha256.as_ref() != hash.as_slice())
    {
        return Err("metadata chain does not match its signed hashes".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
    use std::{collections::HashMap, num::NonZeroU64};
    use tough::{editor::RepositoryEditor, schema::RoleKeys};

    #[test]
    fn renewal_preserves_targets_and_rejects_wrong_keys_stale_versions_and_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut public = HashMap::new();
            let mut roles = HashMap::new();
            let mut keys: Vec<Box<dyn KeySource>> = Vec::new();
            for role in [
                RoleType::Root,
                RoleType::Targets,
                RoleType::Snapshot,
                RoleType::Timestamp,
            ] {
                let path = dir.path().join(format!("{role}.pk8"));
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
                roles.insert(
                    role,
                    RoleKeys {
                        keyids: vec![id],
                        threshold: NonZeroU64::new(1).unwrap(),
                        _extra: HashMap::new(),
                    },
                );
                keys.push(Box::new(key));
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
                &keys,
                &SystemRandom::new(),
            )
            .await
            .unwrap();
            fs::write(dir.path().join("root.json"), signed.buffer()).unwrap();
            fs::write(dir.path().join("payload"), b"unchanged approved payload").unwrap();
            let mut editor = RepositoryEditor::new(dir.path().join("root.json"))
                .await
                .unwrap();
            let one = NonZeroU64::new(1).unwrap();
            editor
                .targets_version(one)
                .unwrap()
                .targets_expires("2098-01-01T00:00:00Z".parse().unwrap())
                .unwrap()
                .snapshot_version(one)
                .snapshot_expires("2000-01-01T00:00:00Z".parse().unwrap())
                .timestamp_version(one)
                .timestamp_expires("2000-01-01T00:00:00Z".parse().unwrap());
            editor
                .add_target_path(dir.path().join("payload"))
                .await
                .unwrap();
            editor
                .sign(&keys)
                .await
                .unwrap()
                .write(dir.path().join("old"))
                .await
                .unwrap();
        });
        let args = |out: &str, sequence: &str, key: &str, expires: &str| {
            vec![
                dir.path().join("root.json").display().to_string(),
                dir.path().join("old").display().to_string(),
                dir.path().join(out).display().to_string(),
                sequence.into(),
                expires.into(),
                dir.path().join(key).display().to_string(),
                dir.path().join("timestamp.pk8").display().to_string(),
            ]
        };
        let invoke = |args: Vec<String>| run(&args.iter().map(String::as_str).collect::<Vec<_>>());
        let expiry =
            (jiff::Timestamp::now() + std::time::Duration::from_secs(7 * 86400)).to_string();
        assert!(
            invoke(args("stale", "1", "snapshot.pk8", &expiry))
                .unwrap_err()
                .contains("increase")
        );
        assert!(
            invoke(args("wrong", "2", "targets.pk8", &expiry))
                .unwrap_err()
                .contains("must not authorize")
        );
        assert!(invoke(args("expired", "2", "snapshot.pk8", "2098-01-01T00:00:00Z")).is_err());
        invoke(args("new", "2", "snapshot.pk8", &expiry)).unwrap();
        assert_eq!(
            fs::read(dir.path().join("old/targets.json")).unwrap(),
            fs::read(dir.path().join("new/targets.json")).unwrap()
        );
        runtime.block_on(async {
            let root = fs::read(dir.path().join("root.json")).unwrap();
            let repository = tough::RepositoryLoader::new(
                &root,
                format!("file://{}/new/", dir.path().display())
                    .parse()
                    .unwrap(),
                format!("file://{}/", dir.path().display()).parse().unwrap(),
            )
            .load()
            .await
            .unwrap();
            assert_eq!(repository.timestamp().signed.version.get(), 2);
            assert_eq!(repository.targets().signed.version.get(), 1);
        });
        let target = dir.path().join("old/targets.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
        value["signed"]["version"] = 99.into();
        fs::write(target, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(invoke(args("tampered", "3", "snapshot.pk8", &expiry)).is_err());
    }
}
