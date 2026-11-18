package release

import (
	"bytes"
	"crypto/sha256"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

// The tests' own verifier of the repositories the release commands write:
// tough's RepositoryLoader::load for local folders. The app verifies with
// installer/internal/tuf; this one is independent of both it and the
// signing code.

// loadedRepository is a repository verified by loadRepository.
type loadedRepository struct {
	Root      *tufsign.Root
	Timestamp *tufsign.Document
	Snapshot  *tufsign.Document
	Targets   *tufsign.Targets
	targets   string
}

// loadRepository verifies the repository in metadata, from the trusted
// root file trusted, as of now: the root chain (`N.root.json` from the
// trusted version + 1, each signed by the previous root and itself), then
// timestamp, snapshot and targets, each signed by its role, pinned by the
// role above it (version, length, SHA-256) and unexpired. Targets are read
// from targets/NAME.
func loadRepository(trusted []byte, metadata, targets string, now time.Time) (*loadedRepository, error) {
	root, err := tufsign.ParseRoot(trusted)
	if err != nil {
		return nil, err
	}
	if err := root.VerifyRole(root.Document); err != nil {
		return nil, err
	}
	for {
		next := filepath.Join(metadata, strconv.FormatUint(root.Version+1, 10)+".root.json")
		data, err := os.ReadFile(next)
		if errors.Is(err, os.ErrNotExist) {
			break
		}
		if err != nil {
			return nil, err
		}
		newer, err := tufsign.ParseRoot(data)
		if err != nil {
			return nil, err
		}
		if err := root.VerifyRole(newer.Document); err != nil {
			return nil, err
		}
		if err := newer.VerifyRole(newer.Document); err != nil {
			return nil, err
		}
		if newer.Version != root.Version+1 {
			return nil, fmt.Errorf("root version %d in %s", newer.Version, next)
		}
		root = newer
	}
	if !root.Expires.After(now) {
		return nil, errors.New("root metadata is expired")
	}
	read := func(name string) ([]byte, error) { return os.ReadFile(filepath.Join(metadata, name)) }

	timestampFile, err := read("timestamp.json")
	if err != nil {
		return nil, err
	}
	timestamp, err := tufsign.ParseRole(timestampFile, tufsign.RoleTimestamp)
	if err != nil {
		return nil, err
	}
	if err := root.VerifyRole(timestamp); err != nil {
		return nil, err
	}
	if !timestamp.Expires.After(now) {
		return nil, errors.New("timestamp metadata is expired")
	}
	meta, _ := tufsign.Meta(timestamp)
	pin, ok := meta["snapshot.json"]
	if !ok {
		return nil, errors.New("timestamp does not list snapshot.json")
	}
	snapshotName := "snapshot.json"
	if root.ConsistentSnapshot {
		snapshotName = strconv.FormatUint(pin.Version, 10) + ".snapshot.json"
	}
	snapshotFile, err := read(snapshotName)
	if err != nil {
		return nil, err
	}
	if err := checkMetaPin(pin, snapshotFile); err != nil {
		return nil, err
	}
	snapshot, err := tufsign.ParseRole(snapshotFile, tufsign.RoleSnapshot)
	if err != nil {
		return nil, err
	}
	if err := root.VerifyRole(snapshot); err != nil {
		return nil, err
	}
	if snapshot.Version != pin.Version {
		return nil, errors.New("snapshot version differs from the timestamp's")
	}
	if !snapshot.Expires.After(now) {
		return nil, errors.New("snapshot metadata is expired")
	}
	meta, _ = tufsign.Meta(snapshot)
	pin, ok = meta["targets.json"]
	if !ok {
		return nil, errors.New("snapshot does not list targets.json")
	}
	targetsName := "targets.json"
	if root.ConsistentSnapshot {
		targetsName = strconv.FormatUint(pin.Version, 10) + ".targets.json"
	}
	targetsFile, err := read(targetsName)
	if err != nil {
		return nil, err
	}
	if err := checkMetaPin(pin, targetsFile); err != nil {
		return nil, err
	}
	targetsDoc, err := tufsign.ParseRole(targetsFile, tufsign.RoleTargets)
	if err != nil {
		return nil, err
	}
	if err := root.VerifyRole(targetsDoc); err != nil {
		return nil, err
	}
	if targetsDoc.Version != pin.Version {
		return nil, errors.New("targets version differs from the snapshot's")
	}
	if !targetsDoc.Expires.After(now) {
		return nil, errors.New("targets metadata is expired")
	}
	parsed, err := tufsign.TargetsOf(targetsDoc)
	if err != nil {
		return nil, err
	}
	return &loadedRepository{Root: root, Timestamp: timestamp, Snapshot: snapshot, Targets: parsed, targets: targets}, nil
}

// checkMetaPin checks data against a snapshot or timestamp entry: its
// length and SHA-256, when the entry gives them.
func checkMetaPin(pin tufsign.Metafile, data []byte) error {
	if pin.Length != nil && *pin.Length != uint64(len(data)) {
		return errors.New("metadata length differs from its pin")
	}
	if pin.HasHashes {
		sum := sha256.Sum256(data)
		if !bytes.Equal(sum[:], pin.SHA256) {
			return errors.New("metadata hash differs from its pin")
		}
	}
	return nil
}

// ReadTarget reads target name and checks its signed length and SHA-256.
func (l *loadedRepository) ReadTarget(name string) ([]byte, error) {
	entry, ok := l.Targets.Entries[name]
	if !ok {
		return nil, fmt.Errorf("target %s is not signed", name)
	}
	data, err := os.ReadFile(filepath.Join(l.targets, filepath.FromSlash(name)))
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(data)
	if uint64(len(data)) != entry.Length || !bytes.Equal(sum[:], entry.SHA256) {
		return nil, fmt.Errorf("target %s does not match its signed length and hash", name)
	}
	return data, nil
}
