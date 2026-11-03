use rc2fx_core::*;
use serde_json::json;

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

#[test]
fn capacity_blocks_overfull_drafts_without_changing_current_settings() {
    let mut saved = state();
    saved.selection.channel_capacity = Some(3);
    let mut session = Session::default();
    session.receive(saved.clone()).unwrap();
    session.selection_draft = ids(&["usb-main", "send-chat"]);
    assert!(
        session
            .begin_selection()
            .unwrap_err()
            .to_string()
            .contains("need 4 audio channels")
    );
    assert_eq!(session.saved.as_ref().unwrap(), &saved);
    session.selection_draft = ids(&["mic-1", "usb-main"]);
    assert!(session.begin_selection().is_ok());
    session.selection_draft.clear();
    assert!(session.begin_selection().is_ok());
    saved.selection.channel_capacity = Some(129);
    assert!(saved.validate().is_err());
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
fn ack(saved: &DeviceState, pending: &PendingSelection) -> DeviceState {
    let mut s = saved.clone();
    s.engine.generation += 1;
    s.engine.status = EnginePhase::Restarting;
    s.engine.audio_advancing = false;
    s.selection.status = SelectionPhase::Changing;
    s.selection.allowed = false;
    s.selection.pending_target_ids = pending.request.target_ids.clone();
    s
}

#[test]
fn native_selection_receipt_commits_revision_without_restarting_unrelated_paths() {
    for failed in [false, true] {
        let mut saved = state();
        saved.engine.mode = EngineMode::Native;
        let mut session = Session::default();
        session.receive(saved.clone()).unwrap();
        session.draft[1].gain.db = -7.;
        session.selection_draft = ids(&["mic-1"]);
        let pending = session.begin_selection().unwrap();
        let mut accepted = saved.clone();
        accepted.config.revision += 1;
        accepted.config.selected_targets = pending.request.target_ids.clone();
        accepted.engine.status = EnginePhase::Starting;
        accepted.engine.applied_revision = 0;
        accepted.engine.audio_advancing = false;
        accepted.selection.status = SelectionPhase::Changing;
        accepted.selection.allowed = false;
        accepted.selection.pending_target_ids = pending.request.target_ids.clone();
        // Old applied paths may still be running while changed paths settle.
        accepted.validate().unwrap();
        pending.validate_acknowledgement(&accepted).unwrap();
        let mut wrong = accepted.clone();
        wrong.engine.generation += 1;
        assert!(pending.validate_acknowledgement(&wrong).is_err());
        wrong = accepted.clone();
        wrong.config.revision -= 1;
        assert!(pending.validate_acknowledgement(&wrong).is_err());
        session
            .acknowledge_selection(&pending, accepted.clone())
            .unwrap();
        assert_eq!(session.saved.as_ref(), Some(&accepted));
        assert_eq!(session.draft[1].gain.db, -7.);
        assert!(!session.conflict);
        assert!(!session.selection_dirty());
        assert!(session.begin_apply().is_err());
        let mut terminal = accepted.clone();
        terminal.selection.pending_target_ids.clear();
        terminal.selection.active_target_ids = pending.request.target_ids.clone();
        terminal.selection.status = if failed {
            SelectionPhase::Error
        } else {
            SelectionPhase::Idle
        };
        terminal.engine.status = if failed {
            EnginePhase::Error
        } else {
            EnginePhase::Running
        };
        terminal.engine.audio_advancing = !failed;
        terminal.engine.applied_revision = terminal.config.revision;
        terminal.routing.channels = terminal.config.selected_channels();
        session.receive(terminal.clone()).unwrap();
        session.acknowledge_selection(&pending, accepted).unwrap();
        assert_eq!(session.saved.as_ref(), Some(&terminal));
        assert_eq!(session.draft[1].gain.db, -7.);
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
fn simultaneous_usb_selection_round_trips_and_commits_without_losing_effect_edits() {
    let mut old = state();
    let selected = old
        .config
        .canonical_selection(&ids(&[
            "send-chat",
            "usb-secondary",
            "usb-chat",
            "mic-1",
            "usb-main",
        ]))
        .unwrap();
    old.selection.qualified_target_ids = selected.clone();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session
        .draft
        .iter_mut()
        .find(|t| t.id == "usb-main")
        .unwrap()
        .gain
        .db = -7.;
    session.selection_draft = selected.clone();
    let pending = session.begin_selection().unwrap();
    session
        .acknowledge_selection(&pending, ack(&old, &pending))
        .unwrap();
    let new = committed(&old, &selected);
    assert_eq!(new.config.selected_channels(), 9);
    assert_eq!(
        DeviceState::from_json(&serde_json::to_string(&new).unwrap()).unwrap(),
        new
    );
    session.receive(new).unwrap();
    assert!(!session.conflict);
    assert_eq!(
        session
            .draft
            .iter()
            .find(|t| t.id == "usb-main")
            .unwrap()
            .gain
            .db,
        -7.
    );
    assert_eq!(session.selection_draft, selected);
    assert_eq!(session.begin_apply().unwrap().request.expected_revision, 2);
}
#[test]
fn selection_reservation_leaves_all_effects_and_revision_unchanged() {
    let old = state();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session.draft[1].gain.db = -7.;
    session.selection_draft = ids(&["mic-1"]);
    let effects = session.draft.clone();
    let p = session.begin_selection().unwrap();
    assert_eq!(
        serde_json::to_value(&p.request).unwrap(),
        json!({"expectedRevision":1,"expectedGeneration":7,"targetIds":["mic-1"]})
    );
    session.acknowledge_selection(&p, ack(&old, &p)).unwrap();
    assert_eq!(session.saved.as_ref().unwrap().config, old.config);
    assert_eq!(session.draft, effects);
    assert!(session.begin_apply().is_err());
    assert!(session.begin_selection().is_err());
}
#[test]
fn topology_commit_rebases_unsaved_effects_and_preserves_later_selection_edits() {
    let old = state();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session.draft[1].gain.db = -7.;
    session.selection_draft = ids(&["mic-1"]);
    let p = session.begin_selection().unwrap();
    session.acknowledge_selection(&p, ack(&old, &p)).unwrap();
    session.selection_draft = ids(&["usb-main"]); // another local edit after submission
    session
        .receive(committed(&old, &p.request.target_ids))
        .unwrap();
    assert!(!session.conflict);
    assert_eq!(session.draft[1].gain.db, -7.);
    assert_eq!(session.selection_draft, ids(&["usb-main"]));
    assert_eq!(session.begin_apply().unwrap().request.expected_revision, 2);
}
#[test]
fn external_show_removal_turns_selection_off_without_losing_dirty_eq() {
    let mut old = state();
    old.config.selected_targets = ids(&["mic-1"]);
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    let edited = session.draft.iter().position(|t| t.id == "mic-1").unwrap();
    session.draft[edited].eq.enabled = true;
    session.draft[edited].eq.bands.push(Band {
        gain: 4.,
        ..Band::new("draft-band")
    });
    let draft = session.draft.clone();
    assert!(session.dirty());

    // The runtime's show watcher commits only selection, not effect settings.
    session.receive(committed(&old, &[])).unwrap();
    assert_eq!(session.draft, draft);
    assert_eq!(session.draft[edited].id, "mic-1");
    assert!(session.selection_draft.is_empty());
    assert!(!session.conflict);
    assert!(session.dirty());
    assert_eq!(
        session.saved.as_ref().unwrap().config.targets,
        old.config.targets
    );
    let apply = session.begin_apply().unwrap();
    assert_eq!(apply.request.expected_revision, 2);
}

#[test]
fn concurrent_effect_change_is_not_mistaken_for_topology_only_commit() {
    let old = state();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session.draft[0].gain.db = -7.;
    let mut remote = committed(&old, &ids(&["mic-1"]));
    remote.config.targets[2].gain.db = -4.;
    assert_eq!(session.receive(remote).unwrap(), RemoteUpdate::Conflict);
    assert!(session.begin_apply().is_err());
    assert_eq!(session.draft[0].gain.db, -7.);
}
#[test]
fn late_selection_ack_does_not_regress_success_or_error() {
    for fail in [false, true] {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        session.selection_draft = ids(&["mic-1"]);
        let p = session.begin_selection().unwrap();
        let accepted = ack(&old, &p);
        let mut terminal = committed(&old, &p.request.target_ids);
        if fail {
            terminal = accepted.clone();
            terminal.engine.status = EnginePhase::Error;
            terminal.selection.status = SelectionPhase::Error;
            terminal.selection.pending_target_ids.clear();
        }
        session.receive(terminal.clone()).unwrap();
        session.acknowledge_selection(&p, accepted.clone()).unwrap();
        assert_eq!(session.saved.as_ref().unwrap(), &terminal);
        session.receive_snapshot(accepted).unwrap();
        assert_eq!(session.saved.as_ref().unwrap(), &terminal);
        assert_eq!(session.receive(old).unwrap(), RemoteUpdate::Stale);
    }
}
#[test]
fn selection_rejects_wrong_ack_generation_ids_effects_or_unreserved_status() {
    let old = state();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session.selection_draft = ids(&["mic-1"]);
    let p = session.begin_selection().unwrap();
    let expected = ack(&old, &p);
    for n in 0..6 {
        let mut bad = expected.clone();
        match n {
            0 => bad.engine.generation += 1,
            1 => bad.config.revision += 1,
            2 => bad.config.targets[0].gain.db = -1.,
            3 => bad.selection.pending_target_ids.clear(),
            4 => bad.selection.status = SelectionPhase::Idle,
            _ => bad.selection.allowed = true,
        }
        assert!(p.validate_acknowledgement(&bad).is_err());
    }
}
#[test]
fn effects_and_preset_acknowledgements_cannot_activate_paths() {
    let mut session = Session::default();
    session.receive(state()).unwrap();
    session.draft[0].gain.db = -3.;
    let p = session.begin_apply().unwrap();
    assert!(
        serde_json::to_value(&p.request)
            .unwrap()
            .get("selectedTargets")
            .is_none()
    );
    let mut response = state();
    response.config.revision += 1;
    response.config.targets = p.request.targets.clone();
    response.config.selected_targets = ids(&["mic-1"]);
    assert!(session.acknowledge(&p, response.clone()).is_err());
    assert!(
        session
            .acknowledge_preset(1, &session.draft.clone(), response)
            .is_err()
    );
}
#[test]
fn only_qualified_changed_selections_are_sendable() {
    let mut session = Session::default();
    session.receive(state()).unwrap();
    assert!(session.begin_selection().is_err());
    session.selection_draft = ids(&["analog-1"]);
    assert!(session.begin_selection().is_err());
    session.selection_draft.clear();
    assert!(session.begin_selection().is_ok());
    session.saved.as_mut().unwrap().selection.allowed = false;
    assert!(session.begin_selection().is_err());
}
#[test]
fn explicit_error_retry_can_restore_the_same_saved_paths() {
    let mut s = state();
    s.selection.status = SelectionPhase::Error;
    s.selection.active_target_ids.clear();
    s.engine.status = EnginePhase::Error;
    s.engine.audio_advancing = false;
    let mut session = Session::default();
    session.receive(s.clone()).unwrap();
    assert!(!session.selection_dirty());
    assert!(session.begin_selection().is_ok());
    session.saved.as_mut().unwrap().selection.allowed = false;
    assert!(session.begin_selection().is_err());
    s.config.selected_targets.reverse();
    s.selection.status = SelectionPhase::Idle;
    s.selection.active_target_ids = ids(&["mic-1", "usb-main", "send-chat"]);
    s.engine.status = EnginePhase::Running;
    s.engine.audio_advancing = true;
    assert!(s.is_applied());
}
#[test]
fn successful_same_selection_retry_rebases_local_effect_edits() {
    let mut saved = state();
    saved.selection.status = SelectionPhase::Error;
    saved.selection.active_target_ids.clear();
    saved.engine.status = EnginePhase::Error;
    saved.engine.audio_advancing = false;
    let mut session = Session::default();
    session.receive(saved.clone()).unwrap();
    session.draft[0].gain.db = -9.;
    let p = session.begin_selection().unwrap();
    session.acknowledge_selection(&p, ack(&saved, &p)).unwrap();
    let mut done = committed(&saved, &p.request.target_ids);
    done.selection.status = SelectionPhase::Idle;
    done.engine.status = EnginePhase::Running;
    done.engine.audio_advancing = true;
    session.receive(done).unwrap();
    assert!(!session.conflict);
    assert_eq!(session.draft[0].gain.db, -9.);
    assert_eq!(session.begin_apply().unwrap().request.expected_revision, 2);
}
#[test]
fn applied_counts_selected_channels_and_empty_is_explicit_stock() {
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
    assert!(s.stock_selected());
    assert!(!s.is_applied());
    s.routing.status = RoutingPhase::Error;
    assert!(!s.stock_selected());
}

#[test]
fn late_effect_save_does_not_replace_newer_engine_observations() {
    for restarted in [false, true] {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        // Editing an inactive path remains a settings save, independent of
        // which processing paths are allocated or requested locally.
        session.draft[1].gain.db = -6.;
        session.selection_draft.clear();
        let pending = session.begin_apply().unwrap();
        let mut reply = old.clone();
        reply.config.revision += 1;
        reply.config.targets = pending.request.targets.clone();

        let mut observed = old.engine.clone();
        observed.generation += u64::from(restarted);
        observed.status = if restarted {
            EnginePhase::Restarting
        } else {
            EnginePhase::Error
        };
        observed.audio_advancing = false;
        observed.message = "A newer stream observation".into();
        assert!(session.receive_engine(observed.clone()));
        session.draft[1].gain.db = -7.; // edit while the save is in flight
        session.acknowledge(&pending, reply).unwrap();

        assert_eq!(session.saved.as_ref().unwrap().engine, observed);
        assert_eq!(session.saved.as_ref().unwrap().config.revision, 2);
        assert_eq!(
            session.saved.as_ref().unwrap().config.targets[1].gain.db,
            -6.
        );
        assert_eq!(session.draft[1].gain.db, -7.);
        assert!(session.selection_draft.is_empty());
        assert!(!session.is_applied());
        if restarted {
            assert!(session.begin_apply().is_err());
            assert!(session.begin_selection().is_err());
        }
    }
}

#[test]
fn late_preset_save_keeps_newer_runtime_and_local_drafts() {
    for restarted in [false, true] {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        let original = session.draft.clone();
        let mut reply = old.clone();
        reply.config.revision += 1;
        reply.config.targets[1].gain.db = -4.;
        let mut observed = old.engine.clone();
        observed.generation += u64::from(restarted);
        observed.status = if restarted {
            EnginePhase::Restarting
        } else {
            EnginePhase::Error
        };
        observed.audio_advancing = false;
        assert!(session.receive_engine(observed.clone()));
        session.draft[1].gain.db = -9.;
        session.selection_draft.clear();
        session.acknowledge_preset(1, &original, reply).unwrap();

        assert_eq!(session.saved.as_ref().unwrap().engine, observed);
        assert_eq!(
            session.saved.as_ref().unwrap().config.targets[1].gain.db,
            -4.
        );
        assert_eq!(session.draft[1].gain.db, -9.);
        assert!(session.selection_draft.is_empty());
        assert!(session.conflict);
        assert!(!session.is_applied());
    }
}

#[test]
fn effects_acknowledgement_preserves_observed_path_failure_and_stock_recovery() {
    let old = state();
    let mut session = Session::default();
    session.receive(old.clone()).unwrap();
    session.draft[1].gain.db = -6.;
    let pending = session.begin_apply().unwrap();
    let mut reply = old.clone();
    reply.config.revision += 1;
    reply.config.targets = pending.request.targets.clone();
    // The full stream state can confirm this save before its reply,
    // including a later failure and actual deallocation of all paths.
    let mut terminal = reply.clone();
    terminal.engine.generation += 1;
    terminal.engine.status = EnginePhase::Error;
    terminal.engine.audio_advancing = false;
    terminal.routing.status = RoutingPhase::Bypassed;
    terminal.routing.recovered = terminal.routing.channels;
    terminal.selection.status = SelectionPhase::Error;
    terminal.selection.active_target_ids.clear();
    terminal.selection.allowed = false;
    session.receive(terminal.clone()).unwrap();
    session.acknowledge(&pending, reply).unwrap();
    assert_eq!(session.saved.as_ref().unwrap(), &terminal);
    assert!(!session.dirty());
    assert!(!session.conflict);
    assert!(!session.is_applied());
    assert!(session.begin_selection().is_err());
    assert_eq!(session.receive(old).unwrap(), RemoteUpdate::Stale);
}

#[test]
fn settings_acknowledgement_can_establish_an_unobserved_newer_generation() {
    let mut session = Session::default();
    session.receive(state()).unwrap();
    session.draft[1].gain.db = -6.;
    let pending = session.begin_apply().unwrap();
    let mut reply = state();
    reply.config.revision += 1;
    reply.config.targets = pending.request.targets.clone();
    reply.engine.generation += 1;
    reply.engine.status = EnginePhase::Restarting;
    reply.engine.audio_advancing = false;
    session.acknowledge(&pending, reply.clone()).unwrap();
    assert_eq!(session.saved.as_ref().unwrap(), &reply);
    assert!(!session.receive_engine(state().engine));
}
