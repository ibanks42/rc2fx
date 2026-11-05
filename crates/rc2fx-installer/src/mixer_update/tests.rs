use super::*;
use crate::embed::format;
use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::{collections::HashMap, fs, num::NonZeroU64, path::PathBuf};
use tough::{
    editor::{RepositoryEditor, signed::SignedRole},
    key_source::{KeySource, LocalKeySource},
    schema::{KeyHolder, RoleKeys, RoleType, Root},
};

#[test]
fn configured_production_root_is_valid_and_separates_root_from_release_keys() {
    if !configured() {
        return;
    }
    let root: tough::schema::Signed<Root> = serde_json::from_slice(TRUSTED_ROOT).unwrap();
    root.signed.verify_role(&root).unwrap();
    assert!(root.signed.expires > jiff::Timestamp::now());
    assert!(!root.signed.consistent_snapshot);
    let root_keys = &root.signed.roles[&RoleType::Root].keyids;
    for role in [RoleType::Targets, RoleType::Snapshot, RoleType::Timestamp] {
        let keys = &root.signed.roles[&role];
        assert!(keys.keyids.len() >= keys.threshold.get() as usize);
        assert!(keys.keyids.iter().all(|id| !root_keys.contains(id)));
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    root: Vec<u8>,
    key: PathBuf,
    metadata: MixerTarget,
    payload: Vec<u8>,
}

fn payload() -> Vec<u8> {
    let mut elf = vec![0; 96];
    elf[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
    elf[18] = 0xb7;
    let mut files: format::Files = format::PAYLOAD_NAMES
        .iter()
        .map(|name| {
            let bytes = match *name {
                "firmware-profiles.json" => {
                    include_bytes!("../../../../config/firmware-profiles.json").to_vec()
                }
                "mixer-release.json" => {
                    br#"{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}"#.to_vec()
                }
                "SHA256SUMS" => Vec::new(),
                "rc2fx.sh" => b"#!/bin/sh\nrc2fx_build=release\n".to_vec(),
                n if format::SCRIPTS.contains(&n) => b"#!/bin/sh\n".to_vec(),
                _ => elf.clone(),
            };
            (name.to_string(), bytes)
        })
        .collect();
    let sums: String = format::PINNED
        .iter()
        .map(|name| {
            let bytes = &files.iter().find(|(n, _)| n == name).unwrap().1;
            format!("{}  {name}\n", format::sum(bytes))
        })
        .collect();
    files.iter_mut().find(|(n, _)| n == "SHA256SUMS").unwrap().1 = sums.into_bytes();
    format::wrap(&format::body(&format::view(&files)))
}

impl Fixture {
    async fn rotate_root(&mut self) {
        let old: tough::schema::Signed<Root> =
            serde_json::from_slice(&fs::read(self.dir.path().join("root.json")).unwrap()).unwrap();
        let key = self.dir.path().join("rotated-test-key.pk8");
        fs::write(
            &key,
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let new_source = LocalKeySource { path: key.clone() };
        let public = new_source.as_sign().await.unwrap().tuf_key();
        let id = public.key_id().unwrap();
        let mut root = old.signed.clone();
        root.version = NonZeroU64::new(root.version.get() + 1).unwrap();
        root.keys = HashMap::from([(id.clone(), public)]);
        for role in root.roles.values_mut() {
            role.keyids = vec![id.clone()];
        }
        let old_signature = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(old.signed),
            &[Box::new(LocalKeySource {
                path: self.key.clone(),
            })],
            &SystemRandom::new(),
        )
        .await
        .unwrap();
        let new_signature = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(root),
            &[Box::new(new_source)],
            &SystemRandom::new(),
        )
        .await
        .unwrap();
        let mut signed = new_signature.signed().clone();
        signed
            .signatures
            .extend(old_signature.signed().signatures.clone());
        fs::write(
            self.dir.path().join("root.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
        self.key = key;
    }

    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("test-only-key.pk8");
        fs::write(
            &key,
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        let source = LocalKeySource { path: key.clone() };
        let public = source.as_sign().await.unwrap().tuf_key();
        let id = public.key_id().unwrap();
        let roles = [
            RoleType::Root,
            RoleType::Targets,
            RoleType::Snapshot,
            RoleType::Timestamp,
        ]
        .into_iter()
        .map(|role| {
            (
                role,
                RoleKeys {
                    keyids: vec![id.clone()],
                    threshold: NonZeroU64::new(1).unwrap(),
                    _extra: HashMap::new(),
                },
            )
        })
        .collect();
        let root = Root {
            spec_version: "1.0.1".into(),
            consistent_snapshot: false,
            version: NonZeroU64::new(1).unwrap(),
            expires: "2099-01-01T00:00:00Z".parse().unwrap(),
            keys: HashMap::from([(id, public)]),
            roles,
            _extra: HashMap::new(),
        };
        let signed = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(root),
            &[Box::new(source)],
            &SystemRandom::new(),
        )
        .await
        .unwrap();
        let root = signed.buffer().clone();
        fs::write(dir.path().join("root.json"), &root).unwrap();
        let payload = payload();
        let bundle = Envelope::standalone(&payload).unwrap().bundle().unwrap();
        let catalog: crate::embed::Catalog =
            serde_json::from_slice(&bundle["firmware-profiles.json"]).unwrap();
        let metadata = MixerTarget {
            version: "1.2.3".into(),
            payload_format: 1,
            protocol_version: 1,
            minimum_app_version: "1.0.1".into(),
            firmware_profiles: catalog
                .profiles
                .iter()
                .filter(|p| crate::firmware::supported(p))
                .map(|p| p.id.clone())
                .collect(),
        };
        Self {
            dir,
            root,
            key,
            metadata,
            payload,
        }
    }

    async fn publish(&self, sequence: u64, expires: &str) {
        let path = self.dir.path().join("payload");
        fs::write(&path, &self.payload).unwrap();
        let (_, mut target) = RepositoryEditor::build_target(&path).await.unwrap();
        target.custom.insert(
            "rc2fxMixer".into(),
            serde_json::to_value(&self.metadata).unwrap(),
        );
        let mut editor = RepositoryEditor::new(self.dir.path().join("root.json"))
            .await
            .unwrap();
        let catalog_path = self.dir.path().join("firmware-profiles.json");
        let bundle = Envelope::standalone(&self.payload)
            .unwrap()
            .bundle()
            .unwrap();
        fs::write(&catalog_path, &bundle["firmware-profiles.json"]).unwrap();
        let (_, mut catalog_target) = RepositoryEditor::build_target(&catalog_path).await.unwrap();
        catalog_target
            .custom
            .insert("rc2fxCatalog".into(), self.metadata.version.clone().into());
        editor
            .add_target("firmware-profiles.json", catalog_target)
            .unwrap();
        let sequence = NonZeroU64::new(sequence).unwrap();
        let expires: jiff::Timestamp = expires.parse().unwrap();
        editor
            .targets_version(sequence)
            .unwrap()
            .targets_expires(expires)
            .unwrap()
            .snapshot_version(sequence)
            .snapshot_expires(expires)
            .timestamp_version(sequence)
            .timestamp_expires(expires)
            .add_target("payload", target)
            .unwrap();
        let repo = editor
            .sign(&[Box::new(LocalKeySource {
                path: self.key.clone(),
            })])
            .await
            .unwrap();
        repo.write(self.dir.path().join("metadata")).await.unwrap();
    }

    async fn load(&self) -> Result<MixerRepository> {
        let state = self.dir.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let repository = RepositoryLoader::new(
            &self.root,
            Url::from_directory_path(self.dir.path().join("metadata")).unwrap(),
            Url::from_directory_path(self.dir.path()).unwrap(),
        )
        .datastore(state)
        .load()
        .await
        .map_err(error)?;
        Ok(MixerRepository { repository })
    }
}

#[test]
fn signed_catalog_enforces_profile_app_minimum_and_cannot_be_replaced() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new().await;
            let mut bundle = Envelope::standalone(&fixture.payload)
                .unwrap()
                .bundle()
                .unwrap();
            let mut catalog: serde_json::Value =
                serde_json::from_slice(&bundle["firmware-profiles.json"]).unwrap();
            let id = catalog["profiles"][0]["id"].as_str().unwrap().to_owned();
            catalog["profiles"][0]["minimumAppVersion"] = "9.0.0".into();
            bundle.insert(
                "firmware-profiles.json".into(),
                serde_json::to_vec(&catalog).unwrap(),
            );
            let sums: String = format::PINNED
                .iter()
                .map(|name| format!("{}  {name}\n", format::sum(&bundle[*name])))
                .collect();
            bundle.insert("SHA256SUMS".into(), sums.into_bytes());
            let files: format::Files = format::PAYLOAD_NAMES
                .iter()
                .map(|name| (name.to_string(), bundle[*name].clone()))
                .collect();
            fixture.payload = format::wrap(&format::body(&format::view(&files)));
            fixture.publish(1, "2099-01-01T00:00:00Z").await;
            let repository = fixture.load().await.unwrap();
            let verified = repository.catalog().await.unwrap();
            let releases = crate::firmware::releases_from_catalog(
                crate::DeviceModel::ProIi,
                verified.catalog().unwrap(),
            )
            .unwrap();
            let release = releases.iter().find(|r| r.profile == id).unwrap();
            assert!(!release.supported);
            assert_eq!(release.minimum_app_version, "9.0.0");
            assert!(
                repository
                    .download("payload", "1.0.1", 1, &id)
                    .await
                    .unwrap_err()
                    .0
                    .contains("requires app 9.0.0")
            );
            assert!(
                repository
                    .download("payload", "9.0.0", 1, &id)
                    .await
                    .is_ok()
            );
            fs::write(
                fixture.dir.path().join("firmware-profiles.json"),
                b"replacement",
            )
            .unwrap();
            assert!(repository.catalog().await.is_err());
            let mut expired = verified;
            expired.expires = "2000-01-01T00:00:00Z".parse().unwrap();
            assert!(expired.catalog().is_err());
        });
}

#[test]
fn signed_repository_enforces_authenticity_compatibility_expiry_and_rollback() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new().await;
            fixture.publish(2, "2099-01-01T00:00:00Z").await;
            let repository = fixture.load().await.unwrap();
            let profile = &fixture.metadata.firmware_profiles[0];
            let verified = repository
                .download("payload", "1.0.1", 1, profile)
                .await
                .unwrap();
            assert_eq!(verified.metadata.version, "1.2.3");
            let offer = repository.offers().unwrap().remove(0);
            let cache = fixture.dir.path().join("cache");
            let cached = repository
                .accept_cached(&offer, "1.0.1", 1, profile, &cache)
                .await
                .unwrap();
            assert_eq!(cached, verified);
            let source = crate::embed::Source::Verified(cached.clone());
            let catalog = source.catalog().unwrap();
            let selected = catalog.profiles.iter().find(|p| p.id == *profile).unwrap();
            let bundle = source.bundle().unwrap();
            let (entries, digest) = crate::runtime::image_entries(&bundle, selected, None).unwrap();
            assert_eq!(digest, crate::runtime::digest(&verified.bundle));
            assert!(entries.iter().any(|entry| entry.path() == "usr/sbin/rc_audio_mixer"));
            for (name, bytes) in verified.bundle.iter() {
                let expected = format!("opt/rc2fx/{name}");
                assert!(entries.iter().any(|entry| matches!(entry,
                    crate::runtime::Entry::File { path, data, .. } if path == &expected && data == bytes)));
            }
            let mut replaced = offer.clone();
            replaced.sha256 = "0".repeat(64);
            assert!(
                repository
                    .accept_cached(&replaced, "1.0.1", 1, profile, &cache)
                    .await
                    .is_err()
            );
            fs::write(
                cache.join(format!("{}.payload", offer.sha256)),
                b"replacement",
            )
            .unwrap();
            assert!(
                repository
                    .accept_cached(&offer, "1.0.1", 1, profile, &cache)
                    .await
                    .is_ok()
            );
            // An already verified object owns its bytes; changing the cache
            // cannot change what subsequent firmware preparation consumes.
            assert_eq!(cached, verified);
            assert!(
                repository
                    .download("payload", "0.9.0", 1, profile)
                    .await
                    .is_err()
            );
            assert!(
                repository
                    .download("payload", "1.0.1", 2, profile)
                    .await
                    .is_err()
            );
            assert!(
                repository
                    .download("payload", "1.0.1", 1, "unknown")
                    .await
                    .is_err()
            );
            fs::write(fixture.dir.path().join("payload"), b"tampered").unwrap();
            assert!(
                repository
                    .download("payload", "1.0.1", 1, profile)
                    .await
                    .is_err()
            );
            // A previously accepted snapshot cannot be replaced by an older signed one.
            fixture.publish(1, "2099-01-01T00:00:00Z").await;
            assert!(fixture.load().await.is_err());
            fixture.publish(3, "2000-01-01T00:00:00Z").await;
            assert!(fixture.load().await.is_err());
            fixture.publish(4, "2099-01-01T00:00:00Z").await;
            let timestamp = fixture.dir.path().join("metadata/timestamp.json");
            let mut json: serde_json::Value =
                serde_json::from_slice(&fs::read(&timestamp).unwrap()).unwrap();
            json["signed"]["version"] = 999.into();
            fs::write(timestamp, serde_json::to_vec(&json).unwrap()).unwrap();
            assert!(fixture.load().await.is_err());
            // Valid signature, but inconsistent payload identity, must still fail.
            fixture.metadata.version = "9.9.9".into();
            fixture.publish(5, "2099-01-01T00:00:00Z").await;
            let repository = fixture.load().await.unwrap();
            assert!(
                repository
                    .download(
                        "payload",
                        "1.0.1",
                        1,
                        &fixture.metadata.firmware_profiles[0]
                    )
                    .await
                    .is_err()
            );
        });
}

#[test]
fn trusted_root_rotates_only_with_a_valid_old_and_new_signature_chain() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut fixture = Fixture::new().await;
            fixture.publish(1, "2099-01-01T00:00:00Z").await;
            fixture.load().await.unwrap();
            fixture.rotate_root().await;
            fixture.publish(2, "2099-01-01T00:00:00Z").await;
            let repository = fixture.load().await.unwrap();
            let offer = repository.offers().unwrap().remove(0);
            repository
                .accept(&offer, "1.0.1", 1, &offer.metadata.firmware_profiles[0])
                .await
                .unwrap();
            // A different self-signed root cannot replace the root shipped by the app.
            let impostor = Fixture::new().await;
            fs::write(
                fixture.dir.path().join("metadata/2.root.json"),
                &impostor.root,
            )
            .unwrap();
            assert!(fixture.load().await.is_err());
        });
}
