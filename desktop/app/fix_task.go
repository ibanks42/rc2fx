package app

// Standalone guided flow for fixing RØDE's storage layout bug without
// installing RC2FX (state machine in installflow/fixtask.go).

import (
	"fmt"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

func (a *App) openFix(cx *gk.Cx) {
	a.openTask(cx, taskFix)
	if i := a.installer; i != nil {
		i.fix = installflow.NewFix()
	}
	a.notify(cx)
}

func (a *App) startFix(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.fix == nil || i.busy() {
		return
	}
	if !i.access.AllowsFlashing() {
		i.fixOnGrant = true
		a.grantInstallerAccess(cx)
		return
	}
	request, ok := i.fix.Start(installflow.FixSeenOf(i.model) == installflow.FixSeenNormal)
	if !ok || i.fixture {
		return
	}
	i.progress = nil
	i.stageSince = time.Now()
	request.FixDebug = fixDebug()
	if !i.runner.send(request) {
		i.fix.NotSent()
	}
	a.notify(cx)
}

func fixOutcome(i *Installer, reply installer.Reply, err error) {
	i.fix.Outcome(reply, err)
	i.progress = nil
	// Mark the flash settled once the mixer is back so the landing screen doesn't
	// think an update is still in progress.
	switch i.fix.Ending() {
	case installflow.FixEndingFixed, installflow.FixEndingAlready,
		installflow.FixEndingUnrecognized, installflow.FixEndingNotApplied:
		now := uint64(max(time.Now().Unix(), 0))
		i.model.SettledFlash = &now
	}
}

func fixVersion(i *Installer) string {
	if r := i.fix.Reply; r != nil && r.Firmware != "" {
		return r.Firmware
	}
	if v, ok := i.model.USBFirmware(); ok {
		return v
	}
	return ""
}

func withVersion(version string) string {
	if version == "" {
		return "RØDE’s firmware"
	}
	return "RØDE’s firmware " + version
}

func (a *App) fixTask(i *Installer) guidedTask {
	f := i.fix
	t := guidedTask{
		title:  "Fix RØDE’s update bug",
		count:  stepCount(f.Step(), installflow.FixStepCount),
		steps:  f.Steps(),
		scroll: i.controlsScroll,
		unplug: f.Unplug(installflow.FixSeenOf(i.model)),
	}
	switch {
	case f.Kind == installflow.FixBefore:
		a.fixBeforePage(i, &t)
	case f.Kind == installflow.FixRunning:
		a.fixRunningPage(i, &t)
	default:
		a.fixEndedPage(i, &t)
	}
	return t
}

// ---- Before you start (G1) ----------------------------------------------------

func (a *App) fixBeforePage(i *Installer, t *guidedTask) {
	linux := i.access.Kind != installflow.AccessNotNeeded
	asking := i.access.Kind == installflow.AccessAsking
	seen := installflow.FixSeenOf(i.model)
	body := gk.VFlex(s.WFull).Gap(px(16)).Child(taskHeading("Before you start")).
		Child(bullets(
			bullet(txt("This fixes a bug in RØDE’s firmware that can damage files during updates."),
				gk.Link("task-update-bug").Child(txt("What is this?")).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.explainUpdateBug(cx, false) })),
			bullet(txt(fmt.Sprintf("RC2FX is not installed. Your mixer keeps %s, exactly as RØDE ships it.", withVersion(fixVersion(i))))),
			bullet(txt("The mixer’s audio is off for about 10 minutes, and the mixer restarts by itself several times.")),
			bullet(txt("Your mixer’s settings are kept.")),
			bullet(txt("The fix is permanent: it stays through later updates, whether from RØDE’s app or this one.")),
			bullet(txt("Needs an internet connection: the app downloads the firmware from RØDE.")),
		))
	switch seen {
	case installflow.FixSeenUnknown:
		body = body.Child(a.spinnerLine("Looking at your mixer…"))
	case installflow.FixSeenNone:
		body = body.Child(seenRow(Warn, "The app can't see your mixer on USB. Connect its USB 1 port to this computer.", true))
	case installflow.FixSeenTwo:
		body = body.Child(seenRow(Warn, "More than one mixer is connected. Connect only the one to fix.", true))
	case installflow.FixSeenUpdateMode:
		body = body.Child(seenRow(Warn, "The mixer is in update mode. Disconnect its power for 30 seconds, then turn it on again.", true))
	}
	switch i.access.Kind {
	case installflow.AccessAsking:
		body = body.Child(a.spinnerLine("Answer your system’s password prompt to continue."))
	case installflow.AccessRefused:
		body = body.Child(seenRow(Warn, "Access wasn’t granted. "+strings.TrimSuffix(capitalFirst(i.access.Why), ".")+".", true))
	}
	if linux {
		body = body.Child(prose("Your system will ask for your password once, when the fix starts. The app uses it only to update the mixer.", Faint))
	}
	t.body = body
	t.secondary = []gk.ButtonEl{taskButton("task-not-now", "Not now", false).Disabled(i.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })}
	label := "Fix the update bug"
	if !i.access.AllowsFlashing() {
		label += "…"
	}
	t.primary = taskButton("task-start", label, true).
		Disabled(i.busy() || asking || seen != installflow.FixSeenNormal).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startFix(cx) })
}

func (a *App) explainUpdateBug(cx *gk.Cx, install bool) {
	what := "The fix makes that area larger, which stops it from happening again."
	if install {
		what = "During the install the app makes that area larger, which stops it from happening again. It needs no action from you, keeps your settings, and stays fixed if you later remove RC2FX."
	}
	a.confirm(cx, confirmKindOther, Confirm{
		Title: "A bug in RØDE’s firmware",
		Detail: "This is a bug in RØDE’s own firmware, not in RC2FX. It can affect every second firmware update, including the ones RØDE’s app installs.\n\n" +
			"The mixer keeps its system on a small storage area that is laid out in a way that can damage files during an update. Most of the time nothing happens; sometimes the mixer forgets its setup or does not start afterwards.\n\n" +
			what,
		Link:      rodeUpdateBugURL,
		LinkLabel: "Learn more",
		Action:    "Close",
	}, func(*gk.Cx) {}, func(*gk.Cx) {})
}

func fixPassLine(p *installer.Progress, final string) string {
	pass, attempt := installflow.FixPass(p)
	switch {
	case pass == 1 && attempt > 1:
		return fmt.Sprintf("Update 1 of 2: fixing the update bug. The first try didn’t finish, so the app is trying again (try %d of %d).", attempt, autofix.MaxFlashes-1)
	case pass == 1:
		return "Update 1 of 2: fixing the update bug."
	case pass == 2:
		return fmt.Sprintf("Update 2 of 2: installing %s.", final)
	}
	return ""
}

func (a *App) fixUpdatePanel(p *installer.Progress) gk.DivEl {
	fill := installflow.FixTransfer(p)
	waiting := installflow.FixWaiting(p)
	share, value := "", float32(0)
	if fill != nil {
		value = *fill * 100
		share = fmt.Sprintf("%.0f%%", value)
	} else if waiting {
		value = 100
	}
	sending, installing := Accent, surface().Line
	if waiting {
		sending, installing = surface().Line, Accent
	}
	panel := taskPanel().
		Child(gk.HFlex(s.ItemsCenter, s.Gap3, s.TextSm).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(8)).Bg(rgb(sending))).
			Child(gk.Div(s.Flex1, s.MinW0).Child(txt("1. Sending the firmware to the mixer"))).
			Child(gk.Div().TextColor(rgb(Faint)).Child(txt(share)))).
		Child(gk.Div().Mt(px(4)).Mb(px(12)).Child(gk.Progress("task-progress").Value(value))).
		Child(seenRow(installing, "2. The mixer installs it, then restarts. This part has no progress to show.", waiting))
	if line := fixDetailLine(p); fill == nil && line != "" {
		panel = panel.Child(gk.Div(s.Pt2).Child(a.spinnerLine(line)))
	}
	return panel
}

func fixDetailLine(p *installer.Progress) string {
	switch {
	case installflow.FixConfirming(p):
		return "Waiting for the mixer to confirm the fix…"
	case p != nil && p.Detail != "":
		return sentence(p.Detail)
	}
	return ""
}

func sentence(text string) string {
	text = strings.TrimSuffix(text, ".")
	return text + "…"
}

func (a *App) fixRunningPage(i *Installer, t *guidedTask) {
	f := i.fix
	since := i.waitSince
	if since.IsZero() || since.Before(i.stageSince) {
		since = i.stageSince
	}
	soFar := prose(installflow.Elapsed(time.Since(since))+" so far", Faint)
	body := gk.VFlex(s.WFull).Gap(px(16))
	step := f.Step()
	switch step {
	case installflow.FixStepGet:
		line := "Getting RØDE’s firmware ready…"
		if p := f.Event; p != nil && p.Detail != "" {
			line = sentence(p.Detail)
			if stage, _ := installer.StorageFixStage(*p); stage == autofix.StageDownload && p.Total > 0 {
				line = fmt.Sprintf("%s · %.0f of %.0f MB", strings.TrimSuffix(line, "…"), float64(p.Completed)/1e6, float64(p.Total)/1e6)
			}
		}
		body = body.Child(taskHeading("Get RØDE’s firmware")).
			Child(prose(fmt.Sprintf("The app downloads %s from RØDE and adds the fix to it. Nothing on the mixer has changed yet.", withVersion(fixVersion(i))), Muted)).
			Child(a.spinnerLine(line)).
			Child(soFar)
	default:
		title := "Fix the storage"
		switch step {
		case installflow.FixStepReinstall:
			title = "Reinstall RØDE’s firmware"
		case installflow.FixStepCheck:
			title = "Check and finish"
		}
		body = body.Child(taskHeading(title))
		if line := fixPassLine(f.Event, "RØDE’s firmware"); line != "" {
			body = body.Child(prose(line, Muted))
		}
		body = body.Child(a.fixUpdatePanel(f.Event)).
			Child(taskPanel().
				Child(eyebrow("Your mixer’s screen")).
				Child(prose("It says “Updating” while the firmware is sent, then “Installing”. Keep it switched on and connected.", Muted))).
			Child(soFar)
	}
	t.body = body
	stop := "Stop"
	if f.Canceling {
		stop = "Stopping…"
	}
	t.secondary = []gk.ButtonEl{taskButton("task-stop", stop, false).Disabled(!f.CanStop()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.stopTask(cx) })}
	t.primary = taskButton("task-done", "Done", true).Disabled(true)
}

// ---- How it ended (G3, G4, failures) --------------------------------------------

func (a *App) fixEndedPage(i *Installer, t *guidedTask) {
	f := i.fix
	device := i.model.Device.Label()
	version := withVersion(fixVersion(i))
	body := gk.VFlex(s.WFull).Gap(px(16))
	done := func() gk.ButtonEl {
		return taskButton("task-done", "Done", true).Disabled(i.busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })
	}
	switch f.Ending() {
	case installflow.FixEndingFixed:
		body = body.Child(doneHeading("The update bug is fixed")).
			Child(prose(fmt.Sprintf("Your %s runs %s, unchanged. Later updates can no longer damage files this way.", device, version), Muted))
		if f.SavedCopy() {
			body = body.Child(prose("A copy of the mixer’s original storage layout is saved on this computer.", Faint))
		}
		t.body = body
		t.secondary = []gk.ButtonEl{taskButton("task-install", "Install RC2FX…", false).Disabled(i.busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.closeTask(cx, false)
				a.openTask(cx, taskInstall)
			})}
		t.primary = done()
		return
	case installflow.FixEndingAlready:
		t.body = body.Child(doneHeading("Your mixer already had the fix")).
			Child(prose(alreadyFixedLine(f.Reply, version), Muted))
		t.primary = done()
		return
	}
	var title, what string
	r := f.Reply
	switch f.Ending() {
	case installflow.FixEndingUnrecognized:
		title = "The fix can’t be used on this mixer"
		what = fmt.Sprintf("Your mixer’s storage is not laid out the way the app expects, so the app changed nothing on it. %s was installed again, unchanged. There is nothing to try again.", capitalFirst(version))
	case installflow.FixEndingNotApplied:
		if r.Outcome == string(autofix.OutcomeUnconfirmed) {
			title = "The fix didn’t report a result"
			what = fmt.Sprintf("The app can’t tell whether the fix ran, so it counts it as not applied. %s was installed again, and nothing harmful was done.", capitalFirst(version))
		} else {
			title = "The fix didn’t finish"
			what = fmt.Sprintf("The fix couldn’t finish on the mixer. %s was installed again.", capitalFirst(version))
		}
	case installflow.FixEndingRefused:
		title = "The fix can’t run on this mixer"
		what = capitalFirst(strings.TrimSuffix(r.Error, ".")) + "."
	default:
		title = "The fix stopped before it finished"
		why := f.Error
		if r != nil {
			why = r.Error
		}
		what = capitalFirst(strings.TrimSuffix(why, ".")) + "."
	}
	body = body.Child(taskHeading(title)).Child(prose(what, Muted))
	seen := taskPanel().Child(eyebrow("What the app sees now"))
	for n, line := range i.model.SeenNow() {
		color := Warn
		if line.Good {
			color = Accent
		}
		seen = seen.Child(seenRow(color, line.Text, n == 0))
	}
	body = body.Child(seen)
	normal := installflow.FixSeenOf(i.model) == installflow.FixSeenNormal
	if i.access.Kind == installflow.AccessAsking {
		body = body.Child(a.spinnerLine("Answer your system’s password prompt to continue."))
	}
	t.body = body
	if f.CanRetry(true) {
		label := "Try again"
		if !i.access.AllowsFlashing() {
			label = "Try again…"
		}
		if !normal {
			body = body.Child(prose("“Try again” works once the app sees the mixer back in normal mode.", Faint))
			t.body = body
		}
		t.secondary = []gk.ButtonEl{taskButton("task-close", "Close", false).Disabled(i.busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })}
		t.primary = taskButton("task-retry", label, true).
			Disabled(i.busy() || !normal || i.access.Kind == installflow.AccessAsking).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startFix(cx) })
		return
	}
	t.primary = taskButton("task-close", "Close", true).Disabled(i.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })
}

func capitalFirst(text string) string {
	if text == "" || text[0] < 'a' || text[0] > 'z' {
		return text
	}
	return string(text[0]-'a'+'A') + text[1:]
}

func alreadyFixedLine(r *installer.StorageFix, version string) string {
	if r != nil && r.Flashes < 2 {
		return fmt.Sprintf("Nothing needed changing, so the app left it as it was. It runs %s, unchanged.", version)
	}
	return fmt.Sprintf("Nothing needed changing. %s was installed again, unchanged.", capitalFirst(version))
}
