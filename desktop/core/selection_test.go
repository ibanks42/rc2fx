package core

import (
	"slices"
	"strings"
	"testing"
)

// Port of crates/rc2fx-core/tests/selection.rs.

func ids(values ...string) []string { return append([]string{}, values...) }

func TestCapacityBlocksOverfullDraftsWithoutChangingCurrentSettings(t *testing.T) {
	saved := selectionState()
	saved.Selection.ChannelCapacity = new(uint32(3))
	session := DefaultSession()
	receive(t, &session, saved.Clone())
	session.SelectionDraft = ids("usb-main", "send-chat")
	_, err := session.BeginSelection()
	check(t, err != nil && strings.Contains(err.Error(), "need 4 audio channels"), err)
	check(t, session.Saved.Equal(saved))
	session.SelectionDraft = ids("mic-1", "usb-main")
	ok(t, must2(session.BeginSelection()))
	session.SelectionDraft = session.SelectionDraft[:0]
	ok(t, must2(session.BeginSelection()))
	saved.Selection.ChannelCapacity = new(uint32(129))
	fails(t, saved.Validate())
}

func selectionState() DeviceState {
	config := DefaultConfig()
	config.SelectedTargets = ids("mic-1", "usb-main", "send-chat")
	return DeviceState{
		Config: config,
		Engine: EngineStatus{
			Generation:      7,
			Mode:            EngineModeJack,
			Status:          EnginePhaseRunning,
			AppliedRevision: 1,
			SampleRate:      48000,
			BlockSize:       112,
			AudioAdvancing:  true,
		},
		Routing: RoutingStatus{Status: RoutingPhaseActive, Channels: 5},
		Selection: SelectionStatus{
			Status:             SelectionPhaseIdle,
			Allowed:            true,
			QualifiedTargetIDs: ids("mic-1", "usb-main", "send-chat"),
			ActiveTargetIDs:    ids("mic-1", "usb-main", "send-chat"),
			PendingTargetIDs:   ids(),
		},
	}
}

func selectionAck(saved DeviceState, pending PendingSelection) DeviceState {
	s := saved.Clone()
	s.Engine.Generation++
	s.Engine.Status = EnginePhaseRestarting
	s.Engine.AudioAdvancing = false
	s.Selection.Status = SelectionPhaseChanging
	s.Selection.Allowed = false
	s.Selection.PendingTargetIDs = slices.Clone(pending.Request.TargetIDs)
	return s
}

func TestNativeSelectionReceiptCommitsRevisionWithoutRestartingUnrelatedPaths(t *testing.T) {
	for _, failed := range []bool{false, true} {
		saved := selectionState()
		saved.Engine.Mode = EngineModeNative
		session := DefaultSession()
		receive(t, &session, saved.Clone())
		session.Draft[1].Gain.DB = -7.
		session.SelectionDraft = ids("mic-1")
		pending := must(session.BeginSelection())
		accepted := saved.Clone()
		accepted.Config.Revision++
		accepted.Config.SelectedTargets = slices.Clone(pending.Request.TargetIDs)
		accepted.Engine.Status = EnginePhaseStarting
		accepted.Engine.AppliedRevision = 0
		accepted.Engine.AudioAdvancing = false
		accepted.Selection.Status = SelectionPhaseChanging
		accepted.Selection.Allowed = false
		accepted.Selection.PendingTargetIDs = slices.Clone(pending.Request.TargetIDs)
		// Old applied paths may still be running while changed paths settle.
		ok(t, accepted.Validate())
		ok(t, pending.ValidateAcknowledgement(accepted))
		wrong := accepted.Clone()
		wrong.Engine.Generation++
		fails(t, pending.ValidateAcknowledgement(wrong))
		wrong = accepted.Clone()
		wrong.Config.Revision--
		fails(t, pending.ValidateAcknowledgement(wrong))
		ok(t, session.AcknowledgeSelection(pending, accepted.Clone()))
		check(t, session.Saved.Equal(accepted))
		check(t, session.Draft[1].Gain.DB == -7.)
		check(t, !session.Conflict)
		check(t, !session.SelectionDirty())
		fails(t, must2(session.BeginApply()))
		terminal := accepted.Clone()
		terminal.Selection.PendingTargetIDs = terminal.Selection.PendingTargetIDs[:0]
		terminal.Selection.ActiveTargetIDs = slices.Clone(pending.Request.TargetIDs)
		terminal.Selection.Status = SelectionPhaseIdle
		terminal.Engine.Status = EnginePhaseRunning
		if failed {
			terminal.Selection.Status = SelectionPhaseError
			terminal.Engine.Status = EnginePhaseError
		}
		terminal.Engine.AudioAdvancing = !failed
		terminal.Engine.AppliedRevision = terminal.Config.Revision
		terminal.Routing.Channels = terminal.Config.SelectedChannels()
		receive(t, &session, terminal.Clone())
		ok(t, session.AcknowledgeSelection(pending, accepted))
		check(t, session.Saved.Equal(terminal))
		check(t, session.Draft[1].Gain.DB == -7.)
	}
}

func committed(saved DeviceState, selected []string) DeviceState {
	s := saved.Clone()
	s.Engine.Generation++
	s.Config.Revision++
	s.Config.SelectedTargets = slices.Clone(selected)
	s.Selection.ActiveTargetIDs = slices.Clone(selected)
	s.Routing.Channels = s.Config.SelectedChannels()
	s.Engine.AppliedRevision = s.Config.Revision
	return s
}

func TestSchemaFiveRequiresSelectionWithoutALegacyDefault(t *testing.T) {
	c := DefaultConfig()
	check(t, c.SchemaVersion == SchemaVersion)
	check(t, len(c.SelectedTargets) == 0)
	wire := toValue(t, c)
	wire.Remove("selectedTargets")
	fails(t, must2(ConfigFromJSON(wire.String())))
	wire = toValue(t, c)
	wire.Set("schemaVersion", U64Value(3))
	fails(t, must2(ConfigFromJSON(wire.String())))
	state := toValue(t, selectionState())
	state.Remove("selection")
	fails(t, must2(DeviceStateFromJSON(state.String())))
}

func TestSelectionValidatesKnownUniqueTargetsWithoutAPerKindLimit(t *testing.T) {
	c := DefaultConfig()
	for _, selected := range [][]string{ids("mic-1", "mic-1"), ids("unknown")} {
		fails(t, c.ValidateSelection(selected), selected)
	}
	ok(t, c.ValidateSelection(nil))
	// The connected device, not the generic settings model, determines which
	// paths exist and whether their combined allocation fits its hardware.
	for _, selected := range [][]string{
		ids("mic-1", "analog-1"),
		ids("usb-main", "usb-chat", "usb-secondary"),
		ids("send-chat", "headphones-1"),
	} {
		ok(t, c.ValidateSelection(selected))
	}
	check(t, slices.Equal(must(c.CanonicalSelection(ids("send-chat", "mic-1"))), ids("mic-1", "send-chat")))
}

func TestSimultaneousUSBSelectionRoundTripsAndCommitsWithoutLosingEffectEdits(t *testing.T) {
	old := selectionState()
	selected := must(old.Config.CanonicalSelection(ids("send-chat", "usb-secondary", "usb-chat", "mic-1", "usb-main")))
	old.Selection.QualifiedTargetIDs = slices.Clone(selected)
	session := DefaultSession()
	receive(t, &session, old.Clone())
	usbMain := slices.IndexFunc(session.Draft, func(target Target) bool { return target.ID == "usb-main" })
	session.Draft[usbMain].Gain.DB = -7.
	session.SelectionDraft = slices.Clone(selected)
	pending := must(session.BeginSelection())
	ok(t, session.AcknowledgeSelection(pending, selectionAck(old, pending)))
	next := committed(old, selected)
	check(t, next.Config.SelectedChannels() == 9)
	check(t, must(DeviceStateFromJSON(encode(t, next))).Equal(next))
	receive(t, &session, next)
	check(t, !session.Conflict)
	usbMain = slices.IndexFunc(session.Draft, func(target Target) bool { return target.ID == "usb-main" })
	check(t, session.Draft[usbMain].Gain.DB == -7.)
	check(t, slices.Equal(session.SelectionDraft, selected))
	check(t, must(session.BeginApply()).Request.ExpectedRevision == 2)
}

func TestSelectionReservationLeavesAllEffectsAndRevisionUnchanged(t *testing.T) {
	old := selectionState()
	session := DefaultSession()
	receive(t, &session, old.Clone())
	session.Draft[1].Gain.DB = -7.
	session.SelectionDraft = ids("mic-1")
	effects := CloneTargets(session.Draft)
	p := must(session.BeginSelection())
	check(t, toValue(t, p.Request).Equal(parse(t, `{"expectedRevision":1,"expectedGeneration":7,"targetIds":["mic-1"]}`)))
	ok(t, session.AcknowledgeSelection(p, selectionAck(old, p)))
	check(t, session.Saved.Config.Equal(old.Config))
	check(t, TargetsEqual(session.Draft, effects))
	fails(t, must2(session.BeginApply()))
	fails(t, must2(session.BeginSelection()))
}

func TestTopologyCommitRebasesUnsavedEffectsAndPreservesLaterSelectionEdits(t *testing.T) {
	old := selectionState()
	session := DefaultSession()
	receive(t, &session, old.Clone())
	session.Draft[1].Gain.DB = -7.
	session.SelectionDraft = ids("mic-1")
	p := must(session.BeginSelection())
	ok(t, session.AcknowledgeSelection(p, selectionAck(old, p)))
	session.SelectionDraft = ids("usb-main") // another local edit after submission
	receive(t, &session, committed(old, p.Request.TargetIDs))
	check(t, !session.Conflict)
	check(t, session.Draft[1].Gain.DB == -7.)
	check(t, slices.Equal(session.SelectionDraft, ids("usb-main")))
	check(t, must(session.BeginApply()).Request.ExpectedRevision == 2)
}

func TestExternalShowRemovalTurnsSelectionOffWithoutLosingDirtyEQ(t *testing.T) {
	old := selectionState()
	old.Config.SelectedTargets = ids("mic-1")
	session := DefaultSession()
	receive(t, &session, old.Clone())
	edited := slices.IndexFunc(session.Draft, func(target Target) bool { return target.ID == "mic-1" })
	session.Draft[edited].EQ.Enabled = true
	band := NewBand("draft-band")
	band.Gain = 4.
	session.Draft[edited].EQ.Bands = append(session.Draft[edited].EQ.Bands, band)
	draft := CloneTargets(session.Draft)
	check(t, session.Dirty())

	// The runtime's show watcher commits only selection, not effect settings.
	receive(t, &session, committed(old, nil))
	check(t, TargetsEqual(session.Draft, draft))
	check(t, session.Draft[edited].ID == "mic-1")
	check(t, len(session.SelectionDraft) == 0)
	check(t, !session.Conflict)
	check(t, session.Dirty())
	check(t, TargetsEqual(session.Saved.Config.Targets, old.Config.Targets))
	apply := must(session.BeginApply())
	check(t, apply.Request.ExpectedRevision == 2)
}

func TestConcurrentEffectChangeIsNotMistakenForTopologyOnlyCommit(t *testing.T) {
	old := selectionState()
	session := DefaultSession()
	receive(t, &session, old.Clone())
	session.Draft[0].Gain.DB = -7.
	remote := committed(old, ids("mic-1"))
	remote.Config.Targets[2].Gain.DB = -4.
	check(t, receive(t, &session, remote) == RemoteUpdateConflict)
	fails(t, must2(session.BeginApply()))
	check(t, session.Draft[0].Gain.DB == -7.)
}

func TestLateSelectionAckDoesNotRegressSuccessOrError(t *testing.T) {
	for _, fail := range []bool{false, true} {
		old := selectionState()
		session := DefaultSession()
		receive(t, &session, old.Clone())
		session.SelectionDraft = ids("mic-1")
		p := must(session.BeginSelection())
		accepted := selectionAck(old, p)
		terminal := committed(old, p.Request.TargetIDs)
		if fail {
			terminal = accepted.Clone()
			terminal.Engine.Status = EnginePhaseError
			terminal.Selection.Status = SelectionPhaseError
			terminal.Selection.PendingTargetIDs = terminal.Selection.PendingTargetIDs[:0]
		}
		receive(t, &session, terminal.Clone())
		ok(t, session.AcknowledgeSelection(p, accepted.Clone()))
		check(t, session.Saved.Equal(terminal), fail)
		must(session.ReceiveSnapshot(accepted))
		check(t, session.Saved.Equal(terminal), fail)
		check(t, receive(t, &session, old) == RemoteUpdateStale, fail)
	}
}

func TestSelectionRejectsWrongAckGenerationIDsEffectsOrUnreservedStatus(t *testing.T) {
	old := selectionState()
	session := DefaultSession()
	receive(t, &session, old.Clone())
	session.SelectionDraft = ids("mic-1")
	p := must(session.BeginSelection())
	expected := selectionAck(old, p)
	for n := 0; n < 6; n++ {
		bad := expected.Clone()
		switch n {
		case 0:
			bad.Engine.Generation++
		case 1:
			bad.Config.Revision++
		case 2:
			bad.Config.Targets[0].Gain.DB = -1.
		case 3:
			bad.Selection.PendingTargetIDs = bad.Selection.PendingTargetIDs[:0]
		case 4:
			bad.Selection.Status = SelectionPhaseIdle
		default:
			bad.Selection.Allowed = true
		}
		fails(t, p.ValidateAcknowledgement(bad), n)
	}
}

func TestEffectsAndPresetAcknowledgementsCannotActivatePaths(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, selectionState())
	session.Draft[0].Gain.DB = -3.
	p := must(session.BeginApply())
	request := toValue(t, p.Request)
	_, hasSelection := request.Get("selectedTargets")
	check(t, !hasSelection)
	response := selectionState()
	response.Config.Revision++
	response.Config.Targets = CloneTargets(p.Request.Targets)
	response.Config.SelectedTargets = ids("mic-1")
	fails(t, session.Acknowledge(p, response.Clone()))
	fails(t, session.AcknowledgePreset(1, CloneTargets(session.Draft), response))
}

func TestOnlyQualifiedChangedSelectionsAreSendable(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, selectionState())
	fails(t, must2(session.BeginSelection()))
	session.SelectionDraft = ids("analog-1")
	fails(t, must2(session.BeginSelection()))
	session.SelectionDraft = session.SelectionDraft[:0]
	ok(t, must2(session.BeginSelection()))
	session.Saved.Selection.Allowed = false
	fails(t, must2(session.BeginSelection()))
}

func TestExplicitErrorRetryCanRestoreTheSameSavedPaths(t *testing.T) {
	s := selectionState()
	s.Selection.Status = SelectionPhaseError
	s.Selection.ActiveTargetIDs = s.Selection.ActiveTargetIDs[:0]
	s.Engine.Status = EnginePhaseError
	s.Engine.AudioAdvancing = false
	session := DefaultSession()
	receive(t, &session, s.Clone())
	check(t, !session.SelectionDirty())
	ok(t, must2(session.BeginSelection()))
	session.Saved.Selection.Allowed = false
	fails(t, must2(session.BeginSelection()))
	s = s.Clone()
	slices.Reverse(s.Config.SelectedTargets)
	s.Selection.Status = SelectionPhaseIdle
	s.Selection.ActiveTargetIDs = ids("mic-1", "usb-main", "send-chat")
	s.Engine.Status = EnginePhaseRunning
	s.Engine.AudioAdvancing = true
	check(t, s.IsApplied())
}

func TestSuccessfulSameSelectionRetryRebasesLocalEffectEdits(t *testing.T) {
	saved := selectionState()
	saved.Selection.Status = SelectionPhaseError
	saved.Selection.ActiveTargetIDs = saved.Selection.ActiveTargetIDs[:0]
	saved.Engine.Status = EnginePhaseError
	saved.Engine.AudioAdvancing = false
	session := DefaultSession()
	receive(t, &session, saved.Clone())
	session.Draft[0].Gain.DB = -9.
	p := must(session.BeginSelection())
	ok(t, session.AcknowledgeSelection(p, selectionAck(saved, p)))
	done := committed(saved, p.Request.TargetIDs)
	done.Selection.Status = SelectionPhaseIdle
	done.Engine.Status = EnginePhaseRunning
	done.Engine.AudioAdvancing = true
	receive(t, &session, done)
	check(t, !session.Conflict)
	check(t, session.Draft[0].Gain.DB == -9.)
	check(t, must(session.BeginApply()).Request.ExpectedRevision == 2)
}

func TestAppliedCountsSelectedChannelsAndEmptyIsExplicitStock(t *testing.T) {
	old := selectionState()
	s := committed(old, ids("mic-1"))
	check(t, len(s.Config.Targets) == 25)
	check(t, s.Config.SelectedChannels() == 1)
	check(t, s.IsApplied())
	s.Routing.Channels = 5
	check(t, !s.IsApplied())
	s = committed(old, nil)
	s.Engine.Status = EnginePhaseOffline
	s.Engine.AudioAdvancing = false
	s.Engine.AppliedRevision = 0
	s.Routing.Status = RoutingPhaseBypassed
	check(t, s.StockSelected())
	check(t, !s.IsApplied())
	s.Routing.Status = RoutingPhaseError
	check(t, !s.StockSelected())
}

func TestLateEffectSaveDoesNotReplaceNewerEngineObservations(t *testing.T) {
	for _, restarted := range []bool{false, true} {
		old := selectionState()
		session := DefaultSession()
		receive(t, &session, old.Clone())
		// Editing an inactive path remains a settings save, independent of
		// which processing paths are allocated or requested locally.
		session.Draft[1].Gain.DB = -6.
		session.SelectionDraft = session.SelectionDraft[:0]
		pending := must(session.BeginApply())
		reply := old.Clone()
		reply.Config.Revision++
		reply.Config.Targets = CloneTargets(pending.Request.Targets)

		observed := old.Engine
		observed.Status = EnginePhaseError
		if restarted {
			observed.Generation++
			observed.Status = EnginePhaseRestarting
		}
		observed.AudioAdvancing = false
		observed.Message = "A newer stream observation"
		check(t, session.ReceiveEngine(observed))
		session.Draft[1].Gain.DB = -7. // edit while the save is in flight
		ok(t, session.Acknowledge(pending, reply))

		check(t, session.Saved.Engine == observed, restarted)
		check(t, session.Saved.Config.Revision == 2)
		check(t, session.Saved.Config.Targets[1].Gain.DB == -6.)
		check(t, session.Draft[1].Gain.DB == -7.)
		check(t, len(session.SelectionDraft) == 0)
		check(t, !session.IsApplied())
		if restarted {
			fails(t, must2(session.BeginApply()))
			fails(t, must2(session.BeginSelection()))
		}
	}
}

func TestLatePresetSaveKeepsNewerRuntimeAndLocalDrafts(t *testing.T) {
	for _, restarted := range []bool{false, true} {
		old := selectionState()
		session := DefaultSession()
		receive(t, &session, old.Clone())
		original := CloneTargets(session.Draft)
		reply := old.Clone()
		reply.Config.Revision++
		reply.Config.Targets[1].Gain.DB = -4.
		observed := old.Engine
		observed.Status = EnginePhaseError
		if restarted {
			observed.Generation++
			observed.Status = EnginePhaseRestarting
		}
		observed.AudioAdvancing = false
		check(t, session.ReceiveEngine(observed))
		session.Draft[1].Gain.DB = -9.
		session.SelectionDraft = session.SelectionDraft[:0]
		ok(t, session.AcknowledgePreset(1, original, reply))

		check(t, session.Saved.Engine == observed, restarted)
		check(t, session.Saved.Config.Targets[1].Gain.DB == -4.)
		check(t, session.Draft[1].Gain.DB == -9.)
		check(t, len(session.SelectionDraft) == 0)
		check(t, session.Conflict)
		check(t, !session.IsApplied())
	}
}

func TestEffectsAcknowledgementPreservesObservedPathFailureAndStockRecovery(t *testing.T) {
	old := selectionState()
	session := DefaultSession()
	receive(t, &session, old.Clone())
	session.Draft[1].Gain.DB = -6.
	pending := must(session.BeginApply())
	reply := old.Clone()
	reply.Config.Revision++
	reply.Config.Targets = CloneTargets(pending.Request.Targets)
	// The full stream state can confirm this save before its reply,
	// including a later failure and actual deallocation of all paths.
	terminal := reply.Clone()
	terminal.Engine.Generation++
	terminal.Engine.Status = EnginePhaseError
	terminal.Engine.AudioAdvancing = false
	terminal.Routing.Status = RoutingPhaseBypassed
	terminal.Routing.Recovered = terminal.Routing.Channels
	terminal.Selection.Status = SelectionPhaseError
	terminal.Selection.ActiveTargetIDs = terminal.Selection.ActiveTargetIDs[:0]
	terminal.Selection.Allowed = false
	receive(t, &session, terminal.Clone())
	ok(t, session.Acknowledge(pending, reply))
	check(t, session.Saved.Equal(terminal))
	check(t, !session.Dirty())
	check(t, !session.Conflict)
	check(t, !session.IsApplied())
	fails(t, must2(session.BeginSelection()))
	check(t, receive(t, &session, old) == RemoteUpdateStale)
}

func TestSettingsAcknowledgementCanEstablishAnUnobservedNewerGeneration(t *testing.T) {
	session := DefaultSession()
	receive(t, &session, selectionState())
	session.Draft[1].Gain.DB = -6.
	pending := must(session.BeginApply())
	reply := selectionState()
	reply.Config.Revision++
	reply.Config.Targets = CloneTargets(pending.Request.Targets)
	reply.Engine.Generation++
	reply.Engine.Status = EnginePhaseRestarting
	reply.Engine.AudioAdvancing = false
	ok(t, session.Acknowledge(pending, reply.Clone()))
	check(t, session.Saved.Equal(reply))
	check(t, !session.ReceiveEngine(selectionState().Engine))
}
