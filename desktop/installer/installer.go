// Package installer installs RC2FX on a RØDECaster Pro II.
//
// RC2FX is installed by flashing official RØDE firmware that already
// contains it: the runtime, its autostart and the mixer's saved settings are
// added to the root filesystem image on this computer. The mixer is only
// touched through its own USB update mechanism. The desktop app's USB 1
// connection to RC2FX supplies the settings saved beforehand and confirms it
// is running afterwards.
//
// The work folder is a cache: the prepared firmware, settings backups and a
// log of USB commands sent. Device commands are only sent when explicitly
// requested, and an unacknowledged command is never repeated automatically.
package installer

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sync/atomic"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
)

// AppVersion is the desktop app's version and its only copy in the Go code.
// No build stamps it: .github/scripts/release.py sets it to the workspace
// version in Cargo.toml when it prepares a release, in the commit that
// release builds from.
const AppVersion = "0.1.7"

// DeviceModel is the mixer model.
type DeviceModel = firmware.Model

// HID, Volume, Reports and Platform are the operating system's USB access.
type (
	HID      = platform.HID
	Volume   = platform.Volume
	Reports  = platform.Reports
	Platform = platform.Platform
)

// DuoWarning is shown before anything is done for a Duo.
const DuoWarning = "RØDECaster Duo support is experimental and has NOT been tested on hardware. Flashing, recovery and audio routing may fail, cause loud or incorrect audio, or leave the mixer unusable. Back up your RØDE show, disconnect speakers and headphones before testing, and proceed entirely at your own risk. Only Duo firmware is allowed; never flash Pro II firmware onto a Duo."

// payloadMarker keeps the build identity in the executable's data.
var payloadMarker = payloadModeText

// PayloadMode is the retained build identity used by host package checks
// without launching a GUI. Call it once from the application entrypoint so
// the linker retains the marker.
func PayloadMode() string {
	return payloadMarker
}

const (
	savedName   = "installer.json"
	maxDownload = 600 << 20
	maxSettings = 16 << 20
	keyMember   = "home/root/.ssh/authorized_keys"
)

// Error is an installer error: its text is what the user sees.
type Error string

func (e Error) Error() string { return string(e) }

func errorf(format string, args ...any) error {
	return Error(fmt.Sprintf(format, args...))
}

// message is an error's text, or "" for none.
func message(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

// Release is a firmware release the app can offer.
type Release struct {
	_                 struct{} `serde:"default"`
	MinimumAppVersion string   `json:"minimum_app_version"`
	// Profile is the qualified firmware catalog ID used for signed mixer
	// compatibility.
	Profile   string `json:"profile"`
	Model     string `json:"model"`
	Channel   string `json:"channel"`
	Version   string `json:"version"`
	URL       string `json:"url"`
	DigestURL string `json:"digest_url"`
	Supported bool   `json:"rc2fx_supported"`
}

// Prepared is firmware with RC2FX built in, ready to copy to the mixer.
type Prepared struct {
	_ struct{} `serde:"default"`
	// Model is absent only for legacy Pro II caches; never inferred from the
	// mixer hash.
	Model      *firmware.Model `json:"model"`
	Version    string          `json:"version"`
	Channel    string          `json:"channel"`
	Profile    string          `json:"profile"`
	ArchiveSHA string          `json:"archive_sha"`
	ArchiveMD5 string          `json:"archive_md5"`
	// Key is an SSH key an older installer built in; empty in anything built
	// now. A cached image with one is never reused or flashed.
	Key string `json:"key"`
	// KeySHA is the SHA-256 of that key's authorized_keys line.
	KeySHA string `json:"key_sha"`
	// HostKey is the SHA-256 of the SSH host key line built in with that key.
	HostKey string `json:"host_key"`
	// Runtime is the SHA-256 of the RC2FX runtime built in; empty if none.
	Runtime string `json:"runtime"`
	// Settings is the SHA-256 of the settings built in; empty if none.
	Settings string `json:"settings"`
	// Stock is unchanged RØDE firmware, without RC2FX.
	Stock bool `json:"stock"`
}

// ModelOr is the prepared model, or fallback for a legacy cache.
func (p *Prepared) ModelOr(fallback firmware.Model) firmware.Model {
	if p.Model == nil {
		return fallback
	}
	return *p.Model
}

// Equal compares every field, the model by value; two nils are equal.
func (p *Prepared) Equal(o *Prepared) bool {
	if p == nil || o == nil {
		return p == nil && o == nil
	}
	if (p.Model == nil) != (o.Model == nil) || (p.Model != nil && *p.Model != *o.Model) {
		return false
	}
	a, b := *p, *o
	a.Model, b.Model = nil, nil
	return a == b
}

func (p *Prepared) clone() *Prepared {
	if p == nil {
		return nil
	}
	out := *p
	if p.Model != nil {
		m := *p.Model
		out.Model = &m
	}
	return &out
}

// BackupRecord is a saved copy of a mixer's RC2FX settings (`state.json`).
type BackupRecord struct {
	_       struct{} `serde:"default"`
	Serial  string   `json:"serial"`
	File    string   `json:"file"`
	SHA     string   `json:"sha"`
	Created string   `json:"created"`
}

// CommandRecord is a USB command sent to a mixer. Acknowledged is false
// while the result is unknown; such a command is never sent again
// automatically.
type CommandRecord struct {
	_       struct{} `serde:"default"`
	Command string   `json:"command"`
	Serial  string   `json:"serial"`
	At      string   `json:"at"`
	// AtUnix is seconds since the Unix epoch, for "how long ago" checks.
	AtUnix       uint64 `json:"at_unix"`
	Acknowledged bool   `json:"acknowledged"`
	Error        string `json:"error"`
}

// Saved is the work folder's cache.
type Saved struct {
	_        struct{}        `serde:"default"`
	Prepared *Prepared       `json:"prepared"`
	Backups  []BackupRecord  `json:"backups"`
	Commands []CommandRecord `json:"commands"`
}

// BackupFor is the newest backup for a mixer.
func (s *Saved) BackupFor(serial string) *BackupRecord {
	for i := len(s.Backups) - 1; i >= 0; i-- {
		if serial != "" && s.Backups[i].Serial == serial {
			return &s.Backups[i]
		}
	}
	return nil
}

// LastCommand is the newest command record.
func (s *Saved) LastCommand() *CommandRecord {
	if len(s.Commands) == 0 {
		return nil
	}
	return &s.Commands[len(s.Commands)-1]
}

// InPlaceBundle is a downloaded, verified signed release, ready to send to
// a mixer that installs it in place. The mixer verifies all of it again.
type InPlaceBundle struct {
	Version string `json:"version"`
	Size    uint64 `json:"size"`
	SHA256  string `json:"sha256"`
	// Path is the payload file, in this app's own cache.
	Path     string      `json:"path"`
	Metadata RawMetadata `json:"metadata"`
	// Base is the signed payload of the release the mixer runs, when one
	// exists: the upload then carries only what changed.
	Base *string `json:"base,default"`
}

// USBMixer is one connected mixer.
type USBMixer struct {
	Model      DeviceModel `json:"model"`
	Serial     string      `json:"serial"`
	UpdateMode bool        `json:"update_mode"`
	// Firmware is the RØDE firmware this mixer runs, from its USB release
	// number. Read without RC2FX, so it is known on stock firmware too. Nil
	// in update mode, where the number may not be the installed firmware's.
	Firmware *string `json:"firmware,default"`
}

// ServiceKind says whether RC2FX is ready.
type ServiceKind int

const (
	// ServiceReady is running with advancing native audio and its settings
	// applied.
	ServiceReady ServiceKind = iota
	// ServiceStarting is answering, but its audio is not ready yet; the
	// detail says why.
	ServiceStarting
)

// Service is what RC2FX reports over USB 1, as the desktop app's
// connection sees it.
type Service struct {
	Kind   ServiceKind
	Detail string
}

type startingBody struct {
	Detail string `json:"detail"`
}

// MarshalJSON writes serde's externally tagged form.
func (s Service) MarshalJSON() ([]byte, error) {
	if s.Kind == ServiceReady {
		return []byte(`"Ready"`), nil
	}
	return tagged("Starting", startingBody{Detail: s.Detail})
}

// UnmarshalJSON reads serde's externally tagged form.
func (s *Service) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	switch {
	case variant == "Ready" && payload == nil:
		*s = Service{Kind: ServiceReady}
	case variant == "Starting" && payload != nil:
		var body startingBody
		if err := sjson.Unmarshal(payload, &body); err != nil {
			return err
		}
		*s = Service{Kind: ServiceStarting, Detail: body.Detail}
	default:
		return fmt.Errorf("unknown variant `%s`, expected `Ready` or `Starting`", variant)
	}
	return nil
}

// Status is what a probe found.
type Status struct {
	USB      []USBMixer `json:"usb"`
	USBError *string    `json:"usb_error"`
	Saved    Saved      `json:"saved"`
	// PreparedPresent is whether the prepared firmware's files are present
	// (not re-hashed).
	PreparedPresent bool `json:"prepared_present"`
	// UpdateDrive is whether the update drive of a mixer in USB update mode
	// is visible.
	UpdateDrive bool `json:"update_drive"`
	// RecoveryDisk is the size of a mixer's whole eMMC while its bootloader
	// shows it as a USB disk, for RequestRepairInspect.
	RecoveryDisk *uint64 `json:"recovery_disk,default"`
}

// Progress is reported synchronously on the action thread. Total is zero
// when unknown.
type Progress struct {
	_         struct{} `serde:"default"`
	Phase     string   `json:"phase"`
	Completed uint64   `json:"completed"`
	Total     uint64   `json:"total"`
	Detail    string   `json:"detail"`
	// Flash and Carrier say which of the storage fix's flashes a
	// StorageFixPhase event belongs to: its number from 1, and whether it
	// carries the fix's script. Flash is 0 outside a flash.
	Flash   int  `json:"flash"`
	Carrier bool `json:"carrier"`
}

// Canceled is the error of a request stopped through Engine.Cancel.
const Canceled = "Canceled."

// Engine runs installer requests in one work folder.
type Engine struct {
	// source is the payload and catalog this engine installs from: the app's
	// own embedded copy, or a folder for developer tools and tests.
	source   Source
	Work     string
	Platform Platform
	Progress func(Progress)
	// Cancel is set from another goroutine to stop a download; the request
	// then fails with Canceled. Cleared when the next request starts.
	Cancel *atomic.Bool
	http   *http.Client
	model  DeviceModel
	// allowExperimentalDuo is the session policy the desktop sends.
	allowExperimentalDuo bool
	// anchorDir stands in for the helper's root-owned folder in tests.
	anchorDir *string
}

// DownloadOnly is the download-only desktop installer; no checkout or
// bundled runtime fallback.
func DownloadOnly(work string) *Engine {
	return withSource(Source{Kind: SourceDownload}, work)
}

// Verified prepares firmware from authenticated, owned download bytes. The
// flashing mechanism is identical to the embedded payload path.
func Verified(payload *VerifiedMixerPayload, work string) *Engine {
	return withSource(Source{Kind: SourceVerified, Verified: payload}, work)
}

// New is an engine for the installer folder root, caching in work.
func New(root, work string) *Engine {
	return withSource(folderSource(root), work)
}

// WithPlatform is New with a given platform.
func WithPlatform(root, work string, p Platform) *Engine {
	return withSourcePlatform(folderSource(root), work, p)
}

func withSource(source Source, work string) *Engine {
	return withSourcePlatform(source, work, platform.Host())
}

func withSourcePlatform(source Source, work string, p Platform) *Engine {
	return &Engine{
		source:   source,
		Work:     work,
		Platform: p,
		Cancel:   new(atomic.Bool),
		model:    firmware.ModelProIi,
	}
}

func (e *Engine) canceled() bool {
	return e.Cancel != nil && e.Cancel.Load()
}

func (e *Engine) emit(event Progress) {
	if e.Progress != nil {
		e.Progress(event)
	}
}

// reporter is a byte-progress callback for one phase, reporting about
// every MiB.
func (e *Engine) reporter(phase, detail string) func(completed, total uint64) {
	var last *uint64
	return func(completed, total uint64) {
		if last == nil || completed >= *last+(1<<20) || completed == total {
			c := completed
			last = &c
			e.emit(Progress{Phase: phase, Completed: completed, Total: total, Detail: detail})
		}
	}
}

func (e *Engine) step(phase, detail string) {
	e.emit(Progress{Phase: phase, Detail: detail})
}

// Handle runs one request.
func (e *Engine) Handle(request Request) (Reply, error) {
	if err := privateDirs(e.Work); err != nil {
		return Reply{}, err
	}
	// Two installers must not interleave USB commands or cache writes.
	lock, err := openLocalFile(filepath.Join(e.Work, "installer.lock"), false)
	if err != nil {
		return Reply{}, err
	}
	defer lock.Close()
	unlock, err := lockAction(lock)
	if err != nil {
		return Reply{}, Error("another RC2FX installer is working with this mixer folder")
	}
	defer unlock()
	if e.source.Kind == SourceDownload {
		kind := request.Kind
		// The fix checks the prepared firmware only when it flashes it.
		if kind == RequestStorageFix && request.Final {
			kind = RequestFlash
		}
		switch kind {
		case RequestReleases, RequestPrepare, RequestVerify, RequestEnterUSBMode, RequestFlash:
			work := e.Work
			e.step("firmware-check", "Checking signed firmware profiles")
			if _, err := mixerOperation(e, func(ctx context.Context) (struct{}, error) {
				repository, err := official(ctx, work)
				if err != nil {
					return struct{}{}, err
				}
				return struct{}{}, refreshCatalog(ctx, repository)
			}); err != nil {
				return Reply{}, err
			}
		}
	}
	switch request.Kind {
	case RequestAllowExperimentalDuo:
		e.allowExperimentalDuo = request.Allow
		status, err := e.probe()
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyStatus, Status: status}, nil
	case RequestProbe:
		status, err := e.probe()
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyStatus, Status: status}, nil
	case RequestReleases:
		releases, err := e.releases()
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyReleases, Releases: releases}, nil
	case RequestStockReleases:
		e.step("stock-check", "Checking RØDE's firmware list")
		releases, err := e.stockReleases()
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyStockReleases, Releases: releases}, nil
	case RequestMixerReleases:
		e.step("mixer-check", "Checking signed mixer releases")
		work := e.Work
		offers, err := mixerOperation(e, func(ctx context.Context) ([]MixerOffer, error) {
			repository, err := official(ctx, work)
			if err != nil {
				return nil, err
			}
			if err := refreshCatalog(ctx, repository); err != nil {
				return nil, err
			}
			return repository.Offers()
		})
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyMixerReleases, Offers: offers}, nil
	case RequestSelectMixer:
		e.step("mixer-download", "Downloading and verifying mixer software")
		// An unsuccessful selection cannot leave an old payload silently selected.
		e.source = Source{Kind: SourceDownload}
		work := e.Work
		offer, profile := request.Offer, request.FirmwareProfile
		if offer == nil {
			return Reply{}, Error("missing mixer offer")
		}
		payload, err := mixerOperation(e, func(ctx context.Context) (*VerifiedMixerPayload, error) {
			repository, err := official(ctx, work)
			if err != nil {
				return nil, err
			}
			dir, err := metadataDirectory(work)
			if err != nil {
				return nil, err
			}
			return repository.AcceptCached(ctx, offer, AppVersion, core.MixerProtocolVersion, profile, filepath.Join(dir, "payloads"))
		})
		if err != nil {
			return Reply{}, err
		}
		identity, err := identityOf("download", payload.bundle)
		if err != nil {
			return Reply{}, Error(err.Error())
		}
		e.source = Source{Kind: SourceVerified, Verified: payload}
		return Reply{Kind: ReplyMixerSelected, Identity: &identity}, nil
	case RequestInPlaceBundle:
		e.step("mixer-download", "Downloading and verifying mixer software")
		work := e.Work
		offer, profile := request.Offer, request.FirmwareProfile
		if offer == nil {
			return Reply{}, Error("missing mixer offer")
		}
		dir, err := metadataDirectory(work)
		if err != nil {
			return Reply{}, err
		}
		cache := filepath.Join(dir, "payloads")
		if _, err := mixerOperation(e, func(ctx context.Context) (*VerifiedMixerPayload, error) {
			repository, err := official(ctx, work)
			if err != nil {
				return nil, err
			}
			return repository.AcceptCached(ctx, offer, AppVersion, core.MixerProtocolVersion, profile, cache)
		}); err != nil {
			return Reply{}, err
		}
		e.step("mixer-metadata", "Reading the release's signed metadata")
		metadata, err := FetchRawMetadata()
		if err != nil {
			return Reply{}, err
		}
		// Only an optimization: without it the whole release is sent.
		var base *string
		if request.Installed != nil {
			version := *request.Installed
			e.step("mixer-base", "Downloading the mixer's current release")
			found, err := mixerOperation(e, func(ctx context.Context) (*string, error) {
				repository, err := official(ctx, work)
				if err != nil {
					return nil, err
				}
				return repository.BasePayload(ctx, version, cache)
			})
			switch {
			case err != nil && err.Error() == Canceled:
				return Reply{}, err
			case err == nil:
				base = found
			}
		}
		bundle := &InPlaceBundle{
			Version:  offer.Metadata.Version,
			Size:     offer.Length,
			SHA256:   offer.SHA256,
			Path:     filepath.Join(cache, offer.SHA256+".payload"),
			Metadata: *metadata,
			Base:     base,
		}
		return Reply{Kind: ReplyInPlaceBundle, InPlaceBundle: bundle}, nil
	case RequestSelectBundledMixer:
		// Match downloaded selection: failure must discard the old choice.
		e.source = Source{Kind: SourceDownload}
		identity, err := EmbeddedIdentity()
		if err != nil {
			return Reply{}, err
		}
		e.source = Source{Kind: SourceEmbedded}
		return Reply{Kind: ReplyMixerSelected, Identity: &identity}, nil
	case RequestSaveSettings:
		record, err := e.saveSettings(request.Serial, request.Settings)
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyBackup, Backup: record}, nil
	case RequestPrepare:
		// The helper is otherwise probed only when it starts, and the image is
		// built for the mixer connected now.
		if _, err := e.probe(); err != nil {
			return Reply{}, err
		}
		prepared, err := e.prepare(request.Channel, request.Version, request.Serial, request.Stock)
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyPrepared, Prepared: prepared}, nil
	case RequestVerify:
		saved, err := e.load()
		if err != nil {
			return Reply{}, err
		}
		if saved.Prepared == nil {
			return Reply{}, Error("there is no prepared firmware to check")
		}
		prepared := saved.Prepared.clone()
		if err := e.verifyPrepared(prepared); err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyPrepared, Prepared: prepared}, nil
	case RequestEnterUSBMode:
		if err := e.enterUSBMode(request.Serial); err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyDone}, nil
	case RequestFlash:
		if err := e.flash(request.Serial); err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyDone}, nil
	case RequestRepairInspect:
		findings, err := e.repairInspect()
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyRepairFindings, Findings: findings}, nil
	case RequestRepair:
		result, err := e.repair(request.Files)
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyRepaired, Repaired: result}, nil
	case RequestStorageFix:
		fix, err := e.storageFix(request.Final, request.FixDebug && fixDebugAllowed)
		if err != nil {
			return Reply{}, err
		}
		return Reply{Kind: ReplyStorageFix, StorageFix: fix}, nil
	}
	return Reply{}, errorf("unknown request %d", request.Kind)
}

func (e *Engine) load() (*Saved, error) {
	return LoadSaved(e.Work)
}

func (e *Engine) save(saved *Saved) error {
	data, err := sjson.MarshalPretty(saved)
	if err != nil {
		return err
	}
	data = append(data, '\n')
	return atomicFile(filepath.Join(e.Work, savedName), data)
}

func (e *Engine) probe() (*Status, error) {
	saved, err := e.load()
	if err != nil {
		return nil, err
	}
	var usb []USBMixer
	var usbError *string
	items, err := e.Platform.Inventory()
	if err != nil {
		usb, usbError = []USBMixer{}, new(err.Error())
	} else {
		usb = mixers(items)
		if len(usb) == 1 {
			e.model = usb[0].Model
		}
		if len(usb) > 1 {
			usbError = new("More than one RØDECaster is connected; connect only one.")
		} else if err := rejectExperimental(items, "", e.allowExperimentalDuo); err != nil {
			usbError = new(err.Error())
		}
	}
	updateDrive := false
	var driveError *string
	if slices.ContainsFunc(usb, func(m USBMixer) bool { return m.UpdateMode }) {
		volumes, err := e.Platform.Volumes()
		if err != nil {
			driveError = new(err.Error())
		} else {
			updateDrive = slices.ContainsFunc(volumes, func(v Volume) bool {
				return updateSize(v.Total) && slices.ContainsFunc(usb, func(m USBMixer) bool {
					return m.UpdateMode && m.Serial == v.Serial
				})
			})
		}
	}
	if usbError == nil {
		usbError = driveError
	}
	present := saved.Prepared != nil &&
		exists(filepath.Join(e.Work, "archive.tar.gz")) &&
		exists(filepath.Join(e.Work, "archive.md5"))
	// A failed look leaves the repair page waiting; it is not an installer
	// error.
	recoveryDisk, err := platform.RecoveryDiskOf(e.Platform)
	if err != nil {
		recoveryDisk = nil
	}
	return &Status{
		USB:             usb,
		USBError:        usbError,
		Saved:           *saved,
		PreparedPresent: present,
		UpdateDrive:     updateDrive,
		RecoveryDisk:    recoveryDisk,
	}, nil
}

// LoadSaved reads the cache; empty if there is none yet.
func LoadSaved(work string) (*Saved, error) {
	file, err := openWorkFile(filepath.Join(work, savedName))
	if errors.Is(err, os.ErrNotExist) {
		return &Saved{}, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := io.ReadAll(file)
	if err != nil {
		return nil, err
	}
	var saved Saved
	if err := sjson.Unmarshal(data, &saved); err != nil {
		return nil, err
	}
	return &saved, nil
}

func nowUnix() uint64 {
	now := time.Now().Unix()
	if now < 0 {
		return 0
	}
	return uint64(now)
}

func utc() string {
	return time.Unix(int64(nowUnix()), 0).UTC().Format("2006-01-02T15:04:05Z")
}

// DefaultKey is where the developer SSH key lives.
func DefaultKey() string {
	home, _ := homeDir()
	return filepath.Join(home, ".ssh", "rc2fx_rodecaster_pro_ii")
}

// DefaultWork is the installer's cache: progress, downloads and backups. It
// lives in the user's data folder, so it survives moving or replacing the
// app.
func DefaultWork() (string, bool) {
	var base string
	switch runtime.GOOS {
	case "windows":
		base = os.Getenv("LOCALAPPDATA")
		if base == "" || !filepath.IsAbs(base) {
			return "", false
		}
	case "darwin":
		home, ok := homeDir()
		if !ok {
			return "", false
		}
		base = filepath.Join(home, "Library/Application Support")
	default:
		base = os.Getenv("XDG_DATA_HOME")
		if base == "" || !filepath.IsAbs(base) {
			home, ok := homeDir()
			if !ok {
				return "", false
			}
			base = filepath.Join(home, ".local/share")
		}
	}
	return filepath.Join(base, "RC2FX", "installer"), true
}

func tagged(variant string, payload any) ([]byte, error) {
	body, err := sjson.Marshal(payload)
	if err != nil {
		return nil, err
	}
	out := append([]byte{'{'}, sjson.String(variant)...)
	out = append(out, ':')
	out = append(out, body...)
	return append(out, '}'), nil
}
