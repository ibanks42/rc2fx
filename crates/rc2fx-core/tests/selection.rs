use rc2fx_core::*;
use serde_json::json;

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

fn state() -> DeviceState {
    DeviceState {
        config: Config {
            selected_targets: ids(&["mic-1", "usb-main", "send-chat"]),
            ..Config::default()
        },
        engine: EngineStatus {
            generation: 7,
            restart_allowed: false,
            mode: EngineMode::Jack,
            status: EnginePhase::Running,
            applied_revision: 1,
            sample_rate: 48000,
            block_size: 112,
            message: String::new(),
            audio_advancing: true,
        },
        routing: RoutingStatus {
            status: RoutingPhase::Active,
            channels: 5,
            recovered: 0,
            message: String::new(),
        },
        selection: SelectionStatus {
            channel_capacity: None,
            status: SelectionPhase::Idle,
            allowed: true,
            qualified_target_ids: ids(&["mic-1", "usb-main", "send-chat"]),
            active_target_ids: ids(&["mic-1", "usb-main", "send-chat"]),
            pending_target_ids: vec![],
            message: String::new(),
        },
    }
}
fn committed(saved: &DeviceState, selected: &[String]) -> DeviceState {
    let mut s = saved.clone();
    s.engine.generation += 1;
    s.config.revision += 1;
    s.config.selected_targets = selected.to_vec();
    s.selection.active_target_ids = selected.to_vec();
    s.routing.channels = s.config.selected_channels();
    s.engine.applied_revision = s.config.revision;
    s
}

#[test]
fn a_capacity_above_128_channels_is_invalid() {
    let mut saved = state();
    saved.selection.channel_capacity = Some(128);
    saved.validate().unwrap();
    saved.selection.channel_capacity = Some(129);
    assert!(saved.validate().is_err());
}
#[test]
fn schema_five_requires_selection_without_a_legacy_default() {
    let c = Config::default();
    assert_eq!(c.schema_version, SCHEMA_VERSION);
    assert!(c.selected_targets.is_empty());
    let mut wire = serde_json::to_value(&c).unwrap();
    wire.as_object_mut().unwrap().remove("selectedTargets");
    assert!(Config::from_json(&wire.to_string()).is_err());
    wire = serde_json::to_value(&c).unwrap();
    wire["schemaVersion"] = json!(3);
    assert!(Config::from_json(&wire.to_string()).is_err());
    let mut wire = serde_json::to_value(state()).unwrap();
    wire.as_object_mut().unwrap().remove("selection");
    assert!(DeviceState::from_json(&wire.to_string()).is_err());
}
#[test]
fn selection_validates_known_unique_targets_without_a_per_kind_limit() {
    let c = Config::default();
    for selected in [&["mic-1", "mic-1"][..], &["unknown"]] {
        assert!(c.validate_selection(&ids(selected)).is_err());
    }
    assert!(c.validate_selection(&[]).is_ok());
    // The connected device, not the generic settings model, determines which
    // paths exist and whether their combined allocation fits its hardware.
    for selected in [
        &["mic-1", "analog-1"][..],
        &["usb-main", "usb-chat", "usb-secondary"],
        &["send-chat", "headphones-1"],
    ] {
        assert!(c.validate_selection(&ids(selected)).is_ok());
    }
    assert_eq!(
        c.canonical_selection(&ids(&["send-chat", "mic-1"]))
            .unwrap(),
        ids(&["mic-1", "send-chat"])
    );
}
#[test]
fn applied_counts_selected_channels_and_empty_is_never_applied() {
    let old = state();
    let mut s = committed(&old, &ids(&["mic-1"]));
    assert_eq!(s.config.targets.len(), 25);
    assert_eq!(s.config.selected_channels(), 1);
    assert!(s.is_applied());
    s.routing.channels = 5;
    assert!(!s.is_applied());
    s = committed(&old, &[]);
    s.engine.status = EnginePhase::Offline;
    s.engine.audio_advancing = false;
    s.engine.applied_revision = 0;
    s.routing.status = RoutingPhase::Bypassed;
    assert!(!s.is_applied());
}
