package selfupdate

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

func asset(file string) Asset { return Asset{Version: "1.2.3", FileName: file} }

// own is this platform's full package of version, as vpk names it.
func own(version string) string {
	return fmt.Sprintf("%s-%s%s-full.nupkg", PackID, version, channel())
}

func file(t *testing.T, path string) {
	t.Helper()
	if err := os.WriteFile(path, []byte("package"), 0o644); err != nil {
		t.Fatal(err)
	}
}

func exist(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func TestOnlyThisPlatformsPackageOfThatVersionIsAccepted(t *testing.T) {
	if !IsOwnPackage("1.2.3", own("1.2.3")) {
		t.Fatal("own package refused")
	}
	if IsOwnPackage("1.2.4", own("1.2.3")) {
		t.Fatal("another version accepted")
	}
	for _, other := range []string{
		PackID + "-1.2.3-full.nupkg",
		PackID + "-1.2.3-osx-full.nupkg",
		PackID + "-1.2.3-linux-full.nupkg",
	} {
		if other != own("1.2.3") && IsOwnPackage("1.2.3", other) {
			t.Fatalf("accepted %s", other)
		}
	}
	full := func(a Asset, kind string) Asset { a.Type = kind; return a }
	kept := FullPackages([]Asset{
		full(asset(own("1.2.3")), "Full"),
		full(asset(PackID+"-1.2.3-elsewhere-full.nupkg"), "Full"),
		full(asset(own("1.2.3")), "Delta"),
	})
	if len(kept) != 1 || kept[0].FileName != own("1.2.3") {
		t.Fatalf("kept %+v", kept)
	}
	// A signed package of another platform is refused before any check.
	path := filepath.Join(t.TempDir(), "rc2fx-other")
	file(t, path)
	other := asset(PackID + "-1.2.3-elsewhere-full.nupkg")
	result := VerifyFile(other, path, func(_, _, _ string) error { return nil })
	if result == nil || !result.Rejected {
		t.Fatalf("result %v", result)
	}
	if exist(path) {
		t.Fatal("refused file kept")
	}
}

func TestADevelopmentBuildCannotUpdateItself(t *testing.T) {
	t.Setenv("RC2FX_DEV_UPDATE", "")
	os.Unsetenv("RC2FX_DEV_UPDATE")
	// Without Velopack, and with Velopack that finds no install (the test
	// binary is no AppImage, app bundle or Velopack install).
	for _, open := range []Opener{nil, func(*SignedFeed, *LocatorConfig) (Manager, error) {
		return nil, errors.New("not installed")
	}} {
		update := New(open)
		if update.Supported() || update.State() != Idle() {
			t.Fatalf("supported %v, state %+v", update.Supported(), update.State())
		}
	}
}

func TestDownloadFailuresSayWhatToDo(t *testing.T) {
	checksum := &Error{Kind: ErrMismatch, Text: "Checksum did not match for x (expected a, actual b)"}
	if !strings.Contains(Failure(checksum), "did not match") {
		t.Fatal(Failure(checksum))
	}
	other := &Error{Kind: ErrOther, Text: "disk full"}
	if got := Failure(other); got != "The update could not be downloaded (disk full)." {
		t.Fatal(got)
	}
	// What the C library reports comes back as these kinds.
	for text, kind := range map[string]ErrorKind{
		"Checksum did not match for /p/x.partial (expected a, actual b)":             ErrMismatch,
		"Size did not match for /p/x.partial (expected 1, actual 2)\n\nCaused by: x": ErrMismatch,
		"Network error: Http error: timeout":                                         ErrNetwork,
		"disk full\n\nCaused by:\n    os error 28":                                   ErrOther,
	} {
		if e := LibraryError(text); e.Kind != kind || strings.Contains(e.Text, "\n") {
			t.Fatalf("%q read as %+v", text, e)
		}
	}
}

func TestAPackageItsReleaseRejectsIsRemovedAndShown(t *testing.T) {
	packages := t.TempDir()
	path := filepath.Join(packages, own("1.2.3"))
	file(t, path)
	refuse := func(_, _, _ string) error {
		return &installer.VerifyError{Rejected: true, Message: "This update has not been signed yet."}
	}
	pkg := asset(own("1.2.3"))
	result := VerifyPackage(packages, pkg, refuse)
	if exist(path) {
		t.Fatal("rejected package kept")
	}
	outcome := CheckedOutcome(pkg, result)
	if outcome.State != Failed("This update has not been signed yet.") {
		t.Fatalf("state %+v", outcome.State)
	}
	if outcome.State.HasAsset {
		t.Fatal("rejected package offered")
	}
}

func TestAPackageThatCouldNotBeCheckedIsKeptToCheckAgain(t *testing.T) {
	packages := t.TempDir()
	path := filepath.Join(packages, own("1.2.3"))
	file(t, path)
	offline := func(_, _, _ string) error {
		return &installer.VerifyError{Message: "Could not check this update."}
	}
	pkg := asset(own("1.2.3"))
	result := VerifyPackage(packages, pkg, offline)
	if !exist(path) {
		t.Fatal("unchecked package removed")
	}
	outcome := CheckedOutcome(pkg, result)
	if outcome.State != FailedKept("Could not check this update.", pkg) {
		t.Fatalf("outcome %+v", outcome)
	}
	update := empty(nil)
	update.packages, update.state = packages, outcome.State
	// Shown even when today's check offers nothing, with a retry.
	if !update.StaysShown() || update.InProgress() {
		t.Fatal("not shown")
	}
	if !update.Retrying() || !update.OnDisk(update.State().Asset) {
		t.Fatal("no retry")
	}
	update.state = Idle()
	if update.StaysShown() {
		t.Fatal("idle shown")
	}
}

func TestAPackageThatPassesIsReady(t *testing.T) {
	packages := t.TempDir()
	path := filepath.Join(packages, own("1.2.3"))
	file(t, path)
	accept := func(version, name, f string) error {
		if version != "1.2.3" || name != own("1.2.3") {
			t.Fatalf("%s %s", version, name)
		}
		if info, err := os.Stat(f); err != nil || !info.Mode().IsRegular() {
			t.Fatal("no file")
		}
		return nil
	}
	// A feed may name a package in a sub-folder; Velopack stores and the
	// release names only its file name.
	pkg := asset("sub/" + own("1.2.3"))
	result := VerifyPackage(packages, pkg, accept)
	if !exist(path) {
		t.Fatal("package removed")
	}
	outcome := CheckedOutcome(pkg, result)
	if outcome.State != ReadyToApply(pkg) || outcome.State.Version != "1.2.3" {
		t.Fatalf("outcome %+v", outcome)
	}
}

func TestAPackageWithoutAFileNameIsRejectedUnchecked(t *testing.T) {
	packages := t.TempDir()
	never := func(_, _, _ string) error { panic("nothing to check against") }
	if result := VerifyPackage(packages, asset("dir/"), never); result == nil || !result.Rejected {
		t.Fatalf("result %v", result)
	}
}

func TestADownloadIsCheckedUnderItsTemporaryNameAndDiscardedIfRejected(t *testing.T) {
	packages := t.TempDir()
	// Velopack downloads beside the final name and renames only after the
	// source returns.
	partialName := strings.Replace(own("1.2.3"), ".nupkg", ".partial", 1)
	partial := filepath.Join(packages, partialName)
	file(t, partial)
	pkg := asset(own("1.2.3"))
	seen := func(version, name, f string) error {
		if version != "1.2.3" || name != own("1.2.3") || !strings.HasSuffix(f, partialName) {
			t.Fatalf("%s %s %s", version, name, f)
		}
		return &installer.VerifyError{Rejected: true, Message: "mismatch"}
	}
	checks := &Checks{}
	result := VerifyFile(pkg, partial, seen)
	checks.Record(pkg, result)
	if exist(partial) {
		t.Fatal("rejected download kept")
	}
	if _, found := checks.Take(asset(own("1.2.4"))); found {
		t.Fatal("another package's check taken")
	}
	checks.Record(pkg, result)
	if got, found := checks.Take(pkg); !found || got == nil || !got.Rejected || got.Message != "mismatch" {
		t.Fatalf("took %v %v", got, found)
	}
	if _, found := checks.Take(pkg); found {
		t.Fatal("a check is used once")
	}
}

func TestOnlyFullPackagesAreTakenFromTheFeed(t *testing.T) {
	typed := func(kind string) Asset { a := asset(own("1.2.3")); a.Type = kind; return a }
	feed := FullPackages([]Asset{typed("Delta"), typed("full"), typed("Full"), typed("")})
	if len(feed) != 2 {
		t.Fatalf("kept %d", len(feed))
	}
	for _, a := range feed {
		if !strings.EqualFold(a.Type, "Full") {
			t.Fatalf("kept %s", a.Type)
		}
	}
	// What Velopack reads back is exactly what was kept, in its own names.
	raw := fmt.Sprintf(`{"assets":[{"type":"Delta","FileName":%q,"version":"1.2.3"},`+
		`{"Type":"Full","FileName":%q,"Version":"1.2.3","SHA256":"ab","Size":7,"Extra":1}]}`, own("1.2.3"), own("1.2.3"))
	filtered, err := FilterFeed([]byte(raw))
	if err != nil {
		t.Fatal(err)
	}
	var back struct{ Assets []map[string]any }
	if err := json.Unmarshal(filtered, &back); err != nil {
		t.Fatal(err)
	}
	if len(back.Assets) != 1 || back.Assets[0]["Type"] != "Full" || back.Assets[0]["SHA256"] != "ab" ||
		back.Assets[0]["Size"] != float64(7) || back.Assets[0]["Extra"] != nil {
		t.Fatalf("filtered %s", filtered)
	}
	if empty, err := FilterFeed([]byte(`{"Assets":[]}`)); err != nil || string(empty) != `{"Assets":[]}` {
		t.Fatalf("%s %v", empty, err)
	}
}

func TestAMissingPackageIsNotOfferedToRestartInto(t *testing.T) {
	packages := t.TempDir()
	update := empty(nil)
	update.packages = packages
	pkg := asset(own("1.2.3"))
	if update.OnDisk(pkg) {
		t.Fatal("missing package on disk")
	}
	file(t, filepath.Join(packages, own("1.2.3")))
	if !update.OnDisk(pkg) {
		t.Fatal("package not seen")
	}
}

// fakeManager is a Velopack stand-in for the download flow.
type fakeManager struct {
	kind     UpdateCheckKind
	target   Asset
	download func(progress func(int16)) error
	applied  []Asset
}

type fakeUpdate struct{ target Asset }

func (u fakeUpdate) TargetFullRelease() Asset { return u.target }
func (u fakeUpdate) Release()                 {}

func (m *fakeManager) UpdatePendingRestart() (Asset, bool) { return Asset{}, false }
func (m *fakeManager) CheckForUpdates() (UpdateCheckKind, Update, error) {
	if m.kind != UpdateAvailable {
		return m.kind, nil, nil
	}
	return m.kind, fakeUpdate{m.target}, nil
}
func (m *fakeManager) DownloadUpdates(_ Update, progress func(int16)) error {
	return m.download(progress)
}
func (m *fakeManager) WaitExitThenApplyUpdates(a Asset, _, _ bool, _ []string) error {
	m.applied = append(m.applied, a)
	return nil
}

func TestADownloadThatPassesIsReadyAndAppliedOnlyOnARestart(t *testing.T) {
	packages := t.TempDir()
	pkg := asset(own("1.2.3"))
	feed := &SignedFeed{checks: &Checks{}}
	m := &fakeManager{kind: UpdateAvailable, target: pkg}
	m.download = func(progress func(int16)) error {
		progress(40)
		feed.mu.Lock()
		p := feed.progress
		feed.mu.Unlock()
		p.Send(Verifying)
		progress(100) // the C library's last word, after the source returned
		file(t, filepath.Join(packages, own("1.2.3")))
		feed.checks.Record(pkg, nil)
		return nil
	}
	u := empty(m)
	u.feed, u.checks, u.packages = feed, feed.checks, packages
	if kind, _ := u.Start(); kind != StartDownload || u.State() != Downloading(0) {
		t.Fatalf("start %v %+v", kind, u.State())
	}
	progress := NewProgress()
	result := u.Download(progress)()
	if changed, ended := u.Advance(progress); !changed || !ended || u.State() != VerifyingState("1.2.3") {
		t.Fatalf("advance %v %v %+v", changed, ended, u.State())
	}
	if _, check := u.FinishDownload(result); check || u.State() != ReadyToApply(pkg) {
		t.Fatalf("finish %+v", u.State())
	}
	u.LaunchOnQuit()
	if len(m.applied) != 0 {
		t.Fatal("applied without a restart")
	}
	if u.Restart(true) != RestartNothing {
		t.Fatal("restarted while blocked")
	}
	if u.Restart(false) != RestartQuit {
		t.Fatal("no restart")
	}
	u.CancelRestart()
	u.LaunchOnQuit()
	if len(m.applied) != 0 {
		t.Fatal("applied after a canceled restart")
	}
	u.Restart(false)
	u.LaunchOnQuit()
	u.LaunchOnQuit()
	if len(m.applied) != 1 {
		t.Fatalf("applied %d times", len(m.applied))
	}
}

func TestAFailedDownloadSaysWhy(t *testing.T) {
	pkg := asset(own("1.2.3"))
	cases := []struct {
		name     string
		download func(feed *SignedFeed) error
		message  string
		retry    bool
	}{
		{"network", func(feed *SignedFeed) error {
			feed.setFailure(networkError(errors.New("offline")))
			return LibraryError("User vpkc_download_asset_delegate_t returned false to indicate download failed")
		}, "The update could not be downloaded. Check the internet connection and try again.", false},
		{"unavailable", func(feed *SignedFeed) error {
			feed.checks.Record(pkg, &installer.VerifyError{Message: "Could not check."})
			return LibraryError("User vpkc_download_asset_delegate_t returned false to indicate download failed")
		}, "Could not check.", true},
		{"rejected", func(feed *SignedFeed) error {
			feed.checks.Record(pkg, &installer.VerifyError{Rejected: true, Message: "Not signed."})
			return LibraryError("User vpkc_download_asset_delegate_t returned false to indicate download failed")
		}, "Not signed.", false},
		{"checksum after a passed check", func(feed *SignedFeed) error {
			feed.checks.Record(pkg, nil)
			return LibraryError("Checksum did not match for x (expected a, actual b)")
		}, "The downloaded update did not match the release and was discarded. Try again.", false},
	}
	for _, c := range cases {
		feed := &SignedFeed{checks: &Checks{}}
		m := &fakeManager{kind: UpdateAvailable, target: pkg}
		m.download = func(func(int16)) error { return c.download(feed) }
		u := empty(m)
		u.feed, u.checks = feed, feed.checks
		u.Start()
		u.FinishDownload(u.Download(NewProgress())())
		want := Failed(c.message)
		if c.retry {
			want = FailedKept(c.message, pkg)
		}
		if u.State() != want || u.Retrying() != c.retry {
			t.Fatalf("%s: %+v retry %v", c.name, u.State(), u.Retrying())
		}
	}
	m := &fakeManager{kind: NoUpdateAvailable}
	u := empty(m)
	u.Start()
	u.FinishDownload(u.Download(NewProgress())())
	if u.State() != Failed("This update is not ready to download yet. Try again later.") {
		t.Fatalf("%+v", u.State())
	}
}

func TestTheStateCarriesThePackageToApplyOrCheckAgain(t *testing.T) {
	packages := t.TempDir()
	pkg := asset(own("1.2.3"))
	file(t, filepath.Join(packages, own("1.2.3")))
	m := &fakeManager{kind: UpdateAvailable, target: pkg}

	// A preview's Ready has nothing to apply.
	u := empty(m)
	u.packages, u.state = packages, Ready("1.2.3")
	if u.Restart(false) != RestartNothing || u.applyOnQuit != nil {
		t.Fatal("restarted into nothing")
	}

	// A kept package is only checked again, and checking takes it.
	u = empty(m)
	u.packages, u.state = packages, FailedKept("Could not check.", pkg)
	if kind, got := u.Start(); kind != StartCheck || got != pkg {
		t.Fatalf("start %v %+v", kind, got)
	}
	if _, ok := u.BeginCheck(pkg); !ok || u.Retrying() || u.State() != VerifyingState("1.2.3") {
		t.Fatalf("check %+v", u.State())
	}

	// A kept package that went missing is downloaded again.
	u = empty(m)
	u.packages, u.state = t.TempDir(), FailedKept("Could not check.", pkg)
	if kind, _ := u.Start(); kind == StartCheck {
		t.Fatal("checked a package that is gone")
	}

	// A ready package that went missing fails, keeping nothing.
	u = empty(m)
	u.packages, u.state = t.TempDir(), ReadyToApply(pkg)
	if u.Restart(false) != RestartGone || u.State().HasAsset || u.State().Kind != StateFailed {
		t.Fatalf("restart into a missing package: %+v", u.State())
	}
	if u.Restart(false) != RestartNothing {
		t.Fatal("restarted twice")
	}
}
