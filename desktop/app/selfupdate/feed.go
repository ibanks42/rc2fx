package selfupdate

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
)

// SignedFeed is the release feed, checking each package against its signed
// release as it downloads, before Velopack renames it into place. A later
// check is too late on Windows: right after that rename Velopack replaces
// its own updater with the one in the package. Delta packages are left out
// of the feed, so only whole, signed release assets are ever fetched.
//
// Velopack calls it through its custom update source (vpkc), on its own
// threads: ReleaseFeed for check_for_updates, DownloadReleaseEntry for
// download_updates. It does what Velopack's own HttpSource does, in Go: the
// C library has no way to wrap its own HTTP source.
type SignedFeed struct {
	url    string
	checks *Checks
	verify Verify
	client *http.Client

	mu sync.Mutex
	// progress is the running download's, for the Verifying marker.
	progress *Progress
	// failure is why the last feed or download request failed, which the C
	// library reports only as "the callback failed".
	failure *Error
}

// NewSignedFeed is the signed feed at Feed, checking packages with the
// installer's update package verification.
func NewSignedFeed(checks *Checks) *SignedFeed {
	return &SignedFeed{url: Feed, checks: checks, verify: appupdate.VerifyUpdatePackage, client: http.DefaultClient}
}

func (f *SignedFeed) setFailure(err *Error) {
	f.mu.Lock()
	f.failure = err
	f.mu.Unlock()
}

// takeFailure is why the last request failed, forgetting it; nil if it did
// not.
func (f *SignedFeed) takeFailure() *Error {
	f.mu.Lock()
	defer f.mu.Unlock()
	err := f.failure
	f.failure = nil
	return err
}

func (f *SignedFeed) setProgress(p *Progress) {
	f.mu.Lock()
	f.progress = p
	f.mu.Unlock()
}

// join is HttpSource's `Url::parse(url + "/").join(name)`.
func (f *SignedFeed) join(name string) (string, error) {
	base, err := url.Parse(strings.TrimRight(f.url, "/") + "/")
	if err != nil {
		return "", err
	}
	ref, err := url.Parse(name)
	if err != nil {
		return "", err
	}
	return base.ResolveReference(ref).String(), nil
}

// networkError is velopack::Error::Network, for a request that failed or a
// status ureq treats as an error.
func networkError(err error) *Error {
	return &Error{Kind: ErrNetwork, Text: "Network error: " + err.Error()}
}

func (f *SignedFeed) get(address string) (*http.Response, *Error) {
	response, err := f.client.Get(address)
	if err != nil {
		return nil, networkError(err)
	}
	if response.StatusCode >= 400 {
		response.Body.Close()
		return nil, networkError(fmt.Errorf("http status: %d", response.StatusCode))
	}
	return response, nil
}

// assetFeed is VelopackAssetFeed. Velopack reads it leniently (keys in any
// case, missing fields empty, unknown fields ignored), as encoding/json
// does.
type assetFeed struct {
	Assets []Asset `json:"Assets"`
}

// FilterFeed reads a release feed and returns it with only FullPackages,
// written back with Velopack's own field names, so Velopack reads exactly
// the assets checked here.
func FilterFeed(raw []byte) ([]byte, error) {
	var feed assetFeed
	if err := json.Unmarshal(raw, &feed); err != nil {
		return nil, err
	}
	feed.Assets = FullPackages(feed.Assets)
	if feed.Assets == nil {
		feed.Assets = []Asset{}
	}
	return json.Marshal(feed)
}

// ReleaseFeed is get_release_feed: GET `{Feed}/{releasesName}`
// (`releases.{channel}.json`), then FullPackages. (Velopack's HttpSource
// also sent localVersion, id and stagingId as a query, which GitHub's
// release downloads ignore; the C source is not given them.)
func (f *SignedFeed) ReleaseFeed(releasesName string) ([]byte, error) {
	err := f.releaseFeed(releasesName)
	if err.err != nil {
		f.setFailure(err.err)
		return nil, err.err
	}
	f.setFailure(nil)
	return err.feed, nil
}

type feedResult struct {
	feed []byte
	err  *Error
}

func (f *SignedFeed) releaseFeed(releasesName string) feedResult {
	address, err := f.join(releasesName)
	if err != nil {
		return feedResult{err: networkError(err)}
	}
	response, failure := f.get(address)
	if failure != nil {
		return feedResult{err: failure}
	}
	defer response.Body.Close()
	body, err := io.ReadAll(response.Body)
	if err != nil {
		return feedResult{err: networkError(err)}
	}
	filtered, err := FilterFeed(body)
	if err != nil {
		return feedResult{err: &Error{Kind: ErrOther, Text: "Json error: " + err.Error()}}
	}
	return feedResult{feed: filtered}
}

// DownloadReleaseEntry is download_release_entry: download asset to
// localFile (Velopack's `.partial` file) as HttpSource did, reporting 0 to
// 100 through report, then check it against the signed release before
// Velopack may rename it into place. A failure stops Velopack.
func (f *SignedFeed) DownloadReleaseEntry(asset Asset, localFile string, report func(int16)) error {
	if err := f.download(asset, localFile, report); err != nil {
		f.setFailure(err)
		return err
	}
	f.setFailure(nil)
	f.mu.Lock()
	progress := f.progress
	f.mu.Unlock()
	if progress != nil {
		progress.Send(Verifying)
	}
	result := VerifyFile(asset, localFile, f.verify)
	f.checks.Record(asset, result)
	if result != nil && !result.Rejected {
		if name, ok := PackageName(asset.FileName); ok {
			// Kept under its final name to check again without a download.
			// The error below still stops Velopack, so on Windows it never
			// takes its updater from an unchecked package; a package already
			// in place is only checked, never unpacked, by the next attempt.
			os.Rename(localFile, filepath.Join(filepath.Dir(localFile), name))
		}
	}
	if result != nil {
		return result
	}
	return nil
}

// download is velopack's download_url_to_file: progress in steps of 5 %,
// only when the length is known.
func (f *SignedFeed) download(asset Asset, localFile string, report func(int16)) *Error {
	address, err := f.join(asset.FileName)
	if err != nil {
		return networkError(err)
	}
	response, failure := f.get(address)
	if failure != nil {
		return failure
	}
	defer response.Body.Close()
	var total uint64
	known := false
	if length := response.Header.Get("Content-Length"); length != "" {
		total, err = strconv.ParseUint(length, 10, 64)
		known = err == nil
	}
	file, err := os.Create(localFile)
	if err != nil {
		return &Error{Kind: ErrOther, Text: "IO error: " + err.Error()}
	}
	defer file.Close()
	buffer := make([]byte, 2*1024*1024)
	var downloaded uint64
	var last int16
	for {
		n, err := response.Body.Read(buffer)
		if n > 0 {
			if _, werr := file.Write(buffer[:n]); werr != nil {
				return &Error{Kind: ErrOther, Text: "IO error: " + werr.Error()}
			}
			downloaded += uint64(n)
			if known {
				// floor to nearest 5% to reduce message spam
				step := int16(float64(downloaded)/float64(total)*20) * 5
				if step > last {
					last = step
					report(last)
				}
			}
		}
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return networkError(err)
		}
	}
	if err := file.Close(); err != nil {
		return &Error{Kind: ErrOther, Text: "IO error: " + err.Error()}
	}
	return nil
}

// Progress is the download's progress channel: the latest value sent, and
// whether the download has ended (its sender dropped). Once Verifying is
// sent, later percentages are ignored: the C library sends 100 after the
// source returns.
type Progress struct {
	mu        sync.Mutex
	latest    int16
	has       bool
	verifying bool
	ended     bool
	version   string
	fetching  bool
}

// NewProgress is an empty progress channel.
func NewProgress() *Progress { return &Progress{} }

// Send is one progress value: 0 to 100, or Verifying.
func (p *Progress) Send(value int16) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ended || (p.verifying && value != Verifying) {
		return
	}
	p.latest, p.has = value, true
	if value == Verifying {
		p.verifying = true
	}
}

func (p *Progress) end() {
	p.mu.Lock()
	p.ended = true
	p.mu.Unlock()
}

func (p *Progress) setVersion(version string) {
	p.mu.Lock()
	p.version, p.fetching = version, true
	p.mu.Unlock()
}

// drain reads the channel without waiting: the latest value since the last
// drain (has false for none), the version being fetched for Verifying, and
// whether the download has ended.
func (p *Progress) drain() (latest int16, has bool, version string, fetching, ended bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	latest, has = p.latest, p.has
	p.has = false
	return latest, has, p.version, p.fetching, p.ended
}
