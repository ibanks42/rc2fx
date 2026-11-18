//! The mixer accepts what the Go release tooling signs. `fixtures/xtask`
//! holds TUF metadata written by the Go xtask (`desktop/internal/xtask`,
//! regenerated and checked by its `TestXtaskFixtures`) for one packaged
//! release: an Ed25519 repository with fixed keys, and an RSA-3072
//! (RSA-PSS/SHA-256) one rooted in a `mixer-keys` root, each as `mixer-sign`
//! wrote it (`metadata/`) and as `mixer-refresh` renewed it (`refreshed/`).
//! The in-place update verifier must accept every one of them, from the
//! repository's own `root.json`, exactly as it accepts the tough-signed
//! metadata of the other tests.
#![cfg(target_os = "linux")]

use rc2fx_device::mixer_update::install::StoredState;
use rc2fx_device::mixer_update::verify::{MetadataInput, ReleaseRequest, verify};
use serde_json::Value;
use std::path::PathBuf;

const PAYLOAD: &str = "mixer-v1.2.3/rc2fx-mixer-1.2.3.payload";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/xtask")
}

fn text(path: &str) -> String {
    std::fs::read_to_string(fixtures().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// One repository's metadata as the desktop sends it, and the request for
/// the signed payload as the signed targets describe it.
fn request(repo: &str, metadata: &str) -> (Vec<u8>, MetadataInput, ReleaseRequest, String) {
    let root = text(&format!("{repo}/root.json"));
    let input = MetadataInput {
        root: vec![root.clone()],
        timestamp: text(&format!("{repo}/{metadata}/timestamp.json")),
        snapshot: text(&format!("{repo}/{metadata}/snapshot.json")),
        targets: text(&format!("{repo}/{metadata}/targets.json")),
    };
    let targets: Value = serde_json::from_str(&input.targets).unwrap();
    let target = &targets["signed"]["targets"][PAYLOAD];
    let profile = target["custom"]["rc2fxMixer"]["firmwareProfiles"][0]
        .as_str()
        .expect("a signed firmware profile")
        .to_owned();
    let release = ReleaseRequest {
        version: "1.2.3".into(),
        size: target["length"].as_u64().unwrap(),
        sha256: target["hashes"]["sha256"].as_str().unwrap().to_owned(),
        allow_downgrade: false,
    };
    (root.into_bytes(), input, release, profile)
}

#[test]
fn the_mixer_accepts_every_repository_the_go_signer_wrote() {
    let payload = fixtures().join("targets").join(PAYLOAD);
    for repo in ["ed25519", "rsa"] {
        for (metadata, sequence) in [("metadata", 1), ("refreshed", 2)] {
            let (root, input, release, profile) = request(repo, metadata);
            let verified = verify(
                &root,
                &input,
                &release,
                &payload,
                Some(&profile),
                None,
                &StoredState::default(),
            )
            .unwrap_or_else(|e| panic!("{repo}/{metadata}: {e}"));
            assert_eq!(verified.version, "1.2.3", "{repo}/{metadata}");
            assert_eq!(verified.metadata.minimum_app_version, "1.0.1");
            assert_eq!(verified.metadata_versions.root_version, 1);
            assert_eq!(verified.metadata_versions.targets_version, 1);
            assert_eq!(verified.metadata_versions.snapshot_version, sequence);
            assert_eq!(verified.metadata_versions.timestamp_version, sequence);
        }
    }
}

#[test]
fn the_go_signers_metadata_is_refused_when_altered_or_from_another_root() {
    let payload = fixtures().join("targets").join(PAYLOAD);
    let (root, input, release, profile) = request("ed25519", "metadata");
    let check = |root: &[u8], input: &MetadataInput, release: &ReleaseRequest| {
        verify(
            root,
            input,
            release,
            &payload,
            Some(&profile),
            None,
            &StoredState::default(),
        )
    };
    // A changed signed byte breaks the targets signature and its snapshot pin.
    let mut altered = input.clone();
    altered.targets = altered.targets.replacen("\"1.0.1\"", "\"1.0.2\"", 1);
    assert_ne!(altered.targets, input.targets);
    assert!(check(&root, &altered, &release).is_err());
    // A request for bytes other than the signed payload's.
    let mut other = release.clone();
    other.sha256 = "0".repeat(64);
    assert!(check(&root, &input, &other).is_err());
    // The other repository's root does not sign this one.
    let (rsa_root, _, _, _) = request("rsa", "metadata");
    assert!(check(&rsa_root, &input, &release).is_err());
}
