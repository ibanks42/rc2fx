package installflow

// The guided task's decisions: the six steps the Install and Remove tasks
// show, how far a run is among them, where Try again starts, whether the
// mixer may be unplugged, and what the app sees after a failure. Package app
// draws them (guided.go, installer.go).

import (
	"fmt"
	"time"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Mark is how far one step of a guided task's sidebar is.
type Mark int

const (
	MarkDone Mark = iota
	MarkNow
	MarkFailed
	MarkToCome
)

// TaskStep is one step of a guided task's sidebar.
type TaskStep struct {
	Title string
	Mark  Mark
}

// Stage is one of the steps the Install and Remove tasks show. Each groups
// one or more steps of the plan (Steps).
type Stage int

const (
	StageBefore Stage = iota
	StageSave
	StageGet
	StageUpdate
	StageWait
	StageCheck
	// StageCount is the number of stages, and the stage of a finished run.
	StageCount
)

var installStages = [StageCount]string{
	"Before you start",
	"Save your settings",
	"Get the firmware",
	"Update the mixer",
	"Wait for the mixer",
	"Check and finish",
}

var removeStages = [StageCount]string{
	"Before you start",
	"Save your settings",
	"Get RØDE’s firmware",
	"Update the mixer",
	"Wait for the mixer",
	"Check and finish",
}

// StageTitle is a stage's name in the sidebar, for stock firmware or not.
func StageTitle(stage Stage, stock bool) string {
	if stage < 0 || stage >= StageCount {
		return ""
	}
	if stock {
		return removeStages[stage]
	}
	return installStages[stage]
}

// StageOf is the stage a step of the plan belongs to.
func StageOf(step Step) Stage {
	switch step {
	case StepBackup:
		return StageSave
	case StepMixer, StepPrepare:
		return StageGet
	case StepUSBMode, StepFlash:
		return StageUpdate
	}
	return StageWait
}

// Stage is how far the page is: StageBefore until a run starts, StageCount
// once it finished. While RC2FX checks the files the update installed, the
// run is at StageCheck; a request to switch the mixer off and on is part of
// waiting for it. The storage fix updates the mixer and waits for it twice,
// so it moves between those two stages.
func (m *Model) Stage() Stage {
	run := m.Run
	switch {
	case run == nil && m.Finished():
		return StageCount
	case run == nil:
		return StageBefore
	case run.Index < 0 || run.Index >= len(Steps):
		return StageCount
	}
	if m.FixRunning() {
		if FixWaiting(run.Fix.Event) {
			return StageWait
		}
		return StageUpdate
	}
	step := Steps[run.Index]
	if step == StepStart && !m.Stock && run.Flashed && !m.PowerCycle() {
		if service := m.Service(); service != nil && service.Kind == installer.ServiceReady {
			return StageCheck
		}
		if m.damaged() {
			return StageCheck
		}
	}
	return StageOf(step)
}

// PowerCycle reports a run waiting for the mixer to be switched off and on.
func (m *Model) PowerCycle() bool {
	run := m.Run
	return run != nil && run.status.Kind == RunWaiting && run.status.PowerCycle
}

// TaskSteps is the sidebar: every stage, marked done, now, failed or to
// come.
func (m *Model) TaskSteps() []TaskStep {
	at := m.Stage()
	failed := m.Failed()
	steps := make([]TaskStep, 0, StageCount)
	for stage := StageBefore; stage < StageCount; stage++ {
		mark := MarkToCome
		switch {
		case stage < at:
			mark = MarkDone
		case stage == at && failed:
			mark = MarkFailed
		case stage == at:
			mark = MarkNow
		}
		steps = append(steps, TaskStep{Title: StageTitle(stage, m.Stock), Mark: mark})
	}
	return steps
}

// RetryStage is the stage Try again starts from, judged from what the mixer
// reports now; ok false without a failed run.
func (m *Model) RetryStage() (Stage, bool) {
	step, ok := m.RetryFrom()
	if !ok {
		return 0, false
	}
	return StageOf(step), true
}

// UnplugKind is whether the mixer may be unplugged now.
type UnplugKind int

const (
	UnplugNo UnplugKind = iota
	UnplugYes
	// UnplugPowerOnly: its power cable, to switch it off and on; USB 1 stays.
	UnplugPowerOnly
)

// Unplug is the page's "Safe to unplug" note: whether, and why. Reason is
// "" when there is nothing to add.
type Unplug struct {
	Kind   UnplugKind
	Reason string
}

// Unplug says whether the mixer may be unplugged now. Only states known to
// be safe say yes: before anything has started, and after a run finished or
// failed with the mixer back up and answering. Anything uncertain says no.
func (m *Model) Unplug() Unplug {
	run := m.Run
	switch {
	case run == nil && m.Finished():
		return Unplug{Kind: UnplugYes}
	case run == nil:
		switch {
		case m.Status == nil:
			return Unplug{Reason: "the app is still checking the mixer"}
		case m.InUpdateMode():
			return Unplug{Reason: "the mixer is in update mode"}
		case m.recentlyFlashed():
			return Unplug{Reason: "the mixer may still be updating"}
		}
		return Unplug{Kind: UnplugYes, Reason: "nothing has changed yet"}
	case run.status.Kind == RunFailed:
		back := m.usbOne(false) != nil && (!run.Flashed || m.Stock || m.Answering())
		switch {
		case back && !run.Committed && !run.Flashed:
			return Unplug{Kind: UnplugYes, Reason: "nothing on the mixer has changed"}
		case back:
			return Unplug{Kind: UnplugYes, Reason: "the mixer is back to normal"}
		case m.InUpdateMode():
			return Unplug{Reason: "the mixer is in update mode"}
		case len(m.USB()) == 0:
			return Unplug{Reason: "the app can't see the mixer to check it"}
		}
		return Unplug{Reason: "the mixer may still be updating"}
	case m.PowerCycle():
		return Unplug{Kind: UnplugPowerOnly, Reason: "keep USB 1 connected"}
	case m.FixRunning() && !FixTouching(run.Fix.Event):
		return Unplug{Reason: "the install has started"}
	case run.Committed || run.Flashed:
		return Unplug{Reason: "the mixer is updating"}
	case m.Stock:
		return Unplug{Reason: "removing RC2FX has started"}
	}
	return Unplug{Reason: "the install has started"}
}

// SeenLine is one thing the app sees after a failure; Good when it is as it
// should be.
type SeenLine struct {
	Good bool
	Text string
}

// SeenNow is what the app sees now, for a failed run's page: the mixer, and
// what the run already has. Only what the app can observe.
func (m *Model) SeenNow() []SeenLine {
	var seen []SeenLine
	usb := m.USB()
	switch {
	case m.Status == nil:
	case len(usb) == 0:
		seen = append(seen, SeenLine{Text: "The app can't see your mixer on USB."})
	case len(usb) > 1:
		seen = append(seen, SeenLine{Text: "More than one mixer is connected."})
	case usb[0].UpdateMode:
		seen = append(seen, SeenLine{Text: "Your mixer is in update mode, so its audio is off."})
	default:
		seen = append(seen, SeenLine{Good: true, Text: "Your mixer is on."})
		running := "without RC2FX running"
		if m.Answering() {
			running = "with RC2FX running"
		}
		if v, ok := m.USBFirmware(); ok {
			seen = append(seen, SeenLine{Good: true, Text: fmt.Sprintf("It runs RØDE firmware %s, %s.", v, running)})
		} else if m.Answering() {
			seen = append(seen, SeenLine{Good: true, Text: "RC2FX is running on it."})
		} else {
			seen = append(seen, SeenLine{Good: true, Text: "RC2FX isn't running on it."})
		}
	}
	if run := m.Run; run != nil {
		if run.BackedUp {
			seen = append(seen, SeenLine{Good: true, Text: "Your settings are saved on this computer."})
		}
		if run.Prepared {
			seen = append(seen, SeenLine{Good: true, Text: "The firmware is downloaded."})
		}
	}
	if !m.Privileged {
		seen = append(seen, SeenLine{Text: "Administrator access has ended."})
	}
	return seen
}

// FailureTitle is the heading of a failed run's page, from the step that
// failed.
func (m *Model) FailureTitle() string {
	run := m.Run
	if run == nil || run.status.Kind != RunFailed || run.Index < 0 || run.Index >= len(Steps) {
		return ""
	}
	switch Steps[run.Index] {
	case StepBackup:
		return "Couldn’t save your settings"
	case StepMixer:
		return "Couldn’t get RC2FX"
	case StepPrepare:
		return "Couldn’t download RØDE’s firmware"
	case StepUSBMode, StepFlash:
		return "The update didn’t finish"
	}
	if m.damaged() {
		return "The update damaged some files"
	}
	return "The mixer didn’t finish in time"
}

// StartBlocker is why the start button can't be pressed yet, in words; ""
// when it can (or a request is merely in flight).
func (m *Model) StartBlocker() string {
	if m.Busy() || m.CanStartOnceGranted() {
		return ""
	}
	s := m.Status
	switch {
	case s == nil:
		return "Looking at your mixer…"
	case s.USBError != nil:
		return *s.USBError
	case len(s.USB) > 1:
		return "More than one mixer is connected. Connect only the one you want to use."
	case m.Device == firmware.ModelDuo && !m.AllowDuo:
		return "Switch on “Install anyway” for the Duo first."
	}
	if m.Stock {
		switch m.StockCheck.Kind {
		case StockCheckUnchecked, StockCheckChecking:
			return "Checking RØDE’s firmware list…"
		case StockCheckFailed:
			if m.StockCheck.Error == installer.Cancelled {
				return "The check of RØDE’s firmware list was cancelled."
			}
			return fmt.Sprintf("Couldn’t read RØDE’s firmware list. %s", m.StockCheck.Error)
		}
		return "RØDE lists no firmware for this mixer."
	}
	switch {
	case m.FirmwareCheck != FirmwareCheckDone:
		return "Checking which RØDE firmware RC2FX supports…"
	case m.Error != "":
		return m.Error
	case !m.supportedChoice(m.Choice):
		if note, ok := m.FirmwareNote(); ok {
			return note
		}
		return "No RØDE firmware that RC2FX supports was found."
	case m.Offers.Kind == OffersUnchecked || m.Offers.Kind == OffersChecking:
		return "Checking for RC2FX releases…"
	}
	if note, ok := m.MixerNote(); ok {
		return note
	}
	return "Choose the RC2FX version under “Choose versions”."
}

// Recommended reports that the versions chosen are the ones the page picks
// by itself: the default firmware and the newest RC2FX that goes with it.
func (m *Model) Recommended() bool {
	return !m.mixerChosen && sameChoicePtr(m.Choice, defaultChoice(m.Releases))
}

func sameChoicePtr(a, b *Choice) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// SettingsCarried reports that the firmware the run flashed carries the
// mixer's settings, so they were put back.
func (m *Model) SettingsCarried() bool {
	p := m.Prepared()
	return p != nil && p.Settings != ""
}

// Elapsed formats a duration as minutes and seconds, "3:48".
func Elapsed(d time.Duration) string {
	if d < 0 {
		d = 0
	}
	seconds := int(d / time.Second)
	return fmt.Sprintf("%d:%02d", seconds/60, seconds%60)
}
