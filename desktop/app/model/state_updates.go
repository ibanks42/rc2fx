package model

// Pending writes defer their settings acknowledgement, never their live
// status.

import (
	"math"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// ReceivedState is what one received device state did to the session.
type ReceivedState struct {
	Update core.RemoteUpdate
	// RuntimeObserved is true only when the accepted message supplied the
	// runtime status. Loading settings while preserving a stream observation
	// cannot renew it.
	RuntimeObserved bool
}

// Receive applies state to session. pendingApply is the write in flight, if
// any; pendingPresetRevision the revision a preset load in flight expects,
// if any. snapshot marks a request's reply rather than a stream event;
// preserveSnapshotRuntime keeps the stream's runtime observation.
func Receive(
	session *core.Session,
	state core.DeviceState,
	pendingApply *core.PendingApply,
	pendingPresetRevision *uint64,
	snapshot bool,
	preserveSnapshotRuntime bool,
) (ReceivedState, error) {
	if err := state.Validate(); err != nil {
		return ReceivedState{}, err
	}
	saved := session.Saved
	sameGeneration := saved != nil && saved.Engine.Generation == state.Engine.Generation
	ownEcho := CheckEcho(session, state, pendingApply, pendingPresetRevision).Own()
	var (
		update          core.RemoteUpdate
		runtimeObserved bool
		err             error
	)
	switch {
	case ownEcho && snapshot:
		// A request's read has no ordering against the live stream. Defer this
		// write's settings to its ACK without overwriting observed runtime.
		// Stale also tells the UI not to refresh its observation timestamp.
		update, runtimeObserved = core.RemoteUpdateStale, false
	case ownEcho:
		update, err = session.ReceiveRuntime(state)
		runtimeObserved = true
	case preserveSnapshotRuntime:
		update, err = session.ReceiveSnapshot(state)
		runtimeObserved = !sameGeneration
	default:
		// In particular, another controller's later revision is not our echo,
		// even if it happens to contain the same effects. Keep normal conflict
		// and stale-ACK checks active while a write is pending.
		update, err = session.Receive(state)
		runtimeObserved = true
	}
	if err != nil {
		return ReceivedState{}, err
	}
	return ReceivedState{
		Update:          update,
		RuntimeObserved: runtimeObserved && update != core.RemoteUpdateStale,
	}, nil
}

// EchoCheck is Receive's test of whether a state is the echo of our own
// write in flight, part by part (for the save log).
//
// A write is identified by its revision, not its contents: the mixer refuses
// a save or preset load whose expected revision is not current, so while ours
// (expecting R) is in flight, revision R+1 can only be ours. Its stored values
// may differ from what we sent (a mixer before 0.2.2 parses some numbers one
// unit in the last place off); the write's acknowledgement takes them.
type EchoCheck struct {
	// SelectionUnchanged: the state keeps the saved selected targets.
	SelectionUnchanged bool
	// ApplyPending, ApplyFollows: a save is in flight and the state's
	// revision is the one after the save's expected revision.
	ApplyPending, ApplyFollows bool
	// ApplySameTargets: the state's targets equal the save's. Not part of
	// the test; the save log shows it.
	ApplySameTargets bool
	// PresetPending, PresetFollows: a preset load is in flight and the
	// state's revision is the one after the revision it expects.
	PresetPending, PresetFollows bool
}

// Own is whether Receive takes the state as our own echo.
func (c EchoCheck) Own() bool {
	return c.SelectionUnchanged &&
		((c.ApplyPending && c.ApplyFollows) || (c.PresetPending && c.PresetFollows))
}

// CheckEcho runs Receive's own-echo test on state.
func CheckEcho(session *core.Session, state core.DeviceState, pendingApply *core.PendingApply,
	pendingPresetRevision *uint64) EchoCheck {
	saved := session.Saved
	follows := func(revision uint64) bool {
		return revision != math.MaxUint64 && revision+1 == state.Config.Revision
	}
	c := EchoCheck{
		SelectionUnchanged: saved != nil && slices.Equal(saved.Config.SelectedTargets, state.Config.SelectedTargets),
		ApplyPending:       pendingApply != nil,
		PresetPending:      pendingPresetRevision != nil,
	}
	if pendingApply != nil {
		c.ApplyFollows = follows(pendingApply.Request.ExpectedRevision)
		c.ApplySameTargets = core.TargetsEqual(pendingApply.Request.Targets, state.Config.Targets)
	}
	if pendingPresetRevision != nil {
		c.PresetFollows = follows(*pendingPresetRevision)
	}
	return c
}
