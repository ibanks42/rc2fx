package model

import (
	"math"
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func state() core.DeviceState {
	selected := []string{"mic-1", "usb-main", "send-chat"}
	config := core.DefaultConfig()
	config.SelectedTargets = slices.Clone(selected)
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
			QualifiedTargetIDs: slices.Clone(selected),
			ActiveTargetIDs:    slices.Clone(selected),
			PendingTargetIDs:   []string{},
			Message:            "",
		},
	}
}

func failure(state core.DeviceState) core.DeviceState {
	state = state.Clone()
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
	if saved.Engine != observed.Engine {
		t.Fatalf("engine %+v, want %+v", saved.Engine, observed.Engine)
	}
	if saved.Routing != observed.Routing {
		t.Fatalf("routing %+v, want %+v", saved.Routing, observed.Routing)
	}
	if !saved.Selection.Equal(observed.Selection) {
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
	receiveSaved(t, &session, old.Clone())
	session.Draft[1].Gain.DB = -6
	pending, err := session.BeginApply()
	if err != nil {
		t.Fatal(err)
	}
	reply := old.Clone()
	reply.Config.Revision++
	reply.Config.Targets = core.CloneTargets(pending.Request.Targets)
	observed := failure(reply)
	session.Draft[1].Gain.DB = -9
	session.SelectionDraft = session.SelectionDraft[:0]
	if got := mustReceive(t, &session, observed.Clone(), &pending, nil, false, false).Update; got != core.RemoteUpdateUnchanged {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	if !session.Saved.Config.Equal(old.Config) {
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
	if got := mustReceive(t, &session, reply.Clone(), &pending, nil, true, true).Update; got != core.RemoteUpdateStale {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	if err := session.Acknowledge(pending, reply.Clone()); err != nil {
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
	receiveSaved(t, &session, old.Clone())
	original := core.CloneTargets(session.Draft)
	reply := old.Clone()
	reply.Config.Revision++
	reply.Config.Targets[1].Gain.DB = -6
	observed := failure(reply)
	session.Draft[1].Gain.DB = -9
	session.SelectionDraft = session.SelectionDraft[:0]
	mustReceive(t, &session, observed.Clone(), nil, new(uint64(1)), false, false)
	assertRuntime(t, &session, &observed)
	if !session.Saved.Config.Equal(old.Config) {
		t.Fatal("settings accepted before the ACK")
	}
	if got := session.Draft[1].Gain.DB; got != -9 {
		t.Fatalf("draft gain %v", got)
	}
	if err := session.AcknowledgePreset(1, original, reply.Clone()); err != nil {
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
		receiveSaved(t, &session, old.Clone())
		session.Draft[1].Gain.DB = -6
		pending, err := session.BeginApply()
		if err != nil {
			t.Fatal(err)
		}
		original := core.CloneTargets(session.Draft)
		reply := old.Clone()
		reply.Config.Revision++
		reply.Config.Targets = core.CloneTargets(pending.Request.Targets)
		newer := reply.Clone()
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
			presetRevision = new(uint64(1))
		} else {
			pendingApply = &pending
		}
		mustReceive(t, &session, newer.Clone(), pendingApply, presetRevision, false, false)
		if !session.Saved.Equal(newer) {
			t.Fatalf("preset %v: saved is not the newer state", preset)
		}
		if !core.TargetsEqual(session.Draft, original) {
			t.Fatalf("preset %v: draft changed", preset)
		}
		if !session.Conflict {
			t.Fatalf("preset %v: no conflict", preset)
		}
		if preset {
			if err := session.AcknowledgePreset(1, original, reply.Clone()); err == nil {
				t.Fatal("stale preset ACK accepted")
			}
		} else if err := session.Acknowledge(pending, reply.Clone()); err == nil {
			t.Fatal("stale ACK accepted")
		}
		if !session.Saved.Equal(newer) {
			t.Fatalf("preset %v: saved is not the newer state", preset)
		}
	}
}

// The state at the revision after our save's expected revision is our save,
// whatever its contents (the mixer refuses a save whose expected revision is
// not current). Its settings wait for the save's reply; if another write did
// take that revision, the mixer refuses ours, and the refresh that follows
// brings that write in as a conflict.
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
	echo.Config.Targets = core.CloneTargets(pending.Request.Targets)
	observed := failure(echo)
	mustReceive(t, &session, observed.Clone(), &pending, nil, false, false)
	if got := mustReceive(t, &session, echo.Clone(), &pending, nil, false, false).Update; got != core.RemoteUpdateStale {
		t.Fatalf("update %v", got)
	}
	assertRuntime(t, &session, &observed)
	unrelated := observed.Clone()
	unrelated.Config.Targets[2].Gain.DB = -3
	savedBefore := session.Saved.Config.Clone()
	if got := mustReceive(t, &session, unrelated.Clone(), &pending, nil, false, false).Update; got == core.RemoteUpdateConflict {
		t.Fatalf("update %v", got)
	}
	if !session.Saved.Config.Equal(savedBefore) {
		t.Fatal("settings taken before the save's reply")
	}
	// The mixer refused our save: the refresh brings the other write in.
	if got := mustReceive(t, &session, unrelated.Clone(), nil, nil, true, false).Update; got != core.RemoteUpdateConflict {
		t.Fatalf("refresh update %v", got)
	}
	if !session.Saved.Equal(unrelated) {
		t.Fatal("saved is not the unrelated state")
	}
	if got := session.Draft[1].Gain.DB; got != -6 {
		t.Fatalf("draft gain %v", got)
	}
}

// A save's echo and reply with a value the mixer stored one unit in the last
// place off (serde_json's default parser): the echo is ours, the reply is
// accepted, and the draft takes the stored value, so nothing is left to save.
func TestSaveStoredOneULPOffIsAcceptedAndLeavesNothingToSave(t *testing.T) {
	for _, echoFirst := range []bool{false, true} {
		session := core.DefaultSession()
		receiveSaved(t, &session, state())
		sent := 80.190455218073918
		session.Draft[1].Highpass.Frequency = sent
		pending, err := session.BeginApply()
		if err != nil {
			t.Fatal(err)
		}
		stored := state()
		stored.Config.Revision++
		stored.Config.Targets = core.CloneTargets(pending.Request.Targets)
		storedFrequency := math.Nextafter(sent, math.Inf(1))
		stored.Config.Targets[1].Highpass.Frequency = storedFrequency
		if echoFirst {
			if got := mustReceive(t, &session, stored.Clone(), &pending, nil, false, false).Update; got == core.RemoteUpdateConflict {
				t.Fatalf("echo first: echo taken as a conflict")
			}
		}
		if err := session.Acknowledge(pending, stored.Clone()); err != nil {
			t.Fatalf("echo first %v: %v", echoFirst, err)
		}
		if session.Conflict || session.Dirty() {
			t.Fatalf("echo first %v: conflict %v dirty %v", echoFirst, session.Conflict, session.Dirty())
		}
		if got := session.Draft[1].Highpass.Frequency; got != storedFrequency {
			t.Fatalf("echo first %v: draft frequency %v", echoFirst, got)
		}
		if !echoFirst {
			// The stream's copy after the reply changes nothing.
			if got := mustReceive(t, &session, stored.Clone(), nil, nil, false, false).Update; got == core.RemoteUpdateConflict {
				t.Fatal("echo after reply taken as a conflict")
			}
			if session.Dirty() {
				t.Fatal("dirty after the echo")
			}
		}
	}
}

func TestInvalidPresetEchoIsNotARuntimeObservation(t *testing.T) {
	old := state()
	session := core.DefaultSession()
	receiveSaved(t, &session, old.Clone())
	invalid := failure(old)
	invalid.Config.Revision++
	invalid.Config.Targets[1].ID = "unknown-mic"
	// Valid standalone, wrong connected catalog.
	if err := invalid.Validate(); err != nil {
		t.Fatal(err)
	}
	if _, err := Receive(&session, invalid, nil, new(uint64(1)), false, false); err == nil {
		t.Fatal("accepted")
	}
	if !session.Saved.Equal(old) {
		t.Fatal("saved changed")
	}
	if !core.TargetsEqual(session.Draft, old.Config.Targets) {
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
	event := mustReceive(t, &session, observed.Clone(), nil, nil, false, false)
	if !event.RuntimeObserved {
		t.Fatal("event not observed")
	}
	// The request's reply was captured before the failure event in this
	// same engine lifetime, but includes a later settings revision.
	snapshot := observed.Clone()
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
	if session.Saved.Engine != observed.Engine {
		t.Fatal("engine changed")
	}
	if got := session.Saved.Config.Revision; got != 2 {
		t.Fatalf("revision %d", got)
	}
	// A newer lifetime is a new observation even through the refresh path.
	newer := session.Saved.Clone()
	newer.Engine.Generation++
	refresh = mustReceive(t, &session, newer.Clone(), nil, nil, true, true)
	if !refresh.RuntimeObserved {
		t.Fatal("newer lifetime not observed")
	}
	if session.Saved.Engine != newer.Engine {
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
	echo.Config.Targets = core.CloneTargets(pending.Request.Targets)
	http := mustReceive(t, &session, echo.Clone(), &pending, nil, true, true)
	if http.Update != core.RemoteUpdateStale {
		t.Fatalf("update %v", http.Update)
	}
	if http.RuntimeObserved {
		t.Fatal("snapshot echo observed")
	}
	stream := mustReceive(t, &session, echo.Clone(), &pending, nil, false, false)
	if !stream.RuntimeObserved {
		t.Fatal("stream echo not observed")
	}
	observed := failure(echo)
	mustReceive(t, &session, observed.Clone(), &pending, nil, false, false)
	old := mustReceive(t, &session, echo.Clone(), &pending, nil, false, false)
	if old.Update != core.RemoteUpdateStale {
		t.Fatalf("update %v", old.Update)
	}
	if old.RuntimeObserved {
		t.Fatal("stale echo observed")
	}
	assertRuntime(t, &session, &observed)
}
