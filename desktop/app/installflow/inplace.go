package installflow

import (
	"encoding/json"
	"fmt"
	"reflect"
	"time"
)

// In-place update state transitions after a verified bundle is handed to the
// app. Each method only acts in its expected stage, so late or duplicate
// callbacks are ignored.

const (
	inPlaceAskEvery = 3 * time.Second
	inPlaceGiveUp   = 6 * time.Minute
)

func (m *Model) InPlace() InPlace {
	return m.inPlace
}

// ShowInPlace overrides the in-place state for UI fixtures.
func (m *Model) ShowInPlace(p InPlace) {
	m.inPlace = p
}

// InPlaceSendFailed records a transport error while uploading the bundle.
func (m *Model) InPlaceSendFailed(why string) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceFailed,
		Error: fmt.Sprintf("Couldn't reach the mixer. Nothing changed on it. %s", why)}
}

func (m *Model) InPlaceProgress(sent, total uint64) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	// Drive uploads only report completion at the end; if we fall back to USB 1
	// control, progress restarts from zero and clears the drive flag.
	drive := m.inPlace.Drive && sent == total
	m.inPlace = InPlace{Kind: InPlaceUploading, Version: m.inPlace.Version, Sent: sent, Total: total, Drive: drive}
}

// InPlaceCopying marks the active upload as copying through the temporary update drive.
func (m *Model) InPlaceCopying(total uint64) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceUploading, Version: m.inPlace.Version, Total: total, Drive: true}
}

// InPlaceAccepted marks the upload accepted by the mixer and starts polling
// for the install outcome via InPlaceTick.
func (m *Model) InPlaceAccepted(now time.Time) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlaceAsked = now
	m.inPlace = InPlace{Kind: InPlaceInstalling, Version: m.inPlace.Version, Since: now}
}

// InPlaceRefused records that the mixer rejected the uploaded bundle.
func (m *Model) InPlaceRefused(message string) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceFailed,
		Error: fmt.Sprintf("The mixer didn't accept the update. Nothing changed on it. %s", message)}
}

// InPlaceTick drives polling while the mixer is installing. ask is true when
// the caller should query GET api/v1/update/last and pass the result to
// InPlaceOutcome; changed is true if the wait timed out.
func (m *Model) InPlaceTick(now time.Time, connected bool) (ask, changed bool) {
	if m.inPlace.Kind != InPlaceInstalling {
		return false, false
	}
	if now.Sub(m.inPlace.Since) >= inPlaceGiveUp {
		m.inPlace = InPlace{Kind: InPlaceFailed,
			Error: "The mixer didn't say how the update ended. Check the RC2FX version it reports; " +
				"if it didn't change, the previous version is still running."}
		return false, true
	}
	if connected && (m.inPlaceAsked.IsZero() || now.Sub(m.inPlaceAsked) >= inPlaceAskEvery) {
		m.inPlaceAsked = now
		return true, false
	}
	return false, false
}

// InPlaceDismiss resets a finished or failed in-place update back to idle.
func (m *Model) InPlaceDismiss() {
	if !m.inPlace.Active() {
		m.inPlace = InPlace{Kind: InPlaceIdle}
	}
}

// InPlaceOutcome processes the mixer's GET api/v1/update/last response. While
// uploading it records the pre-update baseline; while installing it ignores
// stale or unrelated records and returns true once the active update completes
// so the caller can refresh reported versions.
func (m *Model) InPlaceOutcome(record json.RawMessage) bool {
	switch m.inPlace.Kind {
	case InPlaceUploading:
		m.inPlaceBefore.capture(record)
		return false
	case InPlaceInstalling:
	default:
		return false
	}
	if m.inPlaceBefore.stale(record) {
		return false
	}
	version := m.inPlace.Version
	fields := decodeRecord(record)
	if fields == nil {
		return false
	}
	str := func(key string) (string, bool) {
		v, ok := fields[key].(string)
		return v, ok
	}
	if to, ok := str("to"); !ok || to != version {
		return false
	}
	outcome, _ := str("outcome")
	committed := outcome == "committed"
	reason, _ := str("reason")
	// Don't surface guiShimNeedsReboot to the user: the shim only launches RØDE's
	// screen app, so nothing visible waits on a full reboot.
	var detail string
	switch {
	case committed:
		detail = ""
	case reason != "":
		detail = fmt.Sprintf("The previous version was put back: %s", reason)
	default:
		detail = "The previous version was put back."
	}
	m.inPlace = InPlace{Kind: InPlaceDone, Version: version, Committed: committed, Detail: detail}
	return true
}

// inPlaceBaseline tracks the update/last record read right before uploading.
// Older mixer builds don't clear that record when starting a new install, and
// a retry after a rollback targets the same version, so we ignore matching
// records until we see the value change at least once.
type inPlaceBaseline struct {
	read    bool
	record  map[string]any
	changed bool
}

func decodeRecord(record json.RawMessage) map[string]any {
	var fields map[string]any
	if json.Unmarshal(record, &fields) != nil {
		return nil
	}
	return fields
}

func (b *inPlaceBaseline) capture(record json.RawMessage) {
	if !b.read {
		b.read, b.record = true, decodeRecord(record)
	}
}

func (b *inPlaceBaseline) stale(record json.RawMessage) bool {
	if !b.read || b.changed {
		return false
	}
	if reflect.DeepEqual(decodeRecord(record), b.record) {
		return true
	}
	b.changed = true
	return false
}
