// Package selfupdate manages in-place desktop updates via Velopack (Windows
// installer, macOS pkg, portable zips, and Linux AppImage). Dev and source
// builds fall back to linking to the GitHub release page.
//
// Updates are strictly user-initiated: after downloading, the user triggers
// "Restart to update" when no installer task is active. Because the Velopack
// release feed itself is unsigned, every full package is verified against
// signed release metadata (appupdate.VerifyUpdatePackage) before Velopack
// moves it into place, and again if found pending at startup.
package selfupdate

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
)

const Feed = "https://github.com/ibanks42/rc2fx/releases/latest/download"

// Verifying is a sentinel progress value emitted once download finishes and
// signature verification begins (Velopack only sends 0..100).
const Verifying int16 = -32768

const PackID = "io.github.ibanks42.rc2fx"

type StateKind int

const (
	StateIdle StateKind = iota
	StateDownloading
	StateVerifying
	StateReady
	StateFailed
)

// State represents the current self-update status. Only fields relevant to
// Kind are populated so values are directly comparable with ==.
type State struct {
	Kind     StateKind
	Percent  uint8
	Version  string
	Message  string
	Asset    Asset
	HasAsset bool
}

func Idle() State { return State{Kind: StateIdle} }

func Downloading(percent uint8) State { return State{Kind: StateDownloading, Percent: percent} }

func VerifyingState(version string) State { return State{Kind: StateVerifying, Version: version} }

func Ready(version string) State { return State{Kind: StateReady, Version: version} }

func ReadyToApply(asset Asset) State {
	return State{Kind: StateReady, Version: asset.Version, Asset: asset, HasAsset: true}
}

func Failed(message string) State { return State{Kind: StateFailed, Message: message} }

func FailedKept(message string, asset Asset) State {
	return State{Kind: StateFailed, Message: message, Asset: asset, HasAsset: true}
}

// Asset matches Velopack's VelopackAsset JSON schema.
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

type UpdateCheckKind int

const (
	UpdateAvailable UpdateCheckKind = iota
	NoUpdateAvailable
	RemoteIsEmpty
)

// Update wraps a Velopack UpdateInfo handle that must be freed via Release.
type Update interface {
	TargetFullRelease() Asset
	Release()
}

// Manager abstracts Velopack's UpdateManager for this installation.
type Manager interface {
	UpdatePendingRestart() (Asset, bool)
	CheckForUpdates() (UpdateCheckKind, Update, error)
	DownloadUpdates(update Update, progress func(int16)) error
	WaitExitThenApplyUpdates(asset Asset, silent, restart bool, args []string) error
}

type ErrorKind int

const (
	ErrNetwork ErrorKind = iota
	ErrMismatch
	ErrOther
)

type Error struct {
	Kind ErrorKind
	Text string
}

func (e *Error) Error() string { return e.Text }

// LibraryError classifies an error string from velopack_libc by its first line,
// matching the Display prefixes in velopack 1.3.39.
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

func Failure(err *Error) string {
	switch err.Kind {
	case ErrNetwork:
		return "The update could not be downloaded. Check the internet connection and try again."
	case ErrMismatch:
		return "The downloaded update did not match the release and was discarded. Try again."
	}
	return fmt.Sprintf("The update could not be downloaded (%s).", err.Text)
}

type Checked struct {
	Version  string
	FileName string
	Err      *appupdate.VerifyError
}

// Checks passes the feed's inline verification result back to the download caller.
type Checks struct {
	mu   sync.Mutex
	last *Checked
}

func (c *Checks) Record(asset Asset, result *appupdate.VerifyError) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.last = &Checked{Version: asset.Version, FileName: asset.FileName, Err: result}
}

func (c *Checks) Take(asset Asset) (result *appupdate.VerifyError, found bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	checked := c.last
	c.last = nil
	if checked == nil || checked.Version != asset.Version || checked.FileName != asset.FileName {
		return nil, false
	}
	return checked.Err, true
}

var goos = runtime.GOOS

// PackageName returns the base filename Velopack stores in the packages folder,
// or ok=false if Velopack would sanitize/alter characters in it.
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

func channel() string {
	switch goos {
	case "windows":
		return ""
	case "darwin":
		return "-osx"
	}
	return "-linux"
}

// IsOwnPackage reports whether fileName matches this platform's full nupkg name.
func IsOwnPackage(version, fileName string) bool {
	name, ok := PackageName(fileName)
	return ok && name == fmt.Sprintf("%s-%s%s-full.nupkg", PackID, version, channel())
}

// FullPackages keeps only full packages for the current platform so Velopack
// never fetches unsigned deltas or another platform's asset.
func FullPackages(assets []Asset) []Asset {
	kept := assets[:0:0]
	for _, asset := range assets {
		if strings.EqualFold(asset.Type, "Full") && IsOwnPackage(asset.Version, asset.FileName) {
			kept = append(kept, asset)
		}
	}
	return kept
}

type Verify func(version, asset, file string) error

func asVerifyError(err error) *appupdate.VerifyError {
	if err == nil {
		return nil
	}
	var v *appupdate.VerifyError
	if errors.As(err, &v) {
		return v
	}
	return &appupdate.VerifyError{Message: err.Error()}
}

func rejected(message string) *appupdate.VerifyError {
	return &appupdate.VerifyError{Rejected: true, Message: message}
}

// VerifyFile checks file against its signed release. Rejected packages are
// deleted immediately; transient verification errors leave the file on disk so
// the user can retry verification without re-downloading.
func VerifyFile(asset Asset, file string, verify Verify) *appupdate.VerifyError {
	own := IsOwnPackage(asset.Version, asset.FileName)
	name, named := PackageName(asset.FileName)
	var result *appupdate.VerifyError
	switch {
	case !named || !own:
		result = rejected(fmt.Sprintf("%s is not an update package for this computer.", asset.FileName))
	default:
		result = asVerifyError(verify(asset.Version, name, file))
	}
	if result != nil && result.Rejected {
		os.Remove(file)
	}
	return result
}

func VerifyPackage(packages string, asset Asset, verify Verify) *appupdate.VerifyError {
	name, ok := PackageName(asset.FileName)
	if !ok {
		return rejected(fmt.Sprintf("%s is not an app update package.", asset.FileName))
	}
	return VerifyFile(asset, filepath.Join(packages, name), verify)
}

type Outcome struct {
	State State
}

func CheckedOutcome(asset Asset, result *appupdate.VerifyError) Outcome {
	switch {
	case result == nil:
		return Outcome{State: ReadyToApply(asset)}
	case !result.Rejected:
		return Outcome{State: FailedKept(result.Message, asset)}
	}
	return Outcome{State: Failed(result.Message)}
}
