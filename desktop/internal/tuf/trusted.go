// Package tuf implements the TUF client used by the installer's signed
// repositories (mixer releases and app releases). Metadata parsing uses go-tuf,
// while signature verification runs over the canonical form of the published
// bytes (see verify.go). Refresh rules, rollback floors, cache layout, and
// expiry semantics match sigstore-tuf (used by the mixer) so both sides accept
// the same repositories:
//
//   - Root chain: N.root.json starting from trusted version + 1, each signed by
//     both the previous and new root, up to 32 rotations.
//   - timestamp.json (never version-prefixed), then snapshot.json and
//     targets.json (version-prefixed when consistent snapshots are enabled),
//     each pinned by the preceding role via version, length, and hashes.
//   - Metadata is expired when its expires timestamp is before now.
//   - Verified files are persisted to a store (root.json,
//     root_history/N.root.json, timestamp.json, snapshot.json, targets.json)
//     and act as rollback floors on subsequent runs.
package tuf

import (
	"crypto/sha256"
	"crypto/sha512"
	"encoding/hex"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"
)

// ErrEqualVersion is the "nothing changed" signal of a timestamp update.
var ErrEqualVersion = errors.New("timestamp version equals the trusted one")

// Trusted is the set of currently trusted TUF metadata. It performs no I/O.
type Trusted struct {
	root      *metadata.Metadata[metadata.RootType]
	rootBytes []byte
	timestamp *metadata.Metadata[metadata.TimestampType]
	snapshot  *metadata.Metadata[metadata.SnapshotType]
	targets   map[string]*metadata.Metadata[metadata.TargetsType]
}

func specMajor(version string) error {
	if major, _, _ := strings.Cut(version, "."); major != "1" {
		return fmt.Errorf("unsupported TUF spec_version %q (this client implements 1.x)", version)
	}
	return nil
}

func parseRoot(data []byte) (*metadata.Metadata[metadata.RootType], error) {
	m, err := metadata.Root().FromBytes(data)
	if err != nil {
		return nil, fmt.Errorf("malformed root metadata: %w", err)
	}
	if err := specMajor(m.Signed.SpecVersion); err != nil {
		return nil, err
	}
	if m.Signed.Version < 0 {
		return nil, errors.New("malformed root metadata: negative version")
	}
	return m, nil
}

// expired is sigstore-tuf's rule: expired once `expires` is before now.
func expired(expires, now time.Time) bool { return expires.Before(now) }

func expiredError(role string, expires time.Time) error {
	return fmt.Errorf("%s metadata expired at %s", role, expires.UTC().Format(time.RFC3339))
}

// NewTrusted bootstraps from a pinned root. The root must be correctly
// self-signed; its expiry is checked once root updating is complete.
func NewTrusted(data []byte) (*Trusted, error) {
	root, err := parseRoot(data)
	if err != nil {
		return nil, err
	}
	if err := verifyWith(data, &root.Signed, metadata.ROOT); err != nil {
		return nil, err
	}
	return &Trusted{root: root, rootBytes: append([]byte{}, data...), targets: map[string]*metadata.Metadata[metadata.TargetsType]{}}, nil
}

// Root is the trusted root.
func (t *Trusted) Root() *metadata.RootType { return &t.root.Signed }

// RootBytes is the trusted root's file.
func (t *Trusted) RootBytes() []byte { return t.rootBytes }

// Timestamp is the trusted timestamp, or nil.
func (t *Trusted) Timestamp() *metadata.TimestampType {
	if t.timestamp == nil {
		return nil
	}
	return &t.timestamp.Signed
}

// Snapshot is the trusted snapshot, or nil.
func (t *Trusted) Snapshot() *metadata.SnapshotType {
	if t.snapshot == nil {
		return nil
	}
	return &t.snapshot.Signed
}

// TargetsRole is a trusted targets role by name, or nil.
func (t *Trusted) TargetsRole(name string) *metadata.TargetsType {
	if m := t.targets[name]; m != nil {
		return &m.Signed
	}
	return nil
}

// UpdateRoot incorporates the next root: exactly the trusted version + 1,
// signed to threshold by the trusted root's root role and by its own.
// Intermediate root expiry is not checked, per the TUF spec.
func (t *Trusted) UpdateRoot(data []byte) error {
	next, err := parseRoot(data)
	if err != nil {
		return err
	}
	if next.Signed.Version != t.root.Signed.Version+1 {
		return fmt.Errorf("bad root version: trusted %d, new %d", t.root.Signed.Version, next.Signed.Version)
	}
	// Signed by the old root's authority, and self-consistent under the new
	// root's.
	if err := verifyWith(data, &t.root.Signed, metadata.ROOT); err != nil {
		return err
	}
	if err := verifyWith(data, &next.Signed, metadata.ROOT); err != nil {
		return err
	}
	t.root = next
	t.rootBytes = append([]byte{}, data...)
	// A new root may rotate keys; lower roles are verified again.
	t.timestamp, t.snapshot = nil, nil
	t.targets = map[string]*metadata.Metadata[metadata.TargetsType]{}
	return nil
}

// CheckRootExpired fails when the trusted root has expired.
func (t *Trusted) CheckRootExpired(now time.Time) error {
	if expired(t.root.Signed.Expires, now) {
		return expiredError("root", t.root.Signed.Expires)
	}
	return nil
}

// CheckTimestampExpired fails when a trusted timestamp has expired.
func (t *Trusted) CheckTimestampExpired(now time.Time) error {
	if t.timestamp != nil && expired(t.timestamp.Signed.Expires, now) {
		return expiredError("timestamp", t.timestamp.Signed.Expires)
	}
	return nil
}

// CheckSnapshotExpired fails when a trusted snapshot has expired.
func (t *Trusted) CheckSnapshotExpired(now time.Time) error {
	if t.snapshot != nil && expired(t.snapshot.Signed.Expires, now) {
		return expiredError("snapshot", t.snapshot.Signed.Expires)
	}
	return nil
}

func snapshotPin(ts *metadata.Metadata[metadata.TimestampType]) *metadata.MetaFiles {
	return ts.Signed.Meta["snapshot.json"]
}

// UpdateTimestamp incorporates a timestamp. now nil loads an authenticated
// cached copy, which sets rollback floors even when expired.
func (t *Trusted) UpdateTimestamp(data []byte, now *time.Time) error {
	next, err := metadata.Timestamp().FromBytes(data)
	if err != nil {
		return fmt.Errorf("malformed timestamp metadata: %w", err)
	}
	if err := specMajor(next.Signed.SpecVersion); err != nil {
		return err
	}
	if err := verifyWith(data, &t.root.Signed, metadata.TIMESTAMP); err != nil {
		return err
	}
	if current := t.timestamp; current != nil {
		if next.Signed.Version < current.Signed.Version {
			return fmt.Errorf("timestamp rollback: trusted %d, new %d", current.Signed.Version, next.Signed.Version)
		}
		if next.Signed.Version == current.Signed.Version {
			return ErrEqualVersion
		}
		// The pinned snapshot version must not move backwards either.
		if nm, cm := snapshotPin(next), snapshotPin(current); nm != nil && cm != nil && nm.Version < cm.Version {
			return fmt.Errorf("snapshot (via timestamp) rollback: trusted %d, new %d", cm.Version, nm.Version)
		}
	}
	if now != nil && expired(next.Signed.Expires, *now) {
		return expiredError("timestamp", next.Signed.Expires)
	}
	t.timestamp = next
	return nil
}

// UpdateSnapshot incorporates a snapshot the trusted timestamp pins. now nil
// loads an authenticated cached copy.
func (t *Trusted) UpdateSnapshot(data []byte, now *time.Time) error {
	if t.timestamp == nil {
		return errors.New("cannot load snapshot before timestamp")
	}
	if now != nil && expired(t.timestamp.Signed.Expires, *now) {
		return expiredError("timestamp", t.timestamp.Signed.Expires)
	}
	pin := snapshotPin(t.timestamp)
	if pin == nil {
		return errors.New("timestamp does not pin snapshot.json")
	}
	if err := CheckIntegrity(data, lengthOf(pin.Length), pin.Hashes, "snapshot.json"); err != nil {
		return err
	}
	next, err := metadata.Snapshot().FromBytes(data)
	if err != nil {
		return fmt.Errorf("malformed snapshot metadata: %w", err)
	}
	if err := specMajor(next.Signed.SpecVersion); err != nil {
		return err
	}
	if err := verifyWith(data, &t.root.Signed, metadata.SNAPSHOT); err != nil {
		return err
	}
	if next.Signed.Version != pin.Version {
		return fmt.Errorf("snapshot version %d does not match timestamp pin %d", next.Signed.Version, pin.Version)
	}
	// Relative to the trusted snapshot, no targets metadata may be removed or
	// rolled back (TUF spec 5.5.4).
	if current := t.snapshot; current != nil {
		for name, meta := range current.Signed.Meta {
			nm, ok := next.Signed.Meta[name]
			if !ok {
				return fmt.Errorf("snapshot removes %s, which the trusted snapshot pins", name)
			}
			if nm.Version < meta.Version {
				return fmt.Errorf("%s (via snapshot) rollback: trusted %d, new %d", name, meta.Version, nm.Version)
			}
		}
	}
	if now != nil && expired(next.Signed.Expires, *now) {
		return expiredError("snapshot", next.Signed.Expires)
	}
	t.snapshot = next
	// A new snapshot invalidates previously trusted targets.
	t.targets = map[string]*metadata.Metadata[metadata.TargetsType]{}
	return nil
}

// UpdateTargets incorporates the top-level targets the trusted snapshot
// pins, signed by the root's targets role.
func (t *Trusted) UpdateTargets(data []byte, now time.Time) error {
	if t.snapshot == nil {
		return errors.New("cannot load targets before snapshot")
	}
	// The snapshot authorizing this targets file must itself still be fresh.
	if expired(t.snapshot.Signed.Expires, now) {
		return expiredError("snapshot", t.snapshot.Signed.Expires)
	}
	pin := t.snapshot.Signed.Meta["targets.json"]
	if pin == nil {
		return errors.New("snapshot does not pin targets.json")
	}
	if err := CheckIntegrity(data, lengthOf(pin.Length), pin.Hashes, "targets.json"); err != nil {
		return err
	}
	next, err := metadata.Targets().FromBytes(data)
	if err != nil {
		return fmt.Errorf("malformed targets metadata: %w", err)
	}
	if err := specMajor(next.Signed.SpecVersion); err != nil {
		return err
	}
	if next.Signed.Version != pin.Version {
		return fmt.Errorf("targets.json version %d does not match snapshot pin %d", next.Signed.Version, pin.Version)
	}
	if err := verifyWith(data, &t.root.Signed, metadata.TARGETS); err != nil {
		return err
	}
	if expired(next.Signed.Expires, now) {
		return expiredError("targets", next.Signed.Expires)
	}
	t.targets[metadata.TARGETS] = next
	return nil
}

// verifyWith checks data's signatures against the role's keys in root.
func verifyWith(data []byte, root *metadata.RootType, role string) error {
	env, err := parseEnvelope(data)
	if err != nil {
		return err
	}
	return env.verifyThreshold(root.Keys, root.Roles[role], role)
}

// go-tuf reads an absent length as 0.
func lengthOf(length int64) *int64 {
	if length == 0 {
		return nil
	}
	return &length
}

// CheckIntegrity verifies data against a pinned length and hashes: the
// length must match exactly, every supported hash (sha256, sha512) must
// match, and at least one must be supported.
func CheckIntegrity(data []byte, length *int64, hashes metadata.Hashes, what string) error {
	if length != nil && int64(len(data)) != *length {
		return fmt.Errorf("%s: length %d != pinned %d", what, len(data), *length)
	}
	if hashes == nil {
		return nil
	}
	verified := false
	names := make([]string, 0, len(hashes))
	for name := range hashes {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		var actual []byte
		switch name {
		case "sha256":
			d := sha256.Sum256(data)
			actual = d[:]
		case "sha512":
			d := sha512.Sum512(data)
			actual = d[:]
		default:
			continue
		}
		if hex.EncodeToString(actual) != hex.EncodeToString(hashes[name]) {
			return fmt.Errorf("%s: %s mismatch", what, name)
		}
		verified = true
	}
	if !verified {
		return fmt.Errorf("%s: no supported hash algorithm to verify (%v)", what, names)
	}
	return nil
}
