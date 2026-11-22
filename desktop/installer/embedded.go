package installer

// Embedded and folder-backed installer payload loading.
// Desktop builds compiled with `-tags embedded` carry the staged payload
// directly in the binary (see package embed).

import (
	"bytes"
	"os"
	"path/filepath"
	"sync"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

type Envelope = embed.Envelope

// Identity summarizes an installer payload's build metadata and pinned hashes.
type Identity struct {
	_        struct{}     `serde:"default"`
	Kind     string       `json:"kind"`    // "embedded" or "folder"
	Build    string       `json:"build"`   // "release" or "development"
	Catalog  string       `json:"catalog"` // SHA-256 of firmware-profiles.json
	Release  string       `json:"release"` // version from mixer-release.json
	Runtime  string       `json:"runtime"`
	Files    []FilePin    `json:"files"`
	Profiles []ProfilePin `json:"profiles"`
}

type FilePin struct {
	_      struct{} `serde:"default"`
	Name   string   `json:"name"`
	SHA256 string   `json:"sha256"`
	Size   uint64   `json:"size"`
}

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
	// Re-verify the embedded envelope at runtime, not just at stage time.
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
		Catalog:  common.SHA256Hex(catalogData),
		Release:  release.Version,
		Runtime:  digest(bundle),
		Files:    []FilePin{},
		Profiles: []ProfilePin{},
	}
	for _, name := range embed.PayloadNames {
		data := bundle[name]
		identity.Files = append(identity.Files, FilePin{Name: name, SHA256: common.SHA256Hex(data), Size: uint64(len(data))})
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
	return defaultCatalogCache.Bytes()
}

// EmbeddedService checks mixer state JSON against the bundled (or downloaded)
// runtime profile to report whether the RC2FX service is ready.
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
		// Fail closed if the embedded payload cannot be loaded.
		return Service{Kind: ServiceStarting, Detail: err.Error()}
	}
	return service(p.bundle, state)
}

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

// BundledRelease returns the mixer release embedded in this binary, or an error
// for download-only builds.
func BundledRelease() (core.MixerRelease, error) {
	p, err := payload()
	if err != nil {
		return core.MixerRelease{}, err
	}
	return releaseOf(p.bundle)
}

// NewestSupportedFirmware returns the highest supported RØDE firmware version
// in the catalog for the same mixer model as firmwareProfile.
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
		if p.Model != model || !supported(p) || !appSupports(p, AppVersion) || !core.ValidVersion(p.Version) {
			continue
		}
		// Keep the last entry on ties.
		if order, _ := core.CompareVersions(p.Version, newest); newest == "" || order >= 0 {
			newest = p.Version
		}
	}
	return newest, newest != ""
}

type SourceKind int

const (
	SourceDownload SourceKind = iota // download-only build before a signed payload is chosen
	SourceFolder                     // checkout or release folder (dev tools and tests)
	SourceEmbedded                   // payload compiled into this binary
	SourceVerified                   // TUF-verified remote payload held in memory
)

// Source identifies where the installer gets its payload and firmware catalog.
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
	return s.catalogFrom(&defaultCatalogCache)
}

func (s Source) catalogFrom(cache *CatalogCache) (*Catalog, error) {
	switch s.Kind {
	case SourceDownload:
		return cache.Catalog()
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

// parseCatalog applies the same strict validation as firmware.ParseCatalog
// and checks that all profile versions are comparable.
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
		if p := &catalog.Profiles[i]; !core.ValidVersion(p.Version) || !core.ValidVersion(p.MinimumAppVersion) {
			return nil, Error("invalid firmware catalog profile version")
		}
	}
	return &catalog, nil
}
