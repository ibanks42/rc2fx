package installflow

import (
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func taskMarks(m *Model) []Mark {
	var out []Mark
	for _, step := range m.TaskSteps() {
		out = append(out, step.Mark)
	}
	return out
}

func TestTheSidebarFollowsTheRunThroughEveryStage(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if got := m.Stage(); got != StageBefore {
		t.Fatalf("stage %v", got)
	}
	if got := taskMarks(m); !slices.Equal(got, []Mark{MarkNow, MarkToCome, MarkToCome, MarkToCome, MarkToCome, MarkToCome}) {
		t.Fatalf("marks %v", got)
	}
	m.Start()
	if got := m.Stage(); got != StageGet {
		t.Fatalf("selecting RC2FX is getting the firmware: %v", got)
	}
	m.Run.Index = slices.Index(Steps[:], StepFlash)
	if got := m.Stage(); got != StageUpdate {
		t.Fatalf("stage %v", got)
	}
	m.Run.Index = slices.Index(Steps[:], StepStart)
	m.Run.Flashed = true
	if got := m.Stage(); got != StageWait {
		t.Fatalf("stage %v", got)
	}
	m.SetService(ready())
	if got := m.Stage(); got != StageCheck {
		t.Fatalf("RC2FX answers, so its file check is next: %v", got)
	}
	m.Run.fail("damaged")
	m.imageCheck = imageCheck{kind: imageCheckAnswered, result: core.ImageCheck{Status: core.ImageStatusDamaged}}
	if got := taskMarks(m); !slices.Equal(got, []Mark{MarkDone, MarkDone, MarkDone, MarkDone, MarkDone, MarkFailed}) {
		t.Fatalf("marks %v", got)
	}
	m.Run, m.finished = nil, &Run{}
	if got := taskMarks(m); !slices.Equal(got, []Mark{MarkDone, MarkDone, MarkDone, MarkDone, MarkDone, MarkDone}) {
		t.Fatalf("finished marks %v", got)
	}
}

func TestRemovingNamesItsOwnDownload(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	m.SetStock(true)
	if got := m.TaskSteps()[StageGet].Title; got != "Get RØDE’s firmware" {
		t.Fatalf("title %q", got)
	}
}

// A request to switch the mixer off and on is part of waiting for it, and
// only its power may be unplugged.
func TestAPowerCycleIsPartOfTheWaitAndKeepsUSBConnected(t *testing.T) {
	m := setUp(status(gv1(false), ready(), true, false))
	m.Run = &Run{Index: slices.Index(Steps[:], StepStart), Serial: "GV1", Device: firmware.ModelProIi,
		BackedUp: true, Selected: true, Prepared: true, Flashed: true, Committed: true}
	m.ImageCheckRead(&core.ImageCheck{Status: core.ImageStatusRepaired, Repaired: []string{"a"}}, nil)
	if !m.PowerCycle() || m.Stage() != StageWait {
		t.Fatalf("power cycle %v, stage %v", m.PowerCycle(), m.Stage())
	}
	if u := m.Unplug(); u.Kind != UnplugPowerOnly {
		t.Fatalf("unplug %+v", u)
	}
}

func TestUnplugIsYesOnlyWhenKnownSafe(t *testing.T) {
	// Before anything started.
	m := setUp(status(gv1(false), absent(), false, false))
	if u := m.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("before: %+v", u)
	}
	// Not before the app has looked, nor with a mixer already in update
	// mode or still updating from a flash.
	m.Status = nil
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("unknown: %+v", u)
	}
	m = setUp(status(gv1(true), absent(), false, false))
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("update mode: %+v", u)
	}
	m = setUp(recentFlash(status(nil, absent(), false, false)))
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("recent flash, mixer not seen: %+v", u)
	}
	// A mixer back in normal mode is not updating, whatever was flashed.
	m = setUp(recentFlash(status(gv1(false), absent(), false, false)))
	if u := m.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("recent flash, mixer in normal mode: %+v", u)
	}
	// While running, even before the mixer is touched.
	m = setUp(status(gv1(false), absent(), false, false))
	m.Start()
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("running: %+v", u)
	}
	// Failed with nothing sent to the mixer, and it is there.
	m.Run.fail("offline")
	if u := m.Unplug(); u.Kind != UnplugYes || !strings.Contains(u.Reason, "nothing") {
		t.Fatalf("failed early: %+v", u)
	}
	// Failed after the flash while the mixer hasn't come back with RC2FX.
	m.Run.Flashed, m.Run.Committed = true, true
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("failed after the flash: %+v", u)
	}
	m.SetService(ready())
	if u := m.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("back and answering: %+v", u)
	}
	// Failed with the mixer in update mode.
	m.Status.USB[0].UpdateMode = true
	if u := m.Unplug(); u.Kind != UnplugNo {
		t.Fatalf("update mode: %+v", u)
	}
	// Finished.
	m.Run, m.finished = nil, &Run{}
	if u := m.Unplug(); u.Kind != UnplugYes {
		t.Fatalf("finished: %+v", u)
	}
}

func TestAFailedPageSaysOnlyWhatTheAppSees(t *testing.T) {
	m := setUp(status(gv1(false), absent(), true, false))
	fw := "1.7.7"
	m.Status.USB[0].Firmware = &fw
	m.Run = &Run{Index: slices.Index(Steps[:], StepFlash), Serial: "GV1", Device: firmware.ModelProIi,
		BackedUp: true, Selected: true, Prepared: true, status: RunStatus{Kind: RunFailed, Why: "drive busy"}}
	seen := m.SeenNow()
	var texts []string
	for _, s := range seen {
		texts = append(texts, s.Text)
	}
	want := []string{
		"Your mixer is on.",
		"It runs RØDE firmware 1.7.7, without RC2FX running.",
		"Your settings are saved on this computer.",
		"The firmware is downloaded.",
	}
	if !slices.Equal(texts, want) {
		t.Fatalf("seen %q", texts)
	}
	if m.FailureTitle() != "The update didn’t finish" {
		t.Fatalf("title %q", m.FailureTitle())
	}
	m.Privileged = false
	if last := m.SeenNow(); last[len(last)-1].Good || !strings.Contains(last[len(last)-1].Text, "access") {
		t.Fatalf("lost access isn't said: %+v", last)
	}
	m.Status.USB = nil
	if first := m.SeenNow()[0]; first.Good {
		t.Fatalf("a missing mixer is good: %+v", first)
	}
}

func TestTheStartButtonSaysWhyItCannotBePressed(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if why := m.StartBlocker(); why != "" {
		t.Fatalf("blocked: %q", why)
	}
	// Linux before access: the button asks for it, so it can be pressed.
	m.Privileged = false
	if why := m.StartBlocker(); why != "" || m.CanStart() || !m.CanStartOnceGranted() {
		t.Fatalf("blocked: %q", why)
	}
	m.Status.USB = append(m.Status.USB, installer.USBMixer{Model: firmware.ModelProIi, Serial: "GV2"})
	if why := m.StartBlocker(); !strings.Contains(why, "More than one") {
		t.Fatalf("two mixers: %q", why)
	}
	m = setUp(status(gv1(false), absent(), false, false))
	m.Offers = Offers{Kind: OffersChecking}
	m.Mixer = nil
	if why := m.StartBlocker(); why == "" {
		t.Fatal("not blocked while checking")
	}
	m = NewModel()
	if why := m.StartBlocker(); why == "" {
		t.Fatal("not blocked before looking")
	}
}

func TestTheRecommendedPairIsTheDefault(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if !m.Recommended() {
		t.Fatal("the default is not recommended")
	}
	m.ChooseMixer(*m.Mixer)
	if m.Recommended() {
		t.Fatal("a pick by hand is called recommended")
	}
}

func TestTryAgainNamesTheStageItStartsFrom(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	sel := m.Start()
	prepare := selected(t, m, sel)
	refresh := m.Finish(*prepare, installer.Reply{}, errors.New("offline"))
	report(t, m, refresh, status(gv1(false), absent(), false, false))
	if stage, ok := m.RetryStage(); !ok || stage != StageGet {
		t.Fatalf("retry stage %v %v", stage, ok)
	}
	if m.FailureTitle() != "Couldn’t download RØDE’s firmware" {
		t.Fatalf("title %q", m.FailureTitle())
	}
}

func TestElapsedIsMinutesAndSeconds(t *testing.T) {
	for d, want := range map[time.Duration]string{
		0:                              "0:00",
		3*time.Minute + 48*time.Second: "3:48",
		-time.Second:                   "0:00",
		72 * time.Minute:               "72:00",
	} {
		if got := Elapsed(d); got != want {
			t.Errorf("Elapsed(%v) = %q, want %q", d, got, want)
		}
	}
}
