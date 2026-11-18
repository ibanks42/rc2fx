package installer

// Authenticate complete mixer payloads before exposing their contents to
// the installer.

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
	"slices"
	"sort"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/tuf"
	"github.com/ibanks42/rc2fx/desktop/installer/trust"
)

const (
	maxPayload = (256 << 20) + 256
	maxCatalog = 2 << 20
)

var (
	catalogMu    sync.Mutex
	catalogCache *VerifiedCatalog
)

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

func cachedCatalog() (*Catalog, error) {
	catalogMu.Lock()
	defer catalogMu.Unlock()
	if catalogCache == nil {
		return nil, mixerError("check signed firmware releases before installing")
	}
	c, err := catalogCache.Catalog()
	if err != nil {
		return nil, err
	}
	out := c.Clone()
	return &out, nil
}

func cachedCatalogBytes() ([]byte, error) {
	catalogMu.Lock()
	defer catalogMu.Unlock()
	if catalogCache == nil {
		return nil, mixerError("check signed firmware releases before inspecting firmware")
	}
	if _, err := catalogCache.Catalog(); err != nil {
		return nil, err
	}
	return bytes.Clone(catalogCache.bytes), nil
}

func refreshCatalog(ctx context.Context, repository *MixerRepository) error {
	catalog, err := repository.VerifiedCatalog(ctx)
	if err != nil {
		return err
	}
	catalogMu.Lock()
	defer catalogMu.Unlock()
	if previous := catalogCache; previous != nil {
		older := catalog.rootVersion < previous.rootVersion ||
			(catalog.rootVersion == previous.rootVersion && catalog.targetsVersion < previous.targetsVersion)
		same := catalog.rootVersion == previous.rootVersion && catalog.targetsVersion == previous.targetsVersion
		if older || (same && !bytes.Equal(previous.bytes, catalog.bytes)) {
			return mixerError("firmware catalog is older than the latest verified check or changed under the same metadata version")
		}
	}
	catalogCache = catalog
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
			hashOK = equalFoldASCII(hash, sum([]byte(content)))
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
	if !officialURL(u, host) {
		return mixerError("update repository URLs must use the official HTTPS hosts")
	}
	return nil
}

func officialURL(u *url.URL, host string) bool {
	return u.Scheme == "https" && u.Hostname() == host && u.Port() == "" && u.User == nil
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

// MixerTarget is the compatibility claims covered by the TUF targets
// signature.
type MixerTarget struct {
	_                 struct{} `serde:"deny_unknown_fields"`
	Version           string   `json:"version"`
	PayloadFormat     uint32   `json:"payloadFormat"`
	ProtocolVersion   uint32   `json:"protocolVersion"`
	MinimumAppVersion string   `json:"minimumAppVersion"`
	FirmwareProfiles  []string `json:"firmwareProfiles"`
}

// Equal compares every field.
func (m *MixerTarget) Equal(o *MixerTarget) bool {
	return m.Version == o.Version && m.PayloadFormat == o.PayloadFormat && m.ProtocolVersion == o.ProtocolVersion &&
		m.MinimumAppVersion == o.MinimumAppVersion && slices.Equal(m.FirmwareProfiles, o.FirmwareProfiles)
}

func (m *MixerTarget) clone() MixerTarget {
	out := *m
	out.FirmwareProfiles = slices.Clone(m.FirmwareProfiles)
	return out
}

func supportedIDs(catalog *Catalog) []string {
	ids := []string{}
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; supported(p) {
			ids = append(ids, p.ID)
		}
	}
	return ids
}

// MixerTargetForPayload derives signing metadata from the same complete
// release payload the installer validates. Publishers cannot advertise
// unqualified profiles.
func MixerTargetForPayload(payload *embed.Envelope, minimumAppVersion string) (*MixerTarget, error) {
	bundle, err := payload.Bundle()
	if err != nil {
		return nil, mixerError(err.Error())
	}
	release, err := core.ParseMixerRelease(bundle["mixer-release.json"])
	if err != nil {
		return nil, mixerError(err.Error())
	}
	catalog, err := parseCatalog(bundle["firmware-profiles.json"])
	if err != nil {
		return nil, err
	}
	m := &MixerTarget{
		Version:           release.Version,
		PayloadFormat:     1,
		ProtocolVersion:   release.ProtocolVersion,
		MinimumAppVersion: minimumAppVersion,
		FirmwareProfiles:  supportedIDs(catalog),
	}
	if err := m.validate(); err != nil {
		return nil, err
	}
	if _, err := verifyPayload(payload.AsBytes(), m.clone(), m.FirmwareProfiles[0], nil); err != nil {
		return nil, err
	}
	return m, nil
}

// RequireCompatible refuses a release this app, protocol or firmware
// profile cannot use.
func (m *MixerTarget) RequireCompatible(appVersion string, protocol uint32, profile string) error {
	if err := m.validate(); err != nil {
		return err
	}
	order, ok := core.CompareVersions(appVersion, m.MinimumAppVersion)
	if m.PayloadFormat != 1 || m.ProtocolVersion != protocol || !slices.Contains(m.FirmwareProfiles, profile) || !ok || order < 0 {
		return mixerError("mixer release is incompatible with this app or firmware")
	}
	return nil
}

func comparable(v string) bool {
	_, ok := core.CompareVersions(v, v)
	return ok
}

func (m *MixerTarget) validate() error {
	unique := map[string]bool{}
	for _, p := range m.FirmwareProfiles {
		unique[p] = true
	}
	if !comparable(m.Version) || !comparable(m.MinimumAppVersion) || m.ProtocolVersion == 0 ||
		len(m.FirmwareProfiles) == 0 || slices.Contains(m.FirmwareProfiles, "") || len(unique) != len(m.FirmwareProfiles) {
		return mixerError("invalid signed mixer compatibility metadata")
	}
	return nil
}

// VerifiedMixerPayload is owned, authenticated bytes. No public constructor
// accepts an unchecked bundle.
type VerifiedMixerPayload struct {
	bundle Bundle
	bytes  []byte
}

// MixerRepository is a repository authenticated against a caller-supplied,
// previously trusted root. The desktop caller must supply its compiled-in
// root, never a downloaded root.
type MixerRepository struct {
	updater *tuf.Updater
}

func mixerError(text string) error {
	return Error("Mixer update verification: " + text)
}

// mixerOperation runs an operation that may download, stopping it when the
// engine is canceled or after 30 minutes.
func mixerOperation[T any](e *Engine, operation func(context.Context) (T, error)) (T, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 1800*time.Second)
	defer cancel()
	type result struct {
		value T
		err   error
	}
	done := make(chan result, 1)
	go func() {
		value, err := operation(ctx)
		done <- result{value, err}
	}()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	var zero T
	for {
		if e.canceled() {
			cancel()
			<-done
			return zero, Error(Canceled)
		}
		select {
		case r := <-done:
			if r.err != nil && ctx.Err() == context.DeadlineExceeded {
				return zero, mixerError("download timed out")
			}
			return r.value, r.err
		case <-ticker.C:
		}
	}
}

// VerifiedCatalog is the newest release's signed firmware catalog.
func (r *MixerRepository) VerifiedCatalog(ctx context.Context) (*VerifiedCatalog, error) {
	if err := r.requireCurrent(); err != nil {
		return nil, err
	}
	offers, err := r.Offers()
	if err != nil {
		return nil, err
	}
	if len(offers) == 0 {
		return nil, mixerError("no signed mixer releases")
	}
	newest := offers[0]
	targets, err := r.targets()
	if err != nil {
		return nil, err
	}
	var candidates []string
	for _, name := range sortedTargets(targets) {
		if customString(targets.Targets[name], "rc2fxCatalog") == newest.Metadata.Version {
			candidates = append(candidates, name)
		}
	}
	if len(candidates) != 1 {
		return nil, mixerError("release must identify exactly one signed firmware catalog")
	}
	name := candidates[0]
	target := targets.Targets[name]
	if target.Length <= 0 || target.Length > maxCatalog {
		return nil, mixerError("firmware catalog exceeds size limit")
	}
	data, err := r.read(ctx, name, target)
	if err != nil {
		return nil, err
	}
	if int64(len(data)) != target.Length {
		return nil, mixerError("firmware catalog length mismatch")
	}
	catalog, err := parseCatalog(data)
	if err != nil {
		return nil, err
	}
	supportedIDs := supportedIDs(catalog)
	expected := slices.Clone(newest.Metadata.FirmwareProfiles)
	sort.Strings(supportedIDs)
	sort.Strings(expected)
	if !slices.Equal(supportedIDs, expected) {
		return nil, mixerError("signed firmware catalog disagrees with mixer compatibility")
	}
	expiries, err := r.expiries()
	if err != nil {
		return nil, err
	}
	expires := expiries[0]
	for _, e := range expiries[1:] {
		if e.Before(expires) {
			expires = e
		}
	}
	return &VerifiedCatalog{
		catalog:        *catalog,
		bytes:          data,
		expires:        expires,
		rootVersion:    r.updater.Trusted().Root().Version,
		targetsVersion: targets.Version,
	}, nil
}

func sortedTargets(targets *metadata.TargetsType) []string {
	names := make([]string, 0, len(targets.Targets))
	for name := range targets.Targets {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

// customValue is a target's custom field `key`, or nil.
func customValue(target *metadata.TargetFiles, key string) any {
	if target == nil || target.Custom == nil {
		return nil
	}
	value, err := sjson.Value(*target.Custom)
	if err != nil {
		return nil
	}
	object, _ := value.(map[string]any)
	return object[key]
}

func customString(target *metadata.TargetFiles, key string) string {
	s, _ := customValue(target, key).(string)
	return s
}

// targets is the verified top-level targets.
func (r *MixerRepository) targets() (*metadata.TargetsType, error) {
	t := r.updater.Trusted().TargetsRole(metadata.TARGETS)
	if t == nil {
		return nil, mixerError("signed mixer metadata is incomplete")
	}
	return t, nil
}

// expiries is when each of the four verified roles expires.
func (r *MixerRepository) expiries() ([4]time.Time, error) {
	trusted := r.updater.Trusted()
	timestamp, snapshot := trusted.Timestamp(), trusted.Snapshot()
	targets, err := r.targets()
	if timestamp == nil || snapshot == nil || err != nil {
		return [4]time.Time{}, mixerError("signed mixer metadata is incomplete")
	}
	return [4]time.Time{trusted.Root().Expires, timestamp.Expires, snapshot.Expires, targets.Expires}, nil
}

// read downloads the target name and checks it against its signed length
// and hash before any byte is returned.
func (r *MixerRepository) read(ctx context.Context, name string, target *metadata.TargetFiles) ([]byte, error) {
	data, err := r.updater.DownloadTarget(ctx, target, name)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	return data, nil
}

// target is the signed release target name.
func (r *MixerRepository) target(name string) (*metadata.TargetFiles, error) {
	targets, err := r.targets()
	if err != nil {
		return nil, err
	}
	t := targets.Targets[name]
	if t == nil {
		return nil, mixerError("target disappeared")
	}
	return t, nil
}

func (r *MixerRepository) requireCurrent() error {
	now := time.Now()
	expiries, err := r.expiries()
	if err != nil {
		return err
	}
	for _, e := range expiries {
		if !e.After(now) {
			return mixerError("signed mixer metadata expired; refresh before installation")
		}
	}
	// RC2FX only publishes top-level targets. Avoid extending cached-target
	// trust to delegated roles without checking each delegation's lifetime.
	targets, err := r.targets()
	if err != nil {
		return err
	}
	if d := targets.Delegations; d != nil && (len(d.Roles) > 0 || d.SuccinctRoles != nil) {
		return mixerError("delegated mixer targets are not supported")
	}
	return nil
}

// AcceptCached is Accept through the payload cache in cache.
func (r *MixerRepository) AcceptCached(ctx context.Context, offer *MixerOffer, appVersion string, protocol uint32, profile, cache string) (*VerifiedMixerPayload, error) {
	offers, err := r.Offers()
	if err != nil {
		return nil, err
	}
	if !slices.ContainsFunc(offers, func(o MixerOffer) bool { return o.Equal(offer) }) {
		return nil, mixerError("the selected mixer release changed; refresh and review it again")
	}
	if err := offer.Metadata.RequireCompatible(appVersion, protocol, profile); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(cache, 0o777); err != nil {
		return nil, err
	}
	path := filepath.Join(cache, offer.SHA256+".payload")
	file, err := os.Open(path)
	switch {
	case err == nil:
		data, err := io.ReadAll(io.LimitReader(file, maxPayload+1))
		file.Close()
		if err != nil {
			return nil, err
		}
		if uint64(len(data)) == offer.Length && sum(data) == offer.SHA256 {
			return verifyPayload(data, offer.Metadata.clone(), profile, &appVersion)
		}
		// A damaged cache is not another install source. Re-fetch only the
		// exact same signed offer and atomically replace the cache.
	case !errors.Is(err, fs.ErrNotExist):
		return nil, err
	}
	payload, err := r.Accept(ctx, offer, appVersion, protocol, profile)
	if err != nil {
		return nil, err
	}
	if err := tuf.WriteAtomic(path, payload.bytes); err != nil {
		return nil, mixerError(err.Error())
	}
	return payload, nil
}

// BasePayload is the signed payload of the release a mixer runs, cached in
// cache, for the in-place update to send only what changed since. Nothing
// from it is installed: the mixer rebuilds the new release from its own
// files and checks the result against the new release's signature. Nil when
// no signed release has that version (a development build).
func (r *MixerRepository) BasePayload(ctx context.Context, version, cache string) (*string, error) {
	if err := r.requireCurrent(); err != nil {
		return nil, err
	}
	releases, err := r.Releases()
	if err != nil {
		return nil, err
	}
	i := slices.IndexFunc(releases, func(rel NamedMixerTarget) bool { return rel.Metadata.Version == version })
	if i < 0 {
		return nil, nil
	}
	name := releases[i].Name
	signed, err := r.target(name)
	if err != nil {
		return nil, err
	}
	sha, ok := targetSha256(signed)
	if !ok {
		return nil, mixerError("signed release has no SHA-256")
	}
	length := signed.Length
	if err := os.MkdirAll(cache, 0o777); err != nil {
		return nil, err
	}
	path := filepath.Join(cache, sha+".payload")
	if data, err := os.ReadFile(path); err == nil && int64(len(data)) == length && sum(data) == sha {
		return &path, nil
	}
	// read checks the signed hash before it hands over the bytes.
	data, err := r.read(ctx, name, signed)
	if err != nil {
		return nil, err
	}
	if int64(len(data)) != length {
		return nil, mixerError("payload length differs from signed metadata")
	}
	if err := tuf.WriteAtomic(path, data); err != nil {
		return nil, mixerError(err.Error())
	}
	return &path, nil
}

// Offers is every signed release, newest first.
func (r *MixerRepository) Offers() ([]MixerOffer, error) {
	if err := r.requireCurrent(); err != nil {
		return nil, err
	}
	releases, err := r.Releases()
	if err != nil {
		return nil, err
	}
	offers := []MixerOffer{}
	for _, rel := range releases {
		target, err := r.target(rel.Name)
		if err != nil {
			return nil, err
		}
		sha, ok := targetSha256(target)
		if !ok {
			return nil, mixerError("signed release has no SHA-256")
		}
		offers = append(offers, MixerOffer{SHA256: sha, Length: uint64(target.Length), Target: rel.Name, Metadata: rel.Metadata})
	}
	slices.SortStableFunc(offers, func(a, b MixerOffer) int {
		order, _ := core.CompareVersions(b.Metadata.Version, a.Metadata.Version)
		return order
	})
	return offers, nil
}

// Accept downloads offer if it is still signed.
func (r *MixerRepository) Accept(ctx context.Context, offer *MixerOffer, appVersion string, protocol uint32, profile string) (*VerifiedMixerPayload, error) {
	offers, err := r.Offers()
	if err != nil {
		return nil, err
	}
	if !slices.ContainsFunc(offers, func(o MixerOffer) bool { return o.Equal(offer) }) {
		return nil, mixerError("the selected mixer release changed; refresh and review it again")
	}
	return r.Download(ctx, offer.Target, appVersion, protocol, profile)
}

// LoadMixerRepository opens the official HTTPS repository.
func LoadMixerRepository(ctx context.Context, trustedRoot []byte, metadataURL, targetsURL *url.URL, datastore string) (*MixerRepository, error) {
	if err := checkRepositoryURL(metadataURL, metadataHost); err != nil {
		return nil, err
	}
	if err := checkRepositoryURL(targetsURL, targetsHost); err != nil {
		return nil, err
	}
	repository := tufHTTP(metadataURL, targetsURL, 1800*time.Second)
	return openMixerRepository(ctx, repository, trustedRoot, datastore)
}

// openMixerRepository refreshes repository's metadata from trustedRoot,
// through the verified state in datastore: the newest versions seen there
// are the floor, so older metadata is refused. Every role must be
// unexpired.
func openMixerRepository(ctx context.Context, repository tuf.Repository, trustedRoot []byte, datastore string) (*MixerRepository, error) {
	if err := os.MkdirAll(datastore, 0o777); err != nil {
		return nil, err
	}
	updater, err := tuf.NewUpdater(repository, trustedRoot)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	updater = updater.WithConfig(tufConfig(maxPayload)).WithStore(metadataOnly{tuf.FileStore{Dir: datastore}})
	if err := updater.Refresh(ctx, time.Now()); err != nil {
		return nil, mixerError(err.Error())
	}
	if err := distinctKeys(updater.Trusted().Root()); err != nil {
		return nil, mixerError(err.Error())
	}
	return &MixerRepository{updater: updater}, nil
}

// NamedMixerTarget is a release target and its signed claims.
type NamedMixerTarget struct {
	Name     string
	Metadata MixerTarget
}

// Releases is every target that is a mixer release, in name order.
func (r *MixerRepository) Releases() ([]NamedMixerTarget, error) {
	targets, err := r.targets()
	if err != nil {
		return nil, err
	}
	var out []NamedMixerTarget
	for _, name := range sortedTargets(targets) {
		target := targets.Targets[name]
		if !customHas(target, "rc2fxMixer") {
			continue
		}
		raw, err := sjson.Marshal(customValue(target, "rc2fxMixer"))
		if err != nil {
			return nil, mixerError(err.Error())
		}
		var m MixerTarget
		if err := sjson.Unmarshal(raw, &m); err != nil {
			return nil, mixerError(err.Error())
		}
		if err := m.validate(); err != nil {
			return nil, err
		}
		if target.Length <= 0 || target.Length > maxPayload {
			return nil, mixerError("payload size is outside the supported limit")
		}
		out = append(out, NamedMixerTarget{Name: name, Metadata: m})
	}
	return out, nil
}

func customHas(target *metadata.TargetFiles, key string) bool {
	if target == nil || target.Custom == nil {
		return false
	}
	value, err := sjson.Value(*target.Custom)
	if err != nil {
		return false
	}
	object, ok := value.(map[string]any)
	if !ok {
		return false
	}
	_, ok = object[key]
	return ok
}

// Download downloads and verifies the release target name.
func (r *MixerRepository) Download(ctx context.Context, name, appVersion string, protocol uint32, profile string) (*VerifiedMixerPayload, error) {
	releases, err := r.Releases()
	if err != nil {
		return nil, err
	}
	i := slices.IndexFunc(releases, func(rel NamedMixerTarget) bool { return rel.Name == name })
	if i < 0 {
		return nil, mixerError("target is not a mixer release")
	}
	m := releases[i].Metadata
	if err := m.RequireCompatible(appVersion, protocol, profile); err != nil {
		return nil, err
	}
	target, err := r.target(name)
	if err != nil {
		return nil, err
	}
	// No consumer sees the bytes until their signed hash has been checked.
	data, err := r.read(ctx, name, target)
	if err != nil {
		return nil, err
	}
	if int64(len(data)) != target.Length {
		return nil, mixerError("payload length differs from signed metadata")
	}
	return verifyPayload(data, m, profile, &appVersion)
}

func verifyPayload(data []byte, m MixerTarget, profile string, appVersion *string) (*VerifiedMixerPayload, error) {
	envelope, err := embed.Standalone(data)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	bundle, err := envelope.Bundle()
	if err != nil {
		return nil, mixerError(err.Error())
	}
	kind, err := embed.BuildKind(bundle["rc2fx.sh"])
	if err != nil {
		return nil, mixerError(err.Error())
	}
	if kind != "release" {
		return nil, mixerError("downloaded mixer payload is not a release build")
	}
	release, err := core.ParseMixerRelease(bundle["mixer-release.json"])
	if err != nil {
		return nil, mixerError(err.Error())
	}
	if release.Version != m.Version || release.ProtocolVersion != m.ProtocolVersion {
		return nil, mixerError("signed metadata disagrees with the payload release")
	}
	catalog, err := parseCatalog(bundle["firmware-profiles.json"])
	if err != nil {
		return nil, err
	}
	if appVersion != nil {
		for i := range catalog.Profiles {
			p := &catalog.Profiles[i]
			if p.ID != profile {
				continue
			}
			if !appSupports(p, *appVersion) {
				return nil, mixerError(fmt.Sprintf("firmware %s requires app %s or newer; update the app first", p.Version, p.MinimumAppVersion))
			}
			break
		}
	}
	actual := supportedIDs(catalog)
	declared := slices.Clone(m.FirmwareProfiles)
	sort.Strings(actual)
	sort.Strings(declared)
	if !slices.Equal(actual, declared) {
		return nil, mixerError("signed firmware compatibility differs from the payload catalog")
	}
	if !slices.ContainsFunc(catalog.Profiles, func(p Profile) bool { return p.ID == profile && supported(&p) }) {
		return nil, mixerError("payload does not support the selected firmware profile")
	}
	return &VerifiedMixerPayload{bundle: bundle, bytes: bytes.Clone(data)}, nil
}
