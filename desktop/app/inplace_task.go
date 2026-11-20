package app

import (
	"fmt"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

var inPlaceStages = [3]string{"Before you start", "Update RC2FX", "Check and finish"}

func inPlaceStep(p installflow.InPlace) (at int, failed bool) {
	switch p.Kind {
	case installflow.InPlacePreparing, installflow.InPlaceReady, installflow.InPlaceUploading:
		return 1, false
	case installflow.InPlaceInstalling:
		return 2, false
	case installflow.InPlaceDone:
		if !p.Committed {
			return 2, true
		}
		return 3, false
	case installflow.InPlaceFailed:
		return 1, true
	}
	return 0, false
}

// inPlaceTask builds the guided task UI for an on-mixer RC2FX update (no flash).
func (a *App) inPlaceTask(i *Installer) guidedTask {
	m := i.model
	var offer *installer.MixerOffer
	if !i.preferFlash {
		offer = a.inPlaceOffer()
	}
	p := m.InPlace()
	at, failed := inPlaceStep(p)
	steps := make([]installflow.TaskStep, len(inPlaceStages))
	for n, title := range inPlaceStages {
		mark := installflow.MarkToCome
		switch {
		case n < at:
			mark = installflow.MarkDone
		case n == at && failed:
			mark = installflow.MarkFailed
		case n == at:
			mark = installflow.MarkNow
		}
		steps[n] = installflow.TaskStep{Title: title, Mark: mark}
	}
	answering := a.session.Connected && a.streamConnected
	t := guidedTask{
		title:  "Update RC2FX",
		count:  stepCount(at, len(inPlaceStages)),
		steps:  steps,
		scroll: i.controlsScroll,
	}
	if failed && p.Kind == installflow.InPlaceDone {
		t.count = "Step 3 of 3"
	}
	closeAs := func(id gk.ID, label string, primary, overview bool) gk.ButtonEl {
		return taskButton(id, label, primary).Disabled(i.busy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if a.installer != nil {
					a.installer.model.InPlaceDismiss()
				}
				a.closeTask(cx, overview)
			})
	}
	reinstall := taskButton("task-full-flash", "Reinstall firmware as well…", false).Disabled(i.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if a.installer != nil {
				a.installer.preferFlash = true
				a.installer.model.InPlaceDismiss()
			}
			a.notify(cx)
		})
	retry := taskButton("task-retry", "Try again", true).Disabled(i.busy() || offer == nil).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if a.installer != nil {
				a.installer.model.InPlaceDismiss()
			}
			a.startInPlaceUpdate(cx)
		})
	soFar := prose(installflow.Elapsed(time.Since(i.stageSince))+" so far", Faint)
	installed := "the version it has"
	if m.Installed != nil {
		installed = "RC2FX " + *m.Installed
	}
	from := installed
	if i.inPlaceFrom != "" {
		from = "RC2FX " + i.inPlaceFrom
	}
	body := gk.VFlex(s.WFull).Gap(px(16))
	switch p.Kind {
	case installflow.InPlaceIdle:
		version := ""
		if offer != nil {
			version = offer.Metadata.Version
		}
		fw := ""
		if m.InstalledFirmware != nil {
			fw = " " + *m.InstalledFirmware
		}
		t.body = body.Child(taskHeading(fmt.Sprintf("Update RC2FX to %s", version))).
			Child(prose(fmt.Sprintf("Your mixer runs %s. Only the mixer software changes; RØDE firmware%s stays as it is.", installed, fw), Muted)).
			Child(bullets(
				bullet(txt("Takes about a minute.")),
				bullet(txt("RC2FX restarts on the mixer, which interrupts its audio briefly.")),
				bullet(txt("Nothing else changes: your channels, effects and mixer settings are kept.")),
				bullet(txt(fmt.Sprintf("If %s doesn’t start, the mixer goes back to %s by itself.", version, installed)))))
		t.unplug = installflow.Unplug{Kind: installflow.UnplugYes, Reason: "nothing has changed yet"}
		t.secondary = []gk.ButtonEl{reinstall,
			taskButton("task-not-now", "Not now", false).Disabled(i.busy()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeTask(cx, false) })}
		t.primary = taskButton("task-start", "Update RC2FX", true).Disabled(i.busy() || offer == nil).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startInPlaceUpdate(cx) })
		return t
	case installflow.InPlacePreparing:
		body = body.Child(taskHeading("Update RC2FX")).
			Child(prose("The app downloads the update and checks its signature. Nothing on the mixer has changed yet.", Muted)).
			Child(spinnerLine(fmt.Sprintf("Downloading and checking RC2FX %s…", p.Version))).
			Child(soFar)
	case installflow.InPlaceReady, installflow.InPlaceUploading:
		version := p.Version
		if p.Kind == installflow.InPlaceReady {
			version = p.Bundle.Version
		}
		share := float32(0)
		if p.Total != 0 {
			share = min(max(float32(p.Sent)/float32(p.Total)*100, 0), 100)
		}
		body = body.Child(taskHeading("Update RC2FX")).
			Child(prose(fmt.Sprintf("Sending RC2FX %s to the mixer. The mixer then checks it and restarts RC2FX.", version), Muted)).
			Child(gk.VFlex(s.WFull).Gap(px(8)).
				Child(gk.Progress("task-progress").Value(share)).
				Child(gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.TextSm).
					Child(gk.Div().TextColor(rgb(Text)).Child(txt("Sending"))).
					Child(gk.Div().TextColor(rgb(Faint)).Child(txt(installflow.Elapsed(time.Since(i.stageSince)) + " so far")))))
	case installflow.InPlaceInstalling:
		body = body.Child(taskHeading("Check and finish")).
			Child(prose("RC2FX restarts on the mixer, which interrupts its audio briefly. Keep the mixer on and connected.", Muted)).
			Child(spinnerLine(fmt.Sprintf("The mixer is checking and installing RC2FX %s…", p.Version))).
			Child(soFar)
	case installflow.InPlaceDone:
		t.unplug = afterInPlace(answering)
		if p.Committed {
			body = body.Child(doneHeading(fmt.Sprintf("RC2FX %s is running", p.Version))).
				Child(prose("Your channels and effects are as they were.", Muted))
			if p.Detail != "" {
				body = body.Child(prose(p.Detail, Faint))
			}
			t.body = body
			t.primary = closeAs("task-done", "Done", true, true)
			return t
		}
		body = body.Child(taskHeading(fmt.Sprintf("RC2FX %s didn’t start", p.Version))).
			Child(prose(fmt.Sprintf("The mixer went back to %s by itself. Your channels and effects work as before.", from), Muted))
		if p.Detail != "" {
			body = body.Child(prose(p.Detail, Faint))
		}
		body = body.Child(inPlaceSeen(m, answering)).
			Child(prose(fmt.Sprintf("If it doesn’t start again, “Reinstall firmware as well” installs RC2FX %s together with RØDE’s firmware.", p.Version), Faint))
		t.body = body
		t.secondary = []gk.ButtonEl{reinstall, closeAs("task-close", "Close", false, false)}
		t.primary = retry
		return t
	default: // InPlaceFailed
		t.unplug = afterInPlace(answering)
		body = body.Child(taskHeading("The update didn’t finish")).
			Child(prose(p.Error, Muted)).
			Child(inPlaceSeen(m, answering))
		t.body = body
		if offer != nil {
			t.secondary = []gk.ButtonEl{closeAs("task-close", "Close", false, false)}
			t.primary = retry
		} else {
			t.primary = closeAs("task-close", "Close", true, false)
		}
		return t
	}
	t.body = body
	t.unplug = installflow.Unplug{Reason: "RC2FX is being replaced"}
	t.secondary = []gk.ButtonEl{taskButton("task-stop", "Stop", false).Disabled(true)}
	t.primary = taskButton("task-done", "Done", true).Disabled(true)
	return t
}

func inPlaceSeen(m *installflow.Model, answering bool) gk.DivEl {
	seen := taskPanel().Child(eyebrow("What the app sees now"))
	switch {
	case answering && m.Installed != nil:
		return seen.Child(seenRow(Accent, fmt.Sprintf("RC2FX %s is running.", *m.Installed), true))
	case answering:
		return seen.Child(seenRow(Accent, "RC2FX is running.", true))
	}
	return seen.Child(seenRow(Warn, "RC2FX isn’t answering yet. The mixer may still be restarting it.", true))
}

func afterInPlace(answering bool) installflow.Unplug {
	if answering {
		return installflow.Unplug{Kind: installflow.UnplugYes}
	}
	return installflow.Unplug{Reason: "the mixer is restarting RC2FX"}
}
