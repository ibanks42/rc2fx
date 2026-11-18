package app

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

func inPlaceApp(version string) *App {
	model := installflow.NewModel()
	model.InPlace = installflow.InPlace{Kind: installflow.InPlaceUploading, Version: version}
	return &App{installer: &Installer{model: model}}
}

func installing(a *App) {
	m := a.installer.model
	m.InPlace = installflow.InPlace{Kind: installflow.InPlaceInstalling, Version: m.InPlace.Version, Since: time.Now()}
}

const rolledBack = `{"outcome":"rolledBack","from":"0.1.5","to":"0.2.0","reason":"audio did not start","guiShimNeedsReboot":false}`

func TestAnOlderRecordForTheSameVersionIsNotTheNewOutcome(t *testing.T) {
	a := inPlaceApp("0.2.0")
	// Read before the update was sent: a rollback of an earlier attempt.
	a.inPlaceOutcome(json.RawMessage(rolledBack))
	installing(a)
	// Mixer software that keeps the record reports it while installing.
	a.inPlaceOutcome(json.RawMessage(rolledBack))
	if got := a.installer.model.InPlace.Kind; got != installflow.InPlaceInstalling {
		t.Fatalf("the older record was taken as the outcome: %v", got)
	}
	a.inPlaceOutcome(json.RawMessage(`{"outcome":"committed","from":"0.1.5","to":"0.2.0","guiShimNeedsReboot":false}`))
	if in := a.installer.model.InPlace; in.Kind != installflow.InPlaceDone || !in.Committed {
		t.Fatalf("the new outcome was not taken: %+v", in)
	}
}

func TestTheSameRecordAfterTheMixerClearedItIsTheNewOutcome(t *testing.T) {
	a := inPlaceApp("0.2.0")
	a.inPlaceOutcome(json.RawMessage(rolledBack))
	installing(a)
	// The mixer forgot the old record on accepting the install ...
	a.inPlaceOutcome(nil)
	if got := a.installer.model.InPlace.Kind; got != installflow.InPlaceInstalling {
		t.Fatalf("no record is no outcome: %v", got)
	}
	// ... so the same rollback again is this update's.
	a.inPlaceOutcome(json.RawMessage(rolledBack))
	if in := a.installer.model.InPlace; in.Kind != installflow.InPlaceDone || in.Committed {
		t.Fatalf("the rollback was not taken: %+v", in)
	}
}

func TestWithoutABaselineAMatchingRecordIsTaken(t *testing.T) {
	a := inPlaceApp("0.2.0")
	installing(a)
	a.inPlaceOutcome(json.RawMessage(`{"outcome":"committed","to":"0.1.9"}`))
	if got := a.installer.model.InPlace.Kind; got != installflow.InPlaceInstalling {
		t.Fatalf("another version's record was taken: %v", got)
	}
	a.inPlaceOutcome(json.RawMessage(`{"outcome":"committed","to":"0.2.0"}`))
	if in := a.installer.model.InPlace; in.Kind != installflow.InPlaceDone || !in.Committed {
		t.Fatalf("the outcome was not taken: %+v", in)
	}
}
