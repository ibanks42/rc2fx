// Package tuftest writes signed TUF repositories for tests, shaped the way
// the release tooling writes them (`N.root.json`, `targets.json`,
// `snapshot.json`, `timestamp.json`, no consistent snapshots).
package tuftest

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/sigstore/sigstore/pkg/signature"
	"github.com/theupdateframework/go-tuf/v2/metadata"
)

type signer struct {
	key    *metadata.Key
	signer signature.Signer
}

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func put(t *testing.T, path string, data []byte) {
	t.Helper()
	must(t, os.MkdirAll(filepath.Dir(path), 0o755))
	must(t, os.WriteFile(path, data, 0o644))
}

func newSigner(t *testing.T) *signer {
	t.Helper()
	public, private, err := ed25519.GenerateKey(rand.Reader)
	must(t, err)
	key, err := metadata.KeyFromPublicKey(public)
	must(t, err)
	s, err := signature.LoadED25519Signer(private)
	must(t, err)
	return &signer{key: key, signer: s}
}

func (s *signer) id(t *testing.T) string {
	id, err := s.key.ID()
	must(t, err)
	return id
}

// Repo is a test-only signed TUF repository on disk.
type Repo struct {
	t   *testing.T
	dir string
	// Trusted is the root a client starts from.
	Trusted []byte
	root    *metadata.Metadata[metadata.RootType]
	current []byte
	signer  *signer
}

// MustTime parses an RFC3339 timestamp or fails the test.
func MustTime(t *testing.T, text string) time.Time {
	t.Helper()
	at, err := time.Parse(time.RFC3339, text)
	must(t, err)
	return at
}

// NewRepo creates a test-only root and key, shaped like the production root:
// fixed names under release tags.
func NewRepo(t *testing.T, dir string) *Repo {
	t.Helper()
	s := newSigner(t)
	root := metadata.Root(MustTime(t, "2099-01-01T00:00:00Z"))
	root.Signed.SpecVersion = "1.0.1"
	root.Signed.ConsistentSnapshot = false
	for _, role := range []string{metadata.ROOT, metadata.TARGETS, metadata.SNAPSHOT, metadata.TIMESTAMP} {
		must(t, root.Signed.AddKey(s.key, role))
	}
	_, err := root.Sign(s.signer)
	must(t, err)
	data, err := root.ToBytes(false)
	must(t, err)
	put(t, filepath.Join(dir, "root.json"), data)
	return &Repo{t: t, dir: dir, Trusted: data, root: root, current: data, signer: s}
}

// RotateRoot moves every role to a new key, in a root signed by the old
// and the new root keys.
func (r *Repo) RotateRoot() {
	t := r.t
	next := newSigner(t)
	data, err := r.root.ToBytes(false)
	must(t, err)
	root, err := metadata.Root().FromBytes(data)
	must(t, err)
	root.Signed.Version++
	root.Signed.Keys = map[string]*metadata.Key{next.id(t): next.key}
	for _, role := range root.Signed.Roles {
		role.KeyIDs = []string{next.id(t)}
	}
	root.ClearSignatures()
	_, err = root.Sign(next.signer)
	must(t, err)
	_, err = root.Sign(r.signer.signer)
	must(t, err)
	r.current, err = root.ToBytes(false)
	must(t, err)
	r.root, r.signer = root, next
	put(t, filepath.Join(r.dir, "root.json"), r.current)
}

// Target is one file to include in targets.json.
type Target struct {
	Name   string
	File   string
	Custom map[string]any
}

func pin(version int64, data []byte) *metadata.MetaFiles {
	digest := sha256.Sum256(data)
	return &metadata.MetaFiles{Version: version, Length: int64(len(data)), Hashes: metadata.Hashes{"sha256": digest[:]}}
}

// Sign writes metadata for targets at the given targets, snapshot and
// timestamp versions, all expiring at expires.
func (r *Repo) Sign(targets []Target, versions [3]int64, expires string) {
	t := r.t
	at := MustTime(t, expires)
	tm := metadata.Targets(at)
	tm.Signed.Version = versions[0]
	for _, target := range targets {
		data, err := os.ReadFile(target.File)
		must(t, err)
		file, err := metadata.TargetFile().FromBytes(target.Name, data, "sha256")
		must(t, err)
		if target.Custom != nil {
			custom, err := json.Marshal(target.Custom)
			must(t, err)
			raw := json.RawMessage(custom)
			file.Custom = &raw
		}
		tm.Signed.Targets[target.Name] = file
	}
	_, err := tm.Sign(r.signer.signer)
	must(t, err)
	targetsData, err := tm.ToBytes(false)
	must(t, err)
	sm := metadata.Snapshot(at)
	sm.Signed.Version = versions[1]
	sm.Signed.Meta = map[string]*metadata.MetaFiles{"targets.json": pin(versions[0], targetsData)}
	_, err = sm.Sign(r.signer.signer)
	must(t, err)
	snapshotData, err := sm.ToBytes(false)
	must(t, err)
	ts := metadata.Timestamp(at)
	ts.Signed.Version = versions[2]
	ts.Signed.Meta = map[string]*metadata.MetaFiles{"snapshot.json": pin(versions[1], snapshotData)}
	_, err = ts.Sign(r.signer.signer)
	must(t, err)
	timestampData, err := ts.ToBytes(false)
	must(t, err)
	meta := filepath.Join(r.dir, "metadata")
	put(t, filepath.Join(meta, fmt.Sprintf("%d.root.json", r.root.Signed.Version)), r.current)
	put(t, filepath.Join(meta, "targets.json"), targetsData)
	put(t, filepath.Join(meta, "snapshot.json"), snapshotData)
	put(t, filepath.Join(meta, "timestamp.json"), timestampData)
}
