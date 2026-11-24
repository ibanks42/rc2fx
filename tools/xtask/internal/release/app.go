package release

// Signs desktop app release metadata in a separate TUF repository alongside
// the mixer's (sharing the same root and release keys). Each published release
// asset is recorded as target `v{VERSION}/{ASSET}` so the app can verify
// downloaded updates before applying them.

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/tufsign"
)

const (
	appSignUsage    = "usage: go -C tools/xtask run . app-sign ROOT ASSETS VERSION OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY"
	appRefreshUsage = "usage: go -C tools/xtask run . app-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY"
)

// AppSign implements `xtask app-sign`, validating every file in ASSETS and
// signing them into OUT.
func AppSign(args []string) error {
	if len(args) != 9 {
		return task.Error(appSignUsage)
	}
	rootFile, assetsDir, version, out, sequenceText, expiresText := args[0], args[1], args[2], args[3], args[4], args[5]
	targetsKey, snapshotKey, timestampKey := args[6], args[7], args[8]
	if !stable(version) {
		return task.Errorf("%q is not a stable app version", version)
	}
	assets, names, err := checkedAssets(assetsDir)
	if err != nil {
		return err
	}
	sequence, ok := common.ParseUint(sequenceText, 64)
	if !ok || sequence == 0 {
		return task.Error("sequence must be positive")
	}
	expires, err := tufsign.ParseTimestamp(expiresText)
	if err != nil {
		return task.Errorf("invalid expiry: %s", err)
	}
	if !expires.After(time.Now()) {
		return task.Error("metadata expiry must be in the future")
	}
	if common.PathExists(out) {
		return task.Error("metadata output already exists; use a new directory")
	}
	root, rootBytes, err := loadRoot(rootFile, func(err error) error {
		return task.Errorf("%s: %s", rootFile, task.IOError(err))
	})
	if err != nil {
		return err
	}
	if root.ConsistentSnapshot {
		return task.Error("GitHub release target paths require consistent_snapshot=false")
	}
	// Same policy as mixer releases: targets expire with the root, and only
	// snapshot/timestamp get renewed periodically.
	targetsExpires := root.Expires
	if !expires.Before(targetsExpires) {
		return task.Error("discovery metadata must expire before the targets and root")
	}
	signers, err := releaseKeys(root, [3][2]string{
		{tufsign.RoleTargets, targetsKey},
		{tufsign.RoleSnapshot, snapshotKey},
		{tufsign.RoleTimestamp, timestampKey},
	})
	if err != nil {
		return err
	}
	if err := root.CheckStable(); err != nil {
		return err
	}
	var targets []tufsign.Target
	for _, name := range names {
		target, err := tufsign.BuildTarget(assets[name])
		if err != nil {
			return err
		}
		target.Custom = []core.Member{{Key: "version", Value: core.StringValue(version)}}
		target.Name = "v" + version + "/" + name
		targets = append(targets, target)
	}
	signed, err := tufsign.SignRelease(tufsign.Release{
		RootFile: rootBytes, Root: root,
		TargetsVersion: sequence, TargetsExpires: targetsExpires,
		SnapshotVersion: sequence, SnapshotExpires: expires,
		TimestampVersion: sequence, TimestampExpires: expires,
		Targets: targets,
	}, signers)
	if err != nil {
		return err
	}
	if err := writeStaged(out, signed); err != nil {
		return err
	}
	fmt.Printf("Signed app %s metadata (%d assets) sequence %s; expires %s\n",
		version, len(names), strconv.FormatUint(sequence, 10), tufsign.DisplayTimestamp(expires))
	return nil
}

// AppRefresh implements `xtask app-refresh`, delegating to the shared
// snapshot/timestamp renewal flow.
func AppRefresh(args []string) error {
	if len(args) != 7 {
		return task.Error(appRefreshUsage)
	}
	return Refresh(args)
}

func stable(version string) bool {
	parts := strings.Split(version, ".")
	if len(parts) != 3 {
		return false
	}
	for _, p := range parts {
		if p == "" || (len(p) > 1 && p[0] == '0') {
			return false
		}
		for i := 0; i < len(p); i++ {
			if p[i] < '0' || p[i] > '9' {
				return false
			}
		}
	}
	return true
}

// plain reports whether name is a single path segment using [A-Za-z0-9._+-] without a leading dot.
func plain(name string) bool {
	if name == "" || name[0] == '.' {
		return false
	}
	for i := 0; i < len(name); i++ {
		c := name[i]
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.IndexByte("._+-", c) >= 0) {
			return false
		}
	}
	return true
}

// checkedAssets validates every file in dir and returns their paths and sorted names.
func checkedAssets(dir string) (map[string]string, []string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, nil, task.Error(task.IOError(err))
	}
	assets := map[string]string{}
	for _, entry := range entries {
		name := entry.Name()
		if !utf8.ValidString(name) {
			return nil, nil, task.Errorf("asset name %q is not UTF-8", name)
		}
		if !plain(name) {
			return nil, nil, task.Errorf("unsafe asset name %q", name)
		}
		// Refuse symlinks so we never sign files outside the release directory.
		if !entry.Type().IsRegular() {
			return nil, nil, task.Errorf("%s is not a regular file", name)
		}
		assets[name] = filepath.Join(dir, name)
	}
	if len(assets) == 0 {
		return nil, nil, task.Error("the release has no assets to sign")
	}
	names := make([]string, 0, len(assets))
	for name := range assets {
		names = append(names, name)
	}
	sort.Strings(names)
	return assets, names, nil
}
