package core

import "testing"

// Processing on and off: which routing requests the editor sends and how
// their acknowledgements apply. crates/rc2fx-core/tests/routing.rs keeps the
// state checks the mixer's model still has.

func routingState(enabled bool) DeviceState {
	config := DefaultConfig()
	config.SelectedTargets = []string{"usb-main"}
	status, routing, channels, applied := EnginePhaseOffline, RoutingPhaseBypassed, uint32(0), uint64(0)
	active := []string{}
	if enabled {
		status, routing, channels, applied = EnginePhaseRunning, RoutingPhaseActive, 2, config.Revision
		active = []string{"usb-main"}
	}
	return DeviceState{
		Engine: EngineStatus{
			Generation:      9,
			RestartAllowed:  !enabled,
			Mode:            EngineModeJack,
			Status:          status,
			AppliedRevision: applied,
			SampleRate:      48_000,
			BlockSize:       112,
			AudioAdvancing:  enabled,
		},
		Routing: RoutingStatus{Status: routing, Channels: channels},
		Selection: SelectionStatus{
			Status:             SelectionPhaseIdle,
			Allowed:            true,
			QualifiedTargetIDs: []string{"usb-main"},
			ActiveTargetIDs:    active,
			PendingTargetIDs:   []string{},
		},
		Config: config,
	}
}

func routingAcknowledgement(saved DeviceState, enabled bool) DeviceState {
	ack := saved.Clone()
	ack.Engine.Generation++
	ack.Engine.Status = EnginePhaseRestarting
	ack.Engine.RestartAllowed = false
	ack.Engine.AudioAdvancing = false
	ack.Engine.AppliedRevision = 0
	ack.Routing.Status = RoutingPhaseBypassing
	if enabled {
		ack.Routing.Status = RoutingPhaseStarting
	}
	ack.Selection.Allowed = false
	return ack
}

func TestResumeRequiresExplicitCompleteRetirementNotLiveOrStaleMeters(t *testing.T) {
	ready := routingState(false)
	ok(t, ready.Validate())
	check(t, ready.CanEnableProcessing())
	check(t, !ready.CanDisableProcessing())
	mutations := []func(*DeviceState){
		func(s *DeviceState) { s.Engine.RestartAllowed = false },
		func(s *DeviceState) { s.Engine.Status = EnginePhaseRunning },
		func(s *DeviceState) { s.Engine.Status = EnginePhaseError },
		func(s *DeviceState) { s.Engine.Mode = EngineModeSimulation },
		func(s *DeviceState) { s.Engine.AudioAdvancing = true },
		func(s *DeviceState) { s.Engine.Generation = 9_007_199_254_740_991 },
		func(s *DeviceState) { s.Routing.Status = RoutingPhaseError },
		func(s *DeviceState) { s.Routing.Channels = 2 },
		func(s *DeviceState) { s.Routing.Recovered = 2 },
		func(s *DeviceState) { s.Selection.ActiveTargetIDs = append(s.Selection.ActiveTargetIDs, "usb-main") },
		func(s *DeviceState) { s.Selection.PendingTargetIDs = append(s.Selection.PendingTargetIDs, "usb-main") },
		func(s *DeviceState) { s.Selection.Status = SelectionPhaseChanging },
		func(s *DeviceState) { s.Config.SelectedTargets = s.Config.SelectedTargets[:0] },
	}
	for i, mutate := range mutations {
		changed := ready.Clone()
		mutate(&changed)
		check(t, !changed.CanEnableProcessing(), i, changed)
	}
	check(t, routingState(true).CanDisableProcessing())
}

func TestRoutingAcknowledgementIsBoundToGenerationSettingsAndDirection(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		saved := routingState(!enabled)
		session := DefaultSession()
		receive(t, &session, saved.Clone())
		pending := must(session.BeginRouting(enabled))
		check(t, pending.Request.ExpectedGeneration == 9)
		ack := routingAcknowledgement(saved, enabled)
		ok(t, pending.ValidateAcknowledgement(ack))
		mutations := []func(*DeviceState){
			func(s *DeviceState) { s.Engine.Generation-- },
			func(s *DeviceState) { s.Config.Revision++ },
			func(s *DeviceState) { s.Config.Targets[0].Gain.DB = -4. },
			func(s *DeviceState) { s.Engine.Status = EnginePhaseRunning },
			func(s *DeviceState) { s.Engine.RestartAllowed = true },
			func(s *DeviceState) { s.Engine.AudioAdvancing = true },
			func(s *DeviceState) { s.Engine.AppliedRevision = 1 },
			func(s *DeviceState) { s.Routing.Status = RoutingPhaseActive },
			func(s *DeviceState) { s.Selection.PendingTargetIDs = append(s.Selection.PendingTargetIDs, "usb-main") },
		}
		for i, mutate := range mutations {
			wrong := ack.Clone()
			mutate(&wrong)
			fails(t, pending.ValidateAcknowledgement(wrong), enabled, i)
		}
		completed := routingState(enabled)
		completed.Engine.Generation = 10
		receive(t, &session, completed.Clone())
		session.Draft[0].Gain.DB = -7.
		ok(t, session.AcknowledgeRouting(pending, ack))
		check(t, session.Saved.Equal(completed))
		check(t, session.Draft[0].Gain.DB == -7.)
	}
}

func TestDisconnectedAndDirtyEnableRequestsAreNotSent(t *testing.T) {
	session := DefaultSession()
	fails(t, must2(session.BeginRouting(true)))
	receive(t, &session, routingState(false))
	session.Draft[0].Gain.DB = -5.
	fails(t, must2(session.BeginRouting(true)))
	receive(t, &session, routingState(true))
	// Disabling must remain possible even when the local effects draft is dirty.
	ok(t, must2(session.BeginRouting(false)))
	session.Disconnect()
	fails(t, must2(session.BeginRouting(false)))
}

func nativeState(enabled bool) DeviceState {
	native := routingState(enabled)
	native.Engine.Mode = EngineModeNative
	native.Engine.Status = EnginePhaseRunning
	native.Engine.RestartAllowed = false
	native.Engine.AudioAdvancing = true
	native.Engine.AppliedRevision = native.Config.Revision
	return native
}

func nativeAck(saved DeviceState, enabled bool) DeviceState {
	accepted := routingAcknowledgement(saved, enabled)
	accepted.Engine.Generation = saved.Engine.Generation
	accepted.Engine.Status = EnginePhaseStarting
	return accepted
}

func TestNativeControlsUseConfirmedLivePassthroughWithoutLegacyRetirement(t *testing.T) {
	ready := nativeState(false)
	check(t, ready.CanEnableProcessing())
	check(t, !ready.CanDisableProcessing())
	check(t, ready.Engine.Mode.HasProcessingControls())
	for i, mutate := range []func(*DeviceState){
		func(s *DeviceState) { s.Engine.Status = EnginePhaseStarting },
		func(s *DeviceState) { s.Engine.AudioAdvancing = false },
		func(s *DeviceState) { s.Engine.AppliedRevision = 0 },
		func(s *DeviceState) { s.Engine.RestartAllowed = true },
		func(s *DeviceState) { s.Selection.Allowed = false },
		func(s *DeviceState) { s.Routing.Channels = 2 },
	} {
		changed := ready.Clone()
		mutate(&changed)
		check(t, !changed.CanEnableProcessing(), i)
	}
	check(t, nativeState(true).CanDisableProcessing())
	for _, enabled := range []bool{false, true} {
		empty := nativeState(enabled)
		empty.Config.SelectedTargets = empty.Config.SelectedTargets[:0]
		empty.Selection.ActiveTargetIDs = empty.Selection.ActiveTargetIDs[:0]
		empty.Routing.Channels = 0
		check(t, empty.StockSelected(), enabled)
		check(t, empty.CanEnableProcessing() == !enabled, enabled)
		check(t, empty.CanDisableProcessing() == enabled, enabled)
		check(t, !empty.IsApplied(), enabled)
	}
}

func TestNativeRoutingAckKeepsGenerationAndNeverRegressesLaterStreamState(t *testing.T) {
	for _, enabled := range []bool{false, true} {
		saved := nativeState(!enabled)
		session := DefaultSession()
		receive(t, &session, saved.Clone())
		pending := must(session.BeginRouting(enabled))
		accepted := nativeAck(saved, enabled)
		ok(t, pending.ValidateAcknowledgement(accepted))
		wrong := accepted.Clone()
		wrong.Engine.Generation++
		fails(t, pending.ValidateAcknowledgement(wrong))
		ok(t, session.AcknowledgeRouting(pending, accepted.Clone()))
		check(t, session.Saved.Equal(accepted))
		fails(t, must2(session.BeginApply()))
		fails(t, must2(session.BeginSelection()))
		completed := nativeState(enabled)
		receive(t, &session, completed.Clone())
		ok(t, session.AcknowledgeRouting(pending, accepted))
		check(t, session.Saved.Equal(completed))
	}
}

func TestNativeFailedProcessingCanBeExplicitlyDisabledWithoutLegacyRestartPermission(t *testing.T) {
	failed := nativeState(true)
	failed.Engine.Status = EnginePhaseError
	failed.Engine.AudioAdvancing = false
	failed.Engine.AppliedRevision = 0
	failed.Routing.Status = RoutingPhaseError
	failed.Selection.Status = SelectionPhaseError
	failed.Selection.Allowed = false
	check(t, !failed.CanEnableProcessing())
	check(t, failed.CanDisableProcessing())
	session := DefaultSession()
	receive(t, &session, failed.Clone())
	session.Draft[0].Gain.DB = -6.0
	pending := must(session.BeginRouting(false))
	accepted := nativeAck(failed, false)
	accepted.Selection.Status = SelectionPhaseIdle
	ok(t, pending.ValidateAcknowledgement(accepted))
	failed.Selection.Status = SelectionPhaseChanging
	check(t, !failed.CanDisableProcessing())
	failed.Selection.Status = SelectionPhaseError
	failed.Selection.PendingTargetIDs = append(failed.Selection.PendingTargetIDs, "usb-main")
	check(t, !failed.CanDisableProcessing())
	failed.Selection.PendingTargetIDs = failed.Selection.PendingTargetIDs[:0]
	failed.Engine.Mode = EngineModeJack
	check(t, !failed.CanDisableProcessing())
}
