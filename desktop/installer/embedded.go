package installer

// The installer payload this binary was built with, and where a payload
// comes from: the executable itself (the desktop app) or a folder (developer
// tools and tests).
//
// The desktop app embeds the payload at build time (the `embedded` build
// tag; cmd/stage-payload checks it with exactly the rules a folder's payload
// passes) so installing needs no runtime, catalog or script files beside it.
// See package embed for the payload's layout and the envelope a binary
// carries.

import (
	"bytes"
	"os"
	"path/filepath"
	"sync"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
)

// Envelope is one verified embedded payload.
type Envelope = embed.Envelope

// Identity is the payload this build embeds.
type Identity struct {
	_ struct{} `serde:"default"`
	// Kind is `embedded` or `folder`.
	Kind string `json:"kind"`
	// Build is what `rc2fx.sh` in this payload says: `release` or
	// `development`.
	Build string `json:"build"`
	// Catalog is the SHA-256 of the payload's `firmware-profiles.json`.
	Catalog string `json:"catalog"`
	// Release is the RC2FX mixer release version from the payload's
	// `mixer-release.json`.
	Release string `json:"release"`
	// Runtime is the digest the prepared firmware records as its RC2FX
	// runtime.
	Runtime  string       `json:"runtime"`
	Files    []FilePin    `json:"files"`
	Profiles []ProfilePin `json:"profiles"`
}

// FilePin is one payload file.
type FilePin struct {
	_      struct{} `serde:"default"`
	Name   string   `json:"name"`
	SHA256 string   `json:"sha256"`
	Size   uint64   `json:"size"`
}

// ProfilePin is one firmware profile the payload's catalog pins.
type ProfilePin struct {
	_              struct{} `serde:"default"`
	ID             string   `json:"id"`
	Model          string   `json:"model"`
	Version        string   `json:"version"`
	MixerSHA256    string   `json:"mixerSha256"`
	LauncherSHA256 string   `json:"launcherSha256"`
	ArchiveSHA256  string   `json:"archiveSha256"`
	Supported      bool     `json:"supported"`
}

type loadedPayload struct {
	bundle   Bundle
	identity Identity
}

var (
	payloadOnce   sync.Once
	payloadResult *loadedPayload
	payloadError  string
)

func payload() (*loadedPayload, error) {
	payloadOnce.Do(func() { payloadResult, payloadError = loadPayload() })
	if payloadResult == nil {
		return nil, Error(payloadError)
	}
	return payloadResult, nil
}

func loadPayload() (*loadedPayload, string) {
	if !embed.Present {
		return nil, embed.Missing
	}
	// The blob is checked when it is used, not only when it was staged: the
	// envelope's framing, every file's checksum, SHA256SUMS, the ARM64 ELFs,
	// the catalog and the build line.
	envelope, err := embed.Find(embed.Blob())
	if err != nil {
		return nil, err.Error()
	}
	bundle, err := envelope.Bundle()
	if err != nil {
		return nil, err.Error()
	}
	identity, err := identityOf("embedded", bundle)
	if err != nil {
		return nil, err.Error()
	}
	return &loadedPayload{bundle: bundle, identity: identity}, ""
}

func identityOf(kind string, bundle Bundle) (Identity, error) {
	catalogData, ok := bundle["firmware-profiles.json"]
	if !ok {
		return Identity{}, Error("the payload has no firmware catalog")
	}
	var catalog Catalog
	if err := sjson.Unmarshal(catalogData, &catalog); err != nil {
		return Identity{}, errorf("the payload catalog is not JSON: %v", err)
	}
	script, ok := bundle["rc2fx.sh"]
	if !ok {
		return Identity{}, Error("the payload has no rc2fx.sh")
	}
	build, err := embed.BuildKind(script)
	if err != nil {
		return Identity{}, Error(err.Error())
	}
	release, err := releaseOf(bundle)
	if err != nil {
		return Identity{}, err
	}
	identity := Identity{
		Kind:     kind,
		Build:    build,
		Catalog:  sum(catalogData),
		Release:  release.Version,
		Runtime:  digest(bundle),
		Files:    []FilePin{},
		Profiles: []ProfilePin{},
	}
	for _, name := range embed.PayloadNames {
		data := bundle[name]
		identity.Files = append(identity.Files, FilePin{Name: name, SHA256: sum(data), Size: uint64(len(data))})
	}
	for i := range catalog.Profiles {
		p := &catalog.Profiles[i]
		identity.Profiles = append(identity.Profiles, ProfilePin{
			ID:             p.ID,
			Model:          p.Model,
			Version:        p.Version,
			MixerSHA256:    p.MixerSHA,
			LauncherSHA256: p.LauncherSHA,
			ArchiveSHA256:  p.ArchiveSHA,
			Supported:      supported(p),
		})
	}
	return identity, nil
}

// embeddedBundle is the embedded payload as a bundle the runtime checks.
func embeddedBundle() (Bundle, error) {
	p, err := payload()
	if err != nil {
		return nil, err
	}
	return p.bundle, nil
}

func catalogBytes() ([]byte, error) {
	if embed.Present {
		p, err := payload()
		if err != nil {
			return nil, err
		}
		return bytes.Clone(p.bundle["firmware-profiles.json"]), nil
	}
	return cachedCatalogBytes()
}

// EmbeddedService is whether RC2FX's state (as the app's USB connection
// reads it) says this payload's runtime is ready, the way the app asks
// about a folder's payload (Rust's embed::service).
func EmbeddedService(state []byte) Service {
	if !embed.Present {
		catalog, err := Source{Kind: SourceDownload}.catalog()
		if err == nil {
			for i := range catalog.Profiles {
				if p := &catalog.Profiles[i]; supported(p) {
					if p.Runtime == nil {
						break
					}
					if why := readinessError(state, p.Runtime); why != "" {
						return Service{Kind: ServiceStarting, Detail: why}
					}
					return Service{Kind: ServiceReady}
				}
			}
		}
		return Service{Kind: ServiceStarting, Detail: "No qualified runtime profile"}
	}
	p, err := payload()
	if err != nil {
		// Fail closed: a payload that cannot be read says why the runtime is
		// not ready. It never passes as ready by judging against an empty
		// profile.
		return Service{Kind: ServiceStarting, Detail: err.Error()}
	}
	return service(p.bundle, state)
}

// EmbeddedIdentity is the identity of the payload this binary serves
// (Rust's embed::identity).
func EmbeddedIdentity() (Identity, error) {
	p, err := payload()
	if err != nil {
		return Identity{}, err
	}
	out := p.identity
	out.Files = append([]FilePin{}, out.Files...)
	out.Profiles = append([]ProfilePin{}, out.Profiles...)
	return out, nil
}

func releaseOf(bundle Bundle) (core.MixerRelease, error) {
	data, ok := bundle[embed.ReleaseManifest]
	if !ok {
		return core.MixerRelease{}, Error("the payload has no mixer-release.json")
	}
	release, err := core.ParseMixerRelease(data)
	if err != nil {
		return core.MixerRelease{}, Error(err.Error())
	}
	return release, nil
}

// BundledRelease is the RC2FX mixer release this binary bundles. An error
// when the build embeds no payload.
func BundledRelease() (core.MixerRelease, error) {
	p, err := payload()
	if err != nil {
		return core.MixerRelease{}, err
	}
	return releaseOf(p.bundle)
}

// NewestSupportedFirmware is the newest RØDE firmware version the bundled
// catalog supports for the same mixer model as firmwareProfile (the
// `firmwareProfile` the device reports), compared numerically. False when
// the build embeds no payload, the profile is not in the bundled catalog, or
// nothing is supported for that model.
func NewestSupportedFirmware(firmwareProfile string) (string, bool) {
	source := Source{Kind: SourceDownload}
	if embed.Present {
		source = Source{Kind: SourceEmbedded}
	}
	catalog, err := source.catalog()
	if err != nil {
		return "", false
	}
	return newestSupported(catalog, firmwareProfile)
}

func newestSupported(catalog *Catalog, firmwareProfile string) (string, bool) {
	model := ""
	found := false
	for i := range catalog.Profiles {
		if catalog.Profiles[i].ID == firmwareProfile {
			model, found = catalog.Profiles[i].Model, true
			break
		}
	}
	if !found {
		return "", false
	}
	newest := ""
	for i := range catalog.Profiles {
		p := &catalog.Profiles[i]
		if p.Model != model || !supported(p) || !appSupports(p, AppVersion) || !comparable(p.Version) {
			continue
		}
		// max_by keeps the last of equal maxima.
		if order, _ := core.CompareVersions(p.Version, newest); newest == "" || order >= 0 {
			newest = p.Version
		}
	}
	return newest, newest != ""
}

// SourceKind is where an installer's payload and catalog come from.
type SourceKind int

const (
	// SourceDownload is the download-only desktop before a signed payload
	// has been selected.
	SourceDownload SourceKind = iota
	// SourceFolder is a release folder or a source checkout: `runtime/` (or
	// `target/mixer`), `mixer/scripts` and `config/`. Developer tools and tests
	// only.
	SourceFolder
	// SourceEmbedded is the payload compiled into this executable.
	SourceEmbedded
	// SourceVerified is an authenticated remote payload, held in memory
	// throughout preparation.
	SourceVerified
)

// Source is where an installer's payload and catalog come from.
type Source struct {
	Kind     SourceKind
	Root     string
	Verified *VerifiedMixerPayload
}

func folderSource(root string) Source { return Source{Kind: SourceFolder, Root: root} }

func (s Source) bundle() (Bundle, error) {
	switch s.Kind {
	case SourceFolder:
		return BundleFiles(s.Root)
	case SourceEmbedded:
		return embeddedBundle()
	case SourceVerified:
		out := make(Bundle, len(s.Verified.bundle))
		for k, v := range s.Verified.bundle {
			out[k] = v
		}
		return out, nil
	}
	return nil, Error("Select and verify a mixer software download before preparing firmware")
}

func (s Source) catalog() (*Catalog, error) {
	switch s.Kind {
	case SourceDownload:
		return cachedCatalog()
	case SourceFolder:
		return catalogFromDisk(s.Root)
	}
	bundle, err := s.bundle()
	if err != nil {
		return nil, err
	}
	data, ok := bundle["firmware-profiles.json"]
	if !ok {
		return nil, Error("the runtime has no firmware catalog")
	}
	return parseCatalog(data)
}

func (s Source) service(state []byte) Service {
	bundle, err := s.bundle()
	if err != nil {
		// Fail closed, as the embedded payload does.
		return Service{Kind: ServiceStarting, Detail: err.Error()}
	}
	return service(bundle, state)
}

func catalogFromDisk(root string) (*Catalog, error) {
	data, err := os.ReadFile(filepath.Join(root, "config", "firmware-profiles.json"))
	if err != nil {
		return nil, err
	}
	return parseCatalog(data)
}

// parseCatalog is what a valid catalog is, for every source: the strict
// rules the mixer's own firmware tool applies, and versions the installer
// can compare. Nothing may reach the installer through a looser reading of
// the same file.
func parseCatalog(data []byte) (*Catalog, error) {
	if !utf8.Valid(data) {
		return nil, Error("invalid utf-8 sequence")
	}
	if _, err := firmware.ParseCatalog(string(data)); err != nil {
		return nil, Error(err.Error())
	}
	var catalog Catalog
	if err := sjson.Unmarshal(data, &catalog); err != nil {
		return nil, err
	}
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; !comparable(p.Version) || !comparable(p.MinimumAppVersion) {
			return nil, Error("invalid firmware catalog profile version")
		}
	}
	return &catalog, nil
}
