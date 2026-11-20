//! Shared in-place update DTO and payload envelope fixtures (`fixtures/update/`).
//! Go checks the same files in `desktop/installer/update_fixtures_test.go` and
//! `desktop/transport/update_fixtures_test.go`.

use rc2fx_device::mixer_update::{Chunk, Progress, StartRequest, payload, verify::MixerTarget};
use serde_json::Value;
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/update")
}

#[test]
fn update_dtos_match_shared_fixtures() {
    let dto: Value =
        serde_json::from_slice(&std::fs::read(dir().join("dto.json")).unwrap()).unwrap();

    for case in dto["mixerTargets"]["accept"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let target: MixerTarget = serde_json::from_value(case["json"].clone())
            .unwrap_or_else(|e| panic!("accept mixerTarget {name}: {e}"));
        let roundtrip = serde_json::to_value(&target).unwrap();
        assert_eq!(roundtrip, case["json"], "roundtrip mixerTarget {name}");
    }
    for case in dto["mixerTargets"]["reject"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if let Ok(target) = serde_json::from_value::<MixerTarget>(case["json"].clone()) {
            // Structural deserialization succeeded (e.g. empty profiles);
            // semantic validation must reject it.
            assert!(
                target.firmware_profiles.is_empty(),
                "expected reject for mixerTarget {name}, got {target:?}"
            );
        }
    }

    for case in dto["startRequests"]["accept"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let req: StartRequest = serde_json::from_value(case["json"].clone())
            .unwrap_or_else(|e| panic!("accept startRequest {name}: {e}"));
        assert_eq!(req.version, case["json"]["version"].as_str().unwrap());
        assert_eq!(req.size, case["json"]["size"].as_u64().unwrap());
    }
    for case in dto["startRequests"]["reject"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert!(
            serde_json::from_value::<StartRequest>(case["json"].clone()).is_err(),
            "accepted invalid startRequest {name}"
        );
    }

    for case in dto["chunks"]["accept"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let chunk: Chunk = serde_json::from_value(case["json"].clone())
            .unwrap_or_else(|e| panic!("accept chunk {name}: {e}"));
        assert_eq!(chunk.offset, case["json"]["offset"].as_u64().unwrap());
        assert_eq!(chunk.data, case["json"]["data"].as_str().unwrap());
    }
    for case in dto["chunks"]["reject"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert!(
            serde_json::from_value::<Chunk>(case["json"].clone()).is_err(),
            "accepted invalid chunk {name}"
        );
    }

    for case in dto["progress"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let j = &case["json"];
        let state: &'static str = match j["state"].as_str().unwrap() {
            "accepting" => "accepting",
            "complete" => "complete",
            "installing" => "installing",
            "restarting" => "restarting",
            "failed" => "failed",
            other => panic!("unexpected state {other}"),
        };
        let encoding: Option<&'static str> = match j.get("encoding").and_then(Value::as_str) {
            None => None,
            Some("delta-1") => Some("delta-1"),
            Some(other) => panic!("unexpected encoding {other}"),
        };
        let progress = Progress {
            id: j["id"].as_str().unwrap().to_owned(),
            state,
            version: j["version"].as_str().unwrap().to_owned(),
            size: j["size"].as_u64().unwrap(),
            received: j["received"].as_u64().unwrap(),
            encoding,
            upload_size: j.get("uploadSize").and_then(Value::as_u64),
            error: j.get("error").and_then(Value::as_str).map(str::to_owned),
        };
        let serialized = serde_json::to_value(&progress).unwrap();
        assert_eq!(serialized, *j, "progress {name}");
    }
}

#[test]
fn payload_envelope_roundtrips_and_rejects_bad_fixtures() {
    let valid = std::fs::read(dir().join("envelope.bin")).unwrap();
    let files = payload::read(&mut valid.as_slice()).expect("valid shared envelope must pass");
    assert!(!files.is_empty());

    let bad_dir = dir().join("bad-envelopes");
    let mut bad_files: Vec<PathBuf> = std::fs::read_dir(&bad_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    bad_files.sort();
    assert!(!bad_files.is_empty());
    for path in bad_files {
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            payload::read(&mut bytes.as_slice()).is_err(),
            "accepted malformed envelope {}",
            path.display()
        );
    }
}
