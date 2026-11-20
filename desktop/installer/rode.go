package installer

// Official RØDE releases, verified downloads and the local firmware patch.

import (
	"bytes"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/rode"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// Catalog, Profile, RuntimeProfile and JobLifetime are the firmware
// catalog's shape.
type (
	Catalog        = embed.Catalog
	Profile        = embed.Profile
	RuntimeProfile = embed.RuntimeProfile
	JobLifetime    = embed.JobLifetime
)

func supported(p *Profile) bool {
	r := p.Runtime
	if r == nil {
		return false
	}
	model, err := firmware.ParseModel(p.Model)
	if err != nil {
		return false
	}
	return r.CallbackABI == "jack-process-v1" &&
		r.RouteLayout == model.RouteLayout() &&
		r.SourceEnum == model.SourceEnum() &&
		r.JobLifetime.ABI == "capture-values-v1" &&
		r.SampleRate == 48000 &&
		r.BlockFrames == 112
}

func appSupports(p *Profile, appVersion string) bool {
	if p.MinimumAppVersion == "" {
		return true
	}
	order, ok := core.CompareVersions(appVersion, p.MinimumAppVersion)
	return ok && order >= 0
}

func locationForModel(model firmware.Model, value, suffix, version string) (string, error) {
	return rode.Location(model.Hardware(), value, suffix, version)
}

// patchArchiveForModel writes a copy of src to dst with extra added (see
// archive.Patch). What RC2FX's image needs beyond RØDE's archive is checked
// here: root's authorized_keys must be as empty as RØDE ships it and SSH
// must allow root's key; an image with RC2FX gets the manifest the mixer
// checks on its first boots and must fit the root once unpacked
// (image_check.go).
func patchArchiveForModel(model firmware.Model, src, dst string, extra []archive.Entry, sourceSha *string) (*archive.Patched, error) {
	keyOK, sshOK := false, false
	var manifest strings.Builder
	var usage rootUsage
	return archive.Patch(src, dst, archive.PatchOptions{
		Model:     model.Hardware(),
		Extra:     extra,
		SourceSHA: sourceSha,
		Read:      []string{keyMember, "etc/ssh/sshd_config"},
		Member: func(m archive.Copied) error {
			switch kind := m.Header.EntryType(); {
			case kind == archive.TypeDir:
				usage.block()
			case kind == archive.TypeSymlink:
				usage.symlink(len(m.Header.LinkName()))
			case m.Header.Regular():
				usage.file(m.Size)
			}
			switch m.Name {
			case keyMember:
				if len(bytes.TrimSpace(m.Data)) != 0 {
					return Error("authorized_keys is not empty")
				}
				keyOK = true
			case "etc/ssh/sshd_config":
				text := strings.ToValidUTF8(string(m.Data), "�")
				sshOK = strings.Contains(text, "PermitRootLogin yes") && strings.Contains(text, "AuthorizedKeysFile")
			}
			if m.Header.Regular() && inManifest(m.Name) {
				manifest.WriteString(line(m.SHA256, m.Name))
			}
			return nil
		},
		Finish: func(add func(archive.Entry) error) error {
			for _, entry := range extra {
				if entry.Kind == archive.EntryDir {
					usage.block()
				} else {
					usage.file(uint64(len(entry.Data)))
				}
			}
			// Only an image with RC2FX has the launcher that reads the manifest.
			list := archive.Entry{Kind: archive.EntryFile, Path: manifestMember, Mode: 0o600, Data: []byte(manifest.String())}
			usage.file(uint64(len(list.Data)))
			if err := add(list); err != nil {
				return err
			}
			return usage.check()
		},
		Verify: func() bool { return keyOK && sshOK },
	})
}

func (e *Engine) client() *http.Client {
	if e.http == nil {
		e.http = rode.HTTPClient()
	}
	return e.http
}

func (e *Engine) get(value string, limit uint64) ([]byte, error) {
	return rode.Get(e.client(), value, limit)
}

func (e *Engine) catalog() (*Catalog, error) {
	return e.source.catalogFrom(e.catalogCache())
}

func (e *Engine) releases() ([]Release, error) {
	if err := e.model.RequireRuntimeWith(e.allowExperimentalDuo); err != nil {
		return nil, Error(err.Error())
	}
	return e.InspectReleases(e.model)
}

// stockReleases is the stock firmware RØDE publishes for this model, from
// RØDE's manifest for it.
func (e *Engine) stockReleases() ([]Release, error) {
	if err := e.model.RequireRuntimeWith(e.allowExperimentalDuo); err != nil {
		return nil, Error(err.Error())
	}
	data, err := e.get(e.model.Hardware().ManifestURL(), rode.ManifestBytes)
	if err != nil {
		return nil, err
	}
	return releasesFromManifest(e.model, data)
}

// InspectReleases is read-only discovery, including experimental models. A
// supported experimental profile still requires the separate session
// opt-in.
func (e *Engine) InspectReleases(model firmware.Model) ([]Release, error) {
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	return releasesFromCatalog(model, catalog)
}

// RodeReleases is the firmware RØDE publishes for model, from its manifest,
// newest first. It needs no engine or installer folder, so the app can ask
// while it checks for its own updates.
func RodeReleases(model firmware.Model) ([]Release, error) {
	stock, err := rode.Releases(rode.HTTPClient(), model.Hardware(), manifestCodec)
	if err != nil {
		return nil, err
	}
	return fromRode(model, stock), nil
}

// manifestCodec reads RØDE's manifest as serde_json does.
var manifestCodec = rode.Codec{Marshal: sjson.Marshal, Unmarshal: sjson.Unmarshal}

// releasesFromManifest is the stock releases in RØDE's manifest for model,
// newest first (see rode.ReleasesFromManifest).
func releasesFromManifest(model firmware.Model, data []byte) ([]Release, error) {
	stock, err := rode.ReleasesFromManifest(model.Hardware(), data, manifestCodec)
	if err != nil {
		return nil, err
	}
	return fromRode(model, stock), nil
}

// fromRode is RØDE's releases as releases the app offers.
func fromRode(model firmware.Model, stock []rode.Release) []Release {
	all := []Release{}
	for _, r := range stock {
		all = append(all, Release{
			Model:     model.ID(),
			Channel:   r.Channel,
			Version:   r.Version,
			URL:       r.URL,
			DigestURL: r.DigestURL,
			Supported: true,
		})
	}
	return all
}

// releasesFromCatalog: the signed, qualified catalog decides which firmware
// RC2FX is built into. Stock firmware comes from RØDE's manifest instead.
func releasesFromCatalog(model firmware.Model, c *Catalog) ([]Release, error) {
	all := []Release{}
	for i := range c.Profiles {
		p := &c.Profiles[i]
		if p.Model != model.ID() || !supported(p) {
			continue
		}
		base := fmt.Sprintf("https://update.rode.com/%s/%s", model.FirmwareDirectory(), p.Version)
		u, err := locationForModel(model, base+"/os.tar.gz", "os.tar.gz", p.Version)
		if err != nil {
			return nil, err
		}
		digest, err := locationForModel(model, base+"/os.md5", "os.md5", p.Version)
		if err != nil {
			return nil, err
		}
		all = append(all, Release{
			MinimumAppVersion: p.MinimumAppVersion,
			Profile:           p.ID,
			Model:             model.ID(),
			URL:               u,
			DigestURL:         digest,
			Version:           p.Version,
			// Every currently qualified catalog release is stable.
			Channel:   "stable",
			Supported: appSupports(p, AppVersion),
		})
	}
	// Newest qualified stable release is the default, not catalog file order.
	rode.SortNewestFirst(all, func(r Release) string { return r.Version })
	return all, nil
}

// download fetches an official release to dest, checking RØDE's MD5 and,
// when the catalog pins one, its SHA-256. A matching file is kept. Only
// stock firmware RØDE lists but the catalog doesn't goes without a SHA-256.
func (e *Engine) download(value, dest string, sha *string, md string) error {
	return rode.DownloadFile(rode.Download{
		Client: e.client,
		URL:    value,
		Dest:   dest,
		SHA256: sha,
		MD5:    md,
		Limit:  maxDownload,
		Progress: func(phase string, completed, total uint64, detail string) {
			e.emit(Progress{Phase: phase, Completed: completed, Total: total, Detail: detail})
		},
		Cancel: func() error {
			if e.canceled() {
				return Error(Canceled)
			}
			return nil
		},
	})
}

func (e *Engine) profile(id string) (*Profile, error) {
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	for i := range catalog.Profiles {
		p := &catalog.Profiles[i]
		if p.ID == id && supported(p) && appSupports(p, AppVersion) {
			return p, nil
		}
	}
	return nil, errorf("firmware profile %s is not supported", id)
}

// imageFor is what the image for profile adds: the runtime, autostart and
// the newest settings saved for serial. Returns the entries, the runtime
// digest and the settings digest (empty without settings).
func (e *Engine) imageFor(profile *Profile, serial string) ([]archive.Entry, string, string, error) {
	bundle, err := e.source.bundle()
	if err != nil {
		return nil, "", "", err
	}
	saved, err := e.load()
	if err != nil {
		return nil, "", "", err
	}
	var settings []byte
	if b := saved.BackupFor(serial); b != nil {
		if settings, err = backupSettings(e.Work, b); err != nil {
			return nil, "", "", err
		}
	}
	digest := ""
	if settings != nil {
		digest = archive.Sum(settings)
	}
	entries, runtime, err := imageEntries(bundle, profile, settings)
	if err != nil {
		return nil, "", "", err
	}
	return entries, runtime, digest, nil
}

// prepare downloads and verifies the selected release, then builds firmware
// with RC2FX's runtime, autostart and the newest settings saved for serial.
// A matching earlier preparation is reused after verification.
func (e *Engine) prepare(channel, version, serial string, stock bool) (*Prepared, error) {
	if err := e.model.RequireRuntimeWith(e.allowExperimentalDuo); err != nil {
		return nil, Error(err.Error())
	}
	if stock {
		return e.prepareStock(channel, version)
	}
	// Validate before cache reuse or any preparation side effects.
	releases, err := e.releases()
	if err != nil {
		return nil, err
	}
	if !slices.ContainsFunc(releases, func(r Release) bool { return r.Channel == channel && r.Version == version }) {
		return nil, Error("this firmware is not in this RC2FX build's qualified catalog")
	}
	saved, err := e.load()
	if err != nil {
		return nil, err
	}
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	var p *Profile
	for i := len(catalog.Profiles) - 1; i >= 0; i-- {
		if c := &catalog.Profiles[i]; c.Model == e.model.ID() && c.Version == version {
			p = c
			break
		}
	}
	if p == nil || !supported(p) {
		return nil, Error("this firmware is not supported by RC2FX")
	}
	entries, runtime, settings, err := e.imageFor(p, serial)
	if err != nil {
		return nil, err
	}
	if old := saved.Prepared.clone(); old != nil &&
		!old.Stock &&
		old.ModelOr(firmware.ModelProIi) == e.model &&
		old.Version == version &&
		old.Channel == channel &&
		old.Runtime == runtime &&
		old.Settings == settings &&
		e.verifyPrepared(old) == nil {
		return old, nil
	}
	releases, err = e.releases()
	if err != nil {
		return nil, err
	}
	i := slices.IndexFunc(releases, func(r Release) bool { return r.Channel == channel && r.Version == version })
	if i < 0 {
		return nil, Error("firmware is not in the qualified catalog")
	}
	selected := releases[i]
	md, err := e.get(selected.DigestURL, 128)
	if err != nil {
		return nil, err
	}
	if strings.ToLower(strings.TrimSpace(strings.ToValidUTF8(string(md), "�"))) != p.ArchiveMD5 {
		return nil, Error("RØDE's checksum differs from the inspected release")
	}
	src := filepath.Join(e.Work, "rode-original.tar.gz")
	if err := e.download(selected.URL, src, &p.ArchiveSHA, p.ArchiveMD5); err != nil {
		return nil, err
	}
	dst := filepath.Join(e.Work, "archive.tar.gz")
	e.step("patch", "Building the firmware with RC2FX")
	if e.model == firmware.ModelDuo {
		// Shared executable hashes are NOT board identity. Verify the exact
		// Duo archive, hostname and DTB before adding anything.
		if _, err := InspectArchive(e.model, src); err != nil {
			return nil, err
		}
	}
	built, err := patchArchiveForModel(e.model, src, dst, entries, &p.ArchiveSHA)
	if err != nil {
		return nil, err
	}
	if built.Mixer != p.MixerSHA || built.Launcher != p.LauncherSHA {
		return nil, Error("firmware binaries do not match the supported profile")
	}
	sha, md5sum, err := archive.HashBoth(dst, e.reporter("verify", "Checking the built firmware"))
	if err != nil {
		return nil, err
	}
	if sha != built.ArchiveSHA || md5sum != built.ArchiveMD5 {
		return nil, Error("the built firmware changed on disk; prepare it again")
	}
	model, err := firmware.ParseModel(p.Model)
	if err != nil {
		return nil, Error(err.Error())
	}
	prepared := &Prepared{
		Model:      &model,
		Version:    p.Version,
		Channel:    channel,
		Profile:    p.ID,
		ArchiveSHA: built.ArchiveSHA,
		ArchiveMD5: built.ArchiveMD5,
		Runtime:    runtime,
		Settings:   settings,
	}
	if err := e.anchorPrepared(prepared); err != nil {
		return nil, err
	}
	if err := archive.AtomicFile(filepath.Join(e.Work, "archive.md5"), []byte(prepared.ArchiveMD5)); err != nil {
		return nil, err
	}
	saved.Prepared = prepared.clone()
	if err := e.save(saved); err != nil {
		return nil, err
	}
	return prepared, nil
}

// prepareStock downloads and verifies a release and stages it unchanged:
// stock RØDE firmware, without RC2FX, its key or settings. Only releases
// RØDE lists are offered. RØDE's MD5 checks each one, and the catalog's
// SHA-256 too when it pins that version. The helper records what it staged.
func (e *Engine) prepareStock(channel, version string) (*Prepared, error) {
	saved, err := e.load()
	if err != nil {
		return nil, err
	}
	// Validate before cache reuse or any preparation side effects.
	stock, err := e.stockReleases()
	if err != nil {
		return nil, err
	}
	i := slices.IndexFunc(stock, func(r Release) bool { return r.Channel == channel && r.Version == version })
	if i < 0 {
		return nil, Error("RØDE doesn't list this firmware")
	}
	selected := stock[i]
	if old := saved.Prepared.clone(); old != nil &&
		old.Stock &&
		old.ModelOr(firmware.ModelProIi) == e.model &&
		old.Version == version &&
		old.Channel == channel &&
		e.verifyPrepared(old) == nil {
		return old, nil
	}
	digest, err := e.get(selected.DigestURL, 128)
	if err != nil {
		return nil, err
	}
	md := strings.ToLower(strings.TrimSpace(strings.ToValidUTF8(string(digest), "�")))
	if len(md) != 32 || !isHexText(md) {
		return nil, Error("RØDE's checksum file is not an MD5")
	}
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	var pinned *Profile
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; p.Model == e.model.ID() && p.Version == version && supported(p) {
			pinned = p
			break
		}
	}
	if pinned != nil && pinned.ArchiveMD5 != md {
		return nil, Error("RØDE's checksum differs from the inspected release")
	}
	var sha *string
	if pinned != nil {
		sha = &pinned.ArchiveSHA
	}
	src := filepath.Join(e.Work, "rode-original.tar.gz")
	if err := e.download(selected.URL, src, sha, md); err != nil {
		return nil, err
	}
	if e.model == firmware.ModelDuo {
		if _, err := InspectArchive(e.model, src); err != nil {
			return nil, err
		}
	}
	dst := filepath.Join(e.Work, "archive.tar.gz")
	part := filepath.Join(e.Work, ".archive.tar.gz.part")
	if err := os.Remove(part); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	if err := archive.CopyWithProgress(src, part, e.reporter("verify", "Staging the stock firmware")); err != nil {
		return nil, err
	}
	// The copy is the helper's until now, and the cache must stay the user's.
	if err := chownToInvoker(part); err != nil {
		return nil, err
	}
	if err := archive.ReplaceLocalFile(part, dst); err != nil {
		return nil, err
	}
	archiveSha, archiveMd5, err := archive.HashBoth(dst, e.reporter("verify", "Checking the stock firmware"))
	if err != nil {
		return nil, err
	}
	if archiveMd5 != md || (sha != nil && archiveSha != *sha) {
		return nil, Error("the staged stock firmware does not match RØDE's checksum")
	}
	model := e.model
	profile := ""
	if pinned != nil {
		profile = pinned.ID
	}
	prepared := &Prepared{
		Model:      &model,
		Version:    version,
		Channel:    channel,
		Profile:    profile,
		ArchiveSHA: archiveSha,
		ArchiveMD5: archiveMd5,
		Stock:      true,
	}
	if err := e.anchorPrepared(prepared); err != nil {
		return nil, err
	}
	if err := archive.AtomicFile(filepath.Join(e.Work, "archive.md5"), []byte(md)); err != nil {
		return nil, err
	}
	saved.Prepared = prepared.clone()
	if err := e.save(saved); err != nil {
		return nil, err
	}
	return prepared, nil
}

func chownToInvoker(path string) error {
	file, err := archive.OpenRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	return archive.LocalOwner(file)
}

func isHexText(text string) bool {
	for i := range len(text) {
		c := text[i]
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f' || c >= 'A' && c <= 'F') {
			return false
		}
	}
	return true
}

// verifyPrepared checks the prepared archive and its checksum file, and for
// RC2FX firmware that its runtime is this installer's, before any USB
// command.
func (e *Engine) verifyPrepared(p *Prepared) error {
	// Missing is the legacy Pro II cache format, never automatic detection.
	model := p.ModelOr(firmware.ModelProIi)
	if err := model.RequireRuntimeWith(e.allowExperimentalDuo); err != nil {
		return Error(err.Error())
	}
	// Stock firmware RØDE lists without a catalog pin is checked against what
	// the helper recorded when it staged it.
	var pinned *Profile
	if p.Stock && p.Profile == "" {
		if err := e.checkAnchor(p); err != nil {
			return err
		}
	} else {
		found, err := e.profile(p.Profile)
		if err != nil {
			return err
		}
		if found.Model != model.ID() || found.Version != p.Version {
			return Error("prepared firmware does not match its qualified catalog profile")
		}
		pinned = found
	}
	if p.Stock && pinned != nil && (p.ArchiveSHA != pinned.ArchiveSHA || p.ArchiveMD5 != pinned.ArchiveMD5) {
		return Error("prepared stock firmware differs from its archive pins")
	}
	if model == firmware.ModelDuo {
		if err := verifyArchiveModel(model, filepath.Join(e.Work, "archive.tar.gz")); err != nil {
			return err
		}
	}
	if p.Stock {
		return archiveMatches(e.Work, p, e.reporter("verify", "Checking the prepared firmware"))
	}
	// An older installer could build SSH access into the image.
	if p.Key != "" || p.KeySHA != "" || p.HostKey != "" {
		return Error("the prepared firmware has SSH access built in; prepare it again")
	}
	if err := e.checkAnchor(p); err != nil {
		return err
	}
	if pinned == nil {
		return Error("prepared firmware has no catalog profile")
	}
	bundle, err := e.source.bundle()
	if err != nil {
		return err
	}
	_, runtime, err := imageEntries(bundle, pinned, nil)
	if err != nil {
		return err
	}
	if p.Runtime == "" || p.Runtime != runtime {
		return Error("the prepared firmware has a different RC2FX build; prepare it again")
	}
	return archiveMatches(e.Work, p, e.reporter("verify", "Checking the prepared firmware"))
}
