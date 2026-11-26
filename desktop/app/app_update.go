package app

// Background update checks and release links. Velopack-installed builds handle
// self-updates in selfupdate_glue.go; other builds link to the release page.

import (
	"fmt"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/kit"
	"github.com/ibanks42/goui-kit/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
	"github.com/ibanks42/rc2fx/desktop/app/updates"
	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AppUpdate tracks the latest update check along with the changes dialog state
// and whether the user dismissed the persistent update toast for a version.
type AppUpdate struct {
	updates.AppUpdate
	changes          changesDialog
	dismissedVersion string
}

// link returns an external-link ghost button if updates.SafeURL permits url.
func link(id gk.ID, label, url string) (kit.ButtonEl, bool) {
	url, ok := updates.SafeURL(url)
	if !ok {
		return kit.ButtonEl{}, false
	}
	return kit.Button(id).Ghost().Label(label).Icon(gk.IconExternalLink).
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
	if manual {
		a.appUpdate.dismissedVersion = ""
	}
	a.appUpdate.Checking = true
	a.notify(cx)
	gk.Go(cx, func() appupdate.UpdateSnapshot { return appupdate.CheckForUpdates(model.AppVersion) },
		func(cx *gk.Cx, result appupdate.UpdateSnapshot) {
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

func (a *App) withUpdateToast(cx *gk.Cx, screen gk.Element) gk.Element {
	toast, ok := a.updateToast(cx)
	if !ok {
		return screen
	}
	return gk.Div(s.Relative, s.SizeFull).Child(screen).Child(toast)
}

type updateToastModel struct {
	version    string
	releaseURL string
	inPlace    bool
	state      selfupdate.State
}

func (a *App) updateToastState() (updateToastModel, bool) {
	version, _, offered := a.appUpdate.Offered()
	state := a.selfUpdate.State()
	active := a.selfUpdate.StaysShown()
	available := offered || a.selfUpdate.Previewing()
	if !active && !available {
		return updateToastModel{}, false
	}
	if version == "" {
		version = state.Version
	}
	if !active && version != "" && a.appUpdate.dismissedVersion == version {
		return updateToastModel{}, false
	}
	inPlace := a.selfUpdate.Supported()
	releaseURL, hasRelease := a.appUpdate.ReleaseURL()
	if !inPlace && !hasRelease {
		return updateToastModel{}, false
	}
	return updateToastModel{
		version:    version,
		releaseURL: releaseURL,
		inPlace:    inPlace,
		state:      state,
	}, true
}

// updateToast builds the persistent floating app-update toast with a direct
// Update button so users can update from any screen, including disconnected.
func (a *App) updateToast(cx *gk.Cx) (gk.Element, bool) {
	m, ok := a.updateToastState()
	if !ok {
		return nil, false
	}
	version, releaseURL, inPlace, state := m.version, m.releaseURL, m.inPlace, m.state

	title := "App update available"
	if version != "" {
		title = fmt.Sprintf("RC2FX %s is available", version)
	}
	detail := ""
	var action kit.ButtonEl
	canDismiss := true

	if inPlace {
		switch state.Kind {
		case selfupdate.StateDownloading:
			if version != "" {
				title = fmt.Sprintf("Downloading RC2FX %s", version)
			} else {
				title = "Downloading update"
			}
			detail = fmt.Sprintf("%d%% · restarts when ready", state.Percent)
			action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconLoaderCircle).
				Label(fmt.Sprintf("%d%%", state.Percent)).Disabled(true)
			canDismiss = false
		case selfupdate.StateVerifying:
			if version != "" {
				title = fmt.Sprintf("Checking RC2FX %s", version)
			} else {
				title = "Checking update"
			}
			detail = "Verifying signed release…"
			action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconLoaderCircle).
				Label("Checking").Disabled(true)
			canDismiss = false
		case selfupdate.StateReady:
			if version != "" {
				title = fmt.Sprintf("RC2FX %s is ready", version)
			} else {
				title = "Update is ready"
			}
			reason, blocked := a.installerRestartBlocker()
			detail = "Restart to finish updating."
			if waiting, held := a.updateRestartBlocker(); a.restartWhenReady && held {
				detail = waiting
			} else if blocked {
				detail = reason
			}
			action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconRefreshCw).
				Label("Restart").Disabled(blocked).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.restartToUpdate(cx) })
		case selfupdate.StateFailed:
			title = "App update failed"
			detail = state.Message
			action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconRefreshCw).
				Label("Update").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.downloadAppUpdate(cx) })
		default:
			action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconArrowDown).
				Label("Update").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.downloadAppUpdate(cx) })
		}
	} else {
		action = kit.Button("update-toast-action").Primary().Small().Icon(gk.IconExternalLink).
			Label("Update").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { gk.OpenURL(cx, releaseURL) })
	}

	copyCol := gk.VFlex(s.Flex1, s.MinW0).Gap(px(2)).
		Child(gk.Div(s.FontSemibold, s.TextSm).TextColor(rgb(Text)).Child(txt(title)))
	if detail != "" {
		copyCol = copyCol.Child(gk.Div(s.TextXs).LineHeight(gk.Relative(1.35)).TextColor(rgb(Muted)).Child(txt(detail)))
	}

	controls := gk.HFlex(s.ItemsCenter, s.FlexShrink0).Gap(px(6)).
		Child(action.H(px(30)).Px(px(12)).Rounded(float32(15)).FontSemibold())
	if canDismiss {
		dismissVersion := version
		controls = controls.Child(kit.Button("update-toast-dismiss").Ghost().Small().Icon(gk.IconClose).
			Tooltip("Dismiss").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.appUpdate.dismissedVersion = dismissVersion
				a.notify(cx)
			}))
	}

	toast := gk.HFlex(s.Absolute, s.ItemsCenter, s.Border1, s.ShadowLg).
		Bottom(px(20)).Right(px(20)).MaxW(px(400)).Gap(px(12)).
		Px(px(14)).Py(px(11)).Rounded(px(14)).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().LineStrong)).
		FontFamily("Inter").TextColor(rgb(Text)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(8)).Bg(rgb(Accent))).
		Child(copyCol).
		Child(controls).
		ID("update-toast")
	return toast, true
}
