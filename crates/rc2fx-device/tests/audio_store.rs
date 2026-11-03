#![cfg(unix)]
use rc2fx_core::{Config, ConfigUpdate, SCHEMA_VERSION, Target, TargetKind};
use rc2fx_device::{
    audio::{self, Chain, Group, PrepareError, SaveError},
    store::Store,
};

fn initial() -> Config {
    Config {
        schema_version: SCHEMA_VERSION,
        revision: 1,
        selected_targets: vec!["usb-main".into(), "send-chat".into()],
        targets: vec![
            Target::new("usb-main", "USB main", TargetKind::Playback, 2),
            Target::new("send-chat", "USB chat output", TargetKind::Output, 2),
            Target::new("mic-1", "Mic", TargetKind::Mic, 1),
        ],

    }
}

#[test]
fn durable_effects_and_presets_publish_one_epoch_then_wait_for_all_audio_paths() {
    let directory = tempfile::tempdir().unwrap();
    let config = initial();
    let mut store = Store::open(directory.path(), config.clone()).unwrap();
    let preset = store.create_preset("Before", 1).unwrap();
    let chains = config.targets[..2]
        .iter()
        .map(|target| Chain::new(target, [None, None]).unwrap())
        .collect();
    let mut group = Group::new(chains, 1).unwrap();
    let (mut control, mut callbacks) = group.split().unwrap();
    let mut targets = config.targets.clone();
    targets[0].gain.db = -12.0;
    targets[2].order.swap(0, 1); // Inactive structural change is still saved.
    targets.reverse();
    let candidate = store
        .prepare_apply(ConfigUpdate {
            expected_revision: 1,
            targets: targets.clone(),

        })
        .unwrap();
    let saved = audio::save_and_publish(candidate, &mut control).unwrap();
    assert_eq!(saved.revision, 2);
    assert_eq!(saved.targets, targets);
    assert_eq!(store.config(), &saved);
    assert_eq!(control.published_revision(), 2);
    assert_eq!(control.applied_revision(), 0);
    callbacks[0]
        .process([&[0.2], &[0.2]], [&mut [0.0], &mut [0.0]])
        .unwrap();
    assert_eq!(control.applied_revision(), 0);
    let disk_before = std::fs::read(directory.path().join("state.json")).unwrap();
    let preset_save = store.prepare_preset(&preset.id, 2).unwrap();
    assert!(matches!(
        audio::save_and_publish(preset_save, &mut control),
        Err(SaveError::Audio(PrepareError::Busy))
    ));
    assert_eq!(store.config(), &saved);
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        disk_before
    );
    callbacks[1]
        .process([&[0.2], &[0.2]], [&mut [0.0], &mut [0.0]])
        .unwrap();
    assert_eq!(control.applied_revision(), 2);
    let preset_save = store.prepare_preset(&preset.id, 2).unwrap();
    let restored = audio::save_and_publish(preset_save, &mut control).unwrap();
    assert_eq!(restored.revision, 3);
    assert_eq!(restored.targets, config.targets);
    assert_eq!(restored.selected_targets, config.selected_targets);
    assert_eq!(control.applied_revision(), 0);
    for callback in &mut callbacks {
        callback
            .process([&[0.2], &[0.2]], [&mut [0.0], &mut [0.0]])
            .unwrap();
    }
    assert_eq!(control.applied_revision(), 3);
    drop(store);
    assert_eq!(
        Store::open(directory.path(), config).unwrap().config(),
        &restored
    );
}

#[test]
fn active_structural_change_is_refused_before_disk_or_audio_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = initial();
    config.selected_targets.push("mic-1".into());
    let mut store = Store::open(directory.path(), config.clone()).unwrap();
    let chains = config
        .targets
        .iter()
        .map(|target| Chain::new(target, [None, None]).unwrap())
        .collect();
    let mut group = Group::new(chains, 1).unwrap();
    let (mut control, _) = group.split().unwrap();
    let before = std::fs::read(directory.path().join("state.json")).unwrap();
    let mut targets = config.targets.clone();
    targets[2].suppression.enabled = true;
    let candidate = store
        .prepare_apply(ConfigUpdate {
            expected_revision: 1,
            targets,

        })
        .unwrap();
    assert!(matches!(
        audio::save_and_publish(candidate, &mut control),
        Err(SaveError::Audio(PrepareError::RequiresRestart))
    ));
    assert_eq!(store.config(), &config);
    assert_eq!(
        std::fs::read(directory.path().join("state.json")).unwrap(),
        before
    );
    assert_eq!(control.published_revision(), 1);
}
