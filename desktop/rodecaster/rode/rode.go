// Package rode talks to RØDE's update server: its URLs, the firmware
// manifest it publishes for each model, and verified firmware downloads.
package rode

import (
	"encoding/json"
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

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// ValidURL is whether value is an HTTPS URL with a path on RØDE's update
// server.
func ValidURL(value string) bool {
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

// HTTPClient is the client for RØDE's update server: it never follows a
// redirect away from it.
func HTTPClient() *http.Client {
	return &http.Client{
		Timeout: 20 * time.Minute,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= 8 || !ValidURL(req.URL.String()) {
				return errors.New("redirect away from RØDE's update server")
			}
			return nil
		},
	}
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

// Location is value as an HTTPS URL, if it names suffix for version in
// m's firmware folder on RØDE's update server. A missing scheme is taken as
// HTTPS, as RØDE's manifest leaves it out.
func Location(m model.Model, value, suffix, version string) (string, error) {
	v := value
	if !strings.HasPrefix(value, "https://") {
		v = "https://" + value
	}
	u, err := url.Parse(v)
	ok := err == nil &&
		ValidURL(v) &&
		u.Hostname() == "update.rode.com" &&
		validVersion(version) &&
		u.EscapedPath() == fmt.Sprintf("/%s/%s/%s", m.FirmwareDirectory(), version, suffix) &&
		u.RawQuery == "" && !u.ForceQuery &&
		!strings.Contains(v, "#")
	if !ok {
		return "", fmt.Errorf("unexpected %s firmware location", m.ID())
	}
	return v, nil
}

// ManifestBytes is the largest manifest read.
const ManifestBytes = 256 * 1024

// Get fetches a small file from RØDE's update server, at most limit bytes.
func Get(client *http.Client, value string, limit uint64) ([]byte, error) {
	if !ValidURL(value) {
		return nil, errors.New("expected official RØDE HTTPS URL")
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
		return nil, fmt.Errorf("RØDE returned HTTP %d", response.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, int64(limit)+1))
	if err != nil {
		return nil, err
	}
	if uint64(len(data)) > limit {
		return nil, errors.New("metadata exceeds size limit")
	}
	return data, nil
}

// Release is a stock firmware release RØDE publishes.
type Release struct {
	Model   model.Model
	Channel string
	Version string
	// URL is the archive (`os.tar.gz`); DigestURL its MD5 (`os.md5`).
	URL       string
	DigestURL string
}

// Codec is the JSON the manifest is read with. The zero value is
// encoding/json; an application with its own strict decoder passes it.
type Codec struct {
	Marshal   func(any) ([]byte, error)
	Unmarshal func([]byte, any) error
}

func (c Codec) marshal(v any) ([]byte, error) {
	if c.Marshal == nil {
		return json.Marshal(v)
	}
	return c.Marshal(v)
}

func (c Codec) unmarshal(data []byte, v any) error {
	if c.Unmarshal == nil {
		return json.Unmarshal(data, v)
	}
	return c.Unmarshal(data, v)
}

// Releases is the firmware RØDE publishes for m, from its manifest, newest
// first.
func Releases(client *http.Client, m model.Model, codec Codec) ([]Release, error) {
	data, err := Get(client, m.ManifestURL(), ManifestBytes)
	if err != nil {
		return nil, err
	}
	return ReleasesFromManifest(m, data, codec)
}

// channels are the channels RØDE offers to everyone. Its manifest lists
// others (master, private and closed betas, testing) that are not public
// downloads, so they are never offered. A version on both takes the stable
// name.
var channels = [2][2]string{{"main-version", "stable"}, {"beta-version", "beta"}}

type manifestEntry struct {
	Version string `json:"os-version"`
	URL     string `json:"os-URL"`
	MD5     string `json:"os-md5"`
}

// ReleasesFromManifest is the stock releases in RØDE's manifest for m,
// newest first. Only entries on RØDE's own update server are kept; RØDE's
// MD5 is what checks them when they are downloaded.
func ReleasesFromManifest(m model.Model, data []byte, codec Codec) ([]Release, error) {
	var manifest map[string]any
	if err := codec.unmarshal(data, &manifest); err != nil {
		return nil, fmt.Errorf("RØDE's firmware list is not readable: %v", err)
	}
	all := []Release{}
	for _, pair := range channels {
		key, channel := pair[0], pair[1]
		value, ok := manifest[key]
		if !ok {
			continue
		}
		raw, err := codec.marshal(value)
		if err != nil {
			continue
		}
		var entry manifestEntry
		if err := codec.unmarshal(raw, &entry); err != nil {
			continue
		}
		if slices.ContainsFunc(all, func(r Release) bool { return r.Version == entry.Version }) {
			continue
		}
		// Only an entry for this model's folder on RØDE's update server
		// counts; S3 builds, other models' files and anything else are left
		// out.
		u, err1 := Location(m, entry.URL, "os.tar.gz", entry.Version)
		digest, err2 := Location(m, entry.MD5, "os.md5", entry.Version)
		if err1 != nil || err2 != nil {
			continue
		}
		all = append(all, Release{
			Model:     m,
			Channel:   channel,
			Version:   entry.Version,
			URL:       u,
			DigestURL: digest,
		})
	}
	SortNewestFirst(all, func(r Release) string { return r.Version })
	return all, nil
}

// SortNewestFirst sorts releases by version, newest first, keeping the
// order of equal versions.
func SortNewestFirst[T any](releases []T, version func(T) string) {
	key := func(r T) []uint64 {
		var out []uint64
		for _, p := range strings.Split(version(r), ".") {
			n, err := parseU64(p)
			if err != nil {
				n = 0
			}
			out = append(out, n)
		}
		return out
	}
	slices.SortStableFunc(releases, func(a, b T) int {
		return slices.Compare(key(b), key(a))
	})
}

// parseU64 is an optional '+', then digits only, within 64 bits.
func parseU64(text string) (uint64, error) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" || strings.Trim(digits, "0123456789") != "" {
		return 0, errors.New("invalid digit")
	}
	return strconv.ParseUint(digits, 10, 64)
}

// Download is what DownloadFile needs.
type Download struct {
	// Client is called only when a request is needed.
	Client func() *http.Client
	URL    string
	Dest   string
	// SHA256, when set, is the archive's SHA-256 (a pin the caller holds);
	// MD5 is RØDE's.
	SHA256 *string
	MD5    string
	// Limit is the largest download accepted.
	Limit uint64
	// Progress, when set, gets each progress event: phase is "download" or
	// "verification"; detail is the URL or the algorithm.
	Progress func(phase string, completed, total uint64, detail string)
	// Cancel, when set, is checked before each read; an error stops the
	// download with that error.
	Cancel func() error
}

func (d *Download) emit(phase string, completed, total uint64, detail string) {
	if d.Progress != nil {
		d.Progress(phase, completed, total, detail)
	}
}

// DownloadFile fetches an official release to d.Dest, checking RØDE's MD5
// and, when given, its SHA-256. A matching file is kept without a request.
func DownloadFile(d Download) error {
	value, dest, sha, md := d.URL, d.Dest, d.SHA256, d.MD5
	if !ValidURL(value) || md == "" || (sha != nil && *sha == "") {
		return errors.New("download requires an official URL and its firmware checksums")
	}
	if (sha == nil || archive.Matches(dest, archive.SHA256, *sha) == nil) && archive.Matches(dest, archive.MD5, md) == nil {
		return nil
	}
	t, err := archive.NewTemp(filepath.Dir(dest), ".download-")
	if err != nil {
		return err
	}
	defer t.Cleanup()
	response, err := d.Client().Get(value)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("firmware HTTP %d", response.StatusCode)
	}
	total := uint64(max(response.ContentLength, 0))
	event := func(completed uint64) {
		d.emit("download", completed, total, value)
	}
	event(0)
	buf := make([]byte, 1<<16)
	var n, last uint64
	for {
		if d.Cancel != nil {
			if err := d.Cancel(); err != nil {
				return err
			}
		}
		read, err := response.Body.Read(buf)
		if read > 0 {
			n += uint64(read)
			if n > d.Limit {
				return errors.New("firmware exceeds size limit")
			}
			if _, err := t.Write(buf[:read]); err != nil {
				return err
			}
			if n-last >= 1<<20 {
				last = n
				event(n)
			}
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	event(n)
	if err := t.Finish(); err != nil {
		return err
	}
	if sha != nil {
		if err := d.verifyChecksum(t.Path(), archive.SHA256, *sha); err != nil {
			return err
		}
	}
	if err := d.verifyChecksum(t.Path(), archive.MD5, md); err != nil {
		return err
	}
	if err := os.Rename(t.Path(), dest); err != nil {
		return err
	}
	t.Keep()
	return nil
}

func (d *Download) verifyChecksum(path string, a archive.Algorithm, want string) error {
	file, err := archive.OpenRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	total := uint64(info.Size())
	progress := func(completed uint64) {
		d.emit("verification", completed, total, a.Name())
	}
	progress(0)
	var events []uint64
	got, err := archive.HashReader(file, a, func(n uint64) { events = append(events, n) })
	if err != nil {
		return err
	}
	// Report a bounded number of updates after hashing.
	step := max(len(events)/32, 1)
	for i := 0; i < len(events); i += step {
		progress(events[i])
	}
	if want == "" || got != want {
		return fmt.Errorf("checksum mismatch: %s", path)
	}
	return nil
}
