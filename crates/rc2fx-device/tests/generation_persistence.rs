//! Durable lifecycle tokens across a daemon replacement. A client orders the
//! states it receives by generation, then by settings revision, so a restarted
//! daemon must publish a newer generation with the saved settings unchanged.
//! These tests do not claim that a reserved token proves audio recovery.
use rc2fx_core::{Config, ConfigUpdate};
use rc2fx_device::{
    controller::Controller,
    store::{Store, StoreError},
};
use std::{fs, path::Path};

const MAX_GENERATION: u64 = 9_007_199_254_740_991;

fn initial() -> Config {
    Config {
        selected_targets: vec!["usb-main".into()],
        ..Config::default()
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path.join("state.json")).unwrap()
}

fn disk_generation(path: &Path) -> u64 {
    serde_json::from_slice::<serde_json::Value>(&bytes(path)).unwrap()["engineGeneration"]
        .as_u64()
        .unwrap()
}

#[test]
fn a_reopened_controller_publishes_a_newer_generation_with_the_same_settings() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.generation(), 0);
    let saved = store.config().clone();
    assert_eq!(store.reserve_generation().unwrap(), 1);
    assert_eq!(store.config(), &saved);
    assert_eq!(disk_generation(temporary.path()), 1);
    let controller = Controller::new(store);
    let old = controller.state();
    assert_eq!(
        old.engine.generation, 1,
        "first wire state must use the durable boot token"
    );
    drop(controller);

    let mut reopened = Store::open(temporary.path(), Config::default()).unwrap();
    assert_eq!(reopened.generation(), 1);
    assert_eq!(reopened.reserve_generation().unwrap(), 2);
    assert_eq!(reopened.config(), &saved);
    let current = Controller::new(reopened).state();
    // Newer than anything the old daemon published, so a client never takes
    // an old state, even one with a higher settings revision, for current.
    assert_eq!(current.engine.generation, 2);
    // The settings and their revision are unchanged, so a client keeps its
    // unsaved edits: there is nothing new to reconcile them with.
    assert_eq!(current.config, old.config);
    assert_eq!(current.config, saved);
}

#[test]
fn unpublished_reservation_is_never_reused_after_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 1);
    let visible = Controller::new(store).state();
    // The temporary controller above drops its Store after producing an owned
    // snapshot; a killed process need not have published every durable token.
    assert_eq!(visible.engine.generation, 1);
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 2);
    drop(store); // No reply or event for token 2.
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 3);
    let current = Controller::new(store).state();
    // Past the unpublished token too: a state a killed daemon may have sent
    // under it is older than the current one.
    assert_eq!(current.engine.generation, 3);
    assert_eq!(current.config, visible.config);
    assert_eq!(disk_generation(temporary.path()), 3);
}

#[test]
fn effects_presets_and_selection_preserve_generation_and_aborted_candidate() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let token = store.reserve_generation().unwrap();
    let original = store.config().clone();
    let preset = store.create_preset("Original", original.revision).unwrap();
    let mut targets = original.targets.clone();
    targets[0].gain.db = -9.;
    store
        .apply(ConfigUpdate {
            expected_revision: original.revision,
            targets,
        })
        .unwrap();
    assert_eq!(store.generation(), token);
    store
        .apply_preset(&preset.id, store.config().revision)
        .unwrap();
    store.delete_preset(&preset.id).unwrap();
    assert_eq!(store.config().targets, original.targets);
    assert_eq!(store.generation(), token);

    let before = store.config().clone();
    let disk_before = bytes(temporary.path());
    let requested = vec!["usb-main".into(), "mic-1".into()];
    {
        let prepared = store
            .prepare_selection(before.revision, &requested)
            .unwrap();
        assert_eq!(
            prepared.candidate_config().selected_targets,
            vec!["mic-1", "usb-main"]
        );
        assert_eq!(prepared.candidate_config().revision, before.revision + 1);
        assert_eq!(prepared.candidate_config().targets, before.targets);
        assert_eq!(bytes(temporary.path()), disk_before);
        // Dropping readiness preparation is not a committed selection.
    }
    assert_eq!(store.config(), &before);
    assert_eq!(store.generation(), token);
    assert_eq!(bytes(temporary.path()), disk_before);
    let committed = store
        .prepare_selection(before.revision, &requested)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(committed.selected_targets, vec!["mic-1", "usb-main"]);
    assert_eq!(store.generation(), token);
    assert_eq!(disk_generation(temporary.path()), token);
    let prior_revision = store.config().revision;
    assert_eq!(store.reserve_generation().unwrap(), token + 1);
    assert_eq!(store.config().revision, prior_revision);
    drop(store);
    let reopened = Store::open(temporary.path(), Config::default()).unwrap();
    assert_eq!(reopened.config(), &committed);
    assert_eq!(reopened.generation(), token + 1);
}

#[test]
fn exhausted_generation_refuses_without_wrapping_or_rewriting_settings() {
    let temporary = tempfile::tempdir().unwrap();
    drop(Store::open(temporary.path(), initial()).unwrap());
    let mut disk: serde_json::Value = serde_json::from_slice(&bytes(temporary.path())).unwrap();
    disk["engineGeneration"] = (MAX_GENERATION - 1).into();
    fs::write(
        temporary.path().join("state.json"),
        serde_json::to_vec(&disk).unwrap(),
    )
    .unwrap();
    let mut store = Store::open(temporary.path(), Config::default()).unwrap();
    let config = store.config().clone();
    assert_eq!(store.reserve_generation().unwrap(), MAX_GENERATION);
    let final_bytes = bytes(temporary.path());
    for _ in 0..2 {
        assert!(matches!(
            store.reserve_generation(),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(store.generation(), MAX_GENERATION);
        assert_eq!(store.config(), &config);
        assert_eq!(bytes(temporary.path()), final_bytes);
    }
    drop(store);
    let mut reopened = Store::open(temporary.path(), Config::default()).unwrap();
    assert_eq!(reopened.generation(), MAX_GENERATION);
    assert!(matches!(
        reopened.reserve_generation(),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(bytes(temporary.path()), final_bytes);
}

#[test]
fn persisted_generation_is_required_and_strictly_bounded() {
    let temporary = tempfile::tempdir().unwrap();
    drop(Store::open(temporary.path(), initial()).unwrap());
    let good: serde_json::Value = serde_json::from_slice(&bytes(temporary.path())).unwrap();
    for value in [
        serde_json::Value::Null,
        (-1).into(),
        (MAX_GENERATION + 1).into(),
        1.5.into(),
        "1".into(),
    ] {
        let mut disk = good.clone();
        disk["engineGeneration"] = value;
        let invalid = serde_json::to_vec(&disk).unwrap();
        fs::write(temporary.path().join("state.json"), &invalid).unwrap();
        assert!(matches!(
            Store::open(temporary.path(), initial()),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(bytes(temporary.path()), invalid);
    }
    let mut missing = good;
    missing.as_object_mut().unwrap().remove("engineGeneration");
    let invalid = serde_json::to_vec(&missing).unwrap();
    fs::write(temporary.path().join("state.json"), &invalid).unwrap();
    assert!(matches!(
        Store::open(temporary.path(), initial()),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(bytes(temporary.path()), invalid);
}
