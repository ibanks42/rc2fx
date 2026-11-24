package release

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/tufsign"
)

func TestRenewalPreservesTargetsAndRejectsWrongKeysStaleVersionsAndTampering(t *testing.T) {
	dir := t.TempDir()
	signers := testRoot(t, dir)
	mustWrite(t, filepath.Join(dir, "payload"), []byte("unchanged approved payload"))
	rootFile := mustRead(t, filepath.Join(dir, "root.json"))
	root, err := tufsign.ParseRoot(rootFile)
	if err != nil {
		t.Fatal(err)
	}
	target, err := tufsign.BuildTarget(filepath.Join(dir, "payload"))
	if err != nil {
		t.Fatal(err)
	}
	var all []tufsign.Signer
	for _, role := range allRoles {
		all = append(all, signers[role])
	}
	// Initial release with already-expired snapshot and timestamp.
	old, err := tufsign.SignRelease(tufsign.Release{
		RootFile: rootFile, Root: root,
		TargetsVersion: 1, TargetsExpires: mustTime(t, "2098-01-01T00:00:00Z"),
		SnapshotVersion: 1, SnapshotExpires: mustTime(t, "2000-01-01T00:00:00Z"),
		TimestampVersion: 1, TimestampExpires: mustTime(t, "2000-01-01T00:00:00Z"),
		Targets: []tufsign.Target{target},
	}, all)
	if err != nil {
		t.Fatal(err)
	}
	if err := old.Write(filepath.Join(dir, "old")); err != nil {
		t.Fatal(err)
	}
	args := func(out, sequence, key, expires string) []string {
		return []string{
			filepath.Join(dir, "root.json"),
			filepath.Join(dir, "old"),
			filepath.Join(dir, out),
			sequence,
			expires,
			filepath.Join(dir, key),
			filepath.Join(dir, "timestamp.pk8"),
		}
	}
	expiry := weekAhead()
	if err := Refresh(args("stale", "1", "snapshot.pk8", expiry)); err == nil || !strings.Contains(err.Error(), "increase") {
		t.Fatalf("%v", err)
	}
	if err := Refresh(args("wrong", "2", "targets.pk8", expiry)); err == nil || !strings.Contains(err.Error(), "must not authorize") {
		t.Fatalf("%v", err)
	}
	if Refresh(args("expired", "2", "snapshot.pk8", "2098-01-01T00:00:00Z")) == nil {
		t.Fatal("a renewal lasting as long as targets was accepted")
	}
	if err := Refresh(args("new", "2", "snapshot.pk8", expiry)); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(mustRead(t, filepath.Join(dir, "old", "targets.json")), mustRead(t, filepath.Join(dir, "new", "targets.json"))) {
		t.Fatal("targets changed")
	}
	repository, err := loadRepository(rootFile, filepath.Join(dir, "new"), dir, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if repository.Timestamp.Version != 2 || repository.Targets.Version != 1 {
		t.Fatalf("timestamp %d targets %d", repository.Timestamp.Version, repository.Targets.Version)
	}
	targetsPath := filepath.Join(dir, "old", "targets.json")
	value, err := core.ParseJSONBytes(mustRead(t, targetsPath), 1<<20, 100_000)
	if err != nil {
		t.Fatal(err)
	}
	value.GetMut("signed").Set("version", core.U64Value(99))
	if err := os.WriteFile(targetsPath, []byte(value.String()), 0o666); err != nil {
		t.Fatal(err)
	}
	if Refresh(args("tampered", "3", "snapshot.pk8", expiry)) == nil {
		t.Fatal("tampered targets were renewed")
	}
}
