package app

// Background update checks and release links. Velopack-installed builds handle
// self-updates in selfupdate_glue.go; other builds link to the release page.

import (
	"fmt"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/updates"
	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AppUpdate tracks the latest update check along with the changes dialog state.
type AppUpdate struct {
	updates.AppUpdate
	changes changesDialog
}

// link returns an external-link ghost button if updates.SafeURL permits url.
func link(id gk.ID, label, url string) (gk.ButtonEl, bool) {
	url, ok := updates.SafeURL(url)
	if !ok {
		return gk.ButtonEl{}, false
	}
	return gk.Button(id).Ghost().Label(label).Icon(gk.IconExternalLink).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { gk.OpenURL(cx, url) }), true
}

type rodeAnswer struct {
	model    firmware.Model
	releases []installer.Release
	ok       bool
}

func (a *App) checkAppUpdate(cx *gk.Cx, manual bool) {
	if a.appUpdate.Checking {
		return
	}
	a.appUpdate.Checking = true
	a.notify(cx)
	gk.Go(cx, func() appupdate.UpdateSnapshot { return appupdate.CheckForUpdates(model.AppVersion) },
		func(cx *gk.Cx, result appupdate.UpdateSnapshot) {
			if result.App != nil && result.App.Kind == appupdate.UpdateAvailable {
				gk.PushNotification(cx, fmt.Sprintf("RC2FX %s is available. See Settings > Versions.", result.App.Version))
			}
			a.appUpdate.Finish(result, manual)
			a.notify(cx)
		})
	// Fetch RØDE's release feeds so the Versions view can flag when RØDE is ahead.
	gk.Go(cx, func() []rodeAnswer {
		answers := make([]rodeAnswer, 0, len(firmware.ModelAll))
		for _, m := range firmware.ModelAll {
			releases, err := installer.RodeReleases(m)
			answers = append(answers, rodeAnswer{model: m, releases: releases, ok: err == nil})
		}
		return answers
	}, func(cx *gk.Cx, answers []rodeAnswer) {
		for _, answer := range answers {
			if answer.ok {
				a.appUpdate.SetRode(answer.model, answer.releases)
			}
		}
		a.notify(cx)
	})
	// Run separately so a slow download host does not delay the app update result.
	gk.Go(cx, func() *[]installer.MixerOffer {
		work, _ := workDirectory()
		if offers, ok := updates.SignedMixers(work); ok {
			return &offers
		}
		return nil
	}, func(cx *gk.Cx, signed *[]installer.MixerOffer) {
		if signed != nil {
			a.appUpdate.SetSigned(*signed)
			a.notify(cx)
		} else {
			gk.Quiet(cx)
		}
	})
}
