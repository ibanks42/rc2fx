#![cfg(unix)]
//! Durable lifecycle tokens and desktop ordering across a daemon replacement.
//! These tests do not open JACK or claim that a reserved token proves recovery.
use rc2fx_core::{Config, ConfigUpdate, RemoteUpdate, Session};
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
fn reopened_controller_advances_session_without_losing_effect_drafts() {
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

    let mut session = Session::default();
    assert_eq!(session.receive(old.clone()).unwrap(), RemoteUpdate::Loaded);
    session.draft[0].gain.db = -7.;
    let draft = session.draft.clone();
    drop(controller);

    // A transient event-stream reconnect keeps this same Session and its high-water
    // mark. Session::disconnect is reserved for an intentional endpoint change.
    let mut reopened = Store::open(temporary.path(), Config::default()).unwrap();
    assert_eq!(reopened.generation(), 1);
    assert_eq!(reopened.reserve_generation().unwrap(), 2);
    assert_eq!(reopened.config(), &saved);
    let controller = Controller::new(reopened);
    let current = controller.state();
    assert_eq!(current.engine.generation, 2);
    assert_eq!(
        session.receive(current.clone()).unwrap(),
        RemoteUpdate::Unchanged
    );
    assert_eq!(session.draft, draft);
    assert!(session.dirty());
    assert!(!session.conflict);

    assert_eq!(session.receive(old.clone()).unwrap(), RemoteUpdate::Stale);
    assert_eq!(
        session.receive_snapshot(old.clone()).unwrap(),
        RemoteUpdate::Stale
    );
    assert_eq!(
        session.receive_runtime(old.clone()).unwrap(),
        RemoteUpdate::Stale
    );
    assert!(!session.receive_engine(old.engine.clone()));
    // A retired generation cannot win by carrying a higher settings revision.
    let mut delayed = old;
    delayed.config.revision += 1;
    assert_eq!(session.receive(delayed).unwrap(), RemoteUpdate::Stale);
    assert_eq!(session.saved.as_ref().unwrap(), &current);
    assert_eq!(session.draft, draft);
}

#[test]
fn unpublished_reservation_is_never_reused_after_reopen() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 1);
    let mut session = Session::default();
    let mut visible = Controller::new(store).state();
    // The temporary controller above drops its Store after producing an owned
    // snapshot; a killed process need not have published every durable token.
    assert_eq!(visible.engine.generation, 1);
    session.receive(visible.clone()).unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 2);
    drop(store); // No reply or event for token 2.
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    assert_eq!(store.reserve_generation().unwrap(), 3);
    let current = Controller::new(store).state();
    assert_eq!(current.engine.generation, 3);
    assert_eq!(
        session.receive(current.clone()).unwrap(),
        RemoteUpdate::Unchanged
    );
    visible.engine.generation = 2;
    assert_eq!(session.receive(visible).unwrap(), RemoteUpdate::Stale);
    assert_eq!(session.saved.as_ref().unwrap(), &current);
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
