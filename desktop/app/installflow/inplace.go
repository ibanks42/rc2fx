package installflow

import (
	"encoding/json"
	"fmt"
	"reflect"
	"time"
)

// The in-place update after the verified release is with the app: the app
// sends it to the mixer and hands over what the mixer says; these methods
// decide what that means. Each only acts in the stage it belongs to, so a
// late or repeated message changes nothing.

const (
	// inPlaceAskEvery is how often the mixer is asked how the update ended
	// while it installs.
	inPlaceAskEvery = 3 * time.Second
	// inPlaceGiveUp is how long the mixer may take to say how the update
	// ended.
	inPlaceGiveUp = 6 * time.Minute
)

// InPlace is where the in-place update stands.
func (m *Model) InPlace() InPlace {
	return m.inPlace
}

// ShowInPlace sets where the in-place update stands, for pages made from
// made-up state (the app's flow fixtures).
func (m *Model) ShowInPlace(p InPlace) {
	m.inPlace = p
}

// InPlaceSendFailed: the app could not send the release it took with
// TakeInPlaceBundle to the mixer, for why.
func (m *Model) InPlaceSendFailed(why string) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceFailed,
		Error: fmt.Sprintf("Couldn't reach the mixer. Nothing changed on it. %s", why)}
}

// InPlaceProgress: sent of total bytes are with the mixer.
func (m *Model) InPlaceProgress(sent, total uint64) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceUploading, Version: m.inPlace.Version, Sent: sent, Total: total}
}

// InPlaceAccepted: the mixer accepted the release at now and installs it,
// restarting its audio. It is asked how that ended from now on (InPlaceTick).
func (m *Model) InPlaceAccepted(now time.Time) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlaceAsked = now
	m.inPlace = InPlace{Kind: InPlaceInstalling, Version: m.inPlace.Version, Since: now}
}

// InPlaceRefused: the mixer did not accept the release, saying message.
func (m *Model) InPlaceRefused(message string) {
	if m.inPlace.Kind != InPlaceUploading {
		return
	}
	m.inPlace = InPlace{Kind: InPlaceFailed,
		Error: fmt.Sprintf("The mixer didn't accept the update. Nothing changed on it. %s", message)}
}

// InPlaceTick is the poll while the mixer installs: ask is true when the
// app should ask the mixer how the update ended (`GET api/v1/update/last`,
// answered through InPlaceOutcome), which needs its connection; changed is
// true when the update gave up waiting.
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

// InPlaceDismiss goes back to no in-place update once one has ended.
func (m *Model) InPlaceDismiss() {
	if !m.inPlace.Active() {
		m.inPlace = InPlace{Kind: InPlaceIdle}
	}
}

// InPlaceOutcome hands over the mixer's record of its last in-place update
// (JSON null is None). Only the outcome of the update in progress counts;
// anything else is an older record. True when it ended the update, so the
// app should read the versions the mixer now runs.
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
	reboot, _ := fields["guiShimNeedsReboot"].(bool)
	var detail string
	switch {
	case committed && reboot:
		detail = "Part of the update applies the next time the mixer restarts."
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

// inPlaceBaseline is the mixer's update/last record as it was just before
// an in-place update was sent. Mixer software that does not clear the record
// when it accepts an install keeps reporting it while the new update runs,
// and it can name the same version (a retry after a rollback), so the same
// record is not taken as the new outcome until the record has been seen to
// change.
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

// capture keeps the first record read before the update, if none was kept.
func (b *inPlaceBaseline) capture(record json.RawMessage) {
	if !b.read {
		b.read, b.record = true, decodeRecord(record)
	}
}

// stale reports that record is still the one from before the update.
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
