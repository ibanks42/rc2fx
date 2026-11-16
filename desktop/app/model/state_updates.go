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
	selectionUnchanged := saved != nil && slices.Equal(saved.Config.SelectedTargets, state.Config.SelectedTargets)
	follows := func(revision uint64) bool {
		return revision != math.MaxUint64 && revision+1 == state.Config.Revision
	}
	ownEcho := selectionUnchanged &&
		((pendingApply != nil &&
			follows(pendingApply.Request.ExpectedRevision) &&
			equal(pendingApply.Request.Targets, state.Config.Targets)) ||
			(pendingPresetRevision != nil && follows(*pendingPresetRevision)))
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
