//! Inline in-place update wire-DTO contract tests and live Go-built payload
//! envelope interoperability tests.

use rc2fx_device::mixer_update::{Chunk, Progress, StartRequest, payload, verify::MixerTarget};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

#[test]
fn update_dtos_match_wire_contract() {
    let valid_target = json!({
        "version": "0.2.1",
        "payloadFormat": 1,
        "protocolVersion": 1,
        "minimumAppVersion": "0.1.7",
        "firmwareProfiles": ["rodecaster-pro-ii-1.7.7", "rodecaster-duo-1.7.7"]
    });
    let target: MixerTarget = serde_json::from_value(valid_target.clone()).unwrap();
    assert_eq!(serde_json::to_value(&target).unwrap(), valid_target);

    for (name, bad) in [
        (
            "unknown-field",
            json!({
                "version": "0.2.1", "payloadFormat": 1, "protocolVersion": 1,
                "minimumAppVersion": "0.1.7", "firmwareProfiles": ["rodecaster-pro-ii-1.7.7"],
                "extra": true
            }),
        ),
        (
            "missing-minimum-app",
            json!({
                "version": "0.2.1", "payloadFormat": 1, "protocolVersion": 1,
                "firmwareProfiles": ["rodecaster-pro-ii-1.7.7"]
            }),
        ),
        (
            "empty-profiles",
            json!({
                "version": "0.2.1", "payloadFormat": 1, "protocolVersion": 1,
                "minimumAppVersion": "0.1.7", "firmwareProfiles": []
            }),
        ),
    ] {
        if let Ok(target) = serde_json::from_value::<MixerTarget>(bad) {
            assert!(
                target.firmware_profiles.is_empty(),
                "expected reject for mixerTarget {name}, got {target:?}"
            );
        }
    }

    for (name, good) in [
        (
            "whole-envelope",
            json!({
                "version": "0.2.1",
                "size": 9403,
                "sha256": "8fb8f5ce993567895775f544a14450a6f9be0660f6ca146f04e6b75428a77a12",
                "metadata": {"root": ["{}"], "timestamp": "{}", "snapshot": "{}", "targets": "{}"}
            }),
        ),
        (
            "delta-upload-with-downgrade",
            json!({
                "version": "0.2.0",
                "size": 9403,
                "sha256": "8fb8f5ce993567895775f544a14450a6f9be0660f6ca146f04e6b75428a77a12",
                "metadata": {"root": ["{}", "{}"], "timestamp": "{}", "snapshot": "{}", "targets": "{}"},
                "allowDowngrade": true,
                "encoding": "delta-1",
                "uploadSize": 1024
            }),
        ),
    ] {
        let req: StartRequest = serde_json::from_value(good.clone())
            .unwrap_or_else(|e| panic!("accept startRequest {name}: {e}"));
        assert_eq!(req.version, good["version"].as_str().unwrap());
        assert_eq!(req.size, good["size"].as_u64().unwrap());
    }
    for (name, bad) in [
        (
            "unknown-field",
            json!({
                "version": "0.2.1", "size": 100,
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "metadata": {"root": ["{}"], "timestamp": "{}", "snapshot": "{}", "targets": "{}"},
                "unexpected": 1
            }),
        ),
        (
            "missing-metadata",
            json!({
                "version": "0.2.1", "size": 100,
                "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
            }),
        ),
    ] {
        assert!(
            serde_json::from_value::<StartRequest>(bad).is_err(),
            "accepted invalid startRequest {name}"
        );
    }

    for (name, good) in [
        (
            "first-chunk",
            json!({"offset": 0, "data": "cmMyZngtdXBkYXRlLWNodW5r"}),
        ),
        (
            "offset-chunk",
            json!({"offset": 196608, "data": "AAH+/w=="}),
        ),
    ] {
        let chunk: Chunk = serde_json::from_value(good.clone()).unwrap();
        assert_eq!(chunk.offset, good["offset"].as_u64().unwrap(), "{name}");
        assert_eq!(chunk.data, good["data"].as_str().unwrap(), "{name}");
    }
    for (name, bad) in [
        (
            "unknown-field",
            json!({"offset": 0, "data": "AA==", "extra": true}),
        ),
        ("missing-offset", json!({"data": "AA=="})),
    ] {
        assert!(
            serde_json::from_value::<Chunk>(bad).is_err(),
            "accepted invalid chunk {name}"
        );
    }

    for (name, progress, want) in [
        (
            "accepting-plain",
            Progress {
                id: "0123456789abcdef".into(),
                state: "accepting",
                version: "0.2.1".into(),
                size: 9403,
                received: 4096,
                encoding: None,
                upload_size: None,
                error: None,
            },
            json!({"id":"0123456789abcdef","state":"accepting","version":"0.2.1","size":9403,"received":4096}),
        ),
        (
            "accepting-delta",
            Progress {
                id: "fedcba9876543210".into(),
                state: "accepting",
                version: "0.2.1".into(),
                size: 9403,
                received: 512,
                encoding: Some("delta-1"),
                upload_size: Some(1024),
                error: None,
            },
            json!({"id":"fedcba9876543210","state":"accepting","version":"0.2.1","size":9403,"received":512,"encoding":"delta-1","uploadSize":1024}),
        ),
        (
            "restarting",
            Progress {
                id: "0123456789abcdef".into(),
                state: "restarting",
                version: "0.2.1".into(),
                size: 9403,
                received: 9403,
                encoding: None,
                upload_size: None,
                error: None,
            },
            json!({"id":"0123456789abcdef","state":"restarting","version":"0.2.1","size":9403,"received":9403}),
        ),
        (
            "failed",
            Progress {
                id: "0123456789abcdef".into(),
                state: "failed",
                version: "0.2.1".into(),
                size: 9403,
                received: 9403,
                encoding: None,
                upload_size: None,
                error: Some("verification failed".into()),
            },
            json!({"id":"0123456789abcdef","state":"failed","version":"0.2.1","size":9403,"received":9403,"error":"verification failed"}),
        ),
    ] {
        let serialized: Value = serde_json::to_value(&progress).unwrap();
        assert_eq!(serialized, want, "progress {name}");
    }
}

#[test]
fn go_built_payload_envelope_parses_and_bad_envelopes_are_rejected() {
    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop");
    let output = Command::new("go")
        .args(["run", "./internal/interop", "update-envelope"])
        .current_dir(&desktop)
        .output()
        .expect("spawn `go run ./internal/interop update-envelope`");
    assert!(
        output.status.success(),
        "go interop update-envelope failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let valid = output.stdout;
    let files = payload::read(&mut valid.as_slice()).expect("valid Go envelope must pass");
    assert!(!files.is_empty());

    let dev_output = Command::new("go")
        .args(["run", "./internal/interop", "update-envelope", "--dev"])
        .current_dir(&desktop)
        .output()
        .expect("spawn `go run ./internal/interop update-envelope --dev`");
    assert!(dev_output.status.success());

    let mut bad_magic = valid.clone();
    bad_magic[0] = b'X';
    let mut bad_digest = valid.clone();
    let tail = bad_digest.len() - 1;
    bad_digest[tail] ^= 0xff;
    let truncated = valid[..valid.len() - 1].to_vec();
    let mut trailing = valid.clone();
    trailing.push(0);

    for (name, bytes) in [
        ("empty", Vec::new()),
        ("bad-magic", bad_magic),
        ("bad-digest", bad_digest),
        ("truncated", truncated),
        ("trailing-byte", trailing),
        ("dev-build", dev_output.stdout),
    ] {
        assert!(
            payload::read(&mut bytes.as_slice()).is_err(),
            "accepted malformed envelope {name}"
        );
    }
}
