package tufsign

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

type Target struct {
	Name   string
	Length uint64
	SHA256 []byte
	Custom []core.Member // omitted when empty, matching tough
}

// BuildTarget hashes the regular file at path into a Target.
func BuildTarget(path string) (Target, error) {
	name := filepath.Base(path)
	if name == "" || name == "." || name == string(filepath.Separator) {
		return Target{}, task.Errorf("Unable to determine file name from path: '%s'", path)
	}
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() {
		return Target{}, task.Errorf("TUF targets must be files, given: '%s'", path)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return Target{}, task.Errorf("Failed to open %s: %s", path, task.IOError(err))
	}
	sum := sha256.Sum256(data)
	return Target{Name: name, Length: uint64(len(data)), SHA256: sum[:]}, nil
}

func (t Target) value() core.Value {
	entry := core.ObjectValue(
		core.Member{Key: "length", Value: core.U64Value(t.Length)},
		core.Member{Key: "hashes", Value: core.ObjectValue(core.Member{Key: "sha256", Value: core.StringValue(hex.EncodeToString(t.SHA256))})},
	)
	if len(t.Custom) > 0 {
		entry.Set("custom", core.ObjectValue(t.Custom...))
	}
	return entry
}

type Release struct {
	RootFile         []byte
	Root             *Root
	TargetsVersion   uint64
	TargetsExpires   time.Time
	SnapshotVersion  uint64
	SnapshotExpires  time.Time
	TimestampVersion uint64
	TimestampExpires time.Time
	Targets          []Target
}

type Repository struct {
	rootName  string
	Root      []byte
	Targets   []byte
	Snapshot  []byte
	Timestamp []byte
}

// SignRelease signs targets.json, snapshot.json, and timestamp.json for a
// release with consistent_snapshot=false.
func SignRelease(release Release, signers []Signer) (*Repository, error) {
	if err := release.Root.CheckStable(); err != nil {
		return nil, err
	}
	if release.Root.ConsistentSnapshot {
		return nil, task.Error("signing requires consistent_snapshot=false")
	}
	targets := RoleValue(RoleTargets, release.TargetsVersion, release.TargetsExpires)
	entries := core.ObjectValue()
	for _, target := range release.Targets {
		if target.Name == "" || target.Name == ".." {
			return nil, task.Errorf("unsafe target name %q", target.Name)
		}
		entries.Set(target.Name, target.value())
	}
	targets.Set("targets", entries)
	targets.Set("delegations", core.ObjectValue(
		core.Member{Key: "keys", Value: core.ObjectValue()},
		core.Member{Key: "roles", Value: core.ArrayValue()},
	))
	targetsFile, err := release.Root.Sign(RoleTargets, targets, signers)
	if err != nil {
		return nil, err
	}
	snapshot := RoleValue(RoleSnapshot, release.SnapshotVersion, release.SnapshotExpires)
	targetsSum := sha256.Sum256(targetsFile)
	snapshot.Set("meta", core.ObjectValue(core.Member{
		Key: "targets.json", Value: MetafileValue(uint64(len(targetsFile)), targetsSum[:], release.TargetsVersion),
	}))
	snapshotFile, err := release.Root.Sign(RoleSnapshot, snapshot, signers)
	if err != nil {
		return nil, err
	}
	timestamp := RoleValue(RoleTimestamp, release.TimestampVersion, release.TimestampExpires)
	snapshotSum := sha256.Sum256(snapshotFile)
	timestamp.Set("meta", core.ObjectValue(core.Member{
		Key: "snapshot.json", Value: MetafileValue(uint64(len(snapshotFile)), snapshotSum[:], release.SnapshotVersion),
	}))
	timestampFile, err := release.Root.Sign(RoleTimestamp, timestamp, signers)
	if err != nil {
		return nil, err
	}
	return &Repository{
		rootName:  strconv.FormatUint(release.Root.Version, 10) + ".root.json",
		Root:      release.RootFile,
		Targets:   targetsFile,
		Snapshot:  snapshotFile,
		Timestamp: timestampFile,
	}, nil
}

// Write writes N.root.json, targets.json, snapshot.json, and timestamp.json into dir.
func (r *Repository) Write(dir string) error {
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return task.Errorf("Failed to create directory %s: %s", dir, task.IOError(err))
	}
	for _, file := range []struct {
		name string
		data []byte
	}{
		{r.rootName, r.Root},
		{"targets.json", r.Targets},
		{"snapshot.json", r.Snapshot},
		{"timestamp.json", r.Timestamp},
	} {
		path := filepath.Join(dir, file.name)
		if err := os.WriteFile(path, file.data, 0o666); err != nil {
			return task.Errorf("Failed to write %s: %s", path, task.IOError(err))
		}
	}
	return nil
}

// NewRoot builds an unsigned version-1 root object with threshold 1 per role.
func NewRoot(expires time.Time, roles []string, keys []Key) (core.Value, error) {
	keyMap := core.ObjectValue()
	roleMap := core.ObjectValue()
	for i, role := range roles {
		id, err := keys[i].ID()
		if err != nil {
			return core.Value{}, err
		}
		keyMap.Set(id, keys[i].Value.Clone())
		roleMap.Set(role, core.ObjectValue(
			core.Member{Key: "keyids", Value: core.ArrayValue(core.StringValue(id))},
			core.Member{Key: "threshold", Value: core.U64Value(1)},
		))
	}
	return core.ObjectValue(
		core.Member{Key: "_type", Value: core.StringValue(RoleRoot)},
		core.Member{Key: "spec_version", Value: core.StringValue(SpecVersion)},
		core.Member{Key: "consistent_snapshot", Value: core.BoolValue(false)},
		core.Member{Key: "version", Value: core.U64Value(1)},
		core.Member{Key: "expires", Value: core.StringValue(FormatExpires(expires))},
		core.Member{Key: "keys", Value: keyMap},
		core.Member{Key: "roles", Value: roleMap},
	), nil
}

// StageDir creates a temporary staging directory inside parent.
func StageDir(parent string) (string, error) {
	dir, err := os.MkdirTemp(parent, ".tmp")
	if err != nil {
		return "", task.Error(task.IOError(err))
	}
	return dir, nil
}
