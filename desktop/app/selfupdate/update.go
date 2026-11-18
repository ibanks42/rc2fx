package selfupdate

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Opener makes Velopack's UpdateManager over feed (vpkc.Open). locator is
// nil to let Velopack find its own paths. nil when this build has no
// Velopack.
type Opener func(feed *SignedFeed, locator *LocatorConfig) (Manager, error)

// SelfUpdate is this copy's self-update: its Velopack manager, if any, and
// the update's state.
type SelfUpdate struct {
	// manager is nil when Velopack did not install this copy.
	manager Manager
	feed    *SignedFeed
	// packages is where Velopack keeps downloaded packages; "" for unknown.
	packages string
	// preview: RC2FX_DEV_UPDATE set a state to show; nothing can be
	// downloaded.
	preview bool
	// state is where the update stands, with the package to apply (Ready)
	// or to check again (Failed) when there is one.
	state State
	// pending is a package found at startup, until it is checked.
	pending *Asset
	// checks is what the feed's check of the last download found.
	checks *Checks
	// applyOnQuit is the downloaded package to apply as RC2FX quits.
	applyOnQuit *Asset
	// verify checks a package against its signed release.
	verify Verify
}

// New is the app's updater: a preview when one is asked for, else
// Velopack's manager when open finds it, else one that only links to the
// release page.
func New(open Opener) SelfUpdate {
	if preview, ok := previewUpdate(); ok {
		return preview
	}
	checks := &Checks{}
	u := empty(nil)
	u.checks = checks
	if open == nil {
		return u
	}
	feed := NewSignedFeed(checks)
	manager, packages, ok := velopackManager(open, feed)
	if !ok {
		return u
	}
	u.manager, u.feed, u.packages = manager, feed, packages
	// A package downloaded earlier and not yet applied is checked again
	// before it may be applied.
	if asset, ok := manager.UpdatePendingRestart(); ok {
		u.pending = &asset
		u.state = VerifyingState(asset.Version)
	}
	return u
}

func empty(manager Manager) SelfUpdate {
	return SelfUpdate{manager: manager, state: Idle(), checks: &Checks{}, verify: installer.VerifyUpdatePackage}
}

// velopackManager opens Velopack's update manager: on Linux with the
// private packages folder (closed to others first), elsewhere with
// Velopack's own paths, of which only the packages folder is needed here.
func velopackManager(open Opener, feed *SignedFeed) (Manager, string, bool) {
	if runtime.GOOS == "linux" {
		config, ok := VelopackLocator()
		if !ok {
			return nil, "", false
		}
		if err := EnsurePrivate(config.PackagesDir); err != nil {
			fmt.Fprintf(os.Stderr, "RC2FX cannot keep updates in %s: %s\n", config.PackagesDir, err)
			return nil, "", false
		}
		manager, err := open(feed, &config)
		if err != nil {
			return nil, "", false
		}
		return manager, config.PackagesDir, true
	}
	// The same search UpdateManager makes, to learn where packages go.
	packages, ok := PackagesDir()
	if !ok {
		return nil, "", false
	}
	manager, err := open(feed, nil)
	if err != nil {
		return nil, "", false
	}
	return manager, packages, true
}

// previewUpdate is RC2FX_DEV_UPDATE: in
// development builds only, shows the update actions in a given state, for
// screenshots: idle, downloading, verifying, ready or failed.
func previewUpdate() (SelfUpdate, bool) {
	if !devBuild {
		return SelfUpdate{}, false
	}
	name, ok := os.LookupEnv("RC2FX_DEV_UPDATE")
	if !ok {
		return SelfUpdate{}, false
	}
	var state State
	switch name {
	case "idle":
		state = Idle()
	case "downloading":
		state = Downloading(42)
	case "verifying":
		state = VerifyingState("9.9.9")
	case "ready":
		state = Ready("9.9.9")
	case "failed":
		state = Failed("The update could not be downloaded. Check the internet connection and try again.")
	default:
		fmt.Fprintf(os.Stderr, "RC2FX_DEV_UPDATE ignored: %s\n", name)
		return SelfUpdate{}, false
	}
	u := empty(nil)
	u.preview, u.state = true, state
	return u, true
}

// Supported is whether this copy can update itself.
func (u *SelfUpdate) Supported() bool { return u.manager != nil || u.preview }

// Previewing is showing a RC2FX_DEV_UPDATE state, which needs no offered
// update.
func (u *SelfUpdate) Previewing() bool { return u.preview }

// InProgress: a download has started or finished, so its button stays even
// when today's check found nothing.
func (u *SelfUpdate) InProgress() bool {
	switch u.state.Kind {
	case StateDownloading, StateVerifying, StateReady:
		return true
	}
	return false
}

// StaysShown: its button stays even when today's check found nothing: a
// download is under way or done, or a check failed, which the user must
// see.
func (u *SelfUpdate) StaysShown() bool { return u.InProgress() || u.state.Kind == StateFailed }

// Retrying: a failed check kept its package, so trying again only checks
// it.
func (u *SelfUpdate) Retrying() bool { return u.state.Kind == StateFailed && u.state.HasAsset }

// State is where the update stands.
func (u *SelfUpdate) State() State { return u.state }

// SetState replaces the state (the view's own failures).
func (u *SelfUpdate) SetState(state State) { u.state = state }

// Packages is the packages folder, "" when unknown.
func (u *SelfUpdate) Packages() string { return u.packages }

// OnDisk is whether the package asset is still in the packages folder.
func (u *SelfUpdate) OnDisk(asset Asset) bool {
	name, ok := PackageName(asset.FileName)
	if u.packages == "" || !ok {
		return false
	}
	info, err := os.Stat(filepath.Join(u.packages, name))
	return err == nil && info.Mode().IsRegular()
}

// LaunchOnQuit starts the updater for a confirmed restart. Called as RC2FX
// quits; the updater waits for this process to exit. A no-op once taken.
func (u *SelfUpdate) LaunchOnQuit() {
	asset := u.applyOnQuit
	u.applyOnQuit = nil
	if u.manager == nil || asset == nil {
		return
	}
	if err := u.manager.WaitExitThenApplyUpdates(*asset, false, true, nil); err != nil {
		fmt.Fprintf(os.Stderr, "RC2FX could not start the updater: %s\n", err)
	}
}

// CancelRestart: a restart was asked for but the quit was then canceled.
func (u *SelfUpdate) CancelRestart() { u.applyOnQuit = nil }

// TakePending is start_self_update's `pending.take()`.
func (u *SelfUpdate) TakePending() (Asset, bool) {
	asset := u.pending
	u.pending = nil
	if asset == nil {
		return Asset{}, false
	}
	return *asset, true
}

// BeginCheck is check_update_package before its background work: it
// returns the check to run off the UI thread, ok false when there is no
// packages folder (nothing changes then).
func (u *SelfUpdate) BeginCheck(asset Asset) (check func() Outcome, ok bool) {
	packages := u.packages
	if packages == "" {
		return nil, false
	}
	u.state = VerifyingState(asset.Version)
	verify := u.verify
	return func() Outcome {
		return CheckedOutcome(asset, VerifyPackage(WorkDirectory(), packages, asset, verify))
	}, true
}

// FinishCheck is finish_check.
func (u *SelfUpdate) FinishCheck(o Outcome) {
	u.state = o.State
}

// StartKind is what download_app_update does first.
type StartKind int

const (
	// StartNothing: no manager, or a download is already under way.
	StartNothing StartKind = iota
	// StartCheck: a kept package that could not be checked is only checked
	// again (BeginCheck with the asset).
	StartCheck
	// StartFailed: the state changed to a failure; redraw.
	StartFailed
	// StartDownload: the state is Downloading(0); run Download in the
	// background and Advance on a 200 ms tick.
	StartDownload
)

// Start is download_app_update up to its background work.
func (u *SelfUpdate) Start() (StartKind, Asset) {
	if u.manager == nil || u.InProgress() {
		return StartNothing, Asset{}
	}
	if u.Retrying() && u.OnDisk(u.state.Asset) {
		return StartCheck, u.state.Asset
	}
	if runtime.GOOS == "linux" && u.packages != "" {
		if err := EnsurePrivate(u.packages); err != nil {
			u.state = Failed(fmt.Sprintf("The update could not be downloaded (%s).", err))
			return StartFailed, Asset{}
		}
	}
	u.state = Downloading(0)
	return StartDownload, Asset{}
}

// DownloadResult is the background download's result.
type DownloadResult struct {
	Asset Asset
	// Checked: the feed checked this download as it arrived, and it passed.
	// Otherwise the package was already on disk and is checked next.
	Checked bool
	// Failed, with Message, and Retry when the package was kept to check
	// again.
	Failed  bool
	Message string
	Retry   *Asset
}

// Download is the background half of download_app_update: check the feed,
// then download, the feed checking the package as it arrives. It ends
// progress when done. Call Start first; it blocks on the network.
func (u *SelfUpdate) Download(progress *Progress) func() DownloadResult {
	manager, feed, checks := u.manager, u.feed, u.checks
	return func() DownloadResult {
		defer progress.end()
		fail := func(message string) DownloadResult { return DownloadResult{Failed: true, Message: message} }
		if feed != nil {
			feed.takeFailure()
		}
		kind, update, err := manager.CheckForUpdates()
		if err != nil {
			return fail(Failure(feedError(feed, err)))
		}
		if kind != UpdateAvailable || update == nil {
			return fail("This update is not ready to download yet. Try again later.")
		}
		defer update.Release()
		asset := update.TargetFullRelease()
		progress.setVersion(asset.Version)
		checks.Take(asset)
		if feed != nil {
			feed.setProgress(progress)
			defer feed.setProgress(nil)
			feed.takeFailure()
		}
		downloaded := manager.DownloadUpdates(update, progress.Send)
		result, found := checks.Take(asset)
		switch {
		case downloaded == nil && found && result == nil:
			return DownloadResult{Asset: asset, Checked: true}
		case downloaded == nil:
			// Already on disk from an earlier download, so the feed
			// fetched nothing; it is checked next.
			return DownloadResult{Asset: asset}
		case found && result != nil && !result.Rejected:
			return DownloadResult{Failed: true, Message: result.Message, Retry: &asset}
		case found && result != nil:
			return fail(result.Error())
		}
		return fail(Failure(feedError(feed, downloaded)))
	}
}

// feedError is the error behind a failed Velopack call: the feed's own when
// its request failed (the C library only says the callback failed), else
// the library's.
func feedError(feed *SignedFeed, err error) *Error {
	if feed != nil {
		if own := feed.takeFailure(); own != nil {
			return own
		}
	}
	if e, ok := err.(*Error); ok {
		return e
	}
	return LibraryError(err.Error())
}

// Advance is one 200 ms tick of the progress loop: the latest value since
// the last tick moves a Downloading state on. changed reports a redraw;
// ended that the download is over and the tick can stop.
func (u *SelfUpdate) Advance(progress *Progress) (changed, ended bool) {
	latest, has, version, fetching, ended := progress.drain()
	if has && u.state.Kind == StateDownloading {
		if latest == Verifying && fetching {
			u.state = VerifyingState(version)
		} else {
			u.state = Downloading(uint8(min(max(latest, 0), 100)))
		}
		changed = true
	}
	return changed, ended
}

// FinishDownload applies the background result. check is set when the
// package was already on disk and must be checked (BeginCheck).
func (u *SelfUpdate) FinishDownload(r DownloadResult) (check Asset, needsCheck bool) {
	switch {
	case r.Failed:
		u.state = Failed(r.Message)
		if r.Retry != nil {
			u.state = FailedKept(r.Message, *r.Retry)
		}
	case r.Checked:
		u.FinishCheck(CheckedOutcome(r.Asset, nil))
	default:
		return r.Asset, true
	}
	return Asset{}, false
}

// RestartKind is what restart_to_update does.
type RestartKind int

const (
	// RestartNothing: blocked, or nothing ready.
	RestartNothing RestartKind = iota
	// RestartGone: the package went missing; the state says so.
	RestartGone
	// RestartQuit: quit for the update (asking about unsaved edits as any
	// quit); the updater starts as RC2FX quits.
	RestartQuit
)

// Restart is restart_to_update up to the quit. blocked is
// update_restart_blocker's answer.
func (u *SelfUpdate) Restart(blocked bool) RestartKind {
	if blocked || u.state.Kind != StateReady || !u.state.HasAsset {
		return RestartNothing
	}
	asset := u.state.Asset
	// Cleaned up or removed since it was checked: nothing to apply.
	if !u.OnDisk(asset) {
		u.state = Failed("The downloaded update is no longer on this computer. Download it again.")
		return RestartGone
	}
	u.applyOnQuit = &asset
	return RestartQuit
}
