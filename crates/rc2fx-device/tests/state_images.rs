#![cfg(unix)]
use rc2fx_core::Config;
use rc2fx_device::store::Store;

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
