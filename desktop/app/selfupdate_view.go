package app

// self_update.rs, the app's half: the state lives in app/selfupdate (with
// Velopack behind app/selfupdate/vpkc); these start its background work and
// show its results. Nothing happens on its own: the user downloads, then
// chooses "Restart to update", which is held back while the installer has a
// run going or the mixer may still be updating (installerRestartBlocker),
// and still asks about unsaved edits like any other quit.

import (
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate/vpkc"
)

// SelfUpdate is self_update.rs SelfUpdate (app/selfupdate), with the
// lowerCamel names app.go calls.
type SelfUpdate struct {
	selfupdate.SelfUpdate
}

// newSelfUpdate is SelfUpdate::new: Velopack's manager when Velopack
// installed this copy (and this build has it), else a copy that only links
// to the release page.
func newSelfUpdate() SelfUpdate { return SelfUpdate{selfupdate.New(vpkc.Open)} }

// launchOnQuit is SelfUpdate::launch_on_quit (a no-op once taken).
func (u *SelfUpdate) launchOnQuit() { u.LaunchOnQuit() }

// cancelRestart is SelfUpdate::cancel_restart.
func (u *SelfUpdate) cancelRestart() { u.CancelRestart() }

// startSelfUpdate is start_self_update: at startup, check a package an
// earlier run downloaded.
func (a *App) startSelfUpdate(cx *gk.Cx) {
	if asset, ok := a.selfUpdate.TakePending(); ok {
		a.checkUpdatePackage(cx, asset)
	}
}

// checkUpdatePackage checks a downloaded package in the background; it
// becomes ready only if it is the signed release.
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

// downloadAppUpdate is download_app_update: download the newest release in
// the background, reporting progress; the feed checks it as it arrives. A
// kept package that could not be checked is only checked again.
func (a *App) downloadAppUpdate(cx *gk.Cx) {
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
	// Progress until the download ends.
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

// updateRestartBlocker is why RC2FX should not restart for an update now,
// ok false when it may.
func (a *App) updateRestartBlocker() (string, bool) { return a.installerRestartBlocker() }

// restartToUpdate quits and applies the downloaded update, once nothing
// holds it back. Unsaved edits are asked about as for any quit.
func (a *App) restartToUpdate(cx *gk.Cx) {
	_, blocked := a.updateRestartBlocker()
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
		// Refused outright (a file dialog is open): no later quit may
		// restart unasked. An open prompt clears it on cancel.
		a.selfUpdate.cancelRestart()
	}
}
