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
// and which app or mixer update toasts the user dismissed.
type AppUpdate struct {
	updates.AppUpdate
	changes              changesDialog
	dismissedVersion     string
	dismissedMixerUpdate string
	signedChecking       bool
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
		a.appUpdate.dismissedMixerUpdate = ""
	}
	a.appUpdate.Checking = true
	a.appUpdate.signedChecking = true
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
		a.appUpdate.signedChecking = false
		if signed != nil {
			a.appUpdate.SetSigned(*signed)
		}
		a.notify(cx)
	})
}

func (a *App) withUpdateToast(cx *gk.Cx, screen gk.Element) gk.Element {
	var toasts []gk.Element
	if toast, ok := a.updateToast(cx); ok {
		toasts = append(toasts, toast)
	}
	if toast, ok := a.mixerUpdateToast(cx); ok {
		toasts = append(toasts, toast)
	}
	if len(toasts) == 0 {
		return screen
	}
	stack := gk.VFlex(s.Absolute, s.ItemsEnd).
		Bottom(px(20)).Right(px(20)).Gap(px(10)).
		Children(toasts...).
		ID("update-toasts")
	return gk.Div(s.Relative, s.SizeFull).Child(screen).Child(stack)
}

func updateToastCard(id gk.ID, title, detail string, action kit.ButtonEl, dismissID gk.ID, onDismiss func(*gk.Cx)) gk.Element {
	copyCol := gk.VFlex(s.Flex1, s.MinW0).Gap(px(2)).
		Child(gk.Div(s.FontSemibold, s.TextSm).TextColor(rgb(Text)).Child(txt(title)))
	if detail != "" {
		copyCol = copyCol.Child(gk.Div(s.TextXs).LineHeight(gk.Relative(1.35)).TextColor(rgb(Muted)).Child(txt(detail)))
	}

	controls := gk.HFlex(s.ItemsCenter, s.FlexShrink0).Gap(px(6)).
		Child(action.H(px(30)).Px(px(12)).Rounded(float32(15)).FontSemibold())
	if onDismiss != nil {
		controls = controls.Child(kit.Button(dismissID).Ghost().Small().Icon(gk.IconClose).
			Tooltip("Dismiss").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { onDismiss(cx) }))
	}

	return gk.HFlex(s.ItemsCenter, s.Border1, s.ShadowLg).
		MaxW(px(400)).Gap(px(12)).
		Px(px(14)).Py(px(11)).Rounded(px(14)).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().LineStrong)).
		FontFamily("Inter").TextColor(rgb(Text)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(8)).Bg(rgb(Accent))).
		Child(copyCol).
		Child(controls).
		ID(id)
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

	var onDismiss func(*gk.Cx)
	if canDismiss {
		dismissVersion := version
		onDismiss = func(cx *gk.Cx) {
			a.appUpdate.dismissedVersion = dismissVersion
			a.notify(cx)
		}
	}
	return updateToastCard("update-toast", title, detail, action, "update-toast-dismiss", onDismiss), true
}

type mixerUpdateToastModel struct {
	version    string
	dismissKey string
	title      string
	detail     string
	inPlace    bool
}

func (a *App) mixerUpdateToastState() (mixerUpdateToastModel, bool) {
	if !a.session.Connected || a.deviceVersions == nil || a.installer != nil {
		return mixerUpdateToastModel{}, false
	}
	// mixerUpdateNotice already displays an inline update card on every connected
	// page when a partial native protocol mismatch is present.
	if a.deviceVersions.NativeProtocol != nil {
		return mixerUpdateToastModel{}, false
	}
	if model.CompatibilityOf(a.deviceVersions.Protocol) == model.CompatibilityMixerNewer {
		return mixerUpdateToastModel{}, false
	}

	if direct, ok := a.directInPlaceOffer(); ok {
		version := direct.offer.Metadata.Version
		dismissKey := "mixer:" + version
		if a.appUpdate.dismissedMixerUpdate == dismissKey {
			return mixerUpdateToastModel{}, false
		}
		return mixerUpdateToastModel{
			version:    version,
			dismissKey: dismissKey,
			title:      fmt.Sprintf("Mixer software %s is available", version),
			detail:     "No flash needed · brief audio pause",
			inPlace:    true,
		}, true
	}

	// Wait for the initial signed-offer fetch to finish before falling back to a
	// flash toast on mixers that support in-place updates.
	if _, signedKnown := a.appUpdate.Signed(); !signedKnown && a.appUpdate.signedChecking &&
		a.deviceVersions.InPlaceUpdates != nil && *a.deviceVersions.InPlaceUpdates > 0 {
		return mixerUpdateToastModel{}, false
	}

	runtime, firmware, newestFirmware := a.versionInputs()
	available := a.availableMixerSoftware()
	return a.flashMixerUpdateToastState(runtime, available, firmware, newestFirmware, a.deviceVersions.Protocol)
}

func (a *App) flashMixerUpdateToastState(runtime, available, firmware, newestFirmware *string, protocol *uint32) (mixerUpdateToastModel, bool) {
	summary := model.Summarize(&model.Inputs{
		InstalledRuntime:        runtime,
		AvailableRuntime:        available,
		InstalledFirmware:       firmware,
		NewestSupportedFirmware: newestFirmware,
		DeviceProtocol:          protocol,
	})
	if !summary.UpdateAvailable() {
		return mixerUpdateToastModel{}, false
	}
	if summary.Runtime != model.StandingUpdateAvailable && summary.Compatibility != model.CompatibilityMixerOlder {
		return mixerUpdateToastModel{}, false
	}

	version := ""
	if available != nil {
		version = *available
	}
	title := "Mixer update available"
	if version != "" {
		title = fmt.Sprintf("Mixer software %s is available", version)
	}
	detail := "Flashes the mixer and restarts."
	dismissKey := "mixer:" + version
	if version == "" {
		dismissKey = "mixer:older"
	}
	if summary.Firmware == model.StandingUpdateAvailable && newestFirmware != nil {
		detail = fmt.Sprintf("Includes RØDE firmware %s · flashes mixer", *newestFirmware)
		dismissKey += "+fw:" + *newestFirmware
	}

	if dismissKey != "" && a.appUpdate.dismissedMixerUpdate == dismissKey {
		return mixerUpdateToastModel{}, false
	}
	return mixerUpdateToastModel{
		version:    version,
		dismissKey: dismissKey,
		title:      title,
		detail:     detail,
	}, true
}

// mixerUpdateToast builds the persistent floating mixer-update toast with a
// direct Update button that opens the in-place updater or guided flash task.
func (a *App) mixerUpdateToast(cx *gk.Cx) (gk.Element, bool) {
	m, ok := a.mixerUpdateToastState()
	if !ok {
		return nil, false
	}
	open := func(cx *gk.Cx) { a.openTask(cx, taskUpdate) }
	if m.inPlace {
		open = a.openInPlaceUpdate
	}
	action := kit.Button("mixer-update-toast-action").Primary().Small().Icon(gk.IconArrowDown).
		Label("Update").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { open(cx) })
	dismissKey := m.dismissKey
	return updateToastCard("mixer-update-toast", m.title, m.detail, action, "mixer-update-toast-dismiss", func(cx *gk.Cx) {
		a.appUpdate.dismissedMixerUpdate = dismissKey
		a.notify(cx)
	}), true
}
