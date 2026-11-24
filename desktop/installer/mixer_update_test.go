package installer

import (
	"bytes"
	"context"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"maps"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/tuf"
	"github.com/ibanks42/rc2fx/desktop/tuf/tuftest"
)

func TestTheEmbeddedTrustRootIsTheConfiguredOne(t *testing.T) {
	if !bytes.Equal(trustedRoot, readConfig(t, "mixer-update-root.json")) {
		t.Fatal("installer/trust/mixer-update-root.json differs from config/mixer-update-root.json")
	}
}

func TestConfiguredProductionRootIsValidAndSeparatesRootFromReleaseKeys(t *testing.T) {
	if !MixerConfigured() {
		return
	}
	// The verifier the app ships accepts it, as the mixer's does.
	trusted, err := tuf.NewTrusted(trustedRoot)
	must(t, err)
	root := trusted.Root()
	must(t, distinctKeys(root))
	if !root.Expires.After(time.Now()) {
		t.Fatal("the production root has expired")
	}
	if root.ConsistentSnapshot {
		t.Fatal("release assets keep fixed names under their tags")
	}
	rootKeys := root.Roles[metadata.ROOT].KeyIDs
	for _, role := range []string{metadata.TARGETS, metadata.SNAPSHOT, metadata.TIMESTAMP} {
		keys := root.Roles[role]
		if len(keys.KeyIDs) < keys.Threshold {
			t.Fatalf("%s cannot meet its threshold", role)
		}
		for _, id := range keys.KeyIDs {
			if slices.Contains(rootKeys, id) {
				t.Fatalf("%s shares a key with root", role)
			}
		}
	}
}

type mixerFixture struct {
	t        *testing.T
	dir      string
	repo     *tuftest.Repo
	metadata MixerTarget
	payload  []byte
}

func filesOf(bundle map[string][]byte) embed.Files {
	var files embed.Files
	for _, name := range embed.PayloadNames {
		files = append(files, embed.File{Name: name, Data: bundle[name]})
	}
	return files
}

func resum(bundle map[string][]byte) {
	var sums strings.Builder
	for _, name := range embed.Pinned {
		sums.WriteString(common.SHA256Hex(bundle[name]) + "  " + name + "\n")
	}
	bundle["SHA256SUMS"] = []byte(sums.String())
}

func mixerPayload(t *testing.T) []byte {
	bundle := map[string][]byte{}
	for _, name := range embed.PayloadNames {
		switch {
		case name == "firmware-profiles.json":
			bundle[name] = readConfig(t, "firmware-profiles.json")
		case name == "mixer-release.json":
			bundle[name] = []byte(`{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}`)
		case name == "SHA256SUMS":
		case name == "rc2fx.sh":
			bundle[name] = []byte("#!/bin/sh\nrc2fx_build=release\n")
		case embed.IsScript(name):
			bundle[name] = []byte("#!/bin/sh\n")
		default:
			bundle[name] = elf(0, 96)
		}
	}
	resum(bundle)
	return embed.Wrap(embed.Body(filesOf(bundle)))
}

func newMixerFixture(t *testing.T) *mixerFixture {
	dir := t.TempDir()
	repo := tuftest.NewRepo(t, dir)
	payload := mixerPayload(t)
	envelope, err := embed.Standalone(payload)
	must(t, err)
	bundle, err := envelope.Bundle()
	must(t, err)
	var catalog Catalog
	must(t, sjson.Unmarshal(bundle["firmware-profiles.json"], &catalog))
	return &mixerFixture{
		t:    t,
		dir:  dir,
		repo: repo,
		metadata: MixerTarget{
			Version:           "1.2.3",
			PayloadFormat:     1,
			ProtocolVersion:   1,
			MinimumAppVersion: "1.0.1",
			FirmwareProfiles:  supportedIDs(&catalog),
		},
		payload: payload,
	}
}

func (f *mixerFixture) publish(sequence int64, expires string) {
	t := f.t
	path := filepath.Join(f.dir, "payload")
	put(t, path, f.payload)
	envelope, err := embed.Standalone(f.payload)
	must(t, err)
	bundle, err := envelope.Bundle()
	must(t, err)
	catalogPath := filepath.Join(f.dir, "firmware-profiles.json")
	put(t, catalogPath, bundle["firmware-profiles.json"])
	raw, err := sjson.Marshal(f.metadata)
	must(t, err)
	value, err := sjson.Value(raw)
	must(t, err)
	f.repo.Sign([]tuftest.Target{
		{Name: "firmware-profiles.json", File: catalogPath, Custom: map[string]any{"rc2fxCatalog": f.metadata.Version}},
		{Name: "payload", File: path, Custom: map[string]any{"rc2fxMixer": value}},
	}, [3]int64{sequence, sequence, sequence}, expires)
}

func (f *mixerFixture) load() (*MixerRepository, error) {
	state := filepath.Join(f.dir, "state")
	must(f.t, os.MkdirAll(state, 0o755))
	return openMixerRepository(context.Background(), tuf.Folders{Metadata: filepath.Join(f.dir, "metadata"), Targets: f.dir}, f.repo.Trusted, state)
}

func TestSignedCatalogEnforcesProfileAppMinimumAndCannotBeReplaced(t *testing.T) {
	ctx := context.Background()
	f := newMixerFixture(t)
	envelope, err := embed.Standalone(f.payload)
	must(t, err)
	bundle, err := envelope.Bundle()
	must(t, err)
	catalog, err := sjson.Value(bundle["firmware-profiles.json"])
	must(t, err)
	first := catalog.(map[string]any)["profiles"].([]any)[0].(map[string]any)
	id := first["id"].(string)
	first["minimumAppVersion"] = "9.0.0"
	bundle["firmware-profiles.json"], err = sjson.Marshal(catalog)
	must(t, err)
	resum(bundle)
	f.payload = embed.Wrap(embed.Body(filesOf(bundle)))
	f.publish(1, "2099-01-01T00:00:00Z")
	repository, err := f.load()
	must(t, err)
	verified, err := repository.VerifiedCatalog(ctx)
	must(t, err)
	verifiedCatalog, err := verified.Catalog()
	must(t, err)
	releases, err := releasesFromCatalog(firmware.ModelProIi, verifiedCatalog)
	must(t, err)
	i := slices.IndexFunc(releases, func(r Release) bool { return r.Profile == id })
	if i < 0 || releases[i].Supported || releases[i].MinimumAppVersion != "9.0.0" {
		t.Fatalf("%+v", releases)
	}
	_, err = repository.Download(ctx, "payload", "1.0.1", 1, id)
	contains(t, err, "requires app 9.0.0")
	_, err = repository.Download(ctx, "payload", "9.0.0", 1, id)
	must(t, err)
	put(t, filepath.Join(f.dir, "firmware-profiles.json"), []byte("replacement"))
	if _, err := repository.VerifiedCatalog(ctx); err == nil {
		t.Fatal("a replaced catalog was accepted")
	}
	expired := *verified
	expired.expires = tuftest.MustTime(t, "2000-01-01T00:00:00Z")
	if _, err := expired.Catalog(); err == nil {
		t.Fatal("an expired catalog authorization was used")
	}
}

func samePayload(a, b *VerifiedMixerPayload) bool {
	return maps.EqualFunc(a.bundle, b.bundle, bytes.Equal) && bytes.Equal(a.bytes, b.bytes)
}

func TestSignedRepositoryEnforcesAuthenticityCompatibilityExpiryAndRollback(t *testing.T) {
	ctx := context.Background()
	f := newMixerFixture(t)
	f.publish(2, "2099-01-01T00:00:00Z")
	repository, err := f.load()
	must(t, err)
	profile := f.metadata.FirmwareProfiles[0]
	verified, err := repository.Download(ctx, "payload", "1.0.1", 1, profile)
	must(t, err)
	if !bytes.Equal(verified.bytes, f.payload) {
		t.Fatal("the verified bytes differ from the published payload")
	}
	offers, err := repository.Offers()
	must(t, err)
	offer := offers[0]
	cache := filepath.Join(f.dir, "cache")
	cached, err := repository.AcceptCached(ctx, &offer, "1.0.1", 1, profile, cache)
	must(t, err)
	if !samePayload(cached, verified) {
		t.Fatal("the cached payload differs")
	}
	source := Source{Kind: SourceVerified, Verified: cached}
	catalog, err := source.catalog()
	must(t, err)
	var selected *Profile
	for i := range catalog.Profiles {
		if catalog.Profiles[i].ID == profile {
			selected = &catalog.Profiles[i]
		}
	}
	sourceBundle, err := source.bundle()
	must(t, err)
	entries, d, err := imageEntries(sourceBundle, selected, nil)
	must(t, err)
	if d != digest(verified.bundle) {
		t.Fatal("digest")
	}
	if !slices.ContainsFunc(entries, func(e archive.Entry) bool { return e.Path == "usr/sbin/rc_audio_mixer" }) {
		t.Fatal("no mixer shim")
	}
	for name, data := range verified.bundle {
		want := "opt/rc2fx/" + name
		if !slices.ContainsFunc(entries, func(e archive.Entry) bool {
			return e.Kind == archive.EntryFile && e.Path == want && bytes.Equal(e.Data, data)
		}) {
			t.Fatalf("%s not in the image", want)
		}
	}
	replaced := offer
	replaced.SHA256 = strings.Repeat("0", 64)
	if _, err := repository.AcceptCached(ctx, &replaced, "1.0.1", 1, profile, cache); err == nil {
		t.Fatal("a changed offer was accepted")
	}
	put(t, filepath.Join(cache, offer.SHA256+".payload"), []byte("replacement"))
	if _, err := repository.AcceptCached(ctx, &offer, "1.0.1", 1, profile, cache); err != nil {
		t.Fatal(err)
	}
	// Cached VerifiedMixerPayload owns its bytes; mutating disk cache doesn't affect it.
	if !samePayload(cached, verified) {
		t.Fatal("the verified payload changed")
	}
	for _, c := range []struct {
		app      string
		protocol uint32
		profile  string
	}{{"0.9.0", 1, profile}, {"1.0.1", 2, profile}, {"1.0.1", 1, "unknown"}} {
		if _, err := repository.Download(ctx, "payload", c.app, c.protocol, c.profile); err == nil {
			t.Fatalf("%+v accepted", c)
		}
	}
	put(t, filepath.Join(f.dir, "payload"), []byte("tampered"))
	if _, err := repository.Download(ctx, "payload", "1.0.1", 1, profile); err == nil {
		t.Fatal("tampered payload accepted")
	}
	// A previously accepted snapshot cannot be replaced by an older signed one.
	f.publish(1, "2099-01-01T00:00:00Z")
	if _, err := f.load(); err == nil {
		t.Fatal("rollback accepted")
	}
	f.publish(3, "2000-01-01T00:00:00Z")
	if _, err := f.load(); err == nil {
		t.Fatal("expired metadata accepted")
	}
	f.publish(4, "2099-01-01T00:00:00Z")
	timestamp := filepath.Join(f.dir, "metadata/timestamp.json")
	data, err := os.ReadFile(timestamp)
	must(t, err)
	value, err := sjson.Value(data)
	must(t, err)
	value.(map[string]any)["signed"].(map[string]any)["version"] = 999
	data, err = sjson.Marshal(value)
	must(t, err)
	must(t, os.WriteFile(timestamp, data, 0o644))
	if _, err := f.load(); err == nil {
		t.Fatal("a changed timestamp was accepted")
	}
	// Valid signature, but inconsistent payload identity, must still fail.
	f.metadata.Version = "9.9.9"
	f.publish(5, "2099-01-01T00:00:00Z")
	repository, err = f.load()
	must(t, err)
	if _, err := repository.Download(ctx, "payload", "1.0.1", 1, f.metadata.FirmwareProfiles[0]); err == nil {
		t.Fatal("an inconsistent payload identity was accepted")
	}
}

func TestTrustedRootRotatesOnlyWithAValidOldAndNewSignatureChain(t *testing.T) {
	ctx := context.Background()
	f := newMixerFixture(t)
	f.publish(1, "2099-01-01T00:00:00Z")
	_, err := f.load()
	must(t, err)
	f.repo.RotateRoot()
	f.publish(2, "2099-01-01T00:00:00Z")
	repository, err := f.load()
	must(t, err)
	offers, err := repository.Offers()
	must(t, err)
	_, err = repository.Accept(ctx, &offers[0], "1.0.1", 1, offers[0].Metadata.FirmwareProfiles[0])
	must(t, err)
	// A different self-signed root cannot replace the root shipped by the app.
	impostor := newMixerFixture(t)
	put(t, filepath.Join(f.dir, "metadata/2.root.json"), impostor.repo.Trusted)
	// The verified chain is kept, so the mirror's copy is never read again...
	kept, err := f.load()
	must(t, err)
	if v := kept.updater.Trusted().Root().Version; v != 2 {
		t.Fatalf("root version %d", v)
	}
	// ...and without it, the chain from the shipped root runs through the
	// impostor and fails.
	must(t, os.RemoveAll(filepath.Join(f.dir, "state")))
	if _, err := f.load(); err == nil {
		t.Fatal("an impostor root was accepted")
	}
}

func TestUpdatesComeOnlyFromTheTwoOfficialHosts(t *testing.T) {
	metadataBase, err := url.Parse(metadataURL)
	must(t, err)
	targetsBase, err := url.Parse(targetsURL)
	must(t, err)
	must(t, checkRepositoryURL(metadataBase, metadataHost))
	must(t, checkRepositoryURL(targetsBase, targetsHost))
	if !strings.HasSuffix(metadataBase.Path, "/") || !strings.HasSuffix(targetsBase.Path, "/") {
		t.Fatal("bases must end in a slash")
	}
	// File names are joined onto the base, so the branch path must survive.
	joined, err := tuf.Join(metadataBase, "1.root.json")
	must(t, err)
	if joined.String() != "https://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/1.root.json" {
		t.Fatal(joined)
	}
	if checkRepositoryURL(metadataBase, targetsHost) == nil || checkRepositoryURL(targetsBase, metadataHost) == nil {
		t.Fatal("hosts swapped")
	}
	for _, bad := range []string{
		"http://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/",
		"https://raw.githubusercontent.com.example.com/x/",
		"https://evil.raw.githubusercontent.com/x/",
		"https://raw.githubusercontent.com:8443/x/",
		"https://user@raw.githubusercontent.com/x/",
	} {
		u, err := url.Parse(bad)
		must(t, err)
		if checkRepositoryURL(u, metadataHost) == nil {
			t.Fatalf("%s accepted", bad)
		}
	}
}

func TestPublishedMetadataBelongsTogetherOnlyWhenListedExactly(t *testing.T) {
	snapshot := `{"signed":{"version":4}}`
	hash := common.SHA256Hex([]byte(snapshot))
	timestamp := `{"signed":{"meta":{"snapshot.json":{"length":` + itoa(len(snapshot)) + `,"hashes":{"sha256":"` + hash + `"}}}}}`
	if !lists(timestamp, "snapshot.json", snapshot) {
		t.Fatal("an exact listing refused")
	}
	// A snapshot published after this timestamp no longer matches it.
	if lists(timestamp, "snapshot.json", `{"signed":{"version":5}}`) {
		t.Fatal("a newer snapshot matched")
	}
	if lists(timestamp, "targets.json", snapshot) || lists("not json", "snapshot.json", snapshot) {
		t.Fatal("unlisted file matched")
	}
	// A listing without hashes or length still names the file.
	if !lists(`{"signed":{"meta":{"targets.json":{"version":3}}}}`, "targets.json", "anything") {
		t.Fatal("a bare listing refused")
	}
}

func itoa(n int) string {
	data, _ := sjson.Marshal(n)
	return string(data)
}
