package model

import (
	"math"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// ReceivedState is the result of applying a device state message to a session.
type ReceivedState struct {
	Update core.RemoteUpdate
	// True only when this message actually updated runtime status (loading settings
	// while preserving an existing stream observation does not renew it).
	RuntimeObserved bool
}

// Receive applies state to session while coordinating with any in-flight save
// (pendingApply) or preset load (pendingPresetRevision). Set snapshot=true for
// request replies (unordered against the event stream) and preserveSnapshotRuntime
// to keep the stream's runtime status across a settings refresh.
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
		// Request replies aren't ordered against the live stream, so defer settings
		// to the write ACK without touching observed runtime or freshness timestamps.
		update, runtimeObserved = core.RemoteUpdateStale, false
	case ownEcho:
		update, err = session.ReceiveRuntime(state)
		runtimeObserved = true
	case preserveSnapshotRuntime:
		update, err = session.ReceiveSnapshot(state)
		runtimeObserved = !sameGeneration
	default:
		// A later revision from another controller isn't our echo even with identical
		// effects, so keep normal conflict and stale-ACK handling active.
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

// EchoCheck breaks down whether an incoming state is the echo of our own
// in-flight write (also logged for save diagnostics).
//
// Matching is by revision, not payload: the mixer rejects writes whose expected
// revision is stale, so while our write expecting R is in flight, R+1 is ours.
// Stored float values can also differ by 1 ULP on mixers before 0.2.2; the write
// ACK reconciles them.
type EchoCheck struct {
	SelectionUnchanged         bool
	ApplyPending, ApplyFollows bool
	// Recorded for save-log diagnostics only; not used by Own.
	ApplySameTargets             bool
	PresetPending, PresetFollows bool
}

// Own reports whether the incoming state should be treated as our own write echo.
func (c EchoCheck) Own() bool {
	return c.SelectionUnchanged &&
		((c.ApplyPending && c.ApplyFollows) || (c.PresetPending && c.PresetFollows))
}

// CheckEcho evaluates whether state matches an in-flight apply or preset load.
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
