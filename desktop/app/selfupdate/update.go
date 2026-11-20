package selfupdate

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
)

// Opener constructs a Velopack Manager backed by feed. Pass nil locator to
// use Velopack's default path discovery.
type Opener func(feed *SignedFeed, locator *LocatorConfig) (Manager, error)

// SelfUpdate holds the Velopack manager (if installed via Velopack) and the
// current in-place update state machine.
type SelfUpdate struct {
	manager     Manager
	feed        *SignedFeed
	packages    string
	preview     bool
	state       State
	pending     *Asset
	checks      *Checks
	applyOnQuit *Asset
	verify      Verify
}

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
	// Re-verify any previously downloaded package before allowing restart.
	if asset, ok := manager.UpdatePendingRestart(); ok {
		u.pending = &asset
		u.state = VerifyingState(asset.Version)
	}
	return u
}

func empty(manager Manager) SelfUpdate {
	return SelfUpdate{manager: manager, state: Idle(), checks: &Checks{}, verify: appupdate.VerifyUpdatePackage}
}

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

// previewUpdate honors RC2FX_DEV_UPDATE (idle, downloading, verifying, ready,
// failed) in dev builds for UI screenshots.
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

func (u *SelfUpdate) Supported() bool { return u.manager != nil || u.preview }

func (u *SelfUpdate) Previewing() bool { return u.preview }

func (u *SelfUpdate) InProgress() bool {
	switch u.state.Kind {
	case StateDownloading, StateVerifying, StateReady:
		return true
	}
	return false
}

func (u *SelfUpdate) StaysShown() bool { return u.InProgress() || u.state.Kind == StateFailed }

func (u *SelfUpdate) Retrying() bool { return u.state.Kind == StateFailed && u.state.HasAsset }

func (u *SelfUpdate) State() State { return u.state }

func (u *SelfUpdate) OnDisk(asset Asset) bool {
	name, ok := PackageName(asset.FileName)
	if u.packages == "" || !ok {
		return false
	}
	info, err := os.Stat(filepath.Join(u.packages, name))
	return err == nil && info.Mode().IsRegular()
}

// LaunchOnQuit starts the Velopack updater on process exit if a restart was
// confirmed. Subsequent calls are no-ops.
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

func (u *SelfUpdate) CancelRestart() { u.applyOnQuit = nil }

func (u *SelfUpdate) TakePending() (Asset, bool) {
	asset := u.pending
	u.pending = nil
	if asset == nil {
		return Asset{}, false
	}
	return *asset, true
}

// BeginCheck transitions to StateVerifying and returns the blocking
// verification closure to run off the UI thread.
func (u *SelfUpdate) BeginCheck(asset Asset) (check func() Outcome, ok bool) {
	packages := u.packages
	if packages == "" {
		return nil, false
	}
	u.state = VerifyingState(asset.Version)
	verify := u.verify
	return func() Outcome {
		return CheckedOutcome(asset, VerifyPackage(packages, asset, verify))
	}, true
}

func (u *SelfUpdate) FinishCheck(o Outcome) {
	u.state = o.State
}

type StartKind int

const (
	StartNothing StartKind = iota
	StartCheck
	StartFailed
	StartDownload
)

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

type DownloadResult struct {
	Asset   Asset
	Checked bool
	Failed  bool
	Message string
	Retry   *Asset
}

// Download returns the blocking download closure to run on a worker goroutine
// after Start returns StartDownload.
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
			// Package was already on disk so the feed did not fetch it; verify next.
			return DownloadResult{Asset: asset}
		case found && result != nil && !result.Rejected:
			return DownloadResult{Failed: true, Message: result.Message, Retry: &asset}
		case found && result != nil:
			return fail(result.Error())
		}
		return fail(Failure(feedError(feed, downloaded)))
	}
}

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

// Advance drains download progress updates into State and reports whether the
// UI should redraw and whether the download has finished.
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

// FinishDownload applies a completed download result, returning needsCheck=true
// if the package was already on disk and still needs verification.
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

type RestartKind int

const (
	RestartNothing RestartKind = iota
	RestartGone
	RestartQuit
)

func (u *SelfUpdate) Restart(blocked bool) RestartKind {
	if blocked || u.state.Kind != StateReady || !u.state.HasAsset {
		return RestartNothing
	}
	asset := u.state.Asset
	if !u.OnDisk(asset) {
		u.state = Failed("The downloaded update is no longer on this computer. Download it again.")
		return RestartGone
	}
	u.applyOnQuit = &asset
	return RestartQuit
}
