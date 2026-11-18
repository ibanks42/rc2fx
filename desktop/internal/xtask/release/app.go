package release

// Sign app release metadata: a TUF repository of its own, beside the
// mixer's, with the same root and release keys. Every asset of one published
// app release becomes a target `v{VERSION}/{ASSET}`, so the app can check a
// downloaded update package before applying it.

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

// sums covers the other assets; it is how they are checked, not something
// the app downloads.
const sums = "SHA256SUMS"

const (
	appSignUsage    = "usage: ./xtask app-sign ROOT ASSETS VERSION OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY"
	appRefreshUsage = "usage: ./xtask app-refresh ROOT METADATA OUT SEQUENCE EXPIRES SNAPSHOT_KEY TIMESTAMP_KEY"
)

// AppSign is `app-sign`: sign every asset in ASSETS, a downloaded copy of
// the published release VERSION. Its SHA256SUMS must list exactly the other
// files, with their hashes, so nothing is signed that the release did not
// publish.
func AppSign(args []string) error {
	if len(args) != 9 {
		return task.Error(appSignUsage)
	}
	rootFile, assetsDir, version, out, sequenceText, expiresText := args[0], args[1], args[2], args[3], args[4], args[5]
	targetsKey, snapshotKey, timestampKey := args[6], args[7], args[8]
	if !stable(version) {
		return task.Errorf("%s is not a stable app version", task.Debug(version))
	}
	assets, names, err := checkedAssets(assetsDir)
	if err != nil {
		return err
	}
	sequence, ok := task.ParseUint(sequenceText, 64)
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
	if exists(out) {
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
	// As for mixer releases: targets stay authorized for as long as the
	// root trusts their key, and only discovery metadata needs renewal.
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

// AppRefresh is `app-refresh`: renew app snapshot and timestamp metadata.
// The mixer's renewal checks nothing specific to the mixer, so the same code
// serves both repositories.
func AppRefresh(args []string) error {
	if len(args) != 7 {
		return task.Error(appRefreshUsage)
	}
	return Refresh(args)
}

// stable is a stable `MAJOR.MINOR.PATCH`, as app tags are.
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

// plain is a name that stays one path segment, both in a target name and on
// disk.
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

func isHex(text string) bool {
	for i := 0; i < len(text); i++ {
		c := text[i]
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f' || c >= 'A' && c <= 'F') {
			return false
		}
	}
	return true
}

// lines splits at '\n', drops a '\r' before it, and gives no empty line
// after a final '\n'.
func lines(text string) []string {
	var out []string
	for text != "" {
		line, rest, found := strings.Cut(text, "\n")
		if found {
			line = strings.TrimSuffix(line, "\r")
		}
		out = append(out, line)
		text = rest
		if !found {
			break
		}
	}
	return out
}

// checkedAssets is the assets in dir by name (and the names in order), each
// one checked against SHA256SUMS, which must list exactly them.
func checkedAssets(dir string) (map[string]string, []string, error) {
	sumsPath := filepath.Join(dir, sums)
	data, err := os.ReadFile(sumsPath)
	if err == nil && !utf8.Valid(data) {
		return nil, nil, task.Errorf("%s: stream did not contain valid UTF-8", sumsPath)
	}
	if err != nil {
		return nil, nil, task.Errorf("%s: %s", sumsPath, task.IOError(err))
	}
	listed := map[string]string{}
	for _, line := range lines(string(data)) {
		hash, name, ok := strings.Cut(line, "  ")
		if !ok {
			return nil, nil, task.Errorf("malformed %s line %s", sums, task.Debug(line))
		}
		if len(hash) != 64 || !isHex(hash) || !plain(name) {
			return nil, nil, task.Errorf("malformed %s line %s", sums, task.Debug(line))
		}
		if _, dup := listed[name]; name == sums || dup {
			return nil, nil, task.Errorf("%s lists %s more than once", sums, name)
		}
		listed[name] = hash
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, nil, task.Error(task.IOError(err))
	}
	assets := map[string]string{}
	for _, entry := range entries {
		name := entry.Name()
		if !utf8.ValidString(name) {
			return nil, nil, task.Errorf("asset name %s is not UTF-8", task.Debug(name))
		}
		if name == sums {
			continue
		}
		// Not followed: a link could sign bytes from outside the release.
		if !entry.Type().IsRegular() {
			return nil, nil, task.Errorf("%s is not a regular file", name)
		}
		expected, ok := listed[name]
		if !ok {
			return nil, nil, task.Errorf("%s is not listed in %s", name, sums)
		}
		delete(listed, name)
		path := filepath.Join(dir, name)
		data, err := readAll(path)
		if err != nil {
			return nil, nil, err
		}
		if !strings.EqualFold(task.Sha256(data), expected) {
			return nil, nil, task.Errorf("%s does not match %s", name, sums)
		}
		assets[name] = path
	}
	if len(listed) > 0 {
		missing := make([]string, 0, len(listed))
		for name := range listed {
			missing = append(missing, name)
		}
		sort.Strings(missing)
		return nil, nil, task.Errorf("%s lists %s, which is missing", sums, missing[0])
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
