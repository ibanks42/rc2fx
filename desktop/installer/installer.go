// Package installer patches official RØDE firmware archives with the RC2FX
// runtime, autostart shims, and backed-up settings, then flashes them through
// the mixer's USB update mechanism.
//
// The work directory caches prepared firmware, settings backups, and a USB
// command log. Unacknowledged device commands are never retried automatically.
package installer

import (
	"context"
	"errors"
	"fmt"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sync/atomic"
	"time"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

// AppVersion is kept in sync with config/app-release.json by
// .github/scripts/release.py.
const AppVersion = "0.2.4"

type DeviceModel = firmware.Model

type (
	HID      = platform.HID
	Volume   = platform.Volume
	Reports  = platform.Reports
	Platform = platform.Platform
)

var payloadMarker = payloadModeText

// PayloadMode returns the build's payload-mode marker.
func PayloadMode() string {
	return payloadMarker
}

// KeepPayloadMode keeps the payload-mode marker in the executable, where
// `go -C tools/xtask run . package` looks for it. Call it once from main. A discarded call
// such as `_ = PayloadMode()` is inlined and removed by the compiler, and the
// linker then drops the marker.
func KeepPayloadMode() {
	runtime.KeepAlive(payloadMarker)
}

const (
	savedName   = "installer.json"
	maxDownload = 600 << 20
	maxSettings = 16 << 20
	keyMember   = "home/root/.ssh/authorized_keys"
)

// Error is a user-facing installer error.
type Error string

func (e Error) Error() string { return string(e) }

func errorf(format string, args ...any) error {
	return Error(fmt.Sprintf(format, args...))
}

func message(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

type Release struct {
	_                 struct{} `serde:"default"`
	MinimumAppVersion string   `json:"minimum_app_version"`
	Profile           string   `json:"profile"`
	Model             string   `json:"model"`
	Channel           string   `json:"channel"`
	Version           string   `json:"version"`
	URL               string   `json:"url"`
	DigestURL         string   `json:"digest_url"`
	Supported         bool     `json:"rc2fx_supported"`
}

// Prepared describes a prepared firmware archive in the work directory.
type Prepared struct {
	_ struct{} `serde:"default"`
	// Model is nil only for legacy Pro II caches.
	Model      *firmware.Model `json:"model"`
	Version    string          `json:"version"`
	Channel    string          `json:"channel"`
	Profile    string          `json:"profile"`
	ArchiveSHA string          `json:"archive_sha"`
	ArchiveMD5 string          `json:"archive_md5"`
	// Legacy SSH fields from older installers; cached images with these set
	// are refused.
	Key      string `json:"key"`
	KeySHA   string `json:"key_sha"`
	HostKey  string `json:"host_key"`
	Runtime  string `json:"runtime"`
	Settings string `json:"settings"`
	Stock    bool   `json:"stock"`
}

func (p *Prepared) ModelOr(fallback firmware.Model) firmware.Model {
	if p.Model == nil {
		return fallback
	}
	return *p.Model
}

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

type BackupRecord struct {
	_       struct{} `serde:"default"`
	Serial  string   `json:"serial"`
	File    string   `json:"file"`
	SHA     string   `json:"sha"`
	Created string   `json:"created"`
}

// CommandRecord logs a USB command sent to a mixer. Unacknowledged commands
// are never retried automatically.
type CommandRecord struct {
	_            struct{} `serde:"default"`
	Command      string   `json:"command"`
	Serial       string   `json:"serial"`
	At           string   `json:"at"`
	AtUnix       uint64   `json:"at_unix"`
	Acknowledged bool     `json:"acknowledged"`
	Error        string   `json:"error"`
}

type Saved struct {
	_        struct{}        `serde:"default"`
	Prepared *Prepared       `json:"prepared"`
	Backups  []BackupRecord  `json:"backups"`
	Commands []CommandRecord `json:"commands"`
}

func (s *Saved) BackupFor(serial string) *BackupRecord {
	for i := len(s.Backups) - 1; i >= 0; i-- {
		if serial != "" && s.Backups[i].Serial == serial {
			return &s.Backups[i]
		}
	}
	return nil
}

func (s *Saved) LastCommand() *CommandRecord {
	if len(s.Commands) == 0 {
		return nil
	}
	return &s.Commands[len(s.Commands)-1]
}

// InPlaceBundle is a verified signed release staged for an in-place mixer update.
// The mixer re-verifies all metadata and hashes on-device.
type InPlaceBundle struct {
	Version  string      `json:"version"`
	Size     uint64      `json:"size"`
	SHA256   string      `json:"sha256"`
	Path     string      `json:"path"`
	Metadata RawMetadata `json:"metadata"`
	// Optional base payload matching the mixer's current release for delta uploads.
	Base *string `json:"base,default"`
}

type USBMixer struct {
	Model      DeviceModel `json:"model"`
	Serial     string      `json:"serial"`
	UpdateMode bool        `json:"update_mode"`
	// USB bcdDevice firmware version; nil in update mode where it may not
	// reflect the installed OS.
	Firmware *string `json:"firmware,default"`
}

type ServiceKind int

const (
	ServiceReady ServiceKind = iota
	ServiceStarting
)

// Service reports whether RC2FX on the mixer is ready or still starting.
type Service struct {
	Kind   ServiceKind
	Detail string
}

type startingBody struct {
	Detail string `json:"detail"`
}

func (s Service) MarshalJSON() ([]byte, error) {
	if s.Kind == ServiceReady {
		return []byte(`"Ready"`), nil
	}
	return tagged("Starting", startingBody{Detail: s.Detail})
}

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

type Status struct {
	USB             []USBMixer `json:"usb"`
	USBError        *string    `json:"usb_error"`
	Saved           Saved      `json:"saved"`
	PreparedPresent bool       `json:"prepared_present"` // files exist on disk (not re-hashed)
	UpdateDrive     bool       `json:"update_drive"`
	// Bootloader eMMC disk size in bytes when visible for RequestRepairInspect.
	RecoveryDisk *uint64 `json:"recovery_disk,default"`
}

// Progress is emitted synchronously during installer operations (Total == 0 if unknown).
type Progress struct {
	_         struct{} `serde:"default"`
	Phase     string   `json:"phase"`
	Completed uint64   `json:"completed"`
	Total     uint64   `json:"total"`
	Detail    string   `json:"detail"`
	// Storage-fix flash index (1-based, 0 outside a flash) and whether it carries the fix script.
	Flash   int  `json:"flash"`
	Carrier bool `json:"carrier"`
}

const Canceled = "Canceled."

// Engine executes installer requests against a single work directory.
type Engine struct {
	source               Source
	Work                 string
	Platform             Platform
	Progress             func(Progress)
	Cancel               *atomic.Bool // set from another goroutine to cancel downloads
	http                 *http.Client
	model                DeviceModel
	allowExperimentalDuo bool
	anchorDir            *string // test override for root-owned anchor directory
	catalogs             *CatalogCache
}

func (e *Engine) catalogCache() *CatalogCache {
	if e.catalogs == nil {
		e.catalogs = new(CatalogCache)
	}
	return e.catalogs
}

func DownloadOnly(work string) *Engine {
	return withSource(Source{Kind: SourceDownload}, work)
}

func New(root, work string) *Engine {
	return withSource(folderSource(root), work)
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
		catalogs: new(CatalogCache),
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

// reporter emits byte progress roughly every 1 MiB and at completion.
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

func (e *Engine) Handle(request Request) (Reply, error) {
	if err := privateDirs(e.Work); err != nil {
		return Reply{}, err
	}
	// Serialize USB commands and cache writes across concurrent installer instances.
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
				return struct{}{}, e.refreshCatalog(ctx, repository)
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
			if err := e.refreshCatalog(ctx, repository); err != nil {
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
		// Clear any previously selected payload so a failed selection can't leave it active.
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
		// Optional delta base; if unavailable, the full payload is uploaded.
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
		// Re-probe so e.model reflects the currently connected mixer.
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
	return archive.AtomicFile(filepath.Join(e.Work, savedName), data)
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
		common.PathExists(filepath.Join(e.Work, "archive.tar.gz")) &&
		common.PathExists(filepath.Join(e.Work, "archive.md5"))
	// Non-fatal: recovery disk absence just leaves the repair view waiting.
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

func LoadSaved(work string) (*Saved, error) {
	file, err := archive.OpenWorkFile(filepath.Join(work, savedName))
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

func utc() string {
	return time.Unix(int64(common.NowUnix()), 0).UTC().Format("2006-01-02T15:04:05Z")
}

// DefaultKey returns the default path for the developer SSH key.
func DefaultKey() string {
	home, _ := homeDir()
	return filepath.Join(home, ".ssh", "rc2fx_rodecaster_pro_ii")
}

// DefaultWork returns the per-user installer cache directory.
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
