//! Live cross-tool TUF verification tests: runs the Go xtask signer into a
//! temporary directory (Ed25519 and RSA-3072, initial `metadata/` and refreshed
//! `refreshed/`) and verifies every repository with the mixer's Rust verifier.

use rc2fx_device::mixer_update::install::StoredState;
use rc2fx_device::mixer_update::verify::{MetadataInput, ReleaseRequest, verify};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const PAYLOAD: &str = "mixer-v1.2.3/rc2fx-mixer-1.2.3.payload";

fn generated_repos() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("rc2fx-xtask-tuf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let xtask = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/xtask");
        let output = Command::new("go")
            .args(["run", ".", "tuf-test-repos"])
            .arg(&dir)
            .current_dir(&xtask)
            .output()
            .expect("spawn `go run . tuf-test-repos` in tools/xtask");
        assert!(
            output.status.success(),
            "xtask tuf-test-repos failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir
    })
}

fn text(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn request(
    dir: &Path,
    repo: &str,
    metadata: &str,
) -> (Vec<u8>, MetadataInput, ReleaseRequest, String) {
    let root = text(dir, &format!("{repo}/root.json"));
    let input = MetadataInput {
        root: vec![root.clone()],
        timestamp: text(dir, &format!("{repo}/{metadata}/timestamp.json")),
        snapshot: text(dir, &format!("{repo}/{metadata}/snapshot.json")),
        targets: text(dir, &format!("{repo}/{metadata}/targets.json")),
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
    let dir = generated_repos();
    let payload = dir.join("targets").join(PAYLOAD);
    for repo in ["ed25519", "rsa"] {
        for (metadata, sequence) in [("metadata", 1), ("refreshed", 2)] {
            let (root, input, release, profile) = request(dir, repo, metadata);
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
    let dir = generated_repos();
    let payload = dir.join("targets").join(PAYLOAD);
    let (root, input, release, profile) = request(dir, "ed25519", "metadata");
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
    let mut altered = input.clone();
    altered.targets = altered.targets.replacen("\"1.0.1\"", "\"1.0.2\"", 1);
    assert_ne!(altered.targets, input.targets);
    assert!(check(&root, &altered, &release).is_err());
    let mut other = release.clone();
    other.sha256 = "0".repeat(64);
    assert!(check(&root, &input, &other).is_err());
    let (rsa_root, _, _, _) = request(dir, "rsa", "metadata");
    assert!(check(&rsa_root, &input, &release).is_err());
}
