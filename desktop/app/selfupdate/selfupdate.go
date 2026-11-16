// Package selfupdate updates RC2FX in place, through Velopack, when Velopack
// installed it: the Windows Setup.exe, the macOS Setup.pkg, their portable
// zips, or the Linux AppImage. Other copies (development builds, builds from
// source) only link to the release page. It is the non-UI half of
// the Rust app's self_update.rs; the buttons are in package app
// (selfupdate_view.go) and Velopack's C library is behind Manager (package
// selfupdate/vpkc, built with the `velopack` tag).
//
// Nothing happens on its own. The update check says a version exists; the
// user downloads it, then chooses "Restart to update". That restart is held
// back while the installer has a run going or the mixer may still be
// updating, and it still asks about unsaved edits like any other quit. The
// Velopack updater starts as RC2FX quits and waits for it to exit.
//
// Updates come from the latest GitHub release: its `releases.{channel}.json`
// names the package and its SHA-256, which Velopack checks after download.
// That feed is not signed, so a package is also checked against the signed
// app release metadata (installer.VerifyUpdatePackage) as it downloads,
// before Velopack moves it into place, and again when one is found at
// startup. Only full packages are taken from the feed.
//
// Nothing here imports gpuikit.dev or cgo: it tests with plain `go test`.
package selfupdate

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Feed is the assets of the release GitHub marks latest; only app releases
// are.
const Feed = "https://github.com/ibanks42/rc2fx/releases/latest/download"

// Verifying is sent on the download's progress once the bytes are in and
// the signed check starts. Velopack sends only 0 to 100 there.
const Verifying int16 = -32768

// PackID is Velopack's package id for RC2FX, as `vpk pack --packId` sets it.
const PackID = "io.github.ibanks42.rc2fx"

// StateKind names a State variant.
type StateKind int

const (
	// StateIdle: waiting for the user to ask.
	StateIdle StateKind = iota
	// StateDownloading: Percent downloaded.
	StateDownloading
	// StateVerifying: downloaded; Version is being checked against its
	// signed release.
	StateVerifying
	// StateReady: Version is downloaded and checked; applied when RC2FX
	// restarts.
	StateReady
	// StateFailed: the last attempt failed (Message); the user may try
	// again.
	StateFailed
)

// State is where an in-place update stands. Only the fields of its Kind are
// set, so two states compare with ==.
type State struct {
	Kind    StateKind
	Percent uint8
	Version string
	Message string
}

// Idle is State::Idle.
func Idle() State { return State{Kind: StateIdle} }

// Downloading is State::Downloading.
func Downloading(percent uint8) State { return State{Kind: StateDownloading, Percent: percent} }

// VerifyingState is State::Verifying.
func VerifyingState(version string) State { return State{Kind: StateVerifying, Version: version} }

// Ready is State::Ready.
func Ready(version string) State { return State{Kind: StateReady, Version: version} }

// Failed is State::Failed.
func Failed(message string) State { return State{Kind: StateFailed, Message: message} }

// Asset is a Velopack asset (VelopackAsset): a package in the feed or on
// disk. The JSON names are Velopack's.
type Asset struct {
	PackageID     string `json:"PackageId"`
	Version       string `json:"Version"`
	Type          string `json:"Type"`
	FileName      string `json:"FileName"`
	SHA1          string `json:"SHA1"`
	SHA256        string `json:"SHA256"`
	Size          uint64 `json:"Size"`
	NotesMarkdown string `json:"NotesMarkdown"`
	NotesHTML     string `json:"NotesHtml"`
}

// UpdateCheckKind is Velopack's UpdateCheck.
type UpdateCheckKind int

const (
	UpdateAvailable UpdateCheckKind = iota
	NoUpdateAvailable
	RemoteIsEmpty
)

// Update is an available update (Velopack's UpdateInfo), kept by the
// Manager that found it until Release.
type Update interface {
	// TargetFullRelease is the package the update installs.
	TargetFullRelease() Asset
	// Release frees it; it must not be used afterwards.
	Release()
}

// Manager is Velopack's UpdateManager for this copy.
type Manager interface {
	// UpdatePendingRestart is a package downloaded earlier and not yet
	// applied (get_update_pending_restart).
	UpdatePendingRestart() (Asset, bool)
	// CheckForUpdates asks the feed. The Update is set only for
	// UpdateAvailable.
	CheckForUpdates() (UpdateCheckKind, Update, error)
	// DownloadUpdates fetches the update into the packages folder, through
	// the SignedFeed, reporting progress 0 to 100. Blocking.
	DownloadUpdates(update Update, progress func(int16)) error
	// WaitExitThenApplyUpdates starts the updater, which waits for this
	// process to exit (up to 60 s), applies asset and restarts RC2FX.
	WaitExitThenApplyUpdates(asset Asset, silent, restart bool, args []string) error
}

// ErrorKind is the part of velopack::Error that failure() tells apart.
type ErrorKind int

const (
	// ErrNetwork is velopack::Error::Network.
	ErrNetwork ErrorKind = iota
	// ErrMismatch is ChecksumInvalid or SizeInvalid.
	ErrMismatch
	// ErrOther is anything else; Text is its message.
	ErrOther
)

// Error is a Velopack or download failure.
type Error struct {
	Kind ErrorKind
	Text string
}

func (e *Error) Error() string { return e.Text }

// LibraryError reads the last error velopack_libc reported. The C library
// gives text, not error kinds: its Display of ChecksumInvalid and
// SizeInvalid, and of Network, start with fixed words (velopack 1.3.39's
// lib.rs), and anything after the first line is the cause chain.
func LibraryError(text string) *Error {
	line, _, _ := strings.Cut(strings.TrimSpace(text), "\n")
	line = strings.TrimSpace(line)
	switch {
	case strings.HasPrefix(line, "Checksum did not match for "), strings.HasPrefix(line, "Size did not match for "):
		return &Error{Kind: ErrMismatch, Text: line}
	case strings.HasPrefix(line, "Network error: "):
		return &Error{Kind: ErrNetwork, Text: line}
	}
	if line == "" {
		line = "unknown error"
	}
	return &Error{Kind: ErrOther, Text: line}
}

// Failure is what the user sees for a failed download.
func Failure(err *Error) string {
	switch err.Kind {
	case ErrNetwork:
		return "The update could not be downloaded. Check the internet connection and try again."
	case ErrMismatch:
		return "The downloaded update did not match the release and was discarded. Try again."
	}
	return fmt.Sprintf("The update could not be downloaded (%s).", err.Text)
}

// Checked is how the feed's check of one download went.
type Checked struct {
	Version  string
	FileName string
	// Err is nil when the package passed.
	Err *installer.VerifyError
}

// Checks is the package the feed last checked as it downloaded, shared with
// it.
type Checks struct {
	mu   sync.Mutex
	last *Checked
}

// Record keeps the outcome of checking asset.
func (c *Checks) Record(asset Asset, result *installer.VerifyError) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.last = &Checked{Version: asset.Version, FileName: asset.FileName, Err: result}
}

// Take is how the feed's check of asset went, if it made one, forgetting
// it. found is false when it made none (or checked another package).
func (c *Checks) Take(asset Asset) (result *installer.VerifyError, found bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	checked := c.last
	c.last = nil
	if checked == nil || checked.Version != asset.Version || checked.FileName != asset.FileName {
		return nil, false
	}
	return checked.Err, true
}

// goos is the platform the name rules below follow (cfg!(windows) and
// cfg!(target_os = "macos") in the Rust).
var goos = runtime.GOOS

// PackageName is the file name Velopack stores fileName under in the
// packages folder, which is also the release asset's name. ok is false when
// Velopack would have to change it, so the two could differ.
func PackageName(fileName string) (string, bool) {
	windows := goos == "windows"
	name := fileName
	if i := strings.LastIndexFunc(fileName, func(c rune) bool { return c == '/' || (windows && c == '\\') }); i >= 0 {
		name = fileName[i+1:]
	}
	changed := func(c rune) bool {
		if windows {
			return strings.ContainsRune(`"<>|:*?`, c) || c < 32
		}
		return c == 0
	}
	if name == "" || strings.ContainsFunc(name, changed) {
		return "", false
	}
	return name, true
}

// channel is the part of vpk's package name for this platform (the default
// Windows channel's name has no channel).
func channel() string {
	switch goos {
	case "windows":
		return ""
	case "darwin":
		return "-osx"
	}
	return "-linux"
}

// IsOwnPackage reports whether fileName is exactly this platform's full
// package of version, as vpk names it.
func IsOwnPackage(version, fileName string) bool {
	name, ok := PackageName(fileName)
	return ok && name == fmt.Sprintf("%s-%s%s-full.nupkg", PackID, version, channel())
}

// FullPackages is the feed without delta packages and without other
// platforms' packages: with no deltas Velopack always fetches the whole
// package, which is what the release signs, and every platform's package is
// signed alike, so only its name ties a package to this platform.
func FullPackages(assets []Asset) []Asset {
	kept := assets[:0:0]
	for _, asset := range assets {
		if strings.EqualFold(asset.Type, "Full") && IsOwnPackage(asset.Version, asset.FileName) {
			kept = append(kept, asset)
		}
	}
	return kept
}

// Verify is installer.VerifyUpdatePackage's signature: work folder,
// version, asset name, file. A failure is a *installer.VerifyError.
type Verify func(work, version, asset, file string) error

// asVerifyError is err as the verifier's error; anything else could not be
// checked.
func asVerifyError(err error) *installer.VerifyError {
	if err == nil {
		return nil
	}
	var v *installer.VerifyError
	if errors.As(err, &v) {
		return v
	}
	return &installer.VerifyError{Message: err.Error()}
}

func rejected(message string) *installer.VerifyError {
	return &installer.VerifyError{Rejected: true, Message: message}
}

// VerifyFile checks file, the download of asset, against its signed release
// before it may be applied. work is "" when the working folder is unknown.
// A package the release rejects is removed, so nothing can apply it
// unchecked later. One that could not be checked (offline, say) is kept: it
// may be good, and checking again needs no download.
func VerifyFile(work string, asset Asset, file string, verify Verify) *installer.VerifyError {
	own := IsOwnPackage(asset.Version, asset.FileName)
	name, named := PackageName(asset.FileName)
	var result *installer.VerifyError
	switch {
	case !named || !own:
		result = rejected(fmt.Sprintf("%s is not an update package for this computer.", asset.FileName))
	case work == "":
		result = &installer.VerifyError{Message: "RC2FX could not find its working folder to check this update."}
	default:
		result = asVerifyError(verify(work, asset.Version, name, file))
	}
	if result != nil && result.Rejected {
		os.Remove(file)
	}
	return result
}

// VerifyPackage is VerifyFile for a package in the packages folder.
func VerifyPackage(work, packages string, asset Asset, verify Verify) *installer.VerifyError {
	name, ok := PackageName(asset.FileName)
	if !ok {
		return rejected(fmt.Sprintf("%s is not an app update package.", asset.FileName))
	}
	return VerifyFile(work, asset, filepath.Join(packages, name), verify)
}

// Outcome is what a finished check leaves.
type Outcome struct {
	State State
	// Ready is the package to apply on restart, only when it passed.
	Ready *Asset
	// Retry is the package kept to check again, when no check could be made.
	Retry *Asset
}

// CheckedOutcome is self_update.rs checked.
func CheckedOutcome(asset Asset, result *installer.VerifyError) Outcome {
	switch {
	case result == nil:
		return Outcome{State: Ready(asset.Version), Ready: &asset}
	case !result.Rejected:
		return Outcome{State: Failed(result.Message), Retry: &asset}
	}
	return Outcome{State: Failed(result.Message)}
}

// WorkDirectory is the installer's folder for downloads and backups
// (work_directory in the Rust app's installer/mod.rs), "" when unknown.
// The feed checks packages there, so this package has its own copy.
func WorkDirectory() string {
	if path, ok := os.LookupEnv("RC2FX_INSTALLER_WORK"); ok {
		if filepath.IsAbs(path) {
			return path
		}
		cwd, err := os.Getwd()
		if err != nil {
			return ""
		}
		return filepath.Join(cwd, path)
	}
	work, ok := installer.DefaultWork()
	if !ok {
		return ""
	}
	return work
}
