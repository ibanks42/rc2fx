//! Shared fixtures from `fixtures/image_check`. Only the mixer parses
//! on-disk records into reports, so this test covers `records` and their
//! serialized output; the Go desktop port (`desktop/core/image_check_fixtures_test.go`)
//! tests reading `reports`.
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
