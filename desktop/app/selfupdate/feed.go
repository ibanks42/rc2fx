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

// SignedFeed is a custom Velopack update source that filters out delta
// packages and verifies each full package against signed release metadata
// before Velopack renames it into place. Verifying earlier is required on
// Windows because Velopack replaces its own updater immediately after rename.
type SignedFeed struct {
	url    string
	checks *Checks
	verify Verify
	client *http.Client

	mu       sync.Mutex
	progress *Progress
	// Captures the real feed/download error since the C library only reports
	// a generic callback failure.
	failure *Error
}

func NewSignedFeed(checks *Checks) *SignedFeed {
	return &SignedFeed{url: Feed, checks: checks, verify: appupdate.VerifyUpdatePackage, client: http.DefaultClient}
}

func (f *SignedFeed) setFailure(err *Error) {
	f.mu.Lock()
	f.failure = err
	f.mu.Unlock()
}

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

type assetFeed struct {
	Assets []Asset `json:"Assets"`
}

// FilterFeed strips delta and foreign-platform entries from a Velopack release
// feed and re-encodes it using Velopack's JSON field names.
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

// ReleaseFeed fetches and filters releases.{channel}.json for Velopack.
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

// DownloadReleaseEntry downloads asset to Velopack's temporary localFile,
// reports progress 0..100, and verifies the signed release before returning.
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
			// Keep the unverified file under its final name so a retry can
			// re-verify without re-downloading. Returning the error still aborts
			// Velopack, so it won't unpack an unverified updater on Windows.
			os.Rename(localFile, filepath.Join(filepath.Dir(localFile), name))
		}
	}
	if result != nil {
		return result
	}
	return nil
}

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
				// Report in 5% increments to avoid flooding progress updates.
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

// Progress relays download percentages and the Verifying sentinel from the
// background download goroutine to the UI tick loop. Once Verifying is sent,
// subsequent percentage reports (e.g. Velopack's trailing 100%) are ignored.
type Progress struct {
	mu        sync.Mutex
	latest    int16
	has       bool
	verifying bool
	ended     bool
	version   string
	fetching  bool
}

func NewProgress() *Progress { return &Progress{} }

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

func (p *Progress) drain() (latest int16, has bool, version string, fetching, ended bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	latest, has = p.latest, p.has
	p.has = false
	return latest, has, p.version, p.fetching, p.ended
}
