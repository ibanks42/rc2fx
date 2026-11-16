package installer

// Signed TUF repositories for tests, written with go-tuf the way the
// release tooling (tough) writes them: `N.root.json`, `targets.json`,
// `snapshot.json` and `timestamp.json`, no consistent snapshots, and
// snapshot and timestamp pins with version, length and SHA-256.

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

type tufSigner struct {
	key    *metadata.Key
	signer signature.Signer
}

func newTufSigner(t *testing.T) *tufSigner {
	t.Helper()
	public, private, err := ed25519.GenerateKey(rand.Reader)
	must(t, err)
	key, err := metadata.KeyFromPublicKey(public)
	must(t, err)
	signer, err := signature.LoadED25519Signer(private)
	must(t, err)
	return &tufSigner{key: key, signer: signer}
}

func (s *tufSigner) id(t *testing.T) string {
	id, err := s.key.ID()
	must(t, err)
	return id
}

type tufRepo struct {
	t   *testing.T
	dir string
	// trusted is the root a client starts from.
	trusted []byte
	root    *metadata.Metadata[metadata.RootType]
	current []byte
	signer  *tufSigner
}

func mustTime(t *testing.T, text string) time.Time {
	at, err := time.Parse(time.RFC3339, text)
	must(t, err)
	return at
}

// newTufRepo is a test-only root and key, shaped like the production root:
// fixed names under release tags.
func newTufRepo(t *testing.T, dir string) *tufRepo {
	t.Helper()
	signer := newTufSigner(t)
	root := metadata.Root(mustTime(t, "2099-01-01T00:00:00Z"))
	root.Signed.SpecVersion = "1.0.1"
	root.Signed.ConsistentSnapshot = false
	for _, role := range []string{metadata.ROOT, metadata.TARGETS, metadata.SNAPSHOT, metadata.TIMESTAMP} {
		must(t, root.Signed.AddKey(signer.key, role))
	}
	_, err := root.Sign(signer.signer)
	must(t, err)
	data, err := root.ToBytes(false)
	must(t, err)
	put(t, filepath.Join(dir, "root.json"), data)
	return &tufRepo{t: t, dir: dir, trusted: data, root: root, current: data, signer: signer}
}

// rotateRoot moves every role to a new key, in a root signed by the old
// and the new root keys.
func (r *tufRepo) rotateRoot() {
	t := r.t
	next := newTufSigner(t)
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

type tufTarget struct {
	name   string
	file   string
	custom map[string]any
}

func pin(version int64, data []byte) *metadata.MetaFiles {
	digest := sha256.Sum256(data)
	return &metadata.MetaFiles{Version: version, Length: int64(len(data)), Hashes: metadata.Hashes{"sha256": digest[:]}}
}

// sign writes metadata for targets at the given targets, snapshot and
// timestamp versions, all expiring at expires.
func (r *tufRepo) sign(targets []tufTarget, versions [3]int64, expires string) {
	t := r.t
	at := mustTime(t, expires)
	tm := metadata.Targets(at)
	tm.Signed.Version = versions[0]
	for _, target := range targets {
		data, err := os.ReadFile(target.file)
		must(t, err)
		file, err := metadata.TargetFile().FromBytes(target.name, data, "sha256")
		must(t, err)
		if target.custom != nil {
			custom, err := json.Marshal(target.custom)
			must(t, err)
			raw := json.RawMessage(custom)
			file.Custom = &raw
		}
		tm.Signed.Targets[target.name] = file
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
