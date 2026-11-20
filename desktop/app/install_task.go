package app

// The Install, Update and Remove tasks' pages, in the guided-task layout
// (guided.go): Before you start, one page per stage while the run goes on,
// a failure re-planned from what the mixer shows now, and the result, which
// stays until the person dismisses it. The decisions are in
// installflow/task.go; installer.go runs the requests.

import (
	"fmt"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// rodeUpdateBugURL is where "Learn more" about RØDE's update bug leads.
// A placeholder: to be replaced with the real write-up before release.
const rodeUpdateBugURL = "https://example.com/rodecaster-update-bug"

// taskScreen is the open task, in front of everything else.
func (a *App) taskScreen(cx *gk.Cx) gk.Element {
	i := a.installer
	if i.repair != nil {
		return a.guidedScreen(a.repairTask(i))
	}
	if i.fix != nil {
		return a.guidedScreen(a.fixTask(i))
	}
	if a.inPlaceShown(i) {
		return a.guidedScreen(a.inPlaceTask(i))
	}
	return a.guidedScreen(a.installTask(i, cx))
}

// taskTitle is the task's name, from what it was opened for.
func taskTitle(i *Installer) string {
	switch {
	case i.model.Stock:
		return "Remove RC2FX"
	case i.kind == taskUpdate:
		return "Update RC2FX"
	}
	return "Install RC2FX"
}

func (a *App) installTask(i *Installer, cx *gk.Cx) guidedTask {
	m := i.model
	stage := m.Stage()
	t := guidedTask{
		title:  taskTitle(i),
		count:  stepCount(int(stage), int(installflow.StageCount)),
		steps:  m.TaskSteps(),
		scroll: i.controlsScroll,
		unplug: m.Unplug(),
	}
	switch run := m.Run; {
	case run == nil && m.Finished():
		a.finishedPage(i, &t)
	case run == nil:
		a.beforePage(i, &t)
	case run.Status().Kind == installflow.RunFailed:
		a.failedPage(i, &t)
	default:
		a.runningPage(i, &t)
	}
	return t
}

// versionLine is what the run installs, as a sentence.
func versionLine(m *installflow.Model) string {
	choice := m.Chosen()
	if m.Stock {
		if choice == nil {
			return "RØDE’s stock firmware."
		}
		return fmt.Sprintf("RØDE’s stock firmware %s.", choice.Version)
	}
	firmware, rc2fx := "", ""
	if choice != nil {
		firmware = " " + choice.Version
	}
	mixer := m.Mixer
	if run := m.Run; run != nil && run.Mixer != nil {
		mixer = run.Mixer
	} else if last := m.LastRun(); last != nil && last.Mixer != nil {
		mixer = last.Mixer
	}
	if mixer != nil {
		rc2fx = " " + mixer.Version()
	}
	return fmt.Sprintf("RØDE firmware%s with RC2FX%s.", firmware, rc2fx)
}

// ---- Before you start ------------------------------------------------------

func (a *App) beforePage(i *Installer, t *guidedTask) {
	m := i.model
	linux := i.access.Kind != installflow.AccessNotNeeded
	asking := i.access.Kind == installflow.AccessAsking
	body := gk.VFlex(s.WFull).Gap(px(16)).Child(taskHeading("Before you start"))
	if m.Stock {
		body = body.Child(bullets(
			bullet(txt(strings.TrimSuffix(versionLine(m), ".")+" is put back on your mixer, without RC2FX.")),
			bullet(txt("The mixer’s audio is off while it updates, about 3 minutes, and the mixer restarts by itself.")),
			bullet(txt("Your mixer’s settings are saved first and put back afterwards.")),
			bullet(txt("Your RC2FX settings stay on this computer, so installing RC2FX again later brings them back.")),
			bullet(txt("Needs an internet connection: the app downloads the firmware from RØDE.")),
		))
	} else {
		// The storage fix runs only where the app can't ask the mixer for
		// its partition table (installflow.WantsStorageFix); the page
		// promises it only then.
		fix := m.WantsStorageFix()
		audio := "The mixer’s audio is off while it updates, about 3 minutes."
		if fix {
			audio = "The mixer’s audio is off while it updates: about 3 minutes, or about 10 the first time, because of the storage fix below."
		}
		points := []gk.Element{
			bullet(txt("RØDE’s firmware is reinstalled on your mixer, with RC2FX added to it.")),
			bullet(txt(audio)),
			bullet(txt("The mixer restarts by itself during the install.")),
			bullet(txt("Your mixer’s settings are saved first and put back afterwards.")),
			bullet(txt("You can undo it later with “Remove RC2FX”, which puts RØDE’s stock firmware back.")),
		}
		if fix {
			points = append(points, bullet(txt("The install also fixes a bug in RØDE’s firmware that can damage files during updates."),
				gk.Link("task-update-bug").Child(txt("What is this?")).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.explainUpdateBug(cx, true) })))
		}
		body = body.Child(bullets(points...))
	}
	for _, warning := range m.WarningsFor(m.Stock) {
		body = body.Child(seenRow(Warn, warning, true))
	}
	if m.DuoConnected() && !m.AllowDuo {
		body = body.Child(a.duoOptIn("task-allow-duo"))
	}
	if !m.Stock {
		body = body.Child(a.versionsDisclosure(i))
	}
	if why := m.StartBlocker(); why != "" {
		body = body.Child(a.blockerLine(i, why))
	}
	switch i.access.Kind {
	case installflow.AccessAsking:
		body = body.Child(spinnerLine("Answer your system’s password prompt to continue."))
	case installflow.AccessRefused:
		body = body.Child(seenRow(Warn, "Access wasn’t granted. "+strings.TrimSuffix(capitalFirst(i.access.Why), ".")+".", true))
	}
	if linux {
		what := "when the install starts"
		if m.Stock {
			what = "when removing starts"
		}
		body = body.Child(prose(fmt.Sprintf("Your system will ask for your password once, %s. The app uses it only to update the mixer.", what), Faint))
	}
	t.body = body
	t.secondary = []gk.ButtonEl{taskButton("task-not-now", "Not now", false).Disabled(i.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })}
	label := "Install RC2FX"
	switch {
	case m.Stock:
		label = "Remove RC2FX"
	case i.kind == taskUpdate:
		label = "Update RC2FX"
	}
	if !i.access.AllowsFlashing() {
		label += "…"
	}
	t.primary = taskButton("task-start", label, true).
		Disabled(i.busy() || asking || !m.CanStartOnceGranted()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startTask(cx) })
}

// versionsDisclosure is "Choose versions (advanced)": closed, it shows the
// pair a run would install; open, the two menus.
func (a *App) versionsDisclosure(i *Installer) gk.DivEl {
	m := i.model
	open := i.versionsOpen
	summary := ""
	if !open && m.Choice != nil && m.Mixer != nil {
		summary = fmt.Sprintf("RØDE firmware %s with RC2FX %s", m.Choice.Version, m.Mixer.Version())
		if m.Recommended() {
			summary += " — recommended"
		}
	}
	chevron := gk.IconChevronRight
	if open {
		chevron = gk.IconChevronDown
	}
	head := gk.HFlex(s.ItemsCenter, s.CursorPointer, s.TextSm).Gap(px(10)).Px(px(14)).Py(px(12)).
		TextColor(rgb(Muted)).Hover(hover().TextColor(rgb(Text))).
		Child(gk.Icon(chevron).Xsmall()).
		Child(txt("Choose versions (advanced)")).
		Child(gk.Div(s.Flex1, s.MinW0, s.Truncate, s.TextRight).TextColor(rgb(Text)).Child(txt(summary))).
		ID("task-versions").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if a.installer != nil {
				a.installer.versionsOpen = !a.installer.versionsOpen
			}
			a.notify(cx)
		})
	busy := m.Busy()
	noReleases := true
	for range m.ReleasesFor() {
		noReleases = false
		break
	}
	field := func(label string, menu gk.Element) gk.DivEl {
		return gk.VFlex(s.Flex1, s.MinW0).Gap(px(6)).
			Child(gk.Div().TextSize(px(12)).TextColor(rgb(Faint)).Child(txt(label))).
			Child(menu)
	}
	inner := gk.VFlex(s.Gap3).Px(px(14)).Pb(px(14)).Pt(px(2)).
		Child(gk.HFlex(s.ItemsStart).Gap(px(14)).
			Child(field("RØDE firmware", pickerSelect(i.firmwareSelect, "task-release", busy || noReleases))).
			Child(field("RC2FX", pickerSelect(i.mixerSelect, "task-mixer", busy || len(m.MixerMenu()) == 0)))).
		Child(prose("The recommended pair is the newest that has been tested together. Older RØDE firmware may lack fixes RØDE has made since.", Faint))
	return gk.VFlex(s.WFull, s.RoundedXl, s.Border1).Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line)).
		Child(head).
		Child(gk.Collapsible().Open(open).Content(inner))
}

// blockerLine is why the start button can't be pressed yet, with a way to
// look again where looking again can help.
func (a *App) blockerLine(i *Installer, why string) gk.DivEl {
	m := i.model
	if strings.HasSuffix(why, "…") {
		return spinnerLine(why)
	}
	var again func(m *installflow.Model) *installer.Request
	switch {
	case m.Stock && m.StockCheck.Kind == installflow.StockCheckFailed:
		again = (*installflow.Model).RecheckStock
	case !m.Stock && m.Offers.Kind == installflow.OffersFailed:
		again = (*installflow.Model).RecheckMixers
	case m.Status != nil && (m.Status.USBError != nil || len(m.Status.USB) > 1 || m.Error != ""):
		again = (*installflow.Model).Probe
	}
	line := gk.HFlex(s.ItemsCenter, s.FlexWrap, s.Gap3).Child(gk.Div(s.Flex1, s.MinW0).Child(prose(why, Warn)))
	if again != nil {
		line = line.Child(gk.Button("task-check-again").Ghost().Small().Label("Check again").
			Disabled(i.busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.installerEvent(cx, again) }))
	}
	return line
}

// duoOptIn is the Duo's one confirmation: RC2FX has not been tested on it.
// Remembered on this computer; the install button waits for it.
func (a *App) duoOptIn(id gk.ID) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Icon(gk.IconTriangleAlert).TextColor(rgb(Warn))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Text)).Child(txt("RC2FX is untested on the Duo. Install anyway."))).
		Child(gk.Switch(id).Checked(a.allowDuo).
			Disabled(a.installer != nil && (a.installer.busy() || a.installer.model.Run != nil)).
			OnClick(func(cx *gk.Cx, value bool) { a.setAllowDuo(cx, value) }))
}

// setAllowDuo saves the Duo opt-in and hands it to an open installer.
func (a *App) setAllowDuo(cx *gk.Cx, value bool) {
	if err := rememberDuo(value); err != nil {
		a.message = fmt.Sprintf("Couldn't save the Duo choice: %s", err)
		a.notify(cx)
		return
	}
	a.allowDuo = value
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.SetAllowDuo(value) })
	a.notify(cx)
}

// ---- While it runs -------------------------------------------------------------

func (a *App) runningPage(i *Installer, t *guidedTask) {
	m := i.model
	now := time.Now()
	soFar := prose(installflow.Elapsed(now.Sub(i.stageSince))+" so far", Faint)
	body := gk.VFlex(s.WFull).Gap(px(16))
	switch stage := m.Stage(); stage {
	case installflow.StageSave:
		line := "Reading your settings…"
		if m.Run.Serial == "" {
			line = "Connect the mixer to this computer’s USB, so its settings can be matched to it."
		}
		body = body.Child(taskHeading("Save your settings")).
			Child(prose("The app reads your mixer’s settings over USB and keeps a copy on this computer. They are put back with the new firmware.", Muted)).
			Child(spinnerLine(line)).
			Child(soFar)
	case installflow.StageGet:
		body = body.Child(taskHeading(installflow.StageTitle(stage, m.Stock))).
			Child(prose(versionLine(m), Muted)).
			Child(spinnerLine(fetchLine(m, i.progress))).
			Child(soFar)
	case installflow.StageUpdate:
		body = body.Child(taskHeading("Update the mixer")).
			Child(prose(versionLine(m), Muted))
		if m.FixRunning() {
			// The storage fix updates the mixer twice; the page says which.
			if line := fixPassLine(m.Run.Fix.Event, strings.TrimSuffix(versionLine(m), ".")); line != "" {
				body = body.Child(prose(line, Muted))
			}
			body = body.Child(fixUpdatePanel(m.Run.Fix.Event))
		} else {
			body = body.Child(updatePanel(m, i.progress))
		}
		body = body.
			Child(taskPanel().
				Child(eyebrow("Your mixer’s screen")).
				Child(prose("It says “Updating” while the firmware is sent, then “Installing”. Keep it switched on and connected.", Muted))).
			Child(soFar)
	case installflow.StageCheck:
		body = body.Child(taskHeading("Check and finish")).
			Child(prose("RC2FX is running. It checks the files the update installed; this takes a minute or two.", Muted)).
			Child(spinnerLine("Checking the files…")).
			Child(soFar)
	default:
		if m.PowerCycle() {
			body = body.Child(powerCycleBody(i))
		} else if m.FixRunning() {
			// Between the fix's two updates: the mixer restarts, then reports
			// the fix's result. The time counts from when this line began.
			event := m.Run.Fix.Event
			waiting := fixWaitLine(m)
			since := i.waitSince
			if since.IsZero() {
				since = i.stageSince
			}
			body = body.Child(taskHeading("Wait for the mixer")).
				Child(prose("The mixer restarts several times. Its screen goes dark between restarts.", Muted))
			if line := fixPassLine(event, strings.TrimSuffix(versionLine(m), ".")); line != "" {
				body = body.Child(prose(line, Muted))
			}
			body = body.Child(taskPanel().Child(gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.Gap3).
				Child(gk.Div(s.Flex1, s.MinW0).Child(spinnerLine(waiting))).
				Child(prose(installflow.Elapsed(now.Sub(since))+" so far", Faint))))
		} else {
			waiting := "Waiting for the mixer to start with RC2FX…"
			if m.Stock {
				waiting = "Waiting for the mixer to start…"
			}
			body = body.Child(taskHeading("Wait for the mixer")).
				Child(prose("The mixer restarts on its own. Its screen goes dark while it does.", Muted)).
				Child(taskPanel().Child(gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.Gap3).
					Child(spinnerLine(waiting)).
					Child(prose(installflow.Elapsed(now.Sub(i.stageSince))+" so far", Faint))))
		}
	}
	if m.TakingLonger(now) && !i.keptWaiting {
		body = body.Child(a.longerPanel(i))
	}
	t.body = body
	stop := "Stop"
	if m.Canceling() {
		stop = "Stopping…"
	}
	t.secondary = []gk.ButtonEl{taskButton("task-stop", stop, false).
		Disabled(!m.CanStop(now) || m.Canceling()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.stopTask(cx) })}
	t.primary = taskButton("task-done", "Done", true).Disabled(true)
}

// fetchLine is how getting the firmware is going: a download's size, never
// file names or checksums.
func fetchLine(m *installflow.Model, p *installer.Progress) string {
	if m.Run != nil && installflow.Steps[m.Run.Index] == installflow.StepMixer && !m.Stock {
		return "Getting RC2FX and checking its signature…"
	}
	if p != nil && p.Phase == "download" {
		if p.Total > 0 {
			return fmt.Sprintf("Downloading the firmware · %.0f of %.0f MB", float64(p.Completed)/1e6, float64(p.Total)/1e6)
		}
		return fmt.Sprintf("Downloading the firmware · %.0f MB", float64(p.Completed)/1e6)
	}
	return "Getting the firmware ready…"
}

// updatePanel is the two parts of updating the mixer: sending the firmware,
// which the app can measure, and the mixer installing it, which it can't.
func updatePanel(m *installflow.Model, p *installer.Progress) gk.DivEl {
	step := installflow.Steps[m.Run.Index]
	var fill *float32
	if step == installflow.StepFlash && m.Busy() && p != nil && p.Phase == "stage" {
		fill = installflow.Fraction(p)
	}
	share, value := "", float32(0)
	if fill != nil {
		value = *fill * 100
		share = fmt.Sprintf("%.0f%%", value)
	}
	sending := surface().Line
	if step == installflow.StepFlash {
		sending = Accent
	}
	panel := taskPanel().
		Child(gk.HFlex(s.ItemsCenter, s.Gap3, s.TextSm).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(8)).Bg(rgb(sending))).
			Child(gk.Div(s.Flex1, s.MinW0).Child(txt("1. Sending the firmware to the mixer"))).
			Child(gk.Div().TextColor(rgb(Faint)).Child(txt(share)))).
		Child(gk.Div().Mt(px(4)).Mb(px(12)).Child(gk.Progress("task-progress").Value(value))).
		Child(seenRow(surface().Line, "2. The mixer installs it, then restarts. This part has no progress to show.", false))
	if step == installflow.StepUSBMode || (step == installflow.StepFlash && !m.Busy()) {
		panel = panel.Child(gk.Div(s.Pt2).Child(spinnerLine("Putting the mixer in update mode…")))
	}
	return panel
}

// powerCycleBody is the request to switch the mixer off and on: the run
// waits for it, its time stops, and it goes on by itself.
func powerCycleBody(i *Installer) gk.DivEl {
	return gk.VFlex(s.WFull).Gap(px(16)).
		Child(taskHeading("Almost done. Turn the mixer off and on again.")).
		Child(prose("The new firmware is written. The mixer needs to be switched off and on once to finish.", Muted)).
		Child(taskPanel().
			Child(numbered(
				"Switch the mixer off with its power button, or unplug its power cable.",
				"Wait until the screen is dark, then switch it on again.",
				"Leave the USB cable connected.")).
			Child(gk.HFlex(s.ItemsCenter, s.Gap3, s.BorderT1).Mt(px(14)).Pt(px(12)).BorderColor(rgb(surface().Line)).
				Child(gk.Div(s.Flex1, s.MinW0).Child(spinnerLine("Waiting for the mixer to come back. The app continues by itself."))).
				Child(prose("Timer paused at "+installflow.Elapsed(i.pausedAt), Faint)))).
		Child(prose("If the mixer asks for its initial setup after it starts, don’t redo it: this restart brings it back.", Faint))
}

// longerPanel is a wait that has had no answer for a while: it says so,
// what can be done, and lets the run be stopped.
func (a *App) longerPanel(i *Installer) gk.DivEl {
	m := i.model
	atStart := installflow.Steps[m.Run.Index] == installflow.StepStart && !m.PowerCycle()
	panel := warnPanel().
		Child(gk.HFlex(s.ItemsCenter, s.Gap3).Mb(px(6)).
			Child(gk.Icon(gk.IconTriangleAlert).TextColor(rgb(Warn))).
			Child(gk.Div(s.FontSemibold).Child(txt("This is taking longer than usual"))))
	if i.waitHelp && atStart {
		panel = panel.Child(gk.Div(s.Mt2).Child(numbered(
			"If the mixer’s screen says “Installing”: keep waiting and leave everything connected.",
			"If it shows the normal mixer view: unplug USB 1 and plug it back in.",
			"If nothing has changed for 10 minutes: unplug the mixer’s power cable, wait 10 seconds, plug it in and switch it on. It starts the system it had before, which takes longer than a normal start. Then press “Try again”.")))
	} else if m.Service() == nil {
		panel = panel.Child(prose("The mixer has not answered yet. It may still be working.", Muted))
	} else {
		panel = panel.Child(prose("RC2FX answers, but the mixer hasn’t finished yet. It may still be working.", Muted))
	}
	buttons := gk.HFlex(s.ItemsCenter, s.Gap2).Mt(px(12)).
		Child(gk.Button("task-keep-waiting").Label("Keep waiting").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if a.installer != nil {
					a.installer.keptWaiting = true
				}
				a.notify(cx)
			}))
	if atStart && !i.waitHelp {
		buttons = buttons.Child(gk.Button("task-wait-help").Ghost().Label("What can I do?").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if a.installer != nil {
					a.installer.waitHelp = true
				}
				a.notify(cx)
			}))
	}
	return panel.Child(buttons)
}

// ---- After a failure -------------------------------------------------------------

func (a *App) failedPage(i *Installer, t *guidedTask) {
	m := i.model
	run := m.Run
	accessEnded := !i.access.AllowsFlashing()
	body := gk.VFlex(s.WFull).Gap(px(16))
	if accessEnded {
		when := "during the install"
		if !run.Committed && !run.Flashed {
			when = "before the mixer was put into update mode"
		}
		body = body.Child(taskHeading("Administrator access ended")).
			Child(prose(fmt.Sprintf("The part of the app that has administrator access stopped %s. This can happen if the computer was locked for a long time.", when), Muted))
	} else {
		body = body.Child(taskHeading(m.FailureTitle())).
			Child(prose(run.Status().Why, Muted))
	}
	seen := taskPanel().Child(eyebrow("What the app sees now"))
	for n, line := range m.SeenNow() {
		color := Warn
		if line.Good {
			color = Accent
		}
		seen = seen.Child(seenRow(color, line.Text, n == 0))
	}
	body = body.Child(seen)
	if from, ok := m.RetryStage(); ok {
		if installflow.StageOf(installflow.Steps[run.Index]) == installflow.StageUpdate && !run.Flashed && !accessEnded {
			body = body.Child(prose("Check that the cable is firmly in USB 1 before trying again.", Faint))
		}
		if m.FixStopped() {
			body = body.Child(prose("“Try again” installs RC2FX without the storage fix. The fix can be tried again later from the first screen.", Faint))
		}
		title := installflow.StageTitle(from, m.Stock)
		if accessEnded {
			body = body.Child(prose(fmt.Sprintf("“Try again” asks for your password, then starts from “%s”.", title), Faint))
		} else {
			body = body.Child(prose(fmt.Sprintf("“Try again” starts again from “%s”.", title), Faint))
		}
	}
	if i.access.Kind == installflow.AccessAsking {
		body = body.Child(spinnerLine("Answer your system’s password prompt to continue."))
	}
	t.body = body
	t.secondary = []gk.ButtonEl{taskButton("task-stop", "Stop", false).Disabled(i.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.stopTask(cx) })}
	label := "Try again"
	if accessEnded {
		label = "Try again…"
	}
	t.primary = taskButton("task-retry", label, true).
		Disabled(i.busy() || i.access.Kind == installflow.AccessAsking).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.retryTask(cx) })
}

// ---- The result ---------------------------------------------------------------

// micChoice is a microphone input the finished page offers to open.
type micChoice struct {
	index int
	name  string
}

// mics is up to four microphone inputs, in the mixer's order.
func (a *App) mics() []micChoice {
	var out []micChoice
	for index := range a.session.Draft {
		target := &a.session.Draft[index]
		if target.Kind != core.TargetKindMic {
			continue
		}
		out = append(out, micChoice{index: index, name: a.targetName(target)})
		if len(out) == 4 {
			break
		}
	}
	return out
}

func (a *App) finishedPage(i *Installer, t *guidedTask) {
	m := i.model
	device := m.Device.Label()
	settings := ""
	if m.SettingsCarried() {
		settings = " Your settings were put back."
	}
	body := gk.VFlex(s.WFull).Gap(px(16))
	if m.Stock {
		body = body.Child(doneHeading("RC2FX is removed")).
			Child(prose(fmt.Sprintf("Your %s runs %s%s", device, versionLine(m), settings), Muted)).
			Child(prose("Your RC2FX settings are kept on this computer. To use RC2FX again, open the app and press “Install RC2FX”.", Muted))
		t.body = body
		t.primary = taskButton("task-done", "Done", true).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, true) })
		return
	}
	title := "RC2FX is installed"
	if i.kind == taskUpdate {
		title = "RC2FX is updated"
	}
	body = body.Child(doneHeading(title)).
		Child(prose(fmt.Sprintf("Your %s runs %s%s", device, versionLine(m), settings), Muted))
	switch m.FixNote() {
	case installflow.FixNoteFixed:
		body = body.Child(prose("Your mixer’s storage was fixed too, so later updates can no longer damage files this way.", Muted))
	case installflow.FixNoteUnrecognized:
		body = body.Child(prose("The storage fix was not applied: your mixer’s storage is not laid out the way the app expects, so it was left as it was.", Muted))
	case installflow.FixNoteNotApplied:
		body = body.Child(prose("The storage fix could not be applied this time; it can be tried again later from the first screen.", Muted))
	}
	mics := a.mics()
	if len(mics) == 0 {
		t.body = body
		t.primary = taskButton("task-done", "Done", true).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, true) })
		return
	}
	chosen := min(max(i.mic, 0), len(mics)-1)
	row := gk.HFlex(s.ItemsCenter, s.FlexWrap).Gap(px(8))
	for n, mic := range mics {
		b := gk.Button(gk.Key("task-mic", n)).Label(mic.name)
		if n == chosen {
			b = b.Primary()
		}
		row = row.Child(b.OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if a.installer != nil {
				a.installer.mic = n
			}
			a.notify(cx)
		}))
	}
	body = body.Child(taskPanel().
		Child(eyebrow("Try it: which input is your microphone on?")).
		Child(row).
		Child(gk.Div(s.Mt2).Child(prose("The app opens that channel with noise suppression ready to switch on.", Faint))))
	t.body = body
	t.secondary = []gk.ButtonEl{taskButton("task-not-now", "Not now", false).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, true) })}
	mic := mics[chosen]
	t.primary = taskButton("task-open-mic", "Open "+mic.name, true).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.closeTask(cx, true)
			if a.installer != nil {
				return
			}
			a.effect = core.EffectSuppression
			a.openChannel(cx, mic.index)
			if !a.firstHintDone && mic.index < len(a.session.Draft) {
				a.firstHintTarget = a.session.Draft[mic.index].ID
			}
		})
}

// fixWaitLine is the waiting panel's line while an install's storage fix
// waits for the mixer; "" at any other time.
func fixWaitLine(m *installflow.Model) string {
	if !m.FixRunning() || m.Stage() != installflow.StageWait || m.PowerCycle() {
		return ""
	}
	if line := fixDetailLine(m.Run.Fix.Event); line != "" {
		return line
	}
	return "Waiting for the mixer…"
}
