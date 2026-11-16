package installer

// Official RØDE releases, verified downloads and the local firmware patch.

import (
	"bytes"
	"compress/gzip"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
)

// Catalog, Profile, RuntimeProfile and JobLifetime are the firmware
// catalog's shape.
type (
	Catalog        = embed.Catalog
	Profile        = embed.Profile
	RuntimeProfile = embed.RuntimeProfile
	JobLifetime    = embed.JobLifetime
)

func validURL(value string) bool {
	u, err := url.Parse(value)
	if err != nil {
		return false
	}
	host := strings.ToLower(u.Hostname())
	// A bare origin has no path.
	rest, ok := strings.CutPrefix(value, "https://")
	hasPath := ok && strings.Contains(rest, "/")
	portOK := u.Port() == ""
	if !portOK {
		port, err := strconv.ParseUint(u.Port(), 10, 16)
		portOK = err == nil && port == 443
	}
	return u.Scheme == "https" &&
		u.User == nil &&
		hasPath &&
		// RØDE's update server only, redirects included: stock firmware RØDE
		// lists without a catalog pin is trusted for coming from here.
		host == "update.rode.com" &&
		portOK
}

func httpClient() *http.Client {
	return &http.Client{
		Timeout: 20 * time.Minute,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= 8 || !validURL(req.URL.String()) {
				return errors.New("redirect away from RØDE's update server")
			}
			return nil
		},
	}
}

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

func validVersion(v string) bool {
	parts := strings.Split(v, ".")
	if len(parts) != 3 {
		return false
	}
	for _, p := range parts {
		if p == "" || strings.Trim(p, "0123456789") != "" {
			return false
		}
	}
	return true
}

func location(value, suffix, version string) (string, error) {
	return locationForModel(firmware.ModelProIi, value, suffix, version)
}

func locationForModel(model firmware.Model, value, suffix, version string) (string, error) {
	v := value
	if !strings.HasPrefix(value, "https://") {
		v = "https://" + value
	}
	u, err := url.Parse(v)
	ok := err == nil &&
		validURL(v) &&
		u.Hostname() == "update.rode.com" &&
		validVersion(version) &&
		u.EscapedPath() == fmt.Sprintf("/%s/%s/%s", model.FirmwareDirectory(), version, suffix) &&
		u.RawQuery == "" && !u.ForceQuery &&
		!strings.Contains(v, "#")
	if !ok {
		return "", errorf("unexpected %s firmware location", model.ID())
	}
	return v, nil
}

func normalized(name string) string {
	name = strings.TrimPrefix(name, "./")
	return strings.TrimSuffix(name, "/")
}

const (
	// maxMixerMember is the largest mixer executable read into memory.
	maxMixerMember = 64 << 20
	// maxSmallMember is the largest launcher, configuration file or tar
	// metadata record.
	maxSmallMember = 1 << 20
)

// patched is what patchArchiveForModel built.
type patched struct {
	// mixer and launcher are the SHA-256 of the mixer executable and its
	// launcher.
	mixer, launcher string
	// archiveSha and archiveMd5 are the SHA-256 and MD5 of the bytes
	// written. They come from the write itself, so another process changing
	// the cache folder cannot alter them.
	archiveSha, archiveMd5 string
}

func patchArchive(src, dst string, extra []Entry) (string, string, error) {
	p, err := patchArchiveForModel(firmware.ModelProIi, src, dst, extra, nil)
	if err != nil {
		return "", "", err
	}
	return p.mixer, p.launcher, nil
}

// patchArchiveForModel writes a copy of src to dst with extra added. Every
// record, including long-name and PAX records, is copied verbatim; root's
// authorized_keys must be as empty as RØDE ships it. With sourceSha, the
// bytes actually read from src must have that SHA-256: a check of the path
// before this call says nothing about what is read here.
func patchArchiveForModel(model firmware.Model, src, dst string, extra []Entry, sourceSha *string) (*patched, error) {
	important := []string{keyMember, "etc/hostname", "etc/ssh/sshd_config", "usr/bin/rc_audio_mixer", "usr/bin/audio-start.sh"}
	t, err := newTemp(filepath.Dir(dst), ".firmware-")
	if err != nil {
		return nil, err
	}
	defer t.cleanup()
	written := hashingWriter(t)
	gz, err := gzip.NewWriterLevel(written, gzip.DefaultCompression)
	if err != nil {
		return nil, err
	}
	builder := &tarWriter{w: gz}
	srcFile, err := openWorkFile(src)
	if err != nil {
		return nil, err
	}
	defer srcFile.Close()
	source := hashingReader(srcFile)
	seen := map[string]bool{}
	hashes := map[string]string{}
	hostOK, sshOK := false, false
	// Names supplied by preceding GNU long-name or PAX records.
	var longName, paxName *string
	paxSize, hasOpt := false, false
	// What the mixer checks on its first boots, and the space the image
	// takes once unpacked (image_check.go).
	var manifest strings.Builder
	var usage rootUsage
	err = walkReader(source, 4<<30, true, func(entry *tarEntry) error {
		header := entry.header
		kind := header.entryType()
		if kind == typeLongName || kind == typePaxLocal {
			if entry.size > maxSmallMember {
				return Error("oversized firmware metadata")
			}
			data, err := readAll(entry)
			if err != nil {
				return err
			}
			if kind == typeLongName {
				name := strings.ToValidUTF8(string(truncateNUL(data)), "�")
				longName = &name
			} else {
				records, err := paxRecords(data)
				if err != nil {
					return err
				}
				_, paxSize = records["size"]
				paxName = nil
				if p, ok := records["path"]; ok {
					name := strings.ToValidUTF8(string(p), "�")
					paxName = &name
				}
			}
			return builder.append(&header, bytes.NewReader(data))
		}
		var name string
		switch {
		case paxName != nil:
			name, paxName = *paxName, nil
		case longName != nil:
			name, longName = *longName, nil
		default:
			name = entry.path
		}
		sizedByPax := paxSize
		paxSize = false
		n := normalized(name)
		if slices.ContainsFunc(extra, func(e Entry) bool { return e.Path == n }) {
			return errorf("the firmware already contains %s", n)
		}
		if n == "opt" {
			hasOpt = true
		}
		switch {
		case kind == typeDir:
			usage.block()
		case kind == typeSymlink:
			usage.symlink(len(truncateNUL(header[157:257])))
		case header.regular():
			usage.file(entry.size)
		}
		record := func(dataSha string) {
			if header.regular() && inManifest(n) {
				manifest.WriteString(line(dataSha, n))
			}
		}
		if slices.Contains(important, n) {
			if seen[n] || !header.regular() || sizedByPax {
				return errorf("duplicate or nonregular required member %s", n)
			}
			seen[n] = true
		}
		if n == keyMember || n == "etc/hostname" || n == "etc/ssh/sshd_config" {
			if entry.size > maxSmallMember {
				return Error("oversized firmware metadata")
			}
			data, err := readAll(entry)
			if err != nil {
				return err
			}
			switch n {
			case keyMember:
				if len(bytes.TrimSpace(data)) != 0 {
					return Error("authorized_keys is not empty")
				}
			case "etc/hostname":
				hostOK = string(trimASCII(data)) == model.Hostname()
			default:
				text := strings.ToValidUTF8(string(data), "�")
				sshOK = strings.Contains(text, "PermitRootLogin yes") && strings.Contains(text, "AuthorizedKeysFile")
			}
			record(sum(data))
			return builder.append(&header, bytes.NewReader(data))
		}
		if n == "usr/bin/rc_audio_mixer" || n == "usr/bin/audio-start.sh" {
			limit := uint64(maxSmallMember)
			if n == "usr/bin/rc_audio_mixer" {
				limit = maxMixerMember
			}
			if entry.size > limit {
				return errorf("oversized firmware member %s", n)
			}
			data, err := readAll(entry)
			if err != nil {
				return err
			}
			digest := sum(data)
			record(digest)
			hashes[n] = digest
			return builder.append(&header, bytes.NewReader(data))
		}
		hashed := hashingReader(entry)
		if err := builder.append(&header, hashed); err != nil {
			return err
		}
		digest, _ := hashed.finish()
		record(digest)
		return nil
	})
	if err != nil {
		return nil, err
	}
	if len(extra) > 0 && !hasOpt {
		if err := appendEntry(builder, Entry{Kind: EntryDir, Path: "opt", Mode: 0o755}); err != nil {
			return nil, err
		}
	}
	for _, entry := range extra {
		if entry.Kind == EntryDir {
			usage.block()
		} else {
			usage.file(uint64(len(entry.Data)))
		}
		if err := appendEntry(builder, entry); err != nil {
			return nil, err
		}
	}
	if len(extra) > 0 {
		// Only an image with RC2FX has the launcher that reads the manifest.
		list := Entry{Kind: EntryFile, Path: manifestMember, Mode: 0o600, Data: []byte(manifest.String())}
		usage.file(uint64(len(list.Data)))
		if err := appendEntry(builder, list); err != nil {
			return nil, err
		}
		if err := usage.check(); err != nil {
			return nil, err
		}
	}
	if _, err := io.Copy(io.Discard, source); err != nil {
		return nil, err
	}
	if sourceSha != nil {
		if got, _ := source.finish(); got != *sourceSha {
			return nil, Error("the downloaded firmware changed while it was being patched")
		}
	}
	if err := builder.finish(); err != nil {
		return nil, err
	}
	if err := gz.Close(); err != nil {
		return nil, err
	}
	archiveSha, archiveMd5 := written.finish()
	if err := t.finish(); err != nil {
		return nil, err
	}
	if !seen[keyMember] || !hostOK || !sshOK || len(hashes) != 2 {
		return nil, errorf("firmware is not the expected %s rootfs", model.Label())
	}
	info, err := os.Stat(t.path)
	if err != nil {
		return nil, err
	}
	if info.Size() > 275<<20 {
		return nil, Error("patched image exceeds update volume limit")
	}
	if err := t.persist(dst); err != nil {
		return nil, err
	}
	return &patched{
		mixer:      hashes["usr/bin/rc_audio_mixer"],
		launcher:   hashes["usr/bin/audio-start.sh"],
		archiveSha: archiveSha,
		archiveMd5: archiveMd5,
	}, nil
}

// trimASCII is Rust's <[u8]>::trim_ascii.
func trimASCII(data []byte) []byte {
	return bytes.Trim(data, " \t\n\r\f")
}

// appendEntry appends one root-owned entry in the image's `./path` style.
func appendEntry(builder *tarWriter, entry Entry) error {
	path := "./" + entry.Path
	kind, size := byte(typeRegular), uint64(len(entry.Data))
	if entry.Kind == EntryDir {
		path += "/"
		kind, size = typeDir, 0
	}
	header, err := gnuHeader(path, kind, entry.Mode, size, nowUnix())
	if err != nil {
		return err
	}
	data := io.Reader(bytes.NewReader(entry.Data))
	if entry.Kind == EntryDir {
		data = bytes.NewReader(nil)
	}
	return builder.append(header, data)
}

// bounded counts bytes written to an inner writer, rejecting anything past
// a limit.
type bounded struct {
	inner     io.Writer
	remaining uint64
}

func (b *bounded) Write(buf []byte) (int, error) {
	if uint64(len(buf)) > b.remaining {
		return 0, errors.New("response exceeds size limit")
	}
	n, err := b.inner.Write(buf)
	b.remaining -= uint64(n)
	return n, err
}

func (e *Engine) client() *http.Client {
	if e.http == nil {
		e.http = httpClient()
	}
	return e.http
}

func (e *Engine) get(value string, limit uint64) ([]byte, error) {
	return get(e.client(), value, limit)
}

func (e *Engine) catalog() (*Catalog, error) {
	return e.source.catalog()
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
	data, err := e.get(rodeManifest(e.model), manifestBytes)
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

// rodeManifest is RØDE's update manifest for model, the list the
// RØDECaster App reads (`FirmwareData.cpp` in version 2.0.115). Each names
// its model's archives.
func rodeManifest(model firmware.Model) string {
	if model == firmware.ModelDuo {
		return "https://update.rode.com/rcp2-mini-manifest.json"
	}
	return "https://update.rode.com/manifest.json"
}

const manifestBytes = 256 * 1024

// get fetches a small file from RØDE's update server, at most limit bytes.
func get(client *http.Client, value string, limit uint64) ([]byte, error) {
	if !validURL(value) {
		return nil, Error("expected official RØDE HTTPS URL")
	}
	request, err := http.NewRequest(http.MethodGet, value, nil)
	if err != nil {
		return nil, err
	}
	short := *client
	short.Timeout = 30 * time.Second
	response, err := short.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, errorf("RØDE returned HTTP %d", response.StatusCode)
	}
	data, err := readLimited(response.Body, int64(limit)+1)
	if err != nil {
		return nil, err
	}
	if uint64(len(data)) > limit {
		return nil, Error("metadata exceeds size limit")
	}
	return data, nil
}

// RodeReleases is the firmware RØDE publishes for model, from its manifest,
// newest first. It needs no engine or installer folder, so the app can ask
// while it checks for its own updates.
func RodeReleases(model firmware.Model) ([]Release, error) {
	data, err := get(httpClient(), rodeManifest(model), manifestBytes)
	if err != nil {
		return nil, err
	}
	return releasesFromManifest(model, data)
}

// rodeChannels are the channels RØDE offers to everyone. Its manifest lists
// others (master, private and closed betas, testing) that are not public
// downloads, so they are never offered. A version on both takes the stable
// name.
var rodeChannels = [2][2]string{{"main-version", "stable"}, {"beta-version", "beta"}}

type manifestEntry struct {
	Version string `json:"os-version"`
	URL     string `json:"os-URL"`
	MD5     string `json:"os-md5"`
}

// releasesFromManifest is the stock releases in RØDE's manifest for model,
// newest first. Only entries on RØDE's own update server are kept; RØDE's
// MD5 is what checks them when they are downloaded.
func releasesFromManifest(model firmware.Model, data []byte) ([]Release, error) {
	var manifest map[string]any
	if err := sjson.Unmarshal(data, &manifest); err != nil {
		return nil, errorf("RØDE's firmware list is not readable: %v", err)
	}
	all := []Release{}
	for _, pair := range rodeChannels {
		key, channel := pair[0], pair[1]
		value, ok := manifest[key]
		if !ok {
			continue
		}
		raw, err := sjson.Marshal(value)
		if err != nil {
			continue
		}
		var entry manifestEntry
		if err := sjson.Unmarshal(raw, &entry); err != nil {
			continue
		}
		if slices.ContainsFunc(all, func(r Release) bool { return r.Version == entry.Version }) {
			continue
		}
		// Only an entry for this model's folder on RØDE's update server
		// counts; S3 builds, other models' files and anything else are left
		// out.
		u, err1 := locationForModel(model, entry.URL, "os.tar.gz", entry.Version)
		digest, err2 := locationForModel(model, entry.MD5, "os.md5", entry.Version)
		if err1 != nil || err2 != nil {
			continue
		}
		all = append(all, Release{
			Model:     model.ID(),
			Channel:   channel,
			Version:   entry.Version,
			URL:       u,
			DigestURL: digest,
			Supported: true,
		})
	}
	sortNewestFirst(all)
	return all, nil
}

func sortNewestFirst(releases []Release) {
	key := func(r Release) []uint64 {
		var out []uint64
		for _, p := range strings.Split(r.Version, ".") {
			n, err := parseU64(p)
			if err != nil {
				n = 0
			}
			out = append(out, n)
		}
		return out
	}
	slices.SortStableFunc(releases, func(a, b Release) int {
		return slices.Compare(key(b), key(a))
	})
}

// parseU64 is Rust's u64::from_str.
func parseU64(text string) (uint64, error) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" || strings.Trim(digits, "0123456789") != "" {
		return 0, errors.New("invalid digit")
	}
	return strconv.ParseUint(digits, 10, 64)
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
	sortNewestFirst(all)
	return all, nil
}

func (e *Engine) verifyChecksum(path string, a algorithm, want string) error {
	file, err := openRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	total := uint64(info.Size())
	progress := func(completed uint64) Progress {
		return Progress{Phase: "verification", Completed: completed, Total: total, Detail: a.name()}
	}
	e.emit(progress(0))
	var events []uint64
	got, err := hashReader(file, a, func(n uint64) { events = append(events, n) })
	if err != nil {
		return err
	}
	// Report a bounded number of updates after hashing.
	step := max(len(events)/32, 1)
	for i := 0; i < len(events); i += step {
		e.emit(progress(events[i]))
	}
	if want == "" || got != want {
		return errorf("checksum mismatch: %s", path)
	}
	return nil
}

// download fetches an official release to dest, checking RØDE's MD5 and,
// when the catalog pins one, its SHA-256. A matching file is kept. Only
// stock firmware RØDE lists but the catalog doesn't goes without a SHA-256.
func (e *Engine) download(value, dest string, sha *string, md string) error {
	if !validURL(value) || md == "" || (sha != nil && *sha == "") {
		return Error("download requires an official URL and its firmware checksums")
	}
	if (sha == nil || matches(dest, algorithmSha256, *sha) == nil) && matches(dest, algorithmMd5, md) == nil {
		return nil
	}
	t, err := newTemp(filepath.Dir(dest), ".download-")
	if err != nil {
		return err
	}
	defer t.cleanup()
	response, err := e.client().Get(value)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return errorf("firmware HTTP %d", response.StatusCode)
	}
	total := uint64(max(response.ContentLength, 0))
	event := func(completed uint64) Progress {
		return Progress{Phase: "download", Completed: completed, Total: total, Detail: value}
	}
	e.emit(event(0))
	buf := make([]byte, 1<<16)
	var n, last uint64
	for {
		if e.cancelled() {
			return Error(Cancelled)
		}
		read, err := response.Body.Read(buf)
		if read > 0 {
			n += uint64(read)
			if n > maxDownload {
				return Error("firmware exceeds size limit")
			}
			if _, err := t.Write(buf[:read]); err != nil {
				return err
			}
			if n-last >= 1<<20 {
				last = n
				e.emit(event(n))
			}
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	e.emit(event(n))
	if err := t.finish(); err != nil {
		return err
	}
	if sha != nil {
		if err := e.verifyChecksum(t.path, algorithmSha256, *sha); err != nil {
			return err
		}
	}
	if err := e.verifyChecksum(t.path, algorithmMd5, md); err != nil {
		return err
	}
	if err := os.Rename(t.path, dest); err != nil {
		return err
	}
	t.keep()
	return nil
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
func (e *Engine) imageFor(profile *Profile, serial string) ([]Entry, string, string, error) {
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
		digest = sum(settings)
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
	if built.mixer != p.MixerSHA || built.launcher != p.LauncherSHA {
		return nil, Error("firmware binaries do not match the supported profile")
	}
	sha, md5sum, err := hashBoth(dst, e.reporter("verify", "Checking the built firmware"))
	if err != nil {
		return nil, err
	}
	if sha != built.archiveSha || md5sum != built.archiveMd5 {
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
		ArchiveSHA: built.archiveSha,
		ArchiveMD5: built.archiveMd5,
		Runtime:    runtime,
		Settings:   settings,
	}
	if err := e.anchorPrepared(prepared); err != nil {
		return nil, err
	}
	if err := atomicFile(filepath.Join(e.Work, "archive.md5"), []byte(prepared.ArchiveMD5)); err != nil {
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
	if err := copyWithProgress(src, part, e.reporter("verify", "Staging the stock firmware")); err != nil {
		return nil, err
	}
	// The copy is the helper's until now, and the cache must stay the user's.
	if err := chownToInvoker(part); err != nil {
		return nil, err
	}
	if err := replaceLocalFile(part, dst); err != nil {
		return nil, err
	}
	archiveSha, archiveMd5, err := hashBoth(dst, e.reporter("verify", "Checking the stock firmware"))
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
	if err := atomicFile(filepath.Join(e.Work, "archive.md5"), []byte(md)); err != nil {
		return nil, err
	}
	saved.Prepared = prepared.clone()
	if err := e.save(saved); err != nil {
		return nil, err
	}
	return prepared, nil
}

func chownToInvoker(path string) error {
	file, err := openRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	return localOwner(file)
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
