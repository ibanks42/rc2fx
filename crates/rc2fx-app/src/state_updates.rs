//! Pending writes defer their settings acknowledgement, never their live status.
use rc2fx_core::{DeviceState, PendingApply, RemoteUpdate, Result, Session};

#[derive(Debug, PartialEq)]
pub(super) struct ReceivedState {
    pub update: RemoteUpdate,
    /// True only when the accepted message supplied the runtime status.
    /// Loading settings while preserving a stream observation cannot renew it.
    pub runtime_observed: bool,
}

pub(super) fn receive(
    session: &mut Session,
    state: DeviceState,
    pending_apply: Option<&PendingApply>,
    pending_preset_revision: Option<u64>,
    snapshot: bool,
    preserve_snapshot_runtime: bool,
) -> Result<ReceivedState> {
    state.validate()?;
    let same_generation = session
        .saved
        .as_ref()
        .is_some_and(|saved| saved.engine.generation == state.engine.generation);
    let selection_unchanged = session
        .saved
        .as_ref()
        .is_some_and(|saved| saved.config.selected_targets == state.config.selected_targets);
    let own_echo = selection_unchanged
        && (pending_apply.is_some_and(|pending| {
            pending.request.expected_revision.checked_add(1) == Some(state.config.revision)
                && pending.request.targets == state.config.targets
        }) || pending_preset_revision
            .is_some_and(|revision| revision.checked_add(1) == Some(state.config.revision)));
    let (update, runtime_observed) = if own_echo {
        if snapshot {
            // A request's read has no ordering against the live stream. Defer this
            // write's settings to its ACK without overwriting observed runtime.
            // Stale also tells the UI not to refresh its observation timestamp.
            (RemoteUpdate::Stale, false)
        } else {
            (session.receive_runtime(state)?, true)
        }
    } else if preserve_snapshot_runtime {
        (session.receive_snapshot(state)?, !same_generation)
    } else {
        // In particular, another controller's later revision is not our echo,
        // even if it happens to contain the same effects. Keep normal conflict
        // and stale-ACK checks active while a write is pending.
        (session.receive(state)?, true)
    };
    let runtime_observed = runtime_observed && update != RemoteUpdate::Stale;
    Ok(ReceivedState {
        update,
        runtime_observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::*;

    fn state() -> DeviceState {
        let selected = vec!["mic-1".into(), "usb-main".into(), "send-chat".into()];
        DeviceState {
            config: Config {
                selected_targets: selected.clone(),
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
                qualified_target_ids: selected.clone(),
                active_target_ids: selected,
                pending_target_ids: vec![],
                message: String::new(),
            },
        }
    }
    fn failure(mut state: DeviceState) -> DeviceState {
        state.engine.generation += 1;
        state.engine.status = EnginePhase::Error;
        state.engine.audio_advancing = false;
        state.routing.status = RoutingPhase::Bypassed;
        state.routing.recovered = state.routing.channels;
        state.selection.status = SelectionPhase::Error;
        state.selection.allowed = false;
        state.selection.active_target_ids.clear();
        state
    }
    fn assert_runtime(session: &Session, observed: &DeviceState) {
        let saved = session.saved.as_ref().unwrap();
        assert_eq!(saved.engine, observed.engine);
        assert_eq!(saved.routing, observed.routing);
        assert_eq!(saved.selection, observed.selection);
        assert!(!session.is_applied());
    }

    #[test]
    fn apply_echo_observes_failure_before_ack_without_accepting_settings_or_losing_edits() {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        session.draft[1].gain.db = -6.;
        let pending = session.begin_apply().unwrap();
        let mut reply = old.clone();
        reply.config.revision += 1;
        reply.config.targets = pending.request.targets.clone();
        let observed = failure(reply.clone());
        session.draft[1].gain.db = -9.;
        session.selection_draft.clear();
        assert_eq!(
            receive(
                &mut session,
                observed.clone(),
                Some(&pending),
                None,
                false,
                false
            )
            .unwrap()
            .update,
            RemoteUpdate::Unchanged
        );
        assert_runtime(&session, &observed);
        assert_eq!(session.saved.as_ref().unwrap().config, old.config);
        assert_eq!(session.draft[1].gain.db, -9.);
        assert!(session.selection_draft.is_empty());
        assert!(!session.conflict);
        // A late refresh reply of the same write cannot undo the stream status.
        assert_eq!(
            receive(
                &mut session,
                reply.clone(),
                Some(&pending),
                None,
                true,
                true,
            )
            .unwrap()
            .update,
            RemoteUpdate::Stale
        );
        assert_runtime(&session, &observed);
        session.acknowledge(&pending, reply).unwrap();
        assert_runtime(&session, &observed);
        assert_eq!(session.saved.as_ref().unwrap().config.revision, 2);
        assert_eq!(session.draft[1].gain.db, -9.);
        assert!(session.selection_draft.is_empty());
    }

    #[test]
    fn preset_echo_observes_failure_and_keeps_draft_until_ack() {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        let original = session.draft.clone();
        let mut reply = old.clone();
        reply.config.revision += 1;
        reply.config.targets[1].gain.db = -6.;
        let observed = failure(reply.clone());
        session.draft[1].gain.db = -9.;
        session.selection_draft.clear();
        receive(&mut session, observed.clone(), None, Some(1), false, false).unwrap();
        assert_runtime(&session, &observed);
        assert_eq!(session.saved.as_ref().unwrap().config, old.config);
        assert_eq!(session.draft[1].gain.db, -9.);
        session.acknowledge_preset(1, &original, reply).unwrap();
        assert_runtime(&session, &observed);
        assert!(session.conflict);
        assert!(session.selection_draft.is_empty());
    }

    #[test]
    fn higher_revision_is_not_an_echo_even_with_matching_effects() {
        for preset in [false, true] {
            let old = state();
            let mut session = Session::default();
            session.receive(old.clone()).unwrap();
            session.draft[1].gain.db = -6.;
            let pending = session.begin_apply().unwrap();
            let original = session.draft.clone();
            let mut reply = old.clone();
            reply.config.revision += 1;
            reply.config.targets = pending.request.targets.clone();
            let mut newer = reply.clone();
            newer.config.revision += 1;
            newer.config.selected_targets.clear();
            newer.selection.active_target_ids.clear();
            newer.engine.generation += 1;
            newer.engine.status = EnginePhase::Offline;
            newer.engine.audio_advancing = false;
            newer.routing.status = RoutingPhase::Bypassed;
            newer.routing.channels = 0;
            receive(
                &mut session,
                newer.clone(),
                (!preset).then_some(&pending),
                preset.then_some(1),
                false,
                false,
            )
            .unwrap();
            assert_eq!(session.saved.as_ref().unwrap(), &newer);
            assert_eq!(session.draft, original);
            assert!(session.conflict);
            if preset {
                assert!(session.acknowledge_preset(1, &original, reply).is_err());
            } else {
                assert!(session.acknowledge(&pending, reply).is_err());
            }
            assert_eq!(session.saved.as_ref().unwrap(), &newer);
        }
    }

    #[test]
    fn old_generation_and_unrelated_exact_revision_cannot_hide_changes() {
        let mut session = Session::default();
        session.receive(state()).unwrap();
        session.draft[1].gain.db = -6.;
        let pending = session.begin_apply().unwrap();
        let mut echo = state();
        echo.config.revision += 1;
        echo.config.targets = pending.request.targets.clone();
        let observed = failure(echo.clone());
        receive(
            &mut session,
            observed.clone(),
            Some(&pending),
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            receive(&mut session, echo, Some(&pending), None, false, false)
                .unwrap()
                .update,
            RemoteUpdate::Stale
        );
        assert_runtime(&session, &observed);
        let mut unrelated = observed;
        unrelated.config.targets[2].gain.db = -3.;
        assert_eq!(
            receive(
                &mut session,
                unrelated.clone(),
                Some(&pending),
                None,
                false,
                false
            )
            .unwrap()
            .update,
            RemoteUpdate::Conflict
        );
        assert_eq!(session.saved.as_ref().unwrap(), &unrelated);
        assert_eq!(session.draft[1].gain.db, -6.);
    }

    #[test]
    fn invalid_preset_echo_is_not_a_runtime_observation() {
        let old = state();
        let mut session = Session::default();
        session.receive(old.clone()).unwrap();
        let mut invalid = failure(old.clone());
        invalid.config.revision += 1;
        invalid.config.targets[1].id = "unknown-mic".into();
        invalid.validate().unwrap(); // valid standalone, wrong connected catalog
        assert!(receive(&mut session, invalid, None, Some(1), false, false).is_err());
        assert_eq!(session.saved.as_ref().unwrap(), &old);
        assert_eq!(session.draft, old.config.targets);
        assert!(!session.conflict);
    }

    #[test]
    fn refresh_preserving_runtime_cannot_renew_its_observation() {
        let mut session = Session::default();
        let initial = receive(&mut session, state(), None, None, false, false).unwrap();
        assert!(initial.runtime_observed);
        let observed = failure(state());
        let event = receive(&mut session, observed.clone(), None, None, false, false).unwrap();
        assert!(event.runtime_observed);
        // The request's reply was captured before the failure event in this
        // same engine lifetime, but includes a later settings revision.
        let mut snapshot = observed.clone();
        snapshot.engine.status = EnginePhase::Running;
        snapshot.engine.audio_advancing = true;
        snapshot.config.revision += 1;
        let refresh = receive(&mut session, snapshot, None, None, true, true).unwrap();
        assert_eq!(refresh.update, RemoteUpdate::Loaded);
        assert!(!refresh.runtime_observed);
        assert_eq!(session.saved.as_ref().unwrap().engine, observed.engine);
        assert_eq!(session.saved.as_ref().unwrap().config.revision, 2);
        // A newer lifetime is a new observation even through the refresh path.
        let mut newer = session.saved.as_ref().unwrap().clone();
        newer.engine.generation += 1;
        let refresh = receive(&mut session, newer.clone(), None, None, true, true).unwrap();
        assert!(refresh.runtime_observed);
        assert_eq!(session.saved.as_ref().unwrap().engine, newer.engine);
    }

    #[test]
    fn stale_write_echo_is_not_an_observation_but_ordered_runtime_is() {
        let mut session = Session::default();
        session.receive(state()).unwrap();
        session.draft[1].gain.db = -6.;
        let pending = session.begin_apply().unwrap();
        let mut echo = state();
        echo.config.revision += 1;
        echo.config.targets = pending.request.targets.clone();
        let http = receive(&mut session, echo.clone(), Some(&pending), None, true, true).unwrap();
        assert_eq!(http.update, RemoteUpdate::Stale);
        assert!(!http.runtime_observed);
        let stream = receive(
            &mut session,
            echo.clone(),
            Some(&pending),
            None,
            false,
            false,
        )
        .unwrap();
        assert!(stream.runtime_observed);
        let observed = failure(echo.clone());
        receive(
            &mut session,
            observed.clone(),
            Some(&pending),
            None,
            false,
            false,
        )
        .unwrap();
        let old = receive(&mut session, echo, Some(&pending), None, false, false).unwrap();
        assert_eq!(old.update, RemoteUpdate::Stale);
        assert!(!old.runtime_observed);
        assert_runtime(&session, &observed);
    }
}
