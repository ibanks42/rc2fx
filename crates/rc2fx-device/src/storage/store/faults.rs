use super::*;
use crate::persistence::CommitPhase;
use std::fs;

fn update(store: &Store) -> ConfigUpdate {
    let mut targets = store.config().targets.clone();
    targets[0].gain.db = -3.0;
    ConfigUpdate {
        expected_revision: store.config().revision,
        targets,
    }
}
fn assert_no_temporary(path: &Path) {
    assert!(fs::read_dir(path).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".state-")
    }));
}

#[test]
fn generation_reservation_obeys_durability_and_never_changes_settings() {
    for phase in [CommitPhase::FileSync, CommitPhase::DirectorySync] {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = Store::open(temporary.path(), Config::default()).unwrap();
        assert_eq!(store.reserve_generation().unwrap(), 1);
        let config = store.config().clone();
        store.directory.fail_next(phase);
        let error = store.reserve_generation().unwrap_err();
        assert_eq!(store.generation(), 1);
        assert_eq!(store.config(), &config);
        if phase == CommitPhase::FileSync {
            assert!(matches!(error, StoreError::Io(_)));
            assert_eq!(store.reserve_generation().unwrap(), 2);
        } else {
            assert!(matches!(error, StoreError::DurabilityUncertain(_)));
            assert!(matches!(
                store.reserve_generation(),
                Err(StoreError::DurabilityUncertain(_))
            ));
            assert!(matches!(
                store.prepare_selection(config.revision, &[]),
                Err(StoreError::DurabilityUncertain(_))
            ));
        }
        drop(store);
        let mut reopened = Store::open(temporary.path(), Config::default()).unwrap();
        assert_eq!(reopened.generation(), 2);
        assert_eq!(reopened.reserve_generation().unwrap(), 3);
        assert_eq!(reopened.config(), &config);
    }
}

#[test]
fn pre_rename_failures_keep_disk_and_memory_and_allow_explicit_retry() {
    for phase in [
        CommitPhase::Write,
        CommitPhase::FileSync,
        CommitPhase::Rename,
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let mut store = Store::open(temporary.path(), Config::default()).unwrap();
        let before = store.config().clone();
        let bytes = fs::read(temporary.path().join("state.json")).unwrap();
        store.directory.fail_next(phase);
        assert!(matches!(
            store.apply(update(&store)),
            Err(StoreError::Io(_))
        ));
        assert_eq!(store.config(), &before);
        assert_eq!(
            fs::read(temporary.path().join("state.json")).unwrap(),
            bytes
        );
        assert!(store.durability_fault().is_none());
        assert_no_temporary(temporary.path());
        assert_eq!(
            store.apply(update(&store)).unwrap().revision,
            before.revision + 1
        );
    }
}

#[test]
fn post_rename_failure_latches_all_mutations_with_truthful_disk_uncertainty() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), Config::default()).unwrap();
    let preset = store.create_preset("Before", 1).unwrap();
    let before = store.config().clone();
    store.directory.fail_next(CommitPhase::DirectorySync);
    assert!(matches!(
        store.apply(update(&store)),
        Err(StoreError::DurabilityUncertain(_))
    ));
    assert_eq!(store.config(), &before);
    assert!(store.durability_fault().is_some());
    let on_disk: DiskState = rc2fx_core::from_json_limited(
        &fs::read_to_string(temporary.path().join("state.json")).unwrap(),
        MAX_STATE_BYTES,
        MAX_STATE_NODES,
    )
    .unwrap();
    assert_eq!(on_disk.config.revision, before.revision + 1);
    assert_eq!(on_disk.config.targets[0].gain.db, -3.0);
    let bytes = fs::read(temporary.path().join("state.json")).unwrap();
    assert!(matches!(
        store.apply(update(&store)),
        Err(StoreError::DurabilityUncertain(_))
    ));
    assert!(matches!(
        store.create_preset("After", 1),
        Err(StoreError::DurabilityUncertain(_))
    ));
    assert!(matches!(
        store.apply_preset(&preset.id, 1),
        Err(StoreError::DurabilityUncertain(_))
    ));
    assert!(matches!(
        store.delete_preset(&preset.id),
        Err(StoreError::DurabilityUncertain(_))
    ));
    assert_eq!(
        fs::read(temporary.path().join("state.json")).unwrap(),
        bytes
    );
    assert_no_temporary(temporary.path());
    drop(store);
    let reopened = Store::open(temporary.path(), Config::default()).unwrap();
    assert_eq!(reopened.config(), &on_disk.config);
    assert!(reopened.durability_fault().is_none());
}

#[test]
fn failed_preset_creation_and_delete_never_mutate_memory() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), Config::default()).unwrap();
    store.directory.fail_next(CommitPhase::Rename);
    assert!(store.create_preset("Failed", 1).is_err());
    assert!(store.presets().is_empty());
    let preset = store.create_preset("Kept", 1).unwrap();
    store.directory.fail_next(CommitPhase::FileSync);
    assert!(store.delete_preset(&preset.id).is_err());
    assert_eq!(store.presets(), vec![preset]);
    assert_no_temporary(temporary.path());
}

#[test]
fn bounded_serializer_stops_before_exceeding_maximum() {
    use std::io::Write;
    let mut output = LimitedOutput(Vec::new());
    output.write_all(&vec![b' '; MAX_STATE_BYTES]).unwrap();
    assert!(output.write_all(b"x").is_err());
    assert_eq!(output.0.len(), MAX_STATE_BYTES);
}

#[test]
fn prepared_commit_moves_the_preallocated_acknowledgement() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), Config::default()).unwrap();
    let prepared = store.prepare_apply(update(&store)).unwrap();
    let targets = prepared.acknowledged.targets.as_ptr();
    let name = prepared.acknowledged.targets[0].name.as_ptr();
    let saved_targets = prepared.candidate.config.targets.as_ptr();
    assert_ne!(targets, saved_targets);
    let acknowledged = prepared.commit().unwrap();
    assert_eq!(acknowledged.targets.as_ptr(), targets);
    assert_eq!(acknowledged.targets[0].name.as_ptr(), name);
    assert_eq!(store.config().targets.as_ptr(), saved_targets);
    assert_eq!(&acknowledged, store.config());
}
