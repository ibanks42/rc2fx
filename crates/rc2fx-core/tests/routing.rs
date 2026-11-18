use rc2fx_core::*;

fn state(enabled: bool) -> DeviceState {
    let config = Config {
        selected_targets: vec!["usb-main".into()],
        ..Config::default()
    };
    DeviceState {
        engine: EngineStatus {
            generation: 9,
            restart_allowed: !enabled,
            mode: EngineMode::Jack,
            status: if enabled {
                EnginePhase::Running
            } else {
                EnginePhase::Offline
            },
            applied_revision: if enabled { config.revision } else { 0 },
            sample_rate: 48_000,
            block_size: 112,
            message: String::new(),
            audio_advancing: enabled,
        },
        routing: RoutingStatus {
            status: if enabled {
                RoutingPhase::Active
            } else {
                RoutingPhase::Bypassed
            },
            channels: if enabled { 2 } else { 0 },
            recovered: 0,
            message: String::new(),
        },
        selection: SelectionStatus {
            channel_capacity: None,
            status: SelectionPhase::Idle,
            allowed: true,
            qualified_target_ids: vec!["usb-main".into()],
            active_target_ids: if enabled {
                vec!["usb-main".into()]
            } else {
                vec![]
            },
            pending_target_ids: vec![],
            message: String::new(),
        },
        config,
    }
}
#[test]
fn resume_requires_explicit_complete_retirement_not_live_or_stale_meters() {
    let ready = state(false);
    ready.validate().unwrap();
    assert!(ready.can_enable_processing());
    assert!(!ready.can_disable_processing());
    let mutations: &[fn(&mut DeviceState)] = &[
        |s| s.engine.restart_allowed = false,
        |s| s.engine.status = EnginePhase::Running,
        |s| s.engine.status = EnginePhase::Error,
        |s| s.engine.mode = EngineMode::Simulation,
        |s| s.engine.audio_advancing = true,
        |s| s.engine.generation = 9_007_199_254_740_991,
        |s| s.routing.status = RoutingPhase::Error,
        |s| s.routing.channels = 2,
        |s| s.routing.recovered = 2,
        |s| s.selection.active_target_ids.push("usb-main".into()),
        |s| s.selection.pending_target_ids.push("usb-main".into()),
        |s| s.selection.status = SelectionPhase::Changing,
        |s| s.config.selected_targets.clear(),
    ];
    for mutate in mutations {
        let mut changed = ready.clone();
        mutate(&mut changed);
        assert!(!changed.can_enable_processing(), "{changed:?}");
    }
    assert!(state(true).can_disable_processing());
}

fn native_state(enabled: bool) -> DeviceState {
    let mut native = state(enabled);
    native.engine.mode = EngineMode::Native;
    native.engine.status = EnginePhase::Running;
    native.engine.restart_allowed = false;
    native.engine.audio_advancing = true;
    native.engine.applied_revision = native.config.revision;
    native
}

#[test]
fn native_controls_use_confirmed_live_passthrough_without_legacy_retirement() {
    let ready = native_state(false);
    assert!(ready.can_enable_processing());
    assert!(!ready.can_disable_processing());
    for mutate in [
        |s: &mut DeviceState| s.engine.status = EnginePhase::Starting,
        |s: &mut DeviceState| s.engine.audio_advancing = false,
        |s: &mut DeviceState| s.engine.applied_revision = 0,
        |s: &mut DeviceState| s.engine.restart_allowed = true,
        |s: &mut DeviceState| s.selection.allowed = false,
        |s: &mut DeviceState| s.routing.channels = 2,
    ] {
        let mut changed = ready.clone();
        mutate(&mut changed);
        assert!(!changed.can_enable_processing());
    }
    assert!(native_state(true).can_disable_processing());
    for enabled in [false, true] {
        let mut empty = native_state(enabled);
        empty.config.selected_targets.clear();
        empty.selection.active_target_ids.clear();
        empty.routing.channels = 0;
        assert_eq!(empty.can_enable_processing(), !enabled);
        assert_eq!(empty.can_disable_processing(), enabled);
        assert!(!empty.is_applied());
    }
}

#[test]
fn native_failed_processing_can_be_explicitly_disabled_without_legacy_restart_permission() {
    let mut failed = native_state(true);
    failed.engine.status = EnginePhase::Error;
    failed.engine.audio_advancing = false;
    failed.engine.applied_revision = 0;
    failed.routing.status = RoutingPhase::Error;
    failed.selection.status = SelectionPhase::Error;
    failed.selection.allowed = false;
    assert!(!failed.can_enable_processing());
    assert!(failed.can_disable_processing());
    failed.selection.status = SelectionPhase::Changing;
    assert!(!failed.can_disable_processing());
    failed.selection.status = SelectionPhase::Error;
    failed.selection.pending_target_ids.push("usb-main".into());
    assert!(!failed.can_disable_processing());
    failed.selection.pending_target_ids.clear();
    failed.engine.mode = EngineMode::Jack;
    assert!(!failed.can_disable_processing());
}
