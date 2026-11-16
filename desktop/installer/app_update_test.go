package installer

import (
	"bufio"
	"fmt"
	"net"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"
)

// The tests talk to a local server.
func init() { allowPlainHTTP = true }

// checkAt is the check, with no feed to read.
func checkAt(latestURL, listURL, current string) UpdateCheck {
	return *snapshotAt(latestURL, listURL, "http://127.0.0.1:9/no-feed", current).App
}

func okReply(body string) *reply { return &reply{status: 200, body: []byte(body)} }

func releaseBody(tag string) string {
	return fmt.Sprintf(`{"tag_name":"%s","draft":false,"prerelease":false,"html_url":"https://evil.example/"}`, tag)
}

func TestANewerStableReleaseIsAvailableWithAConstructedLink(t *testing.T) {
	check := evaluate("0.1.0", okReply(releaseBody("v0.10.0")))
	if check.Kind != UpdateAvailable || check.Version != "0.10.0" || check.URL != "https://github.com/ibanks42/rc2fx/releases/tag/v0.10.0" {
		t.Fatalf("%+v", check)
	}
}

func TestTheSameOrAnOlderReleaseIsUpToDate(t *testing.T) {
	for _, c := range [][2]string{{"0.2.0", "v0.2.0"}, {"0.3.0", "v0.2.9"}, {"1.0.0", "v0.99.99"}} {
		got := evaluate(c[0], okReply(releaseBody(c[1])))
		if !reflect.DeepEqual(got, UpdateCheck{Kind: UpdateUpToDate, Latest: c[1][1:]}) {
			t.Fatalf("%v: %+v", c, got)
		}
	}
}

func TestMixerDraftsPrereleasesAndOddTagsAreNeverOffered(t *testing.T) {
	for _, body := range []string{
		releaseBody("mixer-v0.9.0"),
		releaseBody("0.9.0"),
		releaseBody("v0.9"),
		releaseBody("v0.9.0-rc1"),
		releaseBody("v0.9.0.1"),
		releaseBody("v0..9"),
		releaseBody("v01.2.3"),
		releaseBody("v1.02.3"),
		releaseBody("v1.2.00"),
		`{"tag_name":"v9.0.0","draft":true,"prerelease":false}`,
		`{"tag_name":"v9.0.0","draft":false,"prerelease":true}`,
		"not json",
	} {
		got := evaluate("0.1.0", okReply(body))
		if got.Kind != UpdateFailed || got.Error.Kind != UpdateOther {
			t.Fatalf("%s: %+v", body, got)
		}
	}
}

func TestNoReleaseRateLimitsAndErrorsAreToldApart(t *testing.T) {
	r := func(status int, exhausted bool) *reply {
		return &reply{status: status, rateLimitExhausted: exhausted, body: []byte("{}")}
	}
	if got := evaluate("0.1.0", r(404, false)); got.Kind != UpdateNoRelease {
		t.Fatalf("%+v", got)
	}
	for _, rr := range []*reply{r(429, false), r(403, true)} {
		if got := evaluate("0.1.0", rr); got.Kind != UpdateFailed || got.Error.Kind != UpdateRateLimited {
			t.Fatalf("%+v", got)
		}
	}
	for _, rr := range []*reply{r(403, false), r(500, false), r(301, false)} {
		if got := evaluate("0.1.0", rr); got.Kind != UpdateFailed || got.Error.Kind != UpdateOther {
			t.Fatalf("%+v", got)
		}
	}
}

func TestAnUnreleasableCurrentVersionFailsWithoutARequest(t *testing.T) {
	got := checkAt("http://127.0.0.1:9/never-contacted", "http://127.0.0.1:9/never-contacted", "dev-build")
	if got.Kind != UpdateFailed || got.Error.Kind != UpdateOther {
		t.Fatalf("%+v", got)
	}
}

func listed(tag, body string) string {
	return fmt.Sprintf(`{"tag_name":"%s","draft":false,"prerelease":false,"body":"%s"}`, tag, body)
}

func page(items []string) (*reply, *UpdateError) {
	return okReply("[" + strings.Join(items, ",") + "]"), nil
}

func later() time.Time { return time.Now().Add(60 * time.Second) }

func versionsOf(log *Changelog) []string {
	var out []string
	for _, e := range log.Entries {
		out = append(out, e.Version)
	}
	return out
}

func TestTheChangelogCoversExactlyTheRangeNewestFirst(t *testing.T) {
	items := []string{
		listed("v0.4.0", "Four"),
		listed("mixer-v0.3.5", "Mixer"),
		listed("v0.2.0", "Two"),
		`{"tag_name":"v0.3.1","draft":true,"prerelease":false,"body":"Draft"}`,
		`{"tag_name":"v0.3.2","draft":false,"prerelease":true,"body":"Pre"}`,
		listed("v0.10.0", "Ten, created later than the offered one"),
		listed("v0.3.0", "Three"),
		listed("v0.1.0", "Installed"),
		listed("v0.4.0", "Duplicate"),
	}
	log := changelog("0.1.0", "0.4.0", later(), func(uint32) (*reply, *UpdateError) { return page(items) })
	if log.Status.Kind != ChangelogComplete || !slices.Equal(versionsOf(&log), []string{"0.4.0", "0.3.0", "0.2.0"}) {
		t.Fatalf("%+v", log)
	}
	if log.Entries[0].Notes != "Four" || log.Entries[2].URL != "https://github.com/ibanks42/rc2fx/releases/tag/v0.2.0" {
		t.Fatalf("%+v", log)
	}
}

func TestARangeThatIsNotFullyReadIsPartialNeverComplete(t *testing.T) {
	// A full page every time: the paging limit is reached.
	var full []string
	for i := range perPage {
		full = append(full, listed(fmt.Sprintf("v0.0.%d", i), ""))
	}
	first := slices.Clone(full)
	first[0] = listed("v0.2.0", "Two")
	log := changelog("0.1.0", "0.2.0", later(), func(n uint32) (*reply, *UpdateError) {
		if n == 1 {
			return page(first)
		}
		return page(full)
	})
	if log.Status.Kind != ChangelogPartial || len(log.Entries) != 1 {
		t.Fatalf("%+v", log.Status)
	}
	// The offered release is not in the list.
	log = changelog("0.1.0", "0.3.0", later(), func(uint32) (*reply, *UpdateError) { return page([]string{listed("v0.2.0", "")}) })
	if log.Status.Kind != ChangelogPartial {
		t.Fatalf("%+v", log.Status)
	}
	// A later page fails after some notes were read.
	log = changelog("0.1.0", "0.2.0", later(), func(n uint32) (*reply, *UpdateError) {
		if n == 1 {
			return page(first)
		}
		return nil, &UpdateError{Kind: UpdateOffline}
	})
	if log.Status.Kind != ChangelogPartial {
		t.Fatalf("%+v", log.Status)
	}
	// The deadline has passed before the first page.
	log = changelog("0.1.0", "0.2.0", time.Now(), func(uint32) (*reply, *UpdateError) { return page(first) })
	if log.Status.Kind != ChangelogPartial || len(log.Entries) != 0 {
		t.Fatalf("%+v", log)
	}
}

func TestAnUnreadableListIsUnavailableWithItsReason(t *testing.T) {
	log := changelog("0.1.0", "0.2.0", later(), func(uint32) (*reply, *UpdateError) { return nil, &UpdateError{Kind: UpdateOffline} })
	if log.Status != (ChangelogStatus{Kind: ChangelogUnavailable, Error: UpdateError{Kind: UpdateOffline}}) {
		t.Fatalf("%+v", log.Status)
	}
	log = changelog("0.1.0", "0.2.0", later(), func(uint32) (*reply, *UpdateError) { return &reply{status: 429}, nil })
	if log.Status != (ChangelogStatus{Kind: ChangelogUnavailable, Error: UpdateError{Kind: UpdateRateLimited}}) {
		t.Fatalf("%+v", log.Status)
	}
	log = changelog("0.1.0", "0.2.0", later(), func(uint32) (*reply, *UpdateError) { return okReply("{}"), nil })
	if log.Status.Kind != ChangelogUnavailable || log.Status.Error.Kind != UpdateOther {
		t.Fatalf("%+v", log.Status)
	}
}

func TestLongNotesAreCutAtACharacterBoundary(t *testing.T) {
	text := strings.Repeat("é", maxNotes)
	cut, truncated := cutNotes(text)
	if !truncated || len(cut) > maxNotes || !strings.HasPrefix(text, cut) {
		t.Fatal("cut")
	}
	if cut, truncated := cutNotes("short"); cut != "short" || truncated {
		t.Fatal("short")
	}
}

type row struct {
	version string
	other   *string
}

func some(s string) *string { return &s }

// feedText is (app, mixer) rows, then (mixer, notes) rows, as a feed.
func feedText(apps, mixers []row) string {
	quoted := func(v *string) string {
		if v == nil {
			return "null"
		}
		return `"` + *v + `"`
	}
	var a, m []string
	for _, r := range apps {
		a = append(a, fmt.Sprintf(`{"version":"%s","date":"2026-01-01","notesMarkdown":"App %s","releaseUrl":"https://evil.example/","mixerVersion":%s,"protocolVersion":1,"future":true}`, r.version, r.version, quoted(r.other)))
	}
	for _, r := range mixers {
		m = append(m, fmt.Sprintf(`{"version":"%s","date":null,"notesMarkdown":%s}`, r.version, quoted(r.other)))
	}
	return fmt.Sprintf(`{"schemaVersion":1,"app":{"latest":null,"releases":[%s]},"mixer":{"releases":[%s]},"rode":{}}`, strings.Join(a, ","), strings.Join(m, ","))
}

func historyOf(t *testing.T, body, latest string) *history {
	t.Helper()
	h, err := parseHistory(okReply(body), latest)
	if err != nil {
		t.Fatal(err)
	}
	return h
}

// snapshotOf is a snapshot whose latest published app is latest.
func snapshotOf(t *testing.T, body, latest string) UpdateSnapshot {
	h := historyOf(t, body, latest)
	return UpdateSnapshot{App: &UpdateCheck{Kind: UpdateUpToDate, Latest: latest}, latestMixer: h.publishedMixer(latest), history: h}
}

// fullFeed is apps 1.0.0 to 1.3.0, the mixer changing at 1.1.0 and 1.3.0.
func fullFeed() string {
	return feedText(
		[]row{{"1.3.0", some("1.2.0")}, {"1.2.0", some("1.1.0")}, {"1.1.0", some("1.1.0")}, {"1.0.0", some("1.0.0")}},
		[]row{{"1.2.0", some("Mixer two")}, {"1.1.0", some("Mixer one")}, {"1.0.0", some("Baseline")}},
	)
}

func TestAnUnusableFeedIsRefused(t *testing.T) {
	bodies := []string{
		"not json",
		`{"schemaVersion":2,"app":{"releases":[]},"mixer":{"releases":[]}}`,
		feedText([]row{{"v1.0.0", nil}}, nil),
		feedText([]row{{"1.0", nil}}, nil),
		feedText([]row{{"1.0.0", some("latest")}}, nil),
		feedText([]row{{"1.0.0", nil}, {"1.0.0", nil}}, nil),
		feedText(nil, []row{{"1.0.0-beta", nil}}),
		// Comparable, but not MAJOR.MINOR.PATCH.
		feedText([]row{{"1.0.0", some("1.0")}}, nil),
		feedText([]row{{"1.0.0", some("1.0.0.0")}}, nil),
		feedText([]row{{"1.0.0", some("01.0.0")}}, nil),
		feedText(nil, []row{{"1.0", nil}}),
		feedText(nil, []row{{"1.0.0.0", nil}}),
		feedText(nil, []row{{"01.0.0", nil}}),
		// A mixer listed twice, in any order, with or without conflicting notes.
		feedText(nil, []row{{"1.0.0", some("A")}, {"1.0.0", some("A")}}),
		feedText(nil, []row{{"1.0.0", some("A")}, {"1.1.0", nil}, {"1.0.0", some("B")}}),
		feedText(nil, []row{{"1.0.0", nil}, {"1.0.0", some("B")}}),
		strings.Replace(feedText([]row{{"1.0.0", nil}}, nil), `"protocolVersion":1`, `"protocolVersion":0`, 1),
	}
	for _, body := range bodies {
		if _, err := parseHistory(okReply(body), "1.0.0"); err == nil {
			t.Fatalf("%s accepted", body)
		}
	}
	if _, err := parseHistory(&reply{status: 404}, "1.0.0"); err == nil {
		t.Fatal("404 accepted")
	}
	many := make([]row, maxFeedReleases+1)
	for i := range many {
		many[i] = row{"1.0.0", nil}
	}
	if _, err := parseHistory(okReply(feedText(many, nil)), "1.0.0"); err == nil {
		t.Fatal("too many releases accepted")
	}
}

func TestTheLatestMixerIsWhatTheExactLatestAppBundles(t *testing.T) {
	latest := snapshotOf(t, fullFeed(), "1.3.0")
	one := uint32(1)
	if !reflect.DeepEqual(latest.LatestMixer(), &PublishedMixer{Version: "1.2.0", Protocol: &one, AppVersion: "1.3.0"}) {
		t.Fatalf("%+v", latest.LatestMixer())
	}
	// The feed has not caught up, or its row does not say.
	if snap := snapshotOf(t, fullFeed(), "1.4.0"); snap.LatestMixer() != nil {
		t.Fatal("stale feed")
	}
	unmapped := feedText([]row{{"1.3.0", nil}, {"1.2.0", some("1.1.0")}}, nil)
	if snap := snapshotOf(t, unmapped, "1.3.0"); snap.LatestMixer() != nil {
		t.Fatal("unmapped row")
	}
}

func TestEntriesAboveTheLatestReleaseAreIgnored(t *testing.T) {
	early := snapshotOf(t, fullFeed(), "1.2.0")
	if early.LatestMixer().Version != "1.1.0" {
		t.Fatal(early.LatestMixer().Version)
	}
	update := early.MixerUpdate(some("1.0.0"), some("1.0.0"))
	if update.State != (MixerUpdateState{Kind: MixerAppUpdateRequired, Latest: "1.1.0", AppVersion: "1.2.0"}) {
		t.Fatalf("%+v", update.State)
	}
	// Mixer 1.2.0 is listed only by the app above the latest release.
	if !slices.Equal(versionsOf(update.Changelog), []string{"1.1.0"}) {
		t.Fatalf("%v", versionsOf(update.Changelog))
	}
	log := historyOf(t, fullFeed(), "1.2.0").appChangelog("1.0.0", "1.2.0")
	if !slices.Equal(versionsOf(&log), []string{"1.2.0", "1.1.0"}) {
		t.Fatalf("%v", versionsOf(&log))
	}
}

func TestAppNotesCoverSkippedReleasesCumulatively(t *testing.T) {
	log := historyOf(t, fullFeed(), "1.3.0").appChangelog("1.0.0", "1.3.0")
	if log.Status.Kind != ChangelogComplete || !slices.Equal(versionsOf(&log), []string{"1.3.0", "1.2.0", "1.1.0"}) {
		t.Fatalf("%+v", log)
	}
	if log.Entries[0].Notes != "App 1.3.0" || log.Entries[0].URL != "https://github.com/ibanks42/rc2fx/releases/tag/v1.3.0" {
		t.Fatalf("%+v", log.Entries[0])
	}
}

func TestAppNotesThatCannotBeProvedCompleteArePartial(t *testing.T) {
	h := historyOf(t, fullFeed(), "1.4.0")
	// The offered release is not listed.
	if h.appChangelog("1.0.0", "1.4.0").Status.Kind != ChangelogPartial {
		t.Fatal("unlisted offer")
	}
	// The running version is outside the listed history.
	if h.appChangelog("0.9.0", "1.3.0").Status.Kind != ChangelogPartial {
		t.Fatal("unlisted current")
	}
	missing := strings.Replace(feedText([]row{{"1.1.0", nil}, {"1.0.0", nil}}, nil), `"notesMarkdown":"App 1.1.0"`, `"notesMarkdown":null`, 1)
	if historyOf(t, missing, "1.1.0").appChangelog("1.0.0", "1.1.0").Status.Kind != ChangelogPartial {
		t.Fatal("missing notes")
	}
}

func TestAMixerVersionRepeatedAcrossAppsAppearsOnceUnderItsFirstApp(t *testing.T) {
	update := snapshotOf(t, fullFeed(), "1.3.0").MixerUpdate(some("1.0.0"), some("1.0.0"))
	log := update.Changelog
	if log.Status.Kind != ChangelogComplete || !slices.Equal(versionsOf(log), []string{"1.2.0", "1.1.0"}) {
		t.Fatalf("%+v", log)
	}
	if log.Entries[1].Notes != "Mixer one" || log.Entries[1].URL != "https://github.com/ibanks42/rc2fx/releases/tag/v1.1.0" {
		t.Fatalf("%+v", log.Entries[1])
	}
}

func TestMixerNotesNobodyPublishedAreDropped(t *testing.T) {
	body := feedText(
		[]row{{"1.1.0", some("1.1.0")}, {"1.0.0", some("1.0.0")}},
		[]row{{"1.0.5", some("Never in an app")}, {"1.1.0", some("One")}, {"1.0.0", some("Baseline")}},
	)
	log := snapshotOf(t, body, "1.1.0").MixerUpdate(some("1.0.0"), nil).Changelog
	if log.Status.Kind != ChangelogComplete || !slices.Equal(versionsOf(log), []string{"1.1.0"}) {
		t.Fatalf("%+v", log)
	}
}

func TestMixerHistoryThatCouldHideAReleaseIsPartial(t *testing.T) {
	partial := func(body, latest, installed string, bundled *string) *Changelog {
		t.Helper()
		snap := snapshotOf(t, body, latest)
		log := snap.MixerUpdate(&installed, bundled).Changelog
		if log.Status.Kind != ChangelogPartial {
			t.Fatalf("%s %s: %+v", installed, body, log.Status)
		}
		return log
	}
	// An intermediate app's mixer software is unknown.
	unmapped := feedText(
		[]row{{"1.2.0", some("1.2.0")}, {"1.1.0", nil}, {"1.0.0", some("1.0.0")}},
		[]row{{"1.2.0", some("Two")}, {"1.0.0", some("Baseline")}},
	)
	if v := versionsOf(partial(unmapped, "1.2.0", "1.0.0", nil)); !slices.Equal(v, []string{"1.2.0"}) {
		t.Fatal(v)
	}
	// An intermediate version has no notes, in either way of saying so.
	noNotes := feedText(
		[]row{{"1.2.0", some("1.2.0")}, {"1.1.0", some("1.1.0")}, {"1.0.0", some("1.0.0")}},
		[]row{{"1.2.0", some("Two")}, {"1.1.0", nil}, {"1.0.0", some("Baseline")}},
	)
	log := partial(noNotes, "1.2.0", "1.0.0", nil)
	if !slices.Equal(versionsOf(log), []string{"1.2.0", "1.1.0"}) || log.Entries[1].Notes != "" {
		t.Fatalf("%+v", log)
	}
	absent := feedText(
		[]row{{"1.1.0", some("1.1.0")}, {"1.0.0", some("1.0.0")}},
		[]row{{"1.0.0", some("Baseline")}},
	)
	if v := versionsOf(partial(absent, "1.1.0", "1.0.0", nil)); !slices.Equal(v, []string{"1.1.0"}) {
		t.Fatal(v)
	}
	// The installed version is outside the known history.
	if v := versionsOf(partial(fullFeed(), "1.3.0", "0.9.0", nil)); !slices.Equal(v, []string{"1.2.0", "1.1.0", "1.0.0"}) {
		t.Fatal(v)
	}
	if v := versionsOf(partial(fullFeed(), "1.3.0", "1.0.5", nil)); !slices.Equal(v, []string{"1.2.0", "1.1.0"}) {
		t.Fatal(v)
	}
	// A bundled version newer than the public feed has no notes there.
	if v := versionsOf(partial(fullFeed(), "1.3.0", "1.0.0", some("1.5.0"))); !slices.Equal(v, []string{"1.2.0", "1.1.0"}) {
		t.Fatal(v)
	}
}

func TestAStaleFeedKeepsOldNotesButLeavesThePublishedMixerUnknown(t *testing.T) {
	stale := snapshotOf(t, fullFeed(), "1.4.0")
	update := stale.MixerUpdate(some("1.0.0"), some("1.2.0"))
	if update.State != (MixerUpdateState{Kind: MixerBundled, Version: "1.2.0"}) {
		t.Fatalf("%+v", update.State)
	}
	if update.Changelog.Status.Kind != ChangelogPartial || !slices.Equal(versionsOf(update.Changelog), []string{"1.2.0", "1.1.0"}) {
		t.Fatalf("%+v", update.Changelog)
	}
}

func TestMixerStatesCompareInstalledBundledAndLatest(t *testing.T) {
	known := snapshotOf(t, fullFeed(), "1.3.0") // latest mixer 1.2.0
	bundled := func(v string) MixerUpdateState { return MixerUpdateState{Kind: MixerBundled, Version: v} }
	needsApp := MixerUpdateState{Kind: MixerAppUpdateRequired, Latest: "1.2.0", AppVersion: "1.3.0"}
	current := MixerUpdateState{Kind: MixerCurrent}
	newer := MixerUpdateState{Kind: MixerNewer}
	for _, c := range []struct {
		installed string
		local     *string
		state     MixerUpdateState
	}{
		{"1.2.0", some("1.2.0"), current},
		{"1.2.0", nil, current},
		{"1.2.0", some("1.1.0"), current},
		{"1.3.0", some("1.3.0"), current},
		{"1.3.0", some("1.2.0"), newer},
		{"1.3.0", nil, newer},
		{"1.3.0", some("not-a-version"), newer},
		{"1.1.0", some("1.2.0"), bundled("1.2.0")},
		{"1.1.0", some("1.3.0"), bundled("1.3.0")},
		{"1.2.0", some("1.3.0"), bundled("1.3.0")},
		{"1.1.0", some("1.1.0"), needsApp},
		{"1.1.0", some("1.0.0"), needsApp},
		{"1.1.0", nil, needsApp},
		{"1.0.0", some("1.1.0"), MixerUpdateState{Kind: MixerBundledThenApp, Version: "1.1.0", Latest: "1.2.0", AppVersion: "1.3.0"}},
	} {
		installed := c.installed
		if got := known.MixerUpdate(&installed, c.local).State; got != c.state {
			t.Fatalf("%s %v: %+v", c.installed, c.local, got)
		}
	}
}

func TestAMissingVersionIsUnknownAndNothingIsOffered(t *testing.T) {
	known := snapshotOf(t, fullFeed(), "1.3.0")
	for _, installed := range []*string{nil, some("dev"), some("1.2.0-beta"), some("")} {
		got := known.MixerUpdate(installed, some("1.2.0"))
		if got.State != (MixerUpdateState{Kind: MixerUnknown}) || got.Changelog != nil {
			t.Fatalf("%+v", got)
		}
	}
	for _, upToDate := range []string{"1.2.0", "9.0.0"} {
		if got := known.MixerUpdate(&upToDate, nil); got.Changelog != nil {
			t.Fatalf("%s: %+v", upToDate, got)
		}
	}
}

func TestWithoutACheckOnlyLocalVersionsAreCompared(t *testing.T) {
	failure := SnapshotFromCheck(UpdateCheck{Kind: UpdateFailed, Error: UpdateError{Kind: UpdateOffline}})
	for _, snap := range []UpdateSnapshot{UncheckedSnapshot(), failure} {
		if snap.LatestMixer() != nil {
			t.Fatal("a latest mixer without a check")
		}
		update := snap.MixerUpdate(some("1.0.0"), some("1.1.0"))
		if update.State != (MixerUpdateState{Kind: MixerBundled, Version: "1.1.0"}) {
			t.Fatalf("%+v", update.State)
		}
		if len(update.Changelog.Entries) != 0 || update.Changelog.Status.Kind != ChangelogUnavailable {
			t.Fatalf("%+v", update.Changelog)
		}
		// Not behind what is bundled, but the published version is unknown.
		for _, installed := range []string{"1.1.0", "1.2.0"} {
			if got := snap.MixerUpdate(&installed, some("1.1.0")).State; got.Kind != MixerUnknown {
				t.Fatalf("%s: %+v", installed, got)
			}
		}
	}
	if UncheckedSnapshot().App != nil {
		t.Fatal("an app check without a check")
	}
	log := failure.MixerUpdate(some("1.0.0"), some("1.1.0")).Changelog
	if log.Status != (ChangelogStatus{Kind: ChangelogUnavailable, Error: UpdateError{Kind: UpdateOffline}}) {
		t.Fatalf("%+v", log.Status)
	}
}

// serve serves one canned HTTP reply on a local port.
func serve(t *testing.T, status, headers, body string) string {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	must(t, err)
	reply := fmt.Sprintf("HTTP/1.1 %s\r\n%sContent-Length: %d\r\nConnection: close\r\n\r\n%s", status, headers, len(body), body)
	go func() {
		defer listener.Close()
		conn, err := listener.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		reader := bufio.NewReader(conn)
		for {
			line, err := reader.ReadString('\n')
			if err != nil || len(line) <= 2 {
				break
			}
		}
		conn.Write([]byte(reply))
	}()
	return "http://" + listener.Addr().String() + "/repos/x/y/releases/latest"
}

func listURL(url string) string {
	return strings.TrimSuffix(url, "/repos/x/y/releases/latest") + "/releases"
}

func TestRealHTTPRepliesAreClassified(t *testing.T) {
	url := serve(t, "200 OK", "", releaseBody("v0.2.0"))
	list := listURL(serve(t, "200 OK", "", "["+listed("v0.2.0", "Second")+","+listed("mixer-v0.3.0", "x")+"]"))
	got := checkAt(url, list, "0.1.0")
	if got.Kind != UpdateAvailable || got.Changelog.Status.Kind != ChangelogComplete || len(got.Changelog.Entries) != 1 || got.Changelog.Entries[0].Notes != "Second" {
		t.Fatalf("%+v", got)
	}
	url = serve(t, "404 Not Found", "", `{"message":"Not Found"}`)
	if got := checkAt(url, "http://127.0.0.1:9/unused", "0.1.0"); got.Kind != UpdateNoRelease {
		t.Fatalf("%+v", got)
	}
	url = serve(t, "403 Forbidden", "X-RateLimit-Remaining: 0\r\n", `{"message":"API rate limit exceeded"}`)
	if got := checkAt(url, "http://127.0.0.1:9/unused", "0.1.0"); got.Kind != UpdateFailed || got.Error.Kind != UpdateRateLimited {
		t.Fatalf("%+v", got)
	}
	url = serve(t, "200 OK", "", strings.Repeat("x", maxReleaseBody+10))
	if got := checkAt(url, "http://127.0.0.1:9/unused", "0.1.0"); got.Kind != UpdateFailed || got.Error.Kind != UpdateOther {
		t.Fatalf("%+v", got)
	}
}

func TestAFeedSuppliesTheNotesAndGitHubStaysTheAuthority(t *testing.T) {
	listNeverRead := "http://127.0.0.1:9/unused"
	snap := snapshotAt(serve(t, "200 OK", "", releaseBody("v1.3.0")), listNeverRead, serve(t, "200 OK", "", fullFeed()), "1.2.0")
	if snap.App.Kind != UpdateAvailable || snap.App.Changelog.Status.Kind != ChangelogComplete || !slices.Equal(versionsOf(&snap.App.Changelog), []string{"1.3.0"}) {
		t.Fatalf("%+v", snap.App)
	}
	if snap.LatestMixer().Version != "1.2.0" {
		t.Fatal(snap.LatestMixer())
	}
	// An up-to-date app still learns about the mixer.
	snap = snapshotAt(serve(t, "200 OK", "", releaseBody("v1.3.0")), listNeverRead, serve(t, "200 OK", "", fullFeed()), "1.3.0")
	if snap.App.Kind != UpdateUpToDate || snap.LatestMixer().Version != "1.2.0" {
		t.Fatalf("%+v", snap.App)
	}
	// GitHub's answer is missing, so the feed is not even asked.
	snap = snapshotAt(serve(t, "500 Oops", "", ""), listNeverRead, "http://127.0.0.1:9/unused", "1.0.0")
	if snap.App.Kind != UpdateFailed || snap.LatestMixer() != nil {
		t.Fatalf("%+v", snap.App)
	}
}

func TestAppNotesFallBackToGitHubWithoutAUsableFeed(t *testing.T) {
	github := func() string { return listURL(serve(t, "200 OK", "", "["+listed("v1.4.0", "From GitHub")+"]")) }
	// Missing, and a feed that has not caught up with the release.
	for _, feed := range []string{serve(t, "404 Not Found", "", ""), serve(t, "200 OK", "", fullFeed())} {
		snap := snapshotAt(serve(t, "200 OK", "", releaseBody("v1.4.0")), github(), feed, "1.3.0")
		if snap.App.Kind != UpdateAvailable || snap.App.Changelog.Status.Kind != ChangelogComplete || snap.App.Changelog.Entries[0].Notes != "From GitHub" {
			t.Fatalf("%+v", snap.App)
		}
		if snap.LatestMixer() != nil {
			t.Fatal("a mixer from a stale feed")
		}
	}
	// The prose line is no source of a mixer identity.
	body := `{"tag_name":"v1.4.0","body":"Bundled RC2FX mixer software: 9.9.9."}`
	snap := snapshotAt(serve(t, "200 OK", "", body), github(), serve(t, "404 Not Found", "", ""), "1.3.0")
	if snap.LatestMixer() != nil {
		t.Fatal("a mixer from prose")
	}
}

func TestAnOversizedFeedIsRefused(t *testing.T) {
	snap := snapshotAt(serve(t, "200 OK", "", releaseBody("v1.3.0")), "http://127.0.0.1:9/unused", serve(t, "200 OK", "", strings.Repeat(" ", maxFeedBody+10)), "1.3.0")
	if snap.LatestMixer() != nil {
		t.Fatal("a mixer from an oversized feed")
	}
	status := snap.MixerUpdate(some("1.0.0"), some("1.1.0")).Changelog.Status
	if status.Kind != ChangelogUnavailable || status.Error.Kind != UpdateOther {
		t.Fatalf("%+v", status)
	}
}

func TestNothingListeningReadsAsOffline(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	must(t, err)
	port := listener.Addr().(*net.TCPAddr).Port
	listener.Close()
	// The listener is closed, so the connection is refused.
	got := checkAt(fmt.Sprintf("http://127.0.0.1:%d/", port), "http://127.0.0.1:9/unused", "0.1.0")
	if got.Kind != UpdateFailed || got.Error.Kind != UpdateOffline {
		t.Fatalf("%+v", got)
	}
}

func TestEveryEndpointAndLinkIsOnTheOfficialRepository(t *testing.T) {
	api := "https://api.github.com/repos/" + AppRepository + "/releases"
	if listAPI != api || latestAPI != api+"/latest" || AppReleasesURL != "https://github.com/"+AppRepository+"/releases" {
		t.Fatal("API URLs")
	}
	if !strings.HasPrefix(releaseURL("1.2.3"), AppReleasesURL+"/tag/") {
		t.Fatal("release URL")
	}
	site := "https://ibanks42.github.io/rc2fx"
	if UpdateFeedURL != site+"/updates.json" || AppHistoryURL != site+"/updates/app" || MixerHistoryURL != site+"/updates/mixer" {
		t.Fatal("site URLs")
	}
}
