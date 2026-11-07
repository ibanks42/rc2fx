#![cfg(unix)]
use rc2fx_core::{Config, ConfigUpdate, TargetKind};
use rc2fx_device::store::{MAX_PRESETS, MAX_STATE_BYTES, Store, StoreError};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

fn initial() -> Config {
    let mut config = Config::default();
    config.selected_targets = vec![config.targets[0].id.clone()];
    config
}
fn update(store: &Store) -> ConfigUpdate {
    ConfigUpdate {
        expected_revision: store.config().revision,
        targets: store.config().targets.clone(),
    }
}
fn disk(path: &std::path::Path) -> Vec<u8> {
    fs::read(path.join("state.json")).unwrap()
}

#[test]
fn initialize_lock_reopen_and_permissions() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("nested/data");
    let original = initial();
    let store = Store::open(&path, original.clone()).unwrap();
    assert_eq!(store.config(), &original);
    assert!(matches!(
        Store::open(&path, original.clone()),
        Err(StoreError::Locked)
    ));
    for (path, permission) in [
        (path.clone(), 0o700),
        (path.join("state.json"), 0o600),
        (path.join("daemon.lock"), 0o600),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            permission
        );
    }
    drop(store);
    let mut ignored_initial = original.clone();
    ignored_initial.schema_version = 0;
    let reopened = Store::open(&path, ignored_initial).unwrap();
    assert_eq!(reopened.config(), &original);
    let saved: serde_json::Value = serde_json::from_slice(&disk(&path)).unwrap();
    assert_eq!(saved["presets"], serde_json::json!([]));
}

#[test]
fn effects_preserve_selection_and_topology_and_own_snapshots() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let saved = store.config().clone();
    let mut request = update(&store);
    request.targets[1].gain.db = -9.0; // An inactive path remains editable and saved.
    request.targets[0].name = "My microphone".into();
    request.targets.reverse(); // Order is not an immutable physical identity.
    let mut result = store.apply(request).unwrap();
    assert_eq!(result.revision, saved.revision + 1);
    assert_eq!(result.selected_targets, saved.selected_targets);
    result.targets[0].name = "External mutation".into();
    result.selected_targets.clear();
    assert_ne!(result, *store.config());
    assert!(matches!(
        store.apply(ConfigUpdate {
            expected_revision: saved.revision,
            targets: saved.targets,
        }),
        Err(StoreError::Conflict)
    ));
    let before = disk(temporary.path());
    let mut cases = Vec::new();
    let mut missing = update(&store);
    missing.targets.pop();
    cases.push(missing);
    let mut duplicate = update(&store);
    duplicate.targets[1] = duplicate.targets[0].clone();
    cases.push(duplicate);
    let mut rename_id = update(&store);
    rename_id.targets[0].id = "new-target".into();
    cases.push(rename_id);
    let mut channels = update(&store);
    channels.targets[0].channels = 3 - channels.targets[0].channels;
    cases.push(channels);
    let mut kind = update(&store);
    kind.targets[0].kind = if kind.targets[0].kind == TargetKind::Output {
        TargetKind::Playback
    } else {
        TargetKind::Output
    };
    cases.push(kind);
    let mut nonfinite = update(&store);
    nonfinite.targets[0].gain.db = f64::NAN;
    cases.push(nonfinite);
    for case in cases {
        assert!(matches!(store.apply(case), Err(StoreError::Invalid(_))));
    }
    assert_eq!(disk(temporary.path()), before);
    let expected = store.config().clone();
    drop(store);
    assert_eq!(
        Store::open(temporary.path(), initial()).unwrap().config(),
        &expected
    );
}

#[test]
fn preset_transactions_preserve_selection_and_complete_saved_effects() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let revision = store.config().revision;
    let preset = store.create_preset("Original", revision).unwrap();
    assert_eq!(preset.id.len(), 32);
    assert!(preset.id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(store.config().revision, revision);
    let original = store.config().clone();
    let mut change = update(&store);
    change.targets[1].gain.db = -12.0;
    store.apply(change).unwrap();
    assert!(matches!(
        store.apply_preset(&preset.id, revision),
        Err(StoreError::Conflict)
    ));
    let restored = store.apply_preset(&preset.id, revision + 1).unwrap();
    assert_eq!(restored.targets, original.targets);
    assert_eq!(restored.selected_targets, original.selected_targets);
    assert_eq!(restored.revision, revision + 2);
    let mut summaries = store.presets();
    summaries[0].name.clear();
    summaries[0].id.clear();
    assert_eq!(store.presets()[0], preset);
    assert!(matches!(
        store.create_preset("stale", revision),
        Err(StoreError::Conflict)
    ));
    for name in [String::new(), "é".repeat(65)] {
        assert!(matches!(
            store.create_preset(&name, restored.revision),
            Err(StoreError::Invalid(_))
        ));
    }
    assert!(matches!(
        store.apply_preset("missing", restored.revision),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.delete_preset("missing"),
        Err(StoreError::NotFound)
    ));
    store.delete_preset(&preset.id).unwrap();
    assert_eq!(store.config(), &restored);
    assert!(store.presets().is_empty());
    drop(store);
    assert!(
        Store::open(temporary.path(), initial())
            .unwrap()
            .presets()
            .is_empty()
    );
}

#[test]
fn preset_limit_is_transactional_and_deletion_releases_capacity() {
    let temporary = tempfile::tempdir().unwrap();
    let mut config = initial();
    config.targets.truncate(1);
    let mut store = Store::open(temporary.path(), config).unwrap();
    for index in 0..MAX_PRESETS {
        store.create_preset(&format!("Preset {index}"), 1).unwrap();
    }
    let before = disk(temporary.path());
    assert!(matches!(
        store.create_preset("Too many", 1),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(disk(temporary.path()), before);
    assert_eq!(store.presets().len(), MAX_PRESETS);
    store.delete_preset(&store.presets()[0].id).unwrap();
    store.create_preset("Replacement", 1).unwrap();
    let expected = store.presets();
    drop(store);
    assert_eq!(
        Store::open(temporary.path(), initial()).unwrap().presets(),
        expected
    );
}

#[test]
fn state_strict_decoding_and_failed_open_release_lock() {
    let temporary = tempfile::tempdir().unwrap();
    let valid = serde_json::json!({"engineGeneration":0,"config": initial(), "presets": []});
    let good = serde_json::to_string(&valid).unwrap();
    let mut corrupt = vec![
        format!("{good} {{}}"),
        good.replacen("\"revision\":1", "\"revision\":1,\"revision\":2", 1),
    ];
    for mutation in 0..8 {
        let mut value = valid.clone();
        match mutation {
            0 => {
                value.as_object_mut().unwrap().remove("presets");
            }
            1 => value["presets"] = serde_json::Value::Null,
            2 => value["extra"] = true.into(),
            3 => {
                value["config"]
                    .as_object_mut()
                    .unwrap()
                    .remove("selectedTargets");
            }
            4 => value["config"]["selectedTargets"] = serde_json::Value::Null,
            5 => value["config"]["schemaVersion"] = 3.into(),
            6 => value["config"]["revision"] = 9_007_199_254_740_992_u64.into(),
            7 => {
                value["presets"] =
                    serde_json::json!([{"id":"BAD", "name":"bad", "targets":initial().targets}])
            }
            _ => unreachable!(),
        }
        corrupt.push(value.to_string());
    }
    let mut duplicates = valid.clone();
    let preset = serde_json::json!({"id":"same", "name":"same", "targets":initial().targets});
    duplicates["presets"] = serde_json::json!([preset.clone(), preset]);
    corrupt.push(duplicates.to_string());
    for text in corrupt {
        fs::write(temporary.path().join("state.json"), text).unwrap();
        assert!(matches!(
            Store::open(temporary.path(), initial()),
            Err(StoreError::Invalid(_))
        ));
        fs::write(temporary.path().join("state.json"), &good).unwrap();
        drop(Store::open(temporary.path(), initial()).unwrap());
    }
}

#[test]
fn state_has_larger_explicit_limit_than_http_and_refuses_oversize() {
    let temporary = tempfile::tempdir().unwrap();
    let mut text =
        serde_json::json!({"engineGeneration":0,"config": initial(), "presets": []}).to_string();
    text.push_str(&" ".repeat(512 * 1024));
    assert!(rc2fx_core::from_json::<serde_json::Value>(&text).is_err());
    fs::write(temporary.path().join("state.json"), text).unwrap();
    drop(Store::open(temporary.path(), initial()).unwrap());
    fs::write(
        temporary.path().join("state.json"),
        vec![b' '; MAX_STATE_BYTES + 1],
    )
    .unwrap();
    assert!(Store::open(temporary.path(), initial()).is_err());
}

#[test]
fn persisted_foreign_preset_cannot_change_current_topology() {
    let temporary = tempfile::tempdir().unwrap();
    let original = initial();
    let mut foreign = original.targets.clone();
    foreign.pop();
    fs::write(temporary.path().join("state.json"), serde_json::json!({"engineGeneration":0,"config":original,"presets":[{"id":"foreign","name":"Foreign","targets":foreign}]}).to_string()).unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let before = disk(temporary.path());
    assert!(matches!(
        store.apply_preset("foreign", 1),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(disk(temporary.path()), before);
}

#[test]
fn revision_exhaustion_does_not_wrap_or_write() {
    let temporary = tempfile::tempdir().unwrap();
    let mut config = initial();
    config.revision = 9_007_199_254_740_991;
    let mut store = Store::open(temporary.path(), config).unwrap();
    let before = disk(temporary.path());
    assert!(matches!(
        store.apply(update(&store)),
        Err(StoreError::Invalid(_))
    ));
    assert_eq!(disk(temporary.path()), before);
}

#[test]
fn prepared_effects_can_be_inspected_then_aborted_before_any_save() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let before = store.config().clone();
    let bytes = disk(temporary.path());
    let mut request = update(&store);
    request.targets[1].gain.db = -8.0;
    let prepared = store.prepare_apply(request.clone()).unwrap();
    assert_eq!(prepared.candidate_config().revision, before.revision + 1);
    assert_eq!(
        prepared.candidate_config().selected_targets,
        before.selected_targets
    );
    assert_eq!(prepared.candidate_config().targets[1].gain.db, -8.0);
    assert_eq!(disk(temporary.path()), bytes);
    // A failed audio reservation drops this token without a save or revision.
    drop(prepared);
    assert_eq!(store.config(), &before);
    assert_eq!(disk(temporary.path()), bytes);
    let committed = store.prepare_apply(request).unwrap().commit().unwrap();
    assert_eq!(&committed, store.config());
    assert_eq!(committed.revision, before.revision + 1);
}

#[test]
fn prepared_preset_exposes_effects_before_commit_and_keeps_selection() {
    let temporary = tempfile::tempdir().unwrap();
    let mut store = Store::open(temporary.path(), initial()).unwrap();
    let original = store.config().clone();
    let preset = store.create_preset("Before", original.revision).unwrap();
    let mut request = update(&store);
    request.targets[1].gain.db = -9.0;
    let changed = store.apply(request).unwrap();
    let bytes = disk(temporary.path());
    let prepared = store.prepare_preset(&preset.id, changed.revision).unwrap();
    assert_eq!(prepared.candidate_config().targets, original.targets);
    assert_eq!(
        prepared.candidate_config().selected_targets,
        changed.selected_targets
    );
    assert_eq!(prepared.candidate_config().revision, changed.revision + 1);
    assert_eq!(disk(temporary.path()), bytes);
    drop(prepared);
    assert_eq!(store.config(), &changed);
    assert_eq!(disk(temporary.path()), bytes);
    let restored = store
        .prepare_preset(&preset.id, changed.revision)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(restored.targets, original.targets);
    assert_eq!(&restored, store.config());
}

#[test]
fn symlink_and_nonregular_paths_are_rejected_without_following() {
    let temporary = tempfile::tempdir().unwrap();
    let real = temporary.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = temporary.path().join("link");
    symlink(&real, &link).unwrap();
    assert!(Store::open(&link, initial()).is_err());
    assert!(!real.join("state.json").exists());
    for name in ["state.json", "daemon.lock"] {
        let owned = temporary.path().join(name.replace('.', "-"));
        fs::create_dir(&owned).unwrap();
        let other = temporary.path().join(format!("{name}-other"));
        fs::write(&other, "untouched").unwrap();
        symlink(&other, owned.join(name)).unwrap();
        assert!(Store::open(&owned, initial()).is_err());
        assert_eq!(fs::read_to_string(&other).unwrap(), "untouched");
    }
    let fifo = temporary.path().join("fifo");
    fs::create_dir(&fifo).unwrap();
    let path =
        std::ffi::CString::new(fifo.join("state.json").as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the pathname is NUL-terminated and remains alive through mkfifo.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(Store::open(&fifo, initial()).is_err());
}
