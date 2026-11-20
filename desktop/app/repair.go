package app

// Repair mixer, for a mixer a flash left unable to keep
// its setup, or to start. The user holds the power button for 15 seconds so
// the bootloader shows the mixer's storage over USB; the administrator
// helper then checks it and restores damaged files (the installer's
// repair).
//
// The page is a guided task (guided.go); its state and words are in
// installflow/repair.go. Reached from the first screen while the mixer is in
// storage mode, on systems that can repair it (installer.RepairSupported).

import (
	"fmt"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

// openRepair opens the installer straight on the repair page.
func (a *App) openRepair(cx *gk.Cx) {
	a.openTask(cx, taskInstall)
	if i := a.installer; i != nil {
		i.repair = installflow.NewRepair()
	}
	a.notify(cx)
}

// pollRepair looks for the mixer's storage while waiting, and starts the
// check once it is there and access is granted. Returns whether to redraw.
func (a *App) pollRepair() bool {
	i := a.installer
	if i == nil || i.repair == nil || i.fixture {
		return false
	}
	r := i.repair
	if !r.DueToLook(time.Now()) {
		return false
	}
	// Sysfs, IOKit or SetupAPI: readable without administrator access.
	disk, err := platform.RecoveryDiskOf(platform.Host())
	if err != nil {
		disk = nil
	}
	start, changed := r.Looked(disk, i.access.AllowsFlashing())
	if start {
		i.progress = nil
		if !i.runner.send(installflow.RepairInspectRequest()) {
			r.NotSent()
		}
	}
	return changed
}

// repairOutcome is a finished repair request.
func repairOutcome(i *Installer, request installer.Request, reply installer.Reply, err error) {
	if i.repair == nil {
		return
	}
	i.repair.Outcome(request, reply, err)
	i.progress = nil
}

func (a *App) startRepair(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.repair == nil {
		return
	}
	// What the user saw and approved; the helper checks each again.
	request, ok := i.repair.StartRepair()
	if !ok {
		return
	}
	i.progress = nil
	if !i.runner.send(request) {
		i.repair.NotSent()
	}
	a.notify(cx)
}

func (a *App) repairAgain(cx *gk.Cx) {
	if i := a.installer; i != nil && i.repair != nil {
		i.repair = installflow.NewRepair()
	}
	a.notify(cx)
}

// repairTask is Repair mixer in the guided-task layout (mock-up E1–E3).
func (a *App) repairTask(i *Installer) guidedTask {
	r := i.repair
	t := guidedTask{
		title:  "Repair mixer",
		count:  stepCount(r.Step(), installflow.RepairStepCount),
		steps:  r.Steps(),
		scroll: i.controlsScroll,
		unplug: r.Unplug(),
	}
	stop := func(label string) gk.ButtonEl {
		return taskButton("task-stop", label, false).Disabled(r.Busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })
	}
	body := gk.VFlex(s.WFull).Gap(px(16))
	switch r.Stage.Kind {
	case installflow.RepairWaiting:
		body = body.Child(taskHeading("Put the mixer in storage mode")).
			Child(prose("The app checks the mixer’s system for files a firmware update damaged, and restores them. Use this if the mixer forgets its setup or doesn’t start. Your settings are copied to this computer before anything is written.", Muted)).
			Child(storageModePanels())
		needAccess := !i.access.AllowsFlashing()
		switch {
		case i.access.Kind == installflow.AccessAsking:
			body = body.Child(spinnerLine("Answer your system’s password prompt to continue."))
		case r.Disk != nil && needAccess:
			body = body.Child(seenRow(Accent, "The computer sees the mixer in storage mode.", true))
		default:
			body = body.Child(spinnerLine("Waiting for the mixer in storage mode…"))
		}
		if i.access.Kind == installflow.AccessRefused {
			body = body.Child(seenRow(Warn, "Access wasn’t granted. "+strings.TrimSuffix(capitalFirst(i.access.Why), ".")+".", true))
		}
		if i.access.Kind != installflow.AccessNotNeeded {
			body = body.Child(prose("Your system will ask for your password once, when the check starts. The app uses it only to read and repair the mixer.", Faint))
		}
		t.body = body
		t.secondary = []gk.ButtonEl{stop("Stop")}
		// Where access is missing the button asks for it; the check then
		// starts on the next look. Otherwise it starts by itself.
		label := "Check"
		if needAccess {
			label = "Check…"
		}
		t.primary = taskButton("task-start", label, true).
			Disabled(!needAccess || r.Disk == nil || i.access.Kind == installflow.AccessAsking).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.grantInstallerAccess(cx) })
		return t
	case installflow.RepairChecking, installflow.RepairRepairing:
		heading, what := "Check", "Checking the mixer…"
		if r.Stage.Kind == installflow.RepairRepairing {
			heading, what = "Back up and repair", "Copying your settings, then restoring the damaged files…"
		}
		body = body.Child(taskHeading(heading)).Child(spinnerLine(what))
		if p := i.progress; p != nil && p.Total > 0 {
			body = body.Child(gk.Progress("task-progress").
				Value(min(max(float32(p.Completed)/float32(p.Total)*100, 0), 100)))
		}
		body = body.Child(prose("Keep the mixer connected until this finishes.", Faint))
		t.body = body
		t.secondary = []gk.ButtonEl{stop("Stop")}
		t.primary = taskButton("task-done", "Done", true).Disabled(true)
		return t
	case installflow.RepairFailed:
		heading := "The check didn’t finish"
		if r.Wrote {
			heading = "The repair didn’t finish"
		}
		seen := taskPanel().Child(eyebrow("What the app sees now"))
		if r.Disk != nil {
			seen = seen.Child(seenRow(Accent, "The mixer is still in storage mode.", true))
		} else {
			seen = seen.Child(seenRow(Warn, "The computer doesn’t see the mixer in storage mode.", true))
		}
		t.body = body.Child(taskHeading(heading)).
			Child(prose(r.Stage.Error, Muted)).
			Child(seen).
			Child(prose("“Check again” looks at the mixer again before anything is written.", Faint))
		t.secondary = []gk.ButtonEl{stop("Close")}
		t.primary = taskButton("task-retry", "Check again", true).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.repairAgain(cx) })
		return t
	case installflow.RepairFindings:
		findings := r.Stage.Findings
		if findings == nil {
			findings = &installer.RepairFindings{}
		}
		list := taskPanel()
		for _, line := range installflow.FindingLines(findings) {
			color := Warn
			switch {
			case line.Good:
				color = Accent
			case line.Strong:
				color = Danger
			}
			list = list.Child(seenRow(color, line.Text, line.Strong))
		}
		body = body.Child(taskHeading(installflow.FindingsTitle(findings))).
			Child(prose(installflow.FindingsExplanation(findings), Muted)).
			Child(list)
		again := taskButton("task-recheck", "Check again", false).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.repairAgain(cx) })
		if findings.Repairable() == 0 {
			t.body = body.Child(leaveStorageMode())
			t.secondary = []gk.ButtonEl{again}
			t.primary = taskButton("task-done", "Done", true).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })
			return t
		}
		t.body = body.Child(taskPanel().
			Child(eyebrow("To leave storage mode without repairing")).
			Child(prose("Unplug USB 1 and plug it back in. The mixer starts normally. Nothing has been written yet.", Muted)))
		t.secondary = []gk.ButtonEl{stop("Stop"), again}
		t.primary = taskButton("task-start", "Back up and repair", true).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startRepair(cx) })
		return t
	}
	result := r.Stage.Result
	if result == nil {
		result = &installer.RepairResult{}
	}
	if len(result.Repaired) > 0 {
		body = body.Child(doneHeading(installflow.DoneTitle(result)))
	} else {
		body = body.Child(taskHeading(installflow.DoneTitle(result)))
	}
	body = body.Child(prose("Your settings were copied to this computer before the repair. Now restart the mixer to finish.", Muted))
	if n := len(result.NotRepaired); n > 0 {
		left := "1 file couldn’t be restored and was left as it was."
		if n > 1 {
			left = fmt.Sprintf("%d files couldn’t be restored and were left as they were.", n)
		}
		body = body.Child(seenRow(Warn, left, true))
	}
	t.body = body.Child(leaveStorageMode()).
		Child(prose("Settings backup saved on this computer.", Faint))
	t.primary = taskButton("task-done", "Done", true).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })
	return t
}

// storageModePanels is how to enter storage mode and how to leave it, side
// by side, so nobody is stuck in it.
func storageModePanels() gk.DivEl {
	return gk.HFlex(s.WFull, s.ItemsStretch).Gap(px(14)).
		Child(taskPanel().Style(s.Flex1, s.MinW0).
			Child(eyebrow("To enter storage mode")).
			Child(numbered(
				"Connect the mixer’s USB 1 port to this computer.",
				"Switch the mixer off.",
				"Press and hold the power button for about 15 seconds.",
				"The mixer’s screen stays dark. This page moves on when the computer sees the mixer."))).
		Child(taskPanel().Style(s.Flex1, s.MinW0).
			Child(eyebrow("To leave storage mode")).
			Child(numbered(
				"Unplug USB 1 from this computer.",
				"Plug it back in.",
				"The mixer starts normally.")))
}

// leaveStorageMode is the last step: the mixer doesn't start while USB 1
// holds it in storage mode.
func leaveStorageMode() gk.DivEl {
	return taskPanel().Child(numbered(
		"Unplug USB 1 from this computer and plug it back in.",
		"The mixer starts normally.",
		"If it asks to set up again, don’t: the repaired mixer brings your setup back on this start."))
}
