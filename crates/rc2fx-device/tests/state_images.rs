#![cfg(unix)]
use rc2fx_core::Config;
use rc2fx_device::store::Store;

/// Open a store whose saved state is exactly `bytes`.
fn open_with(bytes: &[u8]) -> Result<Store, rc2fx_device::store::StoreError> {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("state.json"), bytes).unwrap();
    Store::open(directory.path(), Config::default())
}

#[test]
fn saved_state_round_trips_and_keeps_its_raw_bytes() {
    let config = Config::default();
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path(), config.clone()).unwrap();
    let path = directory.path().join("state.json");
    store.reserve_generation().unwrap();
    store.create_preset("Saved voice", config.revision).unwrap();
    drop(store);

    let current = std::fs::read(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&current).unwrap();
    let formatted = serde_json::to_vec_pretty(&value).unwrap();
    std::fs::write(&path, &formatted).unwrap();
    let reopened = Store::open(directory.path(), Config::default()).unwrap();
    assert_eq!(reopened.generation(), 1);
    assert_eq!(reopened.config(), &config);
    assert_eq!(reopened.presets().len(), 1);
    assert_eq!(std::fs::read(&path).unwrap(), formatted);
}

#[test]
fn saved_state_is_strict_and_never_migrated() {
    let directory = tempfile::tempdir().unwrap();
    drop(Store::open(directory.path(), Config::default()).unwrap());
    let current = std::fs::read(directory.path().join("state.json")).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&current).unwrap();
    assert_eq!(value["engineGeneration"], 0);
    assert!(value["presets"].as_array().unwrap().is_empty());
    open_with(&current).unwrap();
    let mut old = value.clone();
    old.as_object_mut().unwrap().remove("engineGeneration");
    assert!(open_with(&serde_json::to_vec(&old).unwrap()).is_err());
    let mut unknown = value.clone();
    unknown["schemaMigration"] = true.into();
    assert!(open_with(&serde_json::to_vec(&unknown).unwrap()).is_err());
    let mut invalid = value;
    invalid["engineGeneration"] = 9_007_199_254_740_992u64.into();
    assert!(open_with(&serde_json::to_vec(&invalid).unwrap()).is_err());
    assert!(open_with(b"{\"engineGeneration\":0,\"engineGeneration\":1}").is_err());
    assert!(open_with(&[0xff]).is_err());
    assert!(open_with(&current[..current.len() - 1]).is_err());
}
