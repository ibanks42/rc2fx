package installer

// TUF authentication and compatibility verification for downloaded mixer payloads.

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/internal/tuf"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

const (
	maxPayload = (256 << 20) + 256
	maxCatalog = 2 << 20
)

// MixerTarget holds the signed compatibility claims from TUF targets metadata.
type MixerTarget struct {
	_                 struct{} `serde:"deny_unknown_fields"`
	Version           string   `json:"version"`
	PayloadFormat     uint32   `json:"payloadFormat"`
	ProtocolVersion   uint32   `json:"protocolVersion"`
	MinimumAppVersion string   `json:"minimumAppVersion"`
	FirmwareProfiles  []string `json:"firmwareProfiles"`
}

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

// MixerTargetForPayload derives signed target metadata from a validated payload
// so publishers cannot advertise unqualified firmware profiles.
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

// VerifiedMixerPayload holds authenticated payload bytes; constructed only
// after TUF and internal validation pass.
type VerifiedMixerPayload struct {
	bundle Bundle
	bytes  []byte
}

// MixerRepository wraps a TUF updater anchored to a caller-supplied trusted root.
type MixerRepository struct {
	updater *tuf.Updater
}

func mixerError(text string) error {
	return Error("Mixer update verification: " + text)
}

// mixerOperation runs a potentially long download with a 30-minute timeout and
// polls Engine.Cancel.
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

// VerifiedCatalog downloads and verifies the firmware catalog for the newest
// signed mixer release.
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

func customValue(target *metadata.TargetFiles, key string) any {
	return tuf.CustomValue(target, key)
}

func customString(target *metadata.TargetFiles, key string) string {
	return tuf.CustomString(target, key)
}

func (r *MixerRepository) targets() (*metadata.TargetsType, error) {
	t := r.updater.Trusted().TargetsRole(metadata.TARGETS)
	if t == nil {
		return nil, mixerError("signed mixer metadata is incomplete")
	}
	return t, nil
}

func (r *MixerRepository) expiries() ([4]time.Time, error) {
	trusted := r.updater.Trusted()
	timestamp, snapshot := trusted.Timestamp(), trusted.Snapshot()
	targets, err := r.targets()
	if timestamp == nil || snapshot == nil || err != nil {
		return [4]time.Time{}, mixerError("signed mixer metadata is incomplete")
	}
	return [4]time.Time{trusted.Root().Expires, timestamp.Expires, snapshot.Expires, targets.Expires}, nil
}

func (r *MixerRepository) read(ctx context.Context, name string, target *metadata.TargetFiles) ([]byte, error) {
	data, err := r.updater.DownloadTarget(ctx, target, name)
	if err != nil {
		return nil, mixerError(err.Error())
	}
	return data, nil
}

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
	// Reject delegations since we don't validate delegated role lifetimes.
	targets, err := r.targets()
	if err != nil {
		return err
	}
	if d := targets.Delegations; d != nil && (len(d.Roles) > 0 || d.SuccinctRoles != nil) {
		return mixerError("delegated mixer targets are not supported")
	}
	return nil
}

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
		if uint64(len(data)) == offer.Length && archive.Sum(data) == offer.SHA256 {
			return verifyPayload(data, offer.Metadata.clone(), profile, &appVersion)
		}
		// Corrupt cache entry: fall through to re-fetch and atomically replace.
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

// BasePayload fetches and caches the signed payload for the mixer's current
// version so in-place updates can send a delta. Returns nil if version isn't
// a signed release (e.g. a dev build).
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
	if data, err := os.ReadFile(path); err == nil && int64(len(data)) == length && archive.Sum(data) == sha {
		return &path, nil
	}
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

// Offers returns all signed mixer releases, newest first.
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

// openMixerRepository refreshes TUF metadata against trustedRoot using
// datastore to enforce rollback floors.
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

type NamedMixerTarget struct {
	Name     string
	Metadata MixerTarget
}

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
	return tuf.CustomHas(target, key)
}

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
