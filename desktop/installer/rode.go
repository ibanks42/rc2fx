package installer

// Official RØDE release discovery, verified downloads, and local archive patching.

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

// patchArchiveForModel patches src into dst with extra entries (see archive.Patch),
// verifying stock SSH invariants, writing the on-device file manifest, and
// checking unpacked root filesystem size.
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

// InspectReleases returns qualified catalog releases for model (read-only).
func (e *Engine) InspectReleases(model firmware.Model) ([]Release, error) {
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	return releasesFromCatalog(model, catalog)
}

// RodeReleases fetches RØDE's published stock releases for model, newest first.
func RodeReleases(model firmware.Model) ([]Release, error) {
	stock, err := rode.Releases(rode.HTTPClient(), model.Hardware(), manifestCodec)
	if err != nil {
		return nil, err
	}
	return fromRode(model, stock), nil
}

var manifestCodec = rode.Codec{Marshal: sjson.Marshal, Unmarshal: sjson.Unmarshal}

func releasesFromManifest(model firmware.Model, data []byte) ([]Release, error) {
	stock, err := rode.ReleasesFromManifest(model.Hardware(), data, manifestCodec)
	if err != nil {
		return nil, err
	}
	return fromRode(model, stock), nil
}

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

// download fetches a release to dest, verifying MD5 and (if pinned) SHA-256.
// Existing matching files are reused without a network request.
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

func findRelease(releases []Release, channel, version string) (Release, bool) {
	i := slices.IndexFunc(releases, func(r Release) bool {
		return r.Channel == channel && r.Version == version
	})
	if i < 0 {
		return Release{}, false
	}
	return releases[i], true
}

func (e *Engine) findCatalogProfile(version string, reverse bool) (*Profile, error) {
	catalog, err := e.catalog()
	if err != nil {
		return nil, err
	}
	if reverse {
		for i := len(catalog.Profiles) - 1; i >= 0; i-- {
			if c := &catalog.Profiles[i]; c.Model == e.model.ID() && c.Version == version {
				return c, nil
			}
		}
		return nil, nil
	}
	for i := range catalog.Profiles {
		if c := &catalog.Profiles[i]; c.Model == e.model.ID() && c.Version == version && supported(c) {
			return c, nil
		}
	}
	return nil, nil
}

func (e *Engine) reusablePrepared(saved *Saved, stock bool, channel, version, runtime, settings string) *Prepared {
	old := saved.Prepared.clone()
	if old == nil || old.Stock != stock || old.ModelOr(firmware.ModelProIi) != e.model ||
		old.Version != version || old.Channel != channel {
		return nil
	}
	if !stock && (old.Runtime != runtime || old.Settings != settings) {
		return nil
	}
	if e.verifyPrepared(old) != nil {
		return nil
	}
	return old
}

func (e *Engine) fetchMD5(digestURL string, checkHex bool) (string, error) {
	raw, err := e.get(digestURL, 128)
	if err != nil {
		return "", err
	}
	md := strings.ToLower(strings.TrimSpace(strings.ToValidUTF8(string(raw), "�")))
	if checkHex && (len(md) != 32 || !isHexText(md)) {
		return "", Error("RØDE's checksum file is not an MD5")
	}
	return md, nil
}

func (e *Engine) downloadVerifiedRelease(selected Release, md string, pinned *Profile) (string, *string, error) {
	if pinned != nil && md != pinned.ArchiveMD5 {
		return "", nil, Error("RØDE's checksum differs from the inspected release")
	}
	var sha *string
	if pinned != nil {
		sha = &pinned.ArchiveSHA
	}
	src := filepath.Join(e.Work, "rode-original.tar.gz")
	if err := e.download(selected.URL, src, sha, md); err != nil {
		return "", nil, err
	}
	if e.model == firmware.ModelDuo {
		// Shared executable hashes are NOT board identity. Verify the exact
		// Duo archive, hostname and DTB before adding anything.
		if _, err := InspectArchive(e.model, src); err != nil {
			return "", nil, err
		}
	}
	return src, sha, nil
}

func (e *Engine) commitPrepared(saved *Saved, prepared *Prepared, md5Text string) (*Prepared, error) {
	if err := e.anchorPrepared(prepared); err != nil {
		return nil, err
	}
	if err := archive.AtomicFile(filepath.Join(e.Work, "archive.md5"), []byte(md5Text)); err != nil {
		return nil, err
	}
	saved.Prepared = prepared.clone()
	if err := e.save(saved); err != nil {
		return nil, err
	}
	return prepared, nil
}

// prepare downloads, verifies, and patches the selected release with RC2FX
// and the latest backed-up settings for serial, reusing a verified cache match.
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
	selected, ok := findRelease(releases, channel, version)
	if !ok {
		return nil, Error("this firmware is not in this RC2FX build's qualified catalog")
	}
	saved, err := e.load()
	if err != nil {
		return nil, err
	}
	p, err := e.findCatalogProfile(version, true)
	if err != nil {
		return nil, err
	}
	if p == nil || !supported(p) {
		return nil, Error("this firmware is not supported by RC2FX")
	}
	entries, runtime, settings, err := e.imageFor(p, serial)
	if err != nil {
		return nil, err
	}
	if old := e.reusablePrepared(saved, false, channel, version, runtime, settings); old != nil {
		return old, nil
	}
	md, err := e.fetchMD5(selected.DigestURL, false)
	if err != nil {
		return nil, err
	}
	src, shaPin, err := e.downloadVerifiedRelease(selected, md, p)
	if err != nil {
		return nil, err
	}
	dst := filepath.Join(e.Work, "archive.tar.gz")
	e.step("patch", "Building the firmware with RC2FX")
	built, err := patchArchiveForModel(e.model, src, dst, entries, shaPin)
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
	return e.commitPrepared(saved, prepared, prepared.ArchiveMD5)
}

// prepareStock downloads, verifies, and stages an unmodified RØDE release.
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
	selected, ok := findRelease(stock, channel, version)
	if !ok {
		return nil, Error("RØDE doesn't list this firmware")
	}
	if old := e.reusablePrepared(saved, true, channel, version, "", ""); old != nil {
		return old, nil
	}
	md, err := e.fetchMD5(selected.DigestURL, true)
	if err != nil {
		return nil, err
	}
	pinned, err := e.findCatalogProfile(version, false)
	if err != nil {
		return nil, err
	}
	src, sha, err := e.downloadVerifiedRelease(selected, md, pinned)
	if err != nil {
		return nil, err
	}
	dst := filepath.Join(e.Work, "archive.tar.gz")
	part := filepath.Join(e.Work, ".archive.tar.gz.part")
	if err := os.Remove(part); err != nil && !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	if err := archive.CopyWithProgress(src, part, e.reporter("verify", "Staging the stock firmware")); err != nil {
		return nil, err
	}
	// Ensure cache files stay owned by the desktop user when run via the root helper.
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
	return e.commitPrepared(saved, prepared, md)
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

// verifyPrepared checks the staged archive, checksums, helper anchor, and
// runtime digest before any USB command is sent.
func (e *Engine) verifyPrepared(p *Prepared) error {
	model := p.ModelOr(firmware.ModelProIi)
	if err := model.RequireRuntimeWith(e.allowExperimentalDuo); err != nil {
		return Error(err.Error())
	}
	// Unpinned stock releases rely on the helper's root-owned anchor.
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
