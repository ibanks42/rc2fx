package tuf

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"
)

// Repository is a source of metadata and target bytes. found is false for
// "not found" (HTTP 404 or 403), which ends the root chain.
type Repository interface {
	FetchMetadata(ctx context.Context, name string, maxLength uint64) (data []byte, found bool, err error)
	FetchTarget(ctx context.Context, path string, maxLength uint64) (data []byte, found bool, err error)
}

// Store keeps verified metadata between runs. Everything loaded from it is
// verified again.
type Store interface {
	Load(name string) (data []byte, found bool, err error)
	Store(name string, data []byte) error
}

// Config holds per-role byte limits and the root-rotation bound.
type Config struct {
	RootMaxLength      uint64
	TimestampMaxLength uint64
	SnapshotMaxLength  uint64
	TargetsMaxLength   uint64
	// TargetMaxLength is the largest target downloaded.
	TargetMaxLength  uint64
	MaxRootRotations uint32
}

// DefaultConfig is sigstore-tuf's (and python-tuf's) defaults.
func DefaultConfig() Config {
	return Config{
		RootMaxLength:      512_000,
		TimestampMaxLength: 16_384,
		SnapshotMaxLength:  2_000_000,
		TargetsMaxLength:   5_000_000,
		TargetMaxLength:    50_000_000,
		MaxRootRotations:   32,
	}
}

// Updater refreshes and verifies metadata from a Repository and downloads
// verified targets.
type Updater struct {
	repo    Repository
	trusted *Trusted
	config  Config
	store   Store
}

// NewUpdater bootstraps an updater from a pinned root.
func NewUpdater(repo Repository, root []byte) (*Updater, error) {
	trusted, err := NewTrusted(root)
	if err != nil {
		return nil, err
	}
	return &Updater{repo: repo, trusted: trusted, config: DefaultConfig()}, nil
}

// WithConfig overrides the limits.
func (u *Updater) WithConfig(c Config) *Updater {
	u.config = c
	return u
}

// WithStore attaches a store. Any cached root chain is verified forward from
// the pinned root at once, so the updater starts from the freshest securely
// reachable root; a cached root that does not chain is ignored.
func (u *Updater) WithStore(store Store) *Updater {
	for {
		next := u.trusted.Root().Version + 1
		data, ok := loadCached(store, fmt.Sprintf("root_history/%d.root.json", next))
		if !ok || u.trusted.UpdateRoot(data) != nil {
			break
		}
	}
	version := u.trusted.Root().Version
	store.Store("root.json", u.trusted.RootBytes())
	store.Store(fmt.Sprintf("root_history/%d.root.json", version), u.trusted.RootBytes())
	u.store = store
	return u
}

// Trusted is the verified metadata.
func (u *Updater) Trusted() *Trusted { return u.trusted }

// Refresh runs the TUF refresh workflow: root chain, timestamp, snapshot,
// top-level targets.
func (u *Updater) Refresh(ctx context.Context, now time.Time) error {
	if err := u.refreshRoot(ctx); err != nil {
		return err
	}
	if err := u.trusted.CheckRootExpired(now); err != nil {
		return err
	}
	u.seedFromStore(now)
	if err := u.refreshTimestamp(ctx, now); err != nil {
		return err
	}
	if err := u.refreshSnapshot(ctx, now); err != nil {
		return err
	}
	return u.refreshTargets(ctx, now)
}

// seedFromStore makes the stored versions the floor the network must meet:
// authenticated expired timestamps and snapshots still count. Invalid cache
// files are skipped.
func (u *Updater) seedFromStore(now time.Time) {
	if u.store == nil {
		return
	}
	ts, tsOK := loadCached(u.store, "timestamp.json")
	snap, snapOK := loadCached(u.store, "snapshot.json")
	tgt, tgtOK := loadCached(u.store, "targets.json")
	if tsOK {
		u.trusted.UpdateTimestamp(ts, nil)
	}
	if snapOK {
		u.trusted.UpdateSnapshot(snap, nil)
	}
	if tgtOK {
		u.trusted.UpdateTargets(tgt, now)
	}
}

func (u *Updater) cachePut(name string, data []byte) {
	if u.store != nil {
		u.store.Store(name, data)
	}
}

func (u *Updater) refreshRoot(ctx context.Context) error {
	start := u.trusted.Root().Version
	last := start + int64(u.config.MaxRootRotations)
	for next := start + 1; next <= last; next++ {
		data, found, err := u.repo.FetchMetadata(ctx, fmt.Sprintf("%d.root.json", next), u.config.RootMaxLength)
		if err != nil {
			return err
		}
		if !found {
			return nil
		}
		if err := u.trusted.UpdateRoot(data); err != nil {
			return err
		}
		// Versioned roots live in their own namespace so a delegated role
		// named e.g. "2.root" can't collide with them.
		u.cachePut(fmt.Sprintf("root_history/%d.root.json", next), data)
		u.cachePut("root.json", data)
	}
	return fmt.Errorf("exceeded %d root rotations without reaching the latest root", u.config.MaxRootRotations)
}

func (u *Updater) refreshTimestamp(ctx context.Context, now time.Time) error {
	// timestamp.json is never version-prefixed, even with consistent
	// snapshots.
	data, found, err := u.repo.FetchMetadata(ctx, "timestamp.json", u.config.TimestampMaxLength)
	if err != nil {
		return err
	}
	if !found {
		return errors.New("timestamp.json not found")
	}
	switch err := u.trusted.UpdateTimestamp(data, &now); {
	case err == nil:
		u.cachePut("timestamp.json", data)
	case errors.Is(err, ErrEqualVersion):
		// Same version as what is already trusted: keep it.
	default:
		return err
	}
	return nil
}

func (u *Updater) refreshSnapshot(ctx context.Context, now time.Time) error {
	ts := u.trusted.Timestamp()
	if ts == nil || ts.Meta["snapshot.json"] == nil {
		return errors.New("timestamp does not pin snapshot")
	}
	version := ts.Meta["snapshot.json"].Version
	// Freeze protection: a timestamp kept unchanged from a previous refresh
	// was last expiry-checked against an older now.
	if err := u.trusted.CheckTimestampExpired(now); err != nil {
		return err
	}
	if snap := u.trusted.Snapshot(); snap != nil && snap.Version == version && !expired(snap.Expires, now) {
		return nil
	}
	name := "snapshot.json"
	if u.trusted.Root().ConsistentSnapshot {
		name = fmt.Sprintf("%d.snapshot.json", version)
	}
	data, found, err := u.repo.FetchMetadata(ctx, name, u.config.SnapshotMaxLength)
	if err != nil {
		return err
	}
	if !found {
		return fmt.Errorf("%s not found", name)
	}
	if err := u.trusted.UpdateSnapshot(data, &now); err != nil {
		return err
	}
	u.cachePut(name, data)
	u.cachePut("snapshot.json", data)
	return nil
}

func (u *Updater) refreshTargets(ctx context.Context, now time.Time) error {
	if err := u.trusted.CheckSnapshotExpired(now); err != nil {
		return err
	}
	snap := u.trusted.Snapshot()
	if snap == nil || snap.Meta["targets.json"] == nil {
		return errors.New("snapshot does not pin targets.json")
	}
	pinned := snap.Meta["targets.json"].Version
	if tgt := u.trusted.TargetsRole(metadata.TARGETS); tgt != nil && tgt.Version == pinned && !expired(tgt.Expires, now) {
		return nil
	}
	name := "targets.json"
	if u.trusted.Root().ConsistentSnapshot {
		name = fmt.Sprintf("%d.targets.json", pinned)
	}
	data, found, err := u.repo.FetchMetadata(ctx, name, u.config.TargetsMaxLength)
	if err != nil {
		return err
	}
	if !found {
		return fmt.Errorf("%s not found", name)
	}
	if err := u.trusted.UpdateTargets(data, now); err != nil {
		return err
	}
	u.cachePut("targets.json", data)
	return nil
}

// DownloadTarget downloads a resolved target, verifies its length and hash
// and writes it through to the store.
func (u *Updater) DownloadTarget(ctx context.Context, target *metadata.TargetFiles, path string) ([]byte, error) {
	if target.Length < 0 || uint64(target.Length) > u.config.TargetMaxLength {
		return nil, fmt.Errorf("%s: pinned length %d exceeds configured max %d", path, target.Length, u.config.TargetMaxLength)
	}
	relative := path
	if u.trusted.Root().ConsistentSnapshot {
		// The hash prefixes the file name only: `dir/<hash>.name`.
		hash, ok := preferredHash(target.Hashes)
		if !ok {
			return nil, fmt.Errorf("%s: no hash to locate consistent target", path)
		}
		if dir, name, found := cutLast(path, "/"); found {
			relative = dir + "/" + hash + "." + name
		} else {
			relative = hash + "." + path
		}
	}
	data, found, err := u.repo.FetchTarget(ctx, relative, uint64(target.Length))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, fmt.Errorf("target %s not found", relative)
	}
	length := target.Length
	if err := CheckIntegrity(data, &length, nonNil(target.Hashes), path); err != nil {
		return nil, err
	}
	u.cachePut("targets/"+path, data)
	return data, nil
}

// A target is never accepted without an integrity check.
func nonNil(h metadata.Hashes) metadata.Hashes {
	if h == nil {
		return metadata.Hashes{}
	}
	return h
}

func cutLast(s, sep string) (string, string, bool) {
	i := strings.LastIndex(s, sep)
	if i < 0 {
		return "", s, false
	}
	return s[:i], s[i+len(sep):], true
}

// preferredHash prefers sha256, then sha512, then the first listed.
func preferredHash(hashes metadata.Hashes) (string, bool) {
	for _, name := range []string{"sha256", "sha512"} {
		if h, ok := hashes[name]; ok {
			return hex.EncodeToString(h), true
		}
	}
	var first string
	for name := range hashes {
		if first == "" || name < first {
			first = name
		}
	}
	if first == "" {
		return "", false
	}
	return hex.EncodeToString(hashes[first]), true
}

func loadCached(store Store, name string) ([]byte, bool) {
	data, found, err := store.Load(name)
	if err != nil || !found {
		return nil, false
	}
	return data, true
}

// FileStore is a Store in a folder. Names may contain `/`; names with `..`
// or absolute ones are refused. Writes are atomic renames.
type FileStore struct {
	Dir string
}

func (s FileStore) path(name string) (string, error) {
	if filepath.IsAbs(name) || strings.HasPrefix(name, "/") {
		return "", fmt.Errorf("unsafe cache name %q", name)
	}
	for _, part := range strings.Split(filepath.ToSlash(name), "/") {
		if part == ".." {
			return "", fmt.Errorf("unsafe cache name %q", name)
		}
	}
	return filepath.Join(s.Dir, filepath.FromSlash(name)), nil
}

// Load reads a stored file.
func (s FileStore) Load(name string) ([]byte, bool, error) {
	path, err := s.path(name)
	if err != nil {
		return nil, false, err
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, err
	}
	return data, true, nil
}

// Store writes a file atomically.
func (s FileStore) Store(name string, data []byte) error {
	path, err := s.path(name)
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return err
	}
	return WriteAtomic(path, data)
}

// WriteAtomic writes data to a uniquely named temporary file beside path,
// syncs it and renames it over path.
func WriteAtomic(path string, data []byte) error {
	nonce := make([]byte, 6)
	if _, err := rand.Read(nonce); err != nil {
		return err
	}
	temp := filepath.Join(filepath.Dir(path), ".tmp"+hex.EncodeToString(nonce))
	file, err := os.OpenFile(temp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	_, err = file.Write(data)
	if err == nil {
		err = file.Sync()
	}
	if cerr := file.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		for attempt := range 5 {
			if err = os.Rename(temp, path); err == nil {
				return nil
			}
			time.Sleep(time.Duration(10*(attempt+1)) * time.Millisecond)
		}
	}
	os.Remove(temp)
	return err
}

// HTTPRepository fetches metadata under one base URL and targets under
// another.
type HTTPRepository struct {
	Metadata *url.URL
	Targets  *url.URL
	Client   *http.Client
}

// NewHTTPRepository is an HTTPS repository with metadata under metadataURL
// and targets under targetsURL. A connection fails after 30 s, a stalled
// read after a minute, and total bounds the whole of any one request.
func NewHTTPRepository(metadataURL, targetsURL *url.URL, total time.Duration) *HTTPRepository {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.DialContext = (&net.Dialer{Timeout: 30 * time.Second, KeepAlive: 30 * time.Second}).DialContext
	transport.TLSHandshakeTimeout = 30 * time.Second
	transport.ResponseHeaderTimeout = 60 * time.Second
	return &HTTPRepository{
		Metadata: withSlash(metadataURL),
		Targets:  withSlash(targetsURL),
		Client:   &http.Client{Transport: transport, Timeout: total},
	}
}

func withSlash(u *url.URL) *url.URL {
	out := *u
	if !strings.HasSuffix(out.Path, "/") {
		out.Path += "/"
	}
	return &out
}

// Join resolves name against a base URL ending in `/`.
func Join(base *url.URL, name string) (*url.URL, error) {
	ref, err := url.Parse(name)
	if err != nil || ref.IsAbs() || ref.Host != "" || strings.HasPrefix(name, "/") {
		return nil, fmt.Errorf("invalid URL %q", name)
	}
	return base.ResolveReference(ref), nil
}

// FetchMetadata fetches a metadata file.
func (r *HTTPRepository) FetchMetadata(ctx context.Context, name string, maxLength uint64) ([]byte, bool, error) {
	u, err := Join(r.Metadata, name)
	if err != nil {
		return nil, false, err
	}
	return BoundedGet(ctx, r.Client, u, maxLength)
}

// FetchTarget fetches a target file.
func (r *HTTPRepository) FetchTarget(ctx context.Context, path string, maxLength uint64) ([]byte, bool, error) {
	u, err := Join(r.Targets, path)
	if err != nil {
		return nil, false, err
	}
	return BoundedGet(ctx, r.Client, u, maxLength)
}

// stallTimeout is the longest a response body may go without data.
const stallTimeout = 60 * time.Second

// BoundedGet reads at most maxLength bytes from u; 404 and 403 are "not
// found".
func BoundedGet(ctx context.Context, client *http.Client, u *url.URL, maxLength uint64) ([]byte, bool, error) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return nil, false, err
	}
	response, err := client.Do(request)
	if err != nil {
		return nil, false, fmt.Errorf("GET %s failed: %w", u, err)
	}
	defer response.Body.Close()
	// 403 is treated as "not found" alongside 404 because object stores
	// return it for missing objects.
	if response.StatusCode == http.StatusNotFound || response.StatusCode == http.StatusForbidden {
		return nil, false, nil
	}
	if response.StatusCode < 200 || response.StatusCode > 299 {
		return nil, false, fmt.Errorf("GET %s returned status %s", u, response.Status)
	}
	if response.ContentLength >= 0 && uint64(response.ContentLength) > maxLength {
		return nil, false, fmt.Errorf("%s: content-length %d exceeds max %d", u, response.ContentLength, maxLength)
	}
	// A stalled read fails; a large but flowing one does not.
	timer := time.AfterFunc(stallTimeout, cancel)
	defer timer.Stop()
	var out []byte
	buf := make([]byte, 64<<10)
	for {
		n, err := response.Body.Read(buf)
		if n > 0 {
			timer.Reset(stallTimeout)
			if uint64(len(out))+uint64(n) > maxLength {
				return nil, false, fmt.Errorf("%s: response exceeds max length %d", u, maxLength)
			}
			out = append(out, buf[:n]...)
		}
		if err == io.EOF {
			return out, true, nil
		}
		if err != nil {
			return nil, false, fmt.Errorf("reading %s failed: %w", u, err)
		}
	}
}

// Folders is a repository in two local folders, for tests and development
// builds' local release folder: what the official hosts serve, laid out the
// same way.
type Folders struct {
	Metadata string
	Targets  string
}

func readFolder(base, name string, maxLength uint64) ([]byte, bool, error) {
	data, err := os.ReadFile(filepath.Join(base, filepath.FromSlash(name)))
	if errors.Is(err, fs.ErrNotExist) {
		return nil, false, nil
	}
	if err != nil {
		return nil, false, fmt.Errorf("%s: %w", name, err)
	}
	if uint64(len(data)) > maxLength {
		return nil, false, fmt.Errorf("%s exceeds %d bytes", name, maxLength)
	}
	return data, true, nil
}

// FetchMetadata reads a metadata file.
func (f Folders) FetchMetadata(_ context.Context, name string, maxLength uint64) ([]byte, bool, error) {
	return readFolder(f.Metadata, name, maxLength)
}

// FetchTarget reads a target file.
func (f Folders) FetchTarget(_ context.Context, path string, maxLength uint64) ([]byte, bool, error) {
	return readFolder(f.Targets, path, maxLength)
}
