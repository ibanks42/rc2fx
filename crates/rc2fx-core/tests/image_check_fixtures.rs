//! The shared image-check fixtures in `fixtures/image_check`. The desktop's
//! Go port (`desktop/core/image_check_fixtures_test.go`) reads the same
//! files: both must make the same report from each record, accept and reject
//! the same `GET api/v1/image-check` answers, and write accepted ones back
//! byte for byte.
use rc2fx_core::ImageCheck;
use serde_json::Value;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/image_check")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn cases() -> Value {
    let cases: Value = serde_json::from_slice(&read("cases.json")).unwrap();
    assert!(!cases["records"].as_array().unwrap().is_empty());
    assert!(!cases["reports"].as_array().unwrap().is_empty());
    cases
}

#[test]
fn records_make_the_same_reports() {
    for case in cases()["records"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let record = case["record"]
            .as_str()
            .map(|file| String::from_utf8(read(file)).unwrap());
        let digest = case["digest"].as_str();
        let check = ImageCheck::from_record(record.as_deref(), digest);
        assert_eq!(
            serde_json::to_string(&check).unwrap(),
            case["expect"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn reports_are_accepted_and_rejected_alike() {
    for case in cases()["reports"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        // What the transport does with the mixer's answer.
        let result = serde_json::from_slice::<Value>(&read(case["file"].as_str().unwrap()))
            .map_err(|e| e.to_string())
            .and_then(|value| {
                serde_json::from_value::<ImageCheck>(value).map_err(|e| e.to_string())
            });
        let accept = case["accept"].as_bool().unwrap();
        assert_eq!(
            result.is_ok(),
            accept,
            "{name} ({}): {result:?}",
            case["why"]
        );
        if let Ok(check) = result {
            assert_eq!(
                serde_json::to_string(&check).unwrap(),
                case["canonical"].as_str().unwrap(),
                "{name}"
            );
        }
    }
}
