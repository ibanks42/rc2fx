package app

// Background-task wiring between the UI and app/selfupdate. Updates never apply
// unasked: the user starts the download, and RC2FX then restarts on its own
// once the package is verified and nothing holds the restart back (an
// installer task, unsaved edits, or a change still reaching the mixer).

import (
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate/vpkc"
)

func newSelfUpdate() selfupdate.SelfUpdate { return selfupdate.New(vpkc.Open) }

// startSelfUpdate verifies any update package left over from a previous run.
func (a *App) startSelfUpdate(cx *gk.Cx) {
	if asset, ok := a.selfUpdate.TakePending(); ok {
		a.checkUpdatePackage(cx, asset)
	}
}

func (a *App) checkUpdatePackage(cx *gk.Cx, asset selfupdate.Asset) {
	check, ok := a.selfUpdate.BeginCheck(asset)
	if !ok {
		return
	}
	a.notify(cx)
	gk.Go(cx, check, func(cx *gk.Cx, outcome selfupdate.Outcome) {
		a.selfUpdate.FinishCheck(outcome)
		a.notify(cx)
	})
}

// downloadAppUpdate downloads the latest release in the background with
// periodic progress updates, or re-verifies a previously downloaded package,
// and schedules an automatic restart once ready.
func (a *App) downloadAppUpdate(cx *gk.Cx) {
	a.restartWhenReady = true
	kind, asset := a.selfUpdate.Start()
	switch kind {
	case selfupdate.StartNothing:
		return
	case selfupdate.StartCheck:
		a.checkUpdatePackage(cx, asset)
		return
	case selfupdate.StartFailed:
		a.notify(cx)
		return
	}
	a.notify(cx)
	progress := selfupdate.NewProgress()
	var tick gk.Timer
	tick = gk.Every(cx, 200*time.Millisecond, func(cx *gk.Cx) {
		changed, ended := a.selfUpdate.Advance(progress)
		if changed {
			a.notify(cx)
		} else {
			gk.Quiet(cx)
		}
		if ended {
			gk.Release(cx, tick)
		}
	})
	gk.Go(cx, a.selfUpdate.Download(progress), func(cx *gk.Cx, result selfupdate.DownloadResult) {
		if asset, check := a.selfUpdate.FinishDownload(result); check {
			a.checkUpdatePackage(cx, asset)
		}
		a.notify(cx)
	})
}

// updateRestartBlocker says what an automatic restart is waiting for.
func (a *App) updateRestartBlocker() (string, bool) {
	if reason, blocked := a.installerRestartBlocker(); blocked {
		return reason, true
	}
	switch {
	case a.session.AnyDirty():
		return "RC2FX restarts once your edits are saved or discarded.", true
	case a.fileBusy || a.confirmOpen != nil || a.pending != nil || a.presets.writing() ||
		a.routingPending != nil || a.transitionBusy():
		return "RC2FX restarts in a moment.", true
	}
	return "", false
}

// autoRestart restarts into a verified update the user asked for, as soon as
// nothing holds it back. Called from poll.
func (a *App) autoRestart(cx *gk.Cx) {
	if !a.restartWhenReady || a.selfUpdate.State().Kind != selfupdate.StateReady {
		return
	}
	if _, held := a.updateRestartBlocker(); held {
		return
	}
	a.restartWhenReady = false
	a.restartToUpdate(cx)
}

func (a *App) restartToUpdate(cx *gk.Cx) {
	_, blocked := a.installerRestartBlocker()
	switch a.selfUpdate.Restart(blocked) {
	case selfupdate.RestartGone:
		a.notify(cx)
	case selfupdate.RestartQuit:
		a.quitForUpdate(cx)
	}
}

func (a *App) quitForUpdate(cx *gk.Cx) {
	if a.confirmClose(cx) {
		gk.Quit(cx)
	} else if !a.closingPrompt() {
		// Close was refused outright (e.g. a file dialog is open), so clear the
		// pending restart flag so a later normal quit doesn't apply the update.
		a.selfUpdate.CancelRestart()
	}
}
