package installer

// Verified firmware catalog cache, raw published TUF metadata assembly for
// in-place mixer updates, and official repository endpoint validation.

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/installer/trust"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/internal/tuf"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

// CatalogCache caches the most recently verified signed firmware catalog and
// rejects rollbacks or silent content changes within the same metadata version.
type CatalogCache struct {
	mu       sync.Mutex
	verified *VerifiedCatalog
}

var defaultCatalogCache CatalogCache

// VerifiedCatalog is a firmware catalog the signed metadata names.
type VerifiedCatalog struct {
	catalog        Catalog
	bytes          []byte
	rootVersion    int64
	targetsVersion int64
	expires        time.Time
}

// Catalog is the verified catalog while its authorization lasts.
func (v *VerifiedCatalog) Catalog() (*Catalog, error) {
	if !time.Now().Before(v.expires) {
		return nil, mixerError("firmware catalog authorization expired; check for updates again")
	}
	return &v.catalog, nil
}

// Catalog returns a clone of the cached catalog while its authorization lasts.
func (c *CatalogCache) Catalog() (*Catalog, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.verified == nil {
		return nil, mixerError("check signed firmware releases before installing")
	}
	cat, err := c.verified.Catalog()
	if err != nil {
		return nil, err
	}
	out := cat.Clone()
	return &out, nil
}

// Bytes returns a copy of the cached catalog's raw bytes while authorized.
func (c *CatalogCache) Bytes() ([]byte, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.verified == nil {
		return nil, mixerError("check signed firmware releases before inspecting firmware")
	}
	if _, err := c.verified.Catalog(); err != nil {
		return nil, err
	}
	return bytes.Clone(c.verified.bytes), nil
}

// Store records a newly verified catalog, rejecting rollbacks or mutations
// under the same metadata version.
func (c *CatalogCache) Store(catalog *VerifiedCatalog) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if previous := c.verified; previous != nil {
		older := catalog.rootVersion < previous.rootVersion ||
			(catalog.rootVersion == previous.rootVersion && catalog.targetsVersion < previous.targetsVersion)
		same := catalog.rootVersion == previous.rootVersion && catalog.targetsVersion == previous.targetsVersion
		if older || (same && !bytes.Equal(previous.bytes, catalog.bytes)) {
			return mixerError("firmware catalog is older than the latest verified check or changed under the same metadata version")
		}
	}
	c.verified = catalog
	return nil
}

func (e *Engine) refreshCatalog(ctx context.Context, repository *MixerRepository) error {
	catalog, err := repository.VerifiedCatalog(ctx)
	if err != nil {
		return err
	}
	if err := e.catalogCache().Store(catalog); err != nil {
		return err
	}
	// Also mirror into the process-level fallback used by EmbeddedService
	// in download-only builds when no Engine handle is in scope.
	_ = defaultCatalogCache.Store(catalog)
	return nil
}

const (
	metadataHost = "raw.githubusercontent.com"
	targetsHost  = "github.com"
	metadataURL  = "https://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/"
	targetsURL   = "https://github.com/ibanks42/rc2fx/releases/download/"
)

// trustedRoot is the compiled-in root.
var trustedRoot = trust.Root()

// MixerOffer is an exact signed offer, safe to pass between the app and
// helper. The helper still retrieves trusted metadata and checks every field
// before accepting it.
type MixerOffer struct {
	_        struct{}    `serde:"deny_unknown_fields"`
	Target   string      `json:"target"`
	SHA256   string      `json:"sha256"`
	Length   uint64      `json:"length"`
	Metadata MixerTarget `json:"metadata"`
}

// Equal compares every field, the metadata by MixerTarget.Equal.
func (o *MixerOffer) Equal(other *MixerOffer) bool {
	return o.Target == other.Target && o.SHA256 == other.SHA256 && o.Length == other.Length && o.Metadata.Equal(&other.Metadata)
}

// RawMetadata is a release's signed metadata exactly as published, for a
// mixer that checks it itself (an in-place update). Each role is its
// file's text: the snapshot and timestamp sign the bytes of the files they
// list.
type RawMetadata struct {
	_ struct{} `serde:"deny_unknown_fields"`
	// Root is every root, oldest first: the mixer follows a key rotation from
	// the root it was built with.
	Root      []string `json:"root"`
	Timestamp string   `json:"timestamp"`
	Snapshot  string   `json:"snapshot"`
	Targets   string   `json:"targets"`
}

const (
	rawMetadataLimit = 1 << 20
	maxRoots         = 8
)

// FetchRawMetadata fetches the published role files byte for byte. The
// mixer verifies them against its own trusted root; this
// only makes sure they belong together, retrying when a release was
// published between two reads.
func FetchRawMetadata() (*RawMetadata, error) {
	if folder, ok := devRepository(); ok {
		return assemble(func(name string) (*string, error) {
			data, err := os.ReadFile(filepath.Join(folder, "metadata", name))
			if errors.Is(err, fs.ErrNotExist) {
				return nil, nil
			}
			if err != nil {
				return nil, err
			}
			text := string(data)
			return &text, nil
		})
	}
	base, err := url.Parse(metadataURL)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	if err := checkRepositoryURL(base, metadataHost); err != nil {
		return nil, err
	}
	client := &http.Client{
		Timeout: 60 * time.Second,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	fetch := func(name string) (*string, error) {
		u, err := tuf.Join(base, name)
		if err != nil {
			return nil, mixerError(err.Error())
		}
		response, err := client.Get(u.String())
		if err != nil {
			return nil, err
		}
		defer response.Body.Close()
		if response.StatusCode == http.StatusNotFound {
			return nil, nil
		}
		if response.StatusCode < 200 || response.StatusCode > 299 {
			return nil, errorf("HTTP status client error (%s) for url (%s)", response.Status, u)
		}
		data, err := io.ReadAll(io.LimitReader(response.Body, rawMetadataLimit+1))
		if err != nil {
			return nil, err
		}
		if len(data) > rawMetadataLimit {
			return nil, mixerError(fmt.Sprintf("signed metadata %s is too large", name))
		}
		if !utf8.Valid(data) {
			return nil, mixerError(fmt.Sprintf("signed metadata %s is not text", name))
		}
		text := string(data)
		return &text, nil
	}
	return assemble(fetch)
}

// assemble is the role files fetch returns, checked to belong together.
func assemble(fetch func(string) (*string, error)) (*RawMetadata, error) {
	required := func(name string) (string, error) {
		text, err := fetch(name)
		if err != nil {
			return "", err
		}
		if text == nil {
			return "", mixerError("signed metadata has no " + name)
		}
		return *text, nil
	}
	var roots []string
	for version := 1; version <= maxRoots; version++ {
		text, err := fetch(fmt.Sprintf("%d.root.json", version))
		if err != nil {
			return nil, err
		}
		if text == nil {
			break
		}
		roots = append(roots, *text)
	}
	if len(roots) == 0 {
		return nil, mixerError("signed metadata has no root")
	}
	for range 3 {
		timestamp, err := required("timestamp.json")
		if err != nil {
			return nil, err
		}
		snapshot, err := required("snapshot.json")
		if err != nil {
			return nil, err
		}
		targets, err := required("targets.json")
		if err != nil {
			return nil, err
		}
		if lists(timestamp, "snapshot.json", snapshot) && lists(snapshot, "targets.json", targets) {
			return &RawMetadata{Root: roots, Timestamp: timestamp, Snapshot: snapshot, Targets: targets}, nil
		}
	}
	return nil, mixerError("signed metadata kept changing while it was read; try again")
}

// lists is whether listing names file with the length and SHA-256 (where
// given) of content, as TUF's timestamp and snapshot do.
func lists(listing, name, content string) bool {
	value, err := sjson.Value([]byte(listing))
	if err != nil {
		return false
	}
	meta, ok := pointer(value, "signed", "meta", name)
	if !ok {
		return false
	}
	object, _ := meta.(map[string]any)
	lengthOK := true
	if length, ok := embed.AsU64(object["length"]); ok {
		lengthOK = length == uint64(len(content))
	}
	hashOK := true
	if hashes, ok := object["hashes"].(map[string]any); ok {
		if hash, ok := hashes["sha256"].(string); ok {
			hashOK = equalFoldASCII(hash, archive.Sum([]byte(content)))
		}
	}
	return lengthOK && hashOK
}

func pointer(value any, path ...string) (any, bool) {
	for _, key := range path {
		object, ok := value.(map[string]any)
		if !ok {
			return nil, false
		}
		if value, ok = object[key]; !ok {
			return nil, false
		}
	}
	return value, true
}

func equalFoldASCII(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range len(a) {
		x, y := a[i], b[i]
		if 'A' <= x && x <= 'Z' {
			x += 'a' - 'A'
		}
		if 'A' <= y && y <= 'Z' {
			y += 'a' - 'A'
		}
		if x != y {
			return false
		}
	}
	return true
}

// checkRepositoryURL: the two addresses updates may come from, over plain
// HTTPS.
func checkRepositoryURL(u *url.URL, host string) error {
	if !tuf.OfficialURL(u, host) {
		return mixerError("update repository URLs must use the official HTTPS hosts")
	}
	return nil
}

// MixerConfigured is false in a checkout without a trust root.
func MixerConfigured() bool {
	return string(bytes.TrimSpace(trustedRoot)) != "null"
}

func official(ctx context.Context, work string) (*MixerRepository, error) {
	if !MixerConfigured() {
		return nil, mixerError("this app has no configured mixer release trust root")
	}
	if folder, ok := devRepository(); ok {
		// Its own floors, so a test root never meets the official one's.
		return openMixerRepository(ctx, tuf.Folders{Metadata: filepath.Join(folder, "metadata"), Targets: folder},
			trustedRoot, filepath.Join(work, "mixer-metadata-dev"))
	}
	state, err := metadataDirectory(work)
	if err != nil {
		return nil, err
	}
	metadata, err := url.Parse(metadataURL)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	targets, err := url.Parse(targetsURL)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	return LoadMixerRepository(ctx, trustedRoot, metadata, targets, state)
}

func metadataDirectory(work string) (string, error) {
	// The Linux helper must not let a desktop-controlled symlink redirect the
	// metadata store's writes as root. Its rollback state is independent and
	// root-owned.
	path, err := helperDirectory("mixer-updates")
	if err != nil {
		return "", err
	}
	if path != "" {
		return path, nil
	}
	return filepath.Join(work, "mixer-metadata"), nil
}
