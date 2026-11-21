package installflow

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

func uploadingInPlace(version string) *Model {
	m := NewModel()
	m.inPlace = InPlace{Kind: InPlaceUploading, Version: version}
	return m
}

const rolledBack = `{"outcome":"rolledBack","from":"0.1.5","to":"0.2.0","reason":"audio did not start","guiShimNeedsReboot":false}`

func TestAnOlderRecordForTheSameVersionIsNotTheNewOutcome(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	// Pre-upload baseline from an earlier rolled-back attempt.
	m.InPlaceOutcome(json.RawMessage(rolledBack))
	m.InPlaceAccepted(time.Now())
	// Mixer keeps returning the old record while installing.
	if m.InPlaceOutcome(json.RawMessage(rolledBack)) || m.InPlace().Kind != InPlaceInstalling {
		t.Fatalf("the older record was taken as the outcome: %+v", m.InPlace())
	}
	if !m.InPlaceOutcome(json.RawMessage(`{"outcome":"committed","from":"0.1.5","to":"0.2.0","guiShimNeedsReboot":false}`)) {
		t.Fatal("the new outcome did not end the update")
	}
	if in := m.InPlace(); in.Kind != InPlaceDone || !in.Committed {
		t.Fatalf("the new outcome was not taken: %+v", in)
	}
}

func TestTheSameRecordAfterTheMixerClearedItIsTheNewOutcome(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	m.InPlaceOutcome(json.RawMessage(rolledBack))
	m.InPlaceAccepted(time.Now())
	// The mixer forgot the old record on accepting the install ...
	m.InPlaceOutcome(nil)
	if got := m.InPlace().Kind; got != InPlaceInstalling {
		t.Fatalf("no record is no outcome: %v", got)
	}
	// ... so the same rollback again is this update's.
	m.InPlaceOutcome(json.RawMessage(rolledBack))
	if in := m.InPlace(); in.Kind != InPlaceDone || in.Committed || !strings.Contains(in.Detail, "audio did not start") {
		t.Fatalf("the rollback was not taken: %+v", in)
	}
}

func TestWithoutABaselineAMatchingRecordIsTaken(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	m.InPlaceAccepted(time.Now())
	m.InPlaceOutcome(json.RawMessage(`{"outcome":"committed","to":"0.1.9"}`))
	if got := m.InPlace().Kind; got != InPlaceInstalling {
		t.Fatalf("another version's record was taken: %v", got)
	}
	m.InPlaceOutcome(json.RawMessage(`{"outcome":"committed","to":"0.2.0"}`))
	if in := m.InPlace(); in.Kind != InPlaceDone || !in.Committed {
		t.Fatalf("the outcome was not taken: %+v", in)
	}
}

func TestEachNewUpdateReadsItsOwnBaseline(t *testing.T) {
	m := NewModel()
	m.inPlace = InPlace{Kind: InPlaceReady, Bundle: inPlaceBundle("0.2.0")}
	m.inPlaceBefore = inPlaceBaseline{read: true, record: decodeRecord(json.RawMessage(`{"to":"0.1.0"}`))}
	if _, ok := m.TakeInPlaceBundle(); !ok {
		t.Fatal("no bundle")
	}
	// The record before this update is the rollback, not the earlier one.
	m.InPlaceOutcome(json.RawMessage(rolledBack))
	m.InPlaceAccepted(time.Now())
	if m.InPlaceOutcome(json.RawMessage(rolledBack)) {
		t.Fatal("the record from before this update was taken")
	}
}

func TestTheMixerReportsTheUploadThenTheInstall(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	m.InPlaceProgress(3, 7)
	if in := m.InPlace(); !reflect.DeepEqual(in, InPlace{Kind: InPlaceUploading, Version: "0.2.0", Sent: 3, Total: 7}) {
		t.Fatalf("progress %+v", in)
	}
	at := time.Now()
	m.InPlaceAccepted(at)
	if in := m.InPlace(); !reflect.DeepEqual(in, InPlace{Kind: InPlaceInstalling, Version: "0.2.0", Since: at}) {
		t.Fatalf("installing %+v", in)
	}
	// Late upload messages change nothing once the mixer installs.
	m.InPlaceProgress(7, 7)
	m.InPlaceRefused("too late")
	m.InPlaceSendFailed("too late")
	if m.InPlace().Kind != InPlaceInstalling {
		t.Fatalf("a late message changed the stage: %+v", m.InPlace())
	}
	// Nothing to dismiss while it is under way.
	m.InPlaceDismiss()
	if m.InPlace().Kind != InPlaceInstalling {
		t.Fatal("dismissed while installing")
	}
}

func TestARefusedOrUnsentUpdateFailsAndCanBeDismissed(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	m.InPlaceRefused("bad signature")
	if in := m.InPlace(); in.Kind != InPlaceFailed || !strings.Contains(in.Error, "bad signature") ||
		!strings.Contains(in.Error, "Nothing changed") {
		t.Fatalf("refused %+v", in)
	}
	m.InPlaceDismiss()
	if !reflect.DeepEqual(m.InPlace(), InPlace{Kind: InPlaceIdle}) {
		t.Fatalf("dismissed %+v", m.InPlace())
	}

	m = uploadingInPlace("0.2.0")
	m.InPlaceSendFailed("not connected")
	if in := m.InPlace(); in.Kind != InPlaceFailed || !strings.Contains(in.Error, "Couldn't reach the mixer") ||
		!strings.Contains(in.Error, "not connected") {
		t.Fatalf("unsent %+v", in)
	}
	// Messages for an update that is over are ignored.
	m.InPlaceAccepted(time.Now())
	if m.InPlace().Kind != InPlaceFailed {
		t.Fatal("a failed update went on")
	}
}

func TestTheMixerIsAskedHowItEndedEveryFewSecondsUntilItGivesUp(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	if ask, changed := m.InPlaceTick(time.Now(), true); ask || changed {
		t.Fatal("asked before the mixer installs")
	}
	start := time.Now()
	m.InPlaceAccepted(start)
	if ask, _ := m.InPlaceTick(start.Add(time.Second), true); ask {
		t.Fatal("asked again within the interval")
	}
	if ask, _ := m.InPlaceTick(start.Add(inPlaceAskEvery), false); ask {
		t.Fatal("asked without a connection")
	}
	if ask, changed := m.InPlaceTick(start.Add(inPlaceAskEvery), true); !ask || changed {
		t.Fatal("not asked after the interval")
	}
	if ask, _ := m.InPlaceTick(start.Add(inPlaceAskEvery+time.Second), true); ask {
		t.Fatal("asked again within the interval")
	}
	ask, changed := m.InPlaceTick(start.Add(inPlaceGiveUp), true)
	if ask || !changed {
		t.Fatalf("ask %v, changed %v at the limit", ask, changed)
	}
	if in := m.InPlace(); in.Kind != InPlaceFailed || !strings.Contains(in.Error, "didn't say how the update ended") {
		t.Fatalf("gave up %+v", in)
	}
	if ask, changed := m.InPlaceTick(start.Add(2*inPlaceGiveUp), true); ask || changed {
		t.Fatal("ticked after giving up")
	}
}

func inPlaceBundle(version string) installer.InPlaceBundle {
	return installer.InPlaceBundle{Version: version, Size: 7}
}

func TestACopyThroughTheUpdateDriveIsShownUntilItFallsBackToUSB(t *testing.T) {
	m := uploadingInPlace("0.2.0")
	m.InPlaceCopying(7)
	if in := m.InPlace(); !in.Drive || in.Total != 7 || in.Kind != InPlaceUploading {
		t.Fatalf("%+v", in)
	}
	// Completing the drive copy keeps the drive flag set.
	m.InPlaceProgress(7, 7)
	if !m.InPlace().Drive {
		t.Fatal("drive forgotten at the end of its copy")
	}
	// Falling back to USB 1 resets progress to zero and clears the drive flag.
	m.InPlaceCopying(7)
	m.InPlaceProgress(0, 7)
	if m.InPlace().Drive {
		t.Fatal("still shown as the drive")
	}
}
