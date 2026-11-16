package app

// app_update.rs: checking GitHub for a newer desktop app. The check only
// notifies. Copies Velopack installed download and install an update when
// the user asks (selfupdate_view.go); other copies link to the release page.
// The logic is app/updates; this starts the checks and draws the links.

import (
	"fmt"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/updates"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AppUpdate is app_update.rs AppUpdate (app/updates), with the changes
// dialog the Versions section opens (kit state, made once in a handler).
type AppUpdate struct {
	updates.AppUpdate
	changes changesDialog
}

// setSigned is AppUpdate::set_signed, which the installer also calls when it
// lists signed releases itself.
func (u *AppUpdate) setSigned(offers []installer.MixerOffer) { u.SetSigned(offers) }

// link is a ghost button opening url, nil (ok false) unless updates.SafeURL
// allows it.
func link(id gk.ID, label, url string) (gk.ButtonEl, bool) {
	url, ok := updates.SafeURL(url)
	if !ok {
		return gk.ButtonEl{}, false
	}
	return gk.Button(id).Ghost().Label(label).Icon(gk.IconExternalLink).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { gk.OpenURL(cx, url) }), true
}

// rodeAnswer is one model's RØDE releases, nil when its manifest could not
// be read.
type rodeAnswer struct {
	model    firmware.Model
	releases []installer.Release
	ok       bool
}

// checkAppUpdate checks in the background; the UI never waits on the
// network.
func (a *App) checkAppUpdate(cx *gk.Cx, manual bool) {
	if a.appUpdate.Checking {
		return
	}
	a.appUpdate.Checking = true
	a.notify(cx)
	gk.Go(cx, func() installer.UpdateSnapshot { return installer.CheckForUpdates(model.AppVersion) },
		func(cx *gk.Cx, result installer.UpdateSnapshot) {
			if result.App != nil && result.App.Kind == installer.UpdateAvailable {
				// Settings has the link; this makes sure it is seen.
				gk.PushNotification(cx, fmt.Sprintf("RC2FX %s is available. See Settings > Versions.", result.App.Version))
			}
			a.appUpdate.Finish(result, manual)
			a.notify(cx)
		})
	// RØDE's own lists, so Versions can say when RØDE is ahead of RC2FX.
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
	// Separate, so a slow download host never holds up the app's answer.
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
