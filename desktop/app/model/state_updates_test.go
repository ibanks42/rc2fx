package model

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func state() core.DeviceState {
	selected := []string{"mic-1", "usb-main", "send-chat"}
	config := core.DefaultConfig()
	config.SelectedTargets = deepCopy(selected)
	return core.DeviceState{
		Config: config,
		Engine: core.EngineStatus{
			Generation:      7,
			RestartAllowed:  false,
			Mode:            core.EngineModeJack,
			Status:          core.EnginePhaseRunning,
			AppliedRevision: 1,
			SampleRate:      48000,
			BlockSize:       112,
			Message:         "",
			AudioAdvancing:  true,
		},
		Routing: core.RoutingStatus{
			Status:    core.RoutingPhaseActive,
			Channels:  5,
			Recovered: 0,
			Message:   "",
		},
		Selection: core.SelectionStatus{
			ChannelCapacity:    nil,
			Status:             core.SelectionPhaseIdle,
			Allowed:            true,
			QualifiedTargetIDs: deepCopy(selected),
			ActiveTargetIDs:    deepCopy(selected),
			PendingTargetIDs:   []string{},
			Message:            "",
		},
	}
}

func failure(state core.DeviceState) core.DeviceState {
	state = deepCopy(state)
	state.Engine.Generation++
	state.Engine.Status = core.EnginePhaseError
	state.Engine.AudioAdvancing = false
	state.Routing.Status = core.RoutingPhaseBypassed
	state.Routing.Recovered = state.Routing.Channels
	state.Selection.Status = core.SelectionPhaseError
	state.Selection.Allowed = false
	state.Selection.ActiveTargetIDs = state.Selection.ActiveTargetIDs[:0]
	return state
}

func assertRuntime(t *testing.T, session *core.Session, observed *core.DeviceState) {
	t.Helper()
	saved := session.Saved
	if !equal(saved.Engine, observed.Engine) {
		t.Fatalf("engine %+v, want %+v", saved.Engine, observed.Engine)
	}
	if !equal(saved.Routing, observed.Routing) {
		t.Fatalf("routing %+v, want %+v", saved.Routing, observed.Routing)
	}
	if !equal(saved.Selection, observed.Selection) {
		t.Fatalf("selection %+v, want %+v", saved.Selection, observed.Selection)
	}
	if session.IsApplied() {
		t.Fatal("applied")
	}
}

func mustReceive(t *testing.T, session *core.Session, state core.DeviceState, pending *core.PendingApply, preset *uint64, snapshot, preserve bool) ReceivedState {
	t.Helper()
	got, err := Receive(session, state, pending, preset, snapshot, preserve)
	if err != nil {
		t.Fatal(err)
	}
	return got
}

func receiveSaved(t *testing.T, session *core.Session, state core.DeviceState) {
	t.Helper()
	if _, err := session.Receive(state); err != nil {
		t.Fatal(err)
	}
}

func TestApplyEchoObservesFailureBeforeAckWithoutAcceptingSettingsOrLosingEdits(t *testing.T) {
	old := state()
	session := core.DefaultSession()
	receiveSaved(t, &session, deepCopy(old))
	session.Draft[1].Gain.DB = -6
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	reply := deepCopy(old)
	reply.Config.Revision++
	reply.Config.Targets = deepCopy(pending.Request.Targets)
	observed := failure(reply)
	session.Draft[1].Gain.DB = -9
	session.SelectionDraft = session.SelectionDraft[:0]
	if got := mustReceive(t, &session, deepCopy(observed), &pending, nil, false, false).Update; got != core.RemoteUpdateUnchanged {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	if !equal(session.Saved.Config, old.Config) {
		t.Fatal("settings accepted before the ACK")
	}
	if got := session.Draft[1].Gain.DB; got != -9 {
		t.Fatalf("draft gain %v", got)
	}
	if len(session.SelectionDraft) != 0 {
		t.Fatalf("selection draft %q", session.SelectionDraft)
	}
	if session.Conflict {
		t.Fatal("conflict")
	}
	// A late refresh reply of the same write cannot undo the stream status.
	if got := mustReceive(t, &session, deepCopy(reply), &pending, nil, true, true).Update; got != core.RemoteUpdateStale {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	if err := session.Acknowledge(pending, deepCopy(reply)); err != nil {
		t.Fatal(err)
	}
	assertRuntime(t, &session, &observed)
	if got := session.Saved.Config.Revision; got != 2 {
		t.Fatalf("revision %d", got)
	}
	if got := session.Draft[1].Gain.DB; got != -9 {
		t.Fatalf("draft gain %v", got)
	}
	if len(session.SelectionDraft) != 0 {
		t.Fatalf("selection draft %q", session.SelectionDraft)
	}
}

func TestPresetEchoObservesFailureAndKeepsDraftUntilAck(t *testing.T) {
	old := state()
	session := core.DefaultSession()
	receiveSaved(t, &session, deepCopy(old))
	original := deepCopy(session.Draft)
	reply := deepCopy(old)
	reply.Config.Revision++
	reply.Config.Targets[1].Gain.DB = -6
	observed := failure(reply)
	session.Draft[1].Gain.DB = -9
	session.SelectionDraft = session.SelectionDraft[:0]
	mustReceive(t, &session, deepCopy(observed), nil, ptr[uint64](1), false, false)
	assertRuntime(t, &session, &observed)
	if !equal(session.Saved.Config, old.Config) {
		t.Fatal("settings accepted before the ACK")
	}
	if got := session.Draft[1].Gain.DB; got != -9 {
		t.Fatalf("draft gain %v", got)
	}
	if err := session.AcknowledgePreset(1, original, deepCopy(reply)); err != nil {
		t.Fatal(err)
	}
	assertRuntime(t, &session, &observed)
	if !session.Conflict {
		t.Fatal("no conflict")
	}
	if len(session.SelectionDraft) != 0 {
		t.Fatalf("selection draft %q", session.SelectionDraft)
	}
}

func TestHigherRevisionIsNotAnEchoEvenWithMatchingEffects(t *testing.T) {
	for _, preset := range []bool{false, true} {
		old := state()
		session := core.DefaultSession()
		receiveSaved(t, &session, deepCopy(old))
		session.Draft[1].Gain.DB = -6
		pending, err := session.BeginApply()
		if err != nil {
			t.Fatal(err)
		}
		original := deepCopy(session.Draft)
		reply := deepCopy(old)
		reply.Config.Revision++
		reply.Config.Targets = deepCopy(pending.Request.Targets)
		newer := deepCopy(reply)
		newer.Config.Revision++
		newer.Config.SelectedTargets = newer.Config.SelectedTargets[:0]
		newer.Selection.ActiveTargetIDs = newer.Selection.ActiveTargetIDs[:0]
		newer.Engine.Generation++
		newer.Engine.Status = core.EnginePhaseOffline
		newer.Engine.AudioAdvancing = false
		newer.Routing.Status = core.RoutingPhaseBypassed
		newer.Routing.Channels = 0
		var pendingApply *core.PendingApply
		var presetRevision *uint64
		if preset {
			presetRevision = ptr[uint64](1)
		} else {
			pendingApply = &pending
		}
		mustReceive(t, &session, deepCopy(newer), pendingApply, presetRevision, false, false)
		if !equal(*session.Saved, newer) {
			t.Fatalf("preset %v: saved is not the newer state", preset)
		}
		if !equal(session.Draft, original) {
			t.Fatalf("preset %v: draft changed", preset)
		}
		if !session.Conflict {
			t.Fatalf("preset %v: no conflict", preset)
		}
		if preset {
			if err := session.AcknowledgePreset(1, original, deepCopy(reply)); err == nil {
				t.Fatal("stale preset ACK accepted")
			}
		} else if err := session.Acknowledge(pending, deepCopy(reply)); err == nil {
			t.Fatal("stale ACK accepted")
		}
		if !equal(*session.Saved, newer) {
			t.Fatalf("preset %v: saved is not the newer state", preset)
		}
	}
}

func TestOldGenerationAndUnrelatedExactRevisionCannotHideChanges(t *testing.T) {
	session := core.DefaultSession()
	receiveSaved(t, &session, state())
	session.Draft[1].Gain.DB = -6
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	echo := state()
	echo.Config.Revision++
	echo.Config.Targets = deepCopy(pending.Request.Targets)
	observed := failure(echo)
	mustReceive(t, &session, deepCopy(observed), &pending, nil, false, false)
	if got := mustReceive(t, &session, deepCopy(echo), &pending, nil, false, false).Update; got != core.RemoteUpdateStale {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	unrelated := deepCopy(observed)
	unrelated.Config.Targets[2].Gain.DB = -3
	if got := mustReceive(t, &session, deepCopy(unrelated), &pending, nil, false, false).Update; got != core.RemoteUpdateConflict {
		t.Fatalf("update %v", got)
	}
	if !equal(*session.Saved, unrelated) {
		t.Fatal("saved is not the unrelated state")
	}
	if got := session.Draft[1].Gain.DB; got != -6 {
		t.Fatalf("draft gain %v", got)
	}
}

func TestInvalidPresetEchoIsNotARuntimeObservation(t *testing.T) {
	old := state()
	session := core.DefaultSession()
	receiveSaved(t, &session, deepCopy(old))
	invalid := failure(old)
	invalid.Config.Revision++
	invalid.Config.Targets[1].ID = "unknown-mic"
	// Valid standalone, wrong connected catalog.
	if err := invalid.Validate(); err != nil {
		t.Fatal(err)
	}
	if _, err := Receive(&session, invalid, nil, ptr[uint64](1), false, false); err == nil {
		t.Fatal("accepted")
	}
	if !equal(*session.Saved, old) {
		t.Fatal("saved changed")
	}
	if !equal(session.Draft, old.Config.Targets) {
		t.Fatal("draft changed")
	}
	if session.Conflict {
		t.Fatal("conflict")
	}
}

func TestRefreshPreservingRuntimeCannotRenewItsObservation(t *testing.T) {
	session := core.DefaultSession()
	initial := mustReceive(t, &session, state(), nil, nil, false, false)
	if !initial.RuntimeObserved {
		t.Fatal("initial state not observed")
	}
	observed := failure(state())
	event := mustReceive(t, &session, deepCopy(observed), nil, nil, false, false)
	if !event.RuntimeObserved {
		t.Fatal("event not observed")
	}
	// The request's reply was captured before the failure event in this
	// same engine lifetime, but includes a later settings revision.
	snapshot := deepCopy(observed)
	snapshot.Engine.Status = core.EnginePhaseRunning
	snapshot.Engine.AudioAdvancing = true
	snapshot.Config.Revision++
	refresh := mustReceive(t, &session, snapshot, nil, nil, true, true)
	if refresh.Update != core.RemoteUpdateLoaded {
		t.Fatalf("update %v", refresh.Update)
	}
	if refresh.RuntimeObserved {
		t.Fatal("refresh renewed the observation")
	}
	if !equal(session.Saved.Engine, observed.Engine) {
		t.Fatal("engine changed")
	}
	if got := session.Saved.Config.Revision; got != 2 {
		t.Fatalf("revision %d", got)
	}
	// A newer lifetime is a new observation even through the refresh path.
	newer := deepCopy(*session.Saved)
	newer.Engine.Generation++
	refresh = mustReceive(t, &session, deepCopy(newer), nil, nil, true, true)
	if !refresh.RuntimeObserved {
		t.Fatal("newer lifetime not observed")
	}
	if !equal(session.Saved.Engine, newer.Engine) {
		t.Fatal("engine not the newer one")
	}
}

func TestStaleWriteEchoIsNotAnObservationButOrderedRuntimeIs(t *testing.T) {
	session := core.DefaultSession()
	receiveSaved(t, &session, state())
	session.Draft[1].Gain.DB = -6
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	echo := state()
	echo.Config.Revision++
	echo.Config.Targets = deepCopy(pending.Request.Targets)
	http := mustReceive(t, &session, deepCopy(echo), &pending, nil, true, true)
	if http.Update != core.RemoteUpdateStale {
		t.Fatalf("update %v", http.Update)
	}
	if http.RuntimeObserved {
		t.Fatal("snapshot echo observed")
	}
	stream := mustReceive(t, &session, deepCopy(echo), &pending, nil, false, false)
	if !stream.RuntimeObserved {
		t.Fatal("stream echo not observed")
	}
	observed := failure(echo)
	mustReceive(t, &session, deepCopy(observed), &pending, nil, false, false)
	old := mustReceive(t, &session, deepCopy(echo), &pending, nil, false, false)
	if old.Update != core.RemoteUpdateStale {
		t.Fatalf("update %v", old.Update)
	}
	if old.RuntimeObserved {
		t.Fatal("stale echo observed")
	}
	assertRuntime(t, &session, &observed)
}
