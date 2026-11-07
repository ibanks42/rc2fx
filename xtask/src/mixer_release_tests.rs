//! Publisher-to-client contract using disposable keys and a synthetic payload.
use super::*;
use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::{collections::HashMap, num::NonZeroU64};
use tough::{
    IntoVec,
    editor::signed::SignedRole,
    key_source::{KeySource, LocalKeySource},
    schema::{KeyHolder, RoleKeys, RoleType, Root},
};

fn fixture_package(base: &Path) -> Vec<u8> {
    let runtime = base.join("runtime");
    let scripts = base.join("xtask/mixer");
    let config = base.join("config");
    for path in [&runtime, &scripts, &config] {
        fs::create_dir_all(path).unwrap();
    }
    let mut elf = vec![0; 96];
    elf[..6].copy_from_slice(&[127, b'E', b'L', b'F', 2, 1]);
    elf[18] = 0xb7;
    for name in [
        "librc2fx_native.so",
        "rc2fxd",
        "rc2fx-supervisor",
        "rc2fx-firmware",
        "librc2fx_gui.so",
        "librc2fx_jackerr.so",
        "rc2fx_jackerr_snapshot",
    ] {
        fs::write(runtime.join(name), &elf).unwrap();
    }
    for (name, bytes) in [
        (
            "firmware-profiles.json",
            include_bytes!("../../config/firmware-profiles.json").as_slice(),
        ),
        (
            "mixer-release.json",
            br#"{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}"#.as_slice(),
        ),
    ] {
        fs::write(runtime.join(name), bytes).unwrap();
        fs::write(config.join(name), bytes).unwrap();
    }
    let checksums: String = [
        "librc2fx_native.so",
        "rc2fxd",
        "rc2fx-supervisor",
        "rc2fx-firmware",
        "firmware-profiles.json",
        "librc2fx_gui.so",
        "librc2fx_jackerr.so",
        "rc2fx_jackerr_snapshot",
        "mixer-release.json",
    ]
    .iter()
    .map(|name| {
        format!(
            "{}  {name}\n",
            sha256(&fs::read(runtime.join(name)).unwrap())
        )
    })
    .collect();
    fs::write(runtime.join("SHA256SUMS"), checksums).unwrap();
    for name in [
        "rc2fx.sh",
        "rc2fx-shim.sh",
        "rc2fx-gui-shim.sh",
        "install.sh",
    ] {
        fs::write(scripts.join(name), b"#!/bin/sh\nrc2fx_build=development\n").unwrap();
    }
    let envelope = Envelope::expected(base, true).unwrap();
    let package = base.join("targets/mixer-v1.2.3");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("rc2fx-mixer-1.2.3.payload"),
        envelope.as_bytes(),
    )
    .unwrap();
    fs::write(
        package.join("firmware-profiles.json"),
        include_bytes!("../../config/firmware-profiles.json"),
    )
    .unwrap();
    fs::write(
        package.join("mixer-compatibility.json"),
        br#"{"minimumAppVersion":"1.0.1"}"#,
    )
    .unwrap();
    fs::write(package.join("THIRD-PARTY-NOTICES.html"), b"fixture notices").unwrap();
    envelope.as_bytes().to_vec()
}

#[test]
fn signing_output_verifies_and_preserves_the_installer_payload_contract() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let payload = fixture_package(base);
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
            let path = base.join(format!("{role}.pk8"));
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
        fs::write(base.join("root.json"), signed.buffer()).unwrap();
    });
    let expiry = (jiff::Timestamp::now() + std::time::Duration::from_secs(7 * 86400)).to_string();
    let arguments = |out: &str, targets: &str| {
        vec![
            base.join("root.json").display().to_string(),
            base.join("targets/mixer-v1.2.3").display().to_string(),
            base.join(out).display().to_string(),
            "1".into(),
            expiry.clone(),
            base.join(targets).display().to_string(),
            base.join("snapshot.pk8").display().to_string(),
            base.join("timestamp.pk8").display().to_string(),
        ]
    };
    let invoke = |args: Vec<String>| sign(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(
        invoke(arguments("rejected", "root.pk8"))
            .unwrap_err()
            .contains("root private keys")
    );
    assert!(!base.join("rejected").exists());
    let mut late = arguments("rejected", "targets.pk8");
    late[4] = "2099-01-01T00:00:00Z".into();
    assert!(invoke(late).unwrap_err().contains("expire before"));
    assert!(!base.join("rejected").exists());
    invoke(arguments("metadata", "targets.pk8")).unwrap();
    runtime.block_on(async {
        let root = fs::read(base.join("root.json")).unwrap();
        let repository = tough::RepositoryLoader::new(
            &root,
            format!("file://{}/metadata/", base.display())
                .parse()
                .unwrap(),
            format!("file://{}/targets/", base.display())
                .parse()
                .unwrap(),
        )
        .load()
        .await
        .unwrap();
        assert_eq!(
            repository.targets().signed.expires,
            repository.root().signed.expires,
            "targets last as long as the root"
        );
        assert!(repository.timestamp().signed.expires < repository.targets().signed.expires);
        assert!(repository.snapshot().signed.expires < repository.targets().signed.expires);
        let name = tough::TargetName::new("mixer-v1.2.3/rc2fx-mixer-1.2.3.payload").unwrap();
        let verified = repository
            .read_target(&name)
            .await
            .unwrap()
            .unwrap()
            .into_vec()
            .await
            .unwrap();
        assert_eq!(verified, payload);
        let envelope = Envelope::standalone(&verified).unwrap();
        let metadata =
            rc2fx_installer::mixer_update::MixerTarget::for_payload(&envelope, "1.0.1").unwrap();
        let signed = &repository.targets().signed.targets[&name].custom["rc2fxMixer"];
        assert_eq!(*signed, serde_json::to_value(metadata).unwrap());
        let catalog_name = tough::TargetName::new("mixer-v1.2.3/firmware-profiles.json").unwrap();
        let catalog = repository
            .read_target(&catalog_name)
            .await
            .unwrap()
            .unwrap()
            .into_vec()
            .await
            .unwrap();
        assert_eq!(
            catalog,
            envelope.bundle().unwrap()["firmware-profiles.json"]
        );
        assert_eq!(
            repository.targets().signed.targets[&catalog_name].custom["rc2fxCatalog"],
            "1.2.3"
        );
        fs::write(
            base.join("targets/mixer-v1.2.3/rc2fx-mixer-1.2.3.payload"),
            b"tampered",
        )
        .unwrap();
        let stream = repository.read_target(&name).await.unwrap().unwrap();
        assert!(stream.into_vec().await.is_err());
    });
    let catalog = base.join("targets/mixer-v1.2.3/firmware-profiles.json");
    // Restore the payload so this failure specifically exercises catalog binding.
    fs::write(
        base.join("targets/mixer-v1.2.3/rc2fx-mixer-1.2.3.payload"),
        payload,
    )
    .unwrap();
    fs::write(catalog, b"{}").unwrap();
    assert!(
        invoke(arguments("bad-catalog", "targets.pk8"))
            .unwrap_err()
            .contains("catalog differs")
    );
    assert!(!base.join("bad-catalog").exists());
    #[cfg(unix)]
    {
        // Exercise the exact PEM files emitted by the user-facing key tool,
        // through release signing and the client's TUF verifier.
        let keys = base.join("generated-keys");
        crate::mixer_keys::create(&keys, &crate::root()).unwrap();
        fs::write(
            base.join("targets/mixer-v1.2.3/firmware-profiles.json"),
            include_bytes!("../../config/firmware-profiles.json"),
        )
        .unwrap();
        let args = vec![
            keys.join("public/root.json").display().to_string(),
            base.join("targets/mixer-v1.2.3").display().to_string(),
            base.join("pem-metadata").display().to_string(),
            "2".into(),
            expiry,
            keys.join("release/targets.pem").display().to_string(),
            keys.join("release/snapshot.pem").display().to_string(),
            keys.join("release/timestamp.pem").display().to_string(),
        ];
        invoke(args).unwrap();
        runtime.block_on(async {
            let root = fs::read(keys.join("public/root.json")).unwrap();
            let repository = tough::RepositoryLoader::new(
                &root,
                format!("file://{}/pem-metadata/", base.display())
                    .parse()
                    .unwrap(),
                format!("file://{}/targets/", base.display())
                    .parse()
                    .unwrap(),
            )
            .load()
            .await
            .unwrap();
            let name = tough::TargetName::new("mixer-v1.2.3/rc2fx-mixer-1.2.3.payload").unwrap();
            let bytes = repository
                .read_target(&name)
                .await
                .unwrap()
                .unwrap()
                .into_vec()
                .await
                .unwrap();
            Envelope::standalone(&bytes).unwrap();
        });
    }
}
