package app

// installer/repair.rs: Repair mixer, for a mixer a flash left unable to keep
// its setup, or to start. The user holds the power button for 15 seconds so
// the bootloader shows the mixer's storage over USB; the administrator
// helper then checks it and restores damaged files (the installer's
// repair). Reached from the first screen whenever RC2FX isn't running.
//
// The page is a step sidebar on the left and the current step on the right.
// The installer will move to the same layout; stepSidebar is shared for
// that. Its state and words are in installflow/repair.go.

import (
	"fmt"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

// stepSidebar is the steps of a guided task down the left: done, current,
// failed or to come. Repair uses it now; the installer will too.
func stepSidebar(steps []installflow.RepairStep) gk.DivEl {
	list := gk.VFlex(s.Gap1, s.FlexShrink0, s.Pr6).W(px(220))
	for n, step := range steps {
		var circle gk.DivEl
		var text uint32
		switch step.Mark {
		case installflow.RepairMarkDone:
			circle = gk.Div().Bg(rgb(surface().Raised)).BorderColor(rgb(Accent)).
				Child(gk.Icon(gk.IconCheck).TextColor(rgb(Accent)).Size(px(12)))
			text = Muted
		case installflow.RepairMarkNow:
			circle = gk.Div().BorderColor(rgb(Accent)).TextColor(rgb(Accent)).Child(txt(fmt.Sprint(n + 1)))
			text = Text
		case installflow.RepairMarkFailed:
			circle = gk.Div().BorderColor(rgb(Danger)).
				Child(gk.Icon(gk.IconCircleX).TextColor(rgb(Danger)).Size(px(12)))
			text = Text
		default:
			circle = gk.Div().BorderColor(rgb(surface().Line)).TextColor(rgb(Faint)).Child(txt(fmt.Sprint(n + 1)))
			text = Faint
		}
		title := gk.Div(s.TextSm).TextColor(rgb(text))
		if step.Mark == installflow.RepairMarkNow {
			title = title.Style(s.FontSemibold)
		}
		list = list.Child(gk.HFlex(s.ItemsCenter, s.Gap3, s.Py2).
			Child(circle.Style(s.FlexShrink0, s.RoundedFull, s.Border1, s.Flex, s.ItemsCenter, s.JustifyCenter, s.TextXs).
				Size(px(22))).
			Child(title.Child(txt(step.Title))))
	}
	return list
}

func repairPanel() gk.DivEl {
	return gk.VFlex(s.Gap2, s.Mt4, s.P4, s.RoundedXl, s.Border1).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line))
}

func repairRow(color uint32, text string) gk.DivEl {
	return gk.HFlex(s.ItemsStart, s.Gap3, s.Py1, s.TextSm).TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(6)).Size(px(8)).Bg(rgb(color))).
		Child(gk.Div(s.Flex1, s.MinW0).Child(txt(text)))
}

func repairHeading(text string) gk.DivEl {
	return gk.Div(s.TextXl, s.FontSemibold).Child(txt(text))
}

// openRepair opens the installer straight on the repair page.
func (a *App) openRepair(cx *gk.Cx) {
	a.openInstaller(cx)
	if i := a.installer; i != nil {
		i.repair = installflow.NewRepair()
	}
	a.installerPage = true
	a.notify(cx)
}

// showRepair switches the open installer to the repair page.
func (a *App) showRepair(cx *gk.Cx) {
	if i := a.installer; i != nil && !i.busy() {
		i.repair = installflow.NewRepair()
	}
	a.notify(cx)
}

// pollRepair looks for the mixer's storage while waiting, and starts the
// check once it is there and access is granted. Returns whether to redraw.
func (a *App) pollRepair() bool {
	i := a.installer
	if i == nil || i.repair == nil {
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

func (a *App) repairScreen(i *Installer, cx *gk.Cx) gk.Element {
	r := i.repair
	if r == nil {
		return gk.Div()
	}
	needAccess := !i.access.AllowsFlashing()
	steps := r.Steps(i.access.Kind != installflow.AccessNotNeeded, needAccess)
	var body gk.DivEl
	if needAccess {
		body = a.accessCard(i, installflow.RepairAccessPurpose, cx)
	} else {
		body = a.repairBody(i, r)
	}
	busy := r.Busy()
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(gk.Div(s.Flex1, s.MinH0, s.Px8, s.Py8).ID("repair-scroll").OverflowYScroll().
			Child(gk.VFlex(s.WFull, s.MxAuto, s.Gap6).MaxW(px(860)).
				Child(gk.Div(s.Text2xl, s.FontSemibold).Child(txt("Repair mixer"))).
				Child(gk.HFlex(s.ItemsStart).
					Child(stepSidebar(steps)).
					Child(gk.Div(s.Flex1, s.MinW0).Child(body))))).
		Child(gk.Div(s.FlexShrink0, s.Px8, s.Pb6).
			Child(gk.HFlex(s.WFull, s.MxAuto, s.JustifyEnd).MaxW(px(860)).
				Child(gk.Button("repair-close").Ghost().Label("Close").Disabled(busy).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeInstaller(cx) }))))
}

func (a *App) repairBody(i *Installer, r *installflow.Repair) gk.DivEl {
	working := func(what string) gk.DivEl {
		line := what
		if i.progress != nil {
			if t := progressText(i.progress); t != "" {
				line = t
			}
		}
		body := gk.VFlex(s.Gap3).
			Child(repairHeading(what)).
			Child(gk.HFlex(s.Gap3, s.ItemsCenter).
				Child(gk.Spinner().Color(rgb(Accent))).
				Child(paragraph(line, Muted)))
		if p := i.progress; p != nil && p.Total > 0 {
			body = body.Child(gk.Progress("repair-progress").
				Value(min(max(float32(p.Completed)/float32(p.Total)*100, 0), 100)))
		}
		return body
	}
	switch r.Stage.Kind {
	case installflow.RepairWaiting:
		return gk.VFlex(s.Gap3).
			Child(repairHeading("Put the mixer in storage mode")).
			Child(paragraph("RC2FX checks the mixer's system for files a flash damaged, and restores them. Your settings are copied to this computer before anything is written.", Muted)).
			Child(repairPanel().
				Child(paragraph("1. Connect the mixer's USB 1 port to this computer.", Muted)).
				Child(paragraph("2. Switch the mixer off.", Muted)).
				Child(paragraph("3. Press and hold the power button for about 15 seconds, until the screen shows only the RØDE logo.", Muted)).
				Child(gk.HFlex(s.Gap3, s.ItemsCenter, s.Mt2, s.Pt3, s.BorderT1).BorderColor(rgb(surface().Line)).
					Child(gk.Spinner().Color(rgb(Accent))).
					Child(paragraph("Waiting for the mixer's storage…", Muted))))
	case installflow.RepairChecking:
		return working("Checking the mixer")
	case installflow.RepairRepairing:
		return working("Repairing the mixer")
	case installflow.RepairFailed:
		return gk.VFlex(s.Gap3).
			Child(repairHeading("The check didn't finish")).
			Child(paragraph(r.Stage.Error, Danger)).
			Child(gk.HFlex().Child(gk.Button("repair-again").Label("Check again").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.repairAgain(cx) })))
	case installflow.RepairFindings:
		return a.repairFindings(r.Stage.Findings)
	}
	return repairDone(r.Stage.Result)
}

func (a *App) repairFindings(findings *installer.RepairFindings) gk.DivEl {
	if findings == nil {
		findings = &installer.RepairFindings{}
	}
	repairable := findings.Repairable()
	settings := Warn
	if findings.SettingsOK {
		settings = Accent
	}
	list := repairPanel().Child(repairRow(settings, installflow.SettingsLine(findings)))
	for _, root := range findings.Roots {
		if len(root.Damaged) == 0 {
			list = list.Child(repairRow(Accent, installflow.RootLine(root)))
		}
		for _, file := range root.Damaged {
			color := Danger
			if file.Why == nil {
				color = Warn
			}
			list = list.Child(repairRow(color, installflow.DamagedLine(root, file)))
		}
	}
	body := gk.VFlex(s.Gap3).
		Child(repairHeading(installflow.FindingsTitle(findings))).
		Child(paragraph(installflow.FindingsExplanation(findings), Muted)).
		Child(list)
	if repairable == 0 {
		body = body.Child(restartSteps())
	}
	buttons := gk.HFlex(s.Gap3, s.Mt2)
	if repairable > 0 {
		buttons = buttons.Child(gk.Button("repair-start").Primary().Label("Back up settings and repair").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startRepair(cx) }))
	}
	buttons = buttons.Child(gk.Button("repair-recheck").Label("Check again").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.repairAgain(cx) }))
	return body.Child(buttons)
}

func repairDone(result *installer.RepairResult) gk.DivEl {
	if result == nil {
		result = &installer.RepairResult{}
	}
	list := repairPanel()
	for _, path := range result.Repaired {
		list = list.Child(repairRow(Accent, fmt.Sprintf("Restored %s", path)))
	}
	for _, n := range result.NotRepaired {
		list = list.Child(repairRow(Danger, fmt.Sprintf("%s: %s", n[0], n[1])))
	}
	return gk.VFlex(s.Gap3).
		Child(repairHeading(installflow.DoneTitle(result))).
		Child(list).
		Child(restartSteps()).
		Child(paragraph(fmt.Sprintf("Settings partition copied to %s", result.Backup), Faint))
}

// restartSteps is leaving storage mode: the mixer doesn't start while USB 1
// holds it there.
func restartSteps() gk.DivEl {
	return repairPanel().
		Child(paragraph("To leave storage mode:", Text)).
		Child(paragraph("1. Unplug USB 1 from this computer.", Muted)).
		Child(paragraph("2. Disconnect the mixer's power for 30 seconds, then turn it on normally.", Muted)).
		Child(paragraph("3. Don't redo the setup if it asks: a repaired mixer brings it back on this start.", Muted))
}
