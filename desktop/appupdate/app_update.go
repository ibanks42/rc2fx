// Package appupdate checks published RC2FX desktop app and mixer release
// feeds and verifies downloaded desktop app update packages against signed
// TUF metadata.
package appupdate

// What the official project has published: the latest RC2FX desktop app,
// the mixer software it bundles, and the release notes leading to both.
//
// Unauthenticated HTTPS requests only. GitHub's latest release decides what
// is published; the update feed on the documentation site supplies the notes
// and which mixer software each app release bundles. Without a usable feed,
// app notes come from GitHub's release list and the published mixer software
// stays unknown. Nothing is downloaded or installed: the result is versions,
// release notes and links.
//
// GitHub's latest release excludes drafts and prereleases; this module still
// refuses anything that is not a published, stable `vMAJOR.MINOR.PATCH` app
// release, such as a `mixer-v` tag left from before mixer runtimes were only
// released inside the app.
//
// A feed entry newer than that release is ignored. That only blocks entries
// above the latest release: that every lower entry was published is the
// official generator's guarantee, from GitHub's release list, and is not
// checked here.
//
// This says what exists, not what can be installed: what this app installs
// comes from its own bundled payload alone.

import (
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"slices"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// The addresses name the official repository on purpose: a build from a fork
// still checks the official releases.
const (
	// AppReleasesURL is the repository's releases page.
	AppReleasesURL = "https://github.com/ibanks42/rc2fx/releases"
	latestAPI      = "https://api.github.com/repos/ibanks42/rc2fx/releases/latest"
	listAPI        = "https://api.github.com/repos/ibanks42/rc2fx/releases"
	// UpdateFeedURL is the update feed, generated from the same notes as the
	// history pages.
	UpdateFeedURL = "https://ibanks42.github.io/rc2fx/updates.json"
	// AppHistoryURL is every app release's notes, on one page.
	AppHistoryURL = "https://ibanks42.github.io/rc2fx/updates/app"
	// MixerHistoryURL is every mixer software release's notes, on one page.
	MixerHistoryURL = "https://ibanks42.github.io/rc2fx/updates/mixer"
	// feedSchema is the only feed schema this build reads. New fields are
	// ignored; a breaking change gets a new URL.
	feedSchema  = 1
	maxFeedBody = 4 * 1024 * 1024
	// maxFeedReleases is per list; far above any real history.
	maxFeedReleases = 2000
	updateTimeout   = 10 * time.Second
	// maxReleaseBody: a release reply is a few KiB; anything past this is
	// refused.
	maxReleaseBody = 1024 * 1024
	// Release-list paging: at most maxPages pages of perPage releases, each
	// page at most maxListBody, all within notesDeadline.
	perPage       = 100
	maxPages      = 5
	maxListBody   = 4 * 1024 * 1024
	notesDeadline = 30 * time.Second
	// maxNotes: one release's notes are cut here, at a character boundary.
	maxNotes = 64 * 1024
)

// allowPlainHTTP lets tests talk to a local server.
var allowPlainHTTP = false

// UpdateCheckKind names an UpdateCheck variant.
type UpdateCheckKind int

const (
	// UpdateUpToDate: no newer release. Latest is the newest published app
	// version, which can be older than a development build.
	UpdateUpToDate UpdateCheckKind = iota
	// UpdateAvailable: a newer release (Version), and the release page (URL)
	// built from its validated tag.
	UpdateAvailable
	// UpdateNoRelease: the repository has no published app release yet.
	UpdateNoRelease
	// UpdateFailed: Error says why.
	UpdateFailed
)

// UpdateCheck is this app's check.
type UpdateCheck struct {
	Kind      UpdateCheckKind
	Latest    string
	Version   string
	URL       string
	Changelog Changelog
	Error     UpdateError
}

// Changelog is the release notes between the installed version and the
// offered one.
type Changelog struct {
	// Entries is every stable app release with installed < version <=
	// offered that was found, newest first.
	Entries []ReleaseNotes
	Status  ChangelogStatus
}

// ReleaseNotes is one release's notes.
type ReleaseNotes struct {
	// Version is `0.2.0`.
	Version string
	// URL is the release page, built from the validated tag.
	URL string
	// Notes is Markdown source, from the feed or GitHub's release body.
	// Never render it as HTML.
	Notes string
	// Truncated: the notes were longer than 64 KiB and were cut.
	Truncated bool
}

// ChangelogStatusKind names a ChangelogStatus variant.
type ChangelogStatusKind int

const (
	// ChangelogComplete: every release in the range is in Entries.
	ChangelogComplete ChangelogStatusKind = iota
	// ChangelogPartial: some releases in the range may be missing (a paging
	// limit or the deadline was reached, a later page failed, or the offered
	// release was not listed). Link to AppReleasesURL for the rest.
	ChangelogPartial
	// ChangelogUnavailable: the release list could not be read; Entries is
	// empty and Error says why.
	ChangelogUnavailable
)

// ChangelogStatus is how complete a changelog is.
type ChangelogStatus struct {
	Kind  ChangelogStatusKind
	Error UpdateError
}

// UpdateErrorKind names an UpdateError variant.
type UpdateErrorKind int

const (
	UpdateOffline UpdateErrorKind = iota
	UpdateRateLimited
	UpdateOther
)

// UpdateError is why a check failed.
type UpdateError struct {
	Kind UpdateErrorKind
	Why  string
}

func (e UpdateError) Error() string {
	switch e.Kind {
	case UpdateOffline:
		return "You appear to be offline"
	case UpdateRateLimited:
		return "GitHub rate limit reached, try again later"
	}
	return "Update check failed: " + e.Why
}

func otherError(why string) UpdateError { return UpdateError{Kind: UpdateOther, Why: why} }

// UpdateSnapshot is what is known about published releases. MixerUpdate
// answers for any mixer without another request, also before any check.
type UpdateSnapshot struct {
	// App is this app's check; nil until one has finished.
	App          *UpdateCheck
	latestMixer  *PublishedMixer
	history      *history
	historyError UpdateError
}

// PublishedMixer is the mixer software bundled by the latest published app.
type PublishedMixer struct {
	Version string
	// Protocol is nil when the feed does not record it.
	Protocol *uint32
	// AppVersion is the app release that bundles it.
	AppVersion string
}

// MixerUpdate is how a mixer's software compares with what is bundled and
// published.
type MixerUpdate struct {
	State MixerUpdateState
	// Changelog is the notes for installed < version <= the newest version
	// State names, newest first; nil when it names none. Complete only when
	// the installed version and every version up to that one are in the
	// known history with notes.
	Changelog *Changelog
}

// MixerUpdateStateKind names a MixerUpdateState variant.
type MixerUpdateStateKind int

const (
	// MixerUnknown: the installed version is missing or not comparable, or
	// nothing newer is bundled and the latest published version is unknown.
	MixerUnknown MixerUpdateStateKind = iota
	// MixerCurrent: as new as the newest of the bundled and latest published
	// versions.
	MixerCurrent
	// MixerNewer: newer than the latest published version and any bundled
	// one. Nothing is offered: that would be a downgrade.
	MixerNewer
	// MixerBundled: the bundled Version is newer than the installed one.
	// Says nothing about what is published when latestMixer is nil, and
	// nothing about whether the installer may run.
	MixerBundled
	// MixerBundledThenApp: the bundled Version is newer than the installed
	// one, and the app AppVersion carries the still newer Latest.
	MixerBundledThenApp
	// MixerAppUpdateRequired: nothing bundled is newer; the newer app
	// AppVersion carries Latest.
	MixerAppUpdateRequired
)

// MixerUpdateState is information only. Whether the installer may run is
// decided from the bundled payload and the firmware it would flash, never
// from this.
type MixerUpdateState struct {
	Kind       MixerUpdateStateKind
	Version    string
	Latest     string
	AppVersion string
}

// SnapshotFromCheck is a check that learned nothing about mixer software.
func SnapshotFromCheck(app UpdateCheck) UpdateSnapshot {
	err := otherError("the update feed was not read")
	if app.Kind == UpdateFailed {
		err = app.Error
	}
	return UpdateSnapshot{App: &app, historyError: err}
}

// UncheckedSnapshot is before any check: only what the caller passes in is
// known.
func UncheckedSnapshot() UpdateSnapshot {
	return UpdateSnapshot{historyError: otherError("no update check has finished")}
}

func optionalVersion(v *string) (string, bool) {
	if v == nil || !comparable(*v) {
		return "", false
	}
	return *v, true
}

// MixerUpdate compares what the mixer reports with what this app bundles
// and what is published. A version that is missing or not comparable is
// unknown, never older or newer. No network: call it again whenever the
// mixer's reported version changes.
func (s UpdateSnapshot) MixerUpdate(installed, bundled *string) MixerUpdate {
	inst, ok := optionalVersion(installed)
	if !ok {
		return MixerUpdate{State: MixerUpdateState{Kind: MixerUnknown}}
	}
	b, hasBundled := optionalVersion(bundled)
	offeredBundled := hasBundled && isNewer(b, inst)
	latest := s.latestMixer
	offeredLatest := latest != nil && isNewer(latest.Version, inst)
	var state MixerUpdateState
	switch {
	case !offeredBundled && !offeredLatest:
		switch {
		case latest == nil:
			state.Kind = MixerUnknown
		case isNewer(inst, latest.Version) && (!hasBundled || isNewer(inst, b)):
			state.Kind = MixerNewer
		default:
			state.Kind = MixerCurrent
		}
	case offeredBundled && !offeredLatest:
		state = MixerUpdateState{Kind: MixerBundled, Version: b}
	case !offeredBundled && offeredLatest:
		state = MixerUpdateState{Kind: MixerAppUpdateRequired, Latest: latest.Version, AppVersion: latest.AppVersion}
	case isNewer(latest.Version, b):
		state = MixerUpdateState{Kind: MixerBundledThenApp, Version: b, Latest: latest.Version, AppVersion: latest.AppVersion}
	default:
		state = MixerUpdateState{Kind: MixerBundled, Version: b}
	}
	var offered string
	switch state.Kind {
	case MixerUnknown, MixerCurrent, MixerNewer:
		return MixerUpdate{State: state}
	case MixerBundled:
		offered = state.Version
	default:
		offered = state.Latest
	}
	var log Changelog
	if s.history != nil {
		log = s.history.mixerChangelog(inst, offered)
	} else {
		log = Changelog{Entries: []ReleaseNotes{}, Status: ChangelogStatus{Kind: ChangelogUnavailable, Error: s.historyError}}
	}
	return MixerUpdate{State: state, Changelog: &log}
}

// CheckForUpdates asks whether a release newer than current (this app's
// version, e.g. `0.1.0`) exists and what mixer software is published, and
// collects the release notes since current. Blocking: up to ten seconds
// each for GitHub and the feed, and about thirty more if the notes must come
// from GitHub's release list. Call it off the UI goroutine.
func CheckForUpdates(current string) UpdateSnapshot {
	return snapshotAt(latestAPI, listAPI, UpdateFeedURL, current)
}

func snapshotAt(latestURL, listURL, feedURL, current string) UpdateSnapshot {
	if !comparable(current) {
		return SnapshotFromCheck(UpdateCheck{Kind: UpdateFailed, Error: otherError(fmt.Sprintf("this app's version %s is not a release version", debugString(current)))})
	}
	reply, err := fetch(latestURL, current, maxReleaseBody, updateTimeout)
	if err != nil {
		return SnapshotFromCheck(UpdateCheck{Kind: UpdateFailed, Error: *err})
	}
	app := evaluate(current, reply)
	// The feed cannot say what is latest, so without GitHub's answer it is
	// not read.
	var latest string
	switch app.Kind {
	case UpdateUpToDate:
		latest = app.Latest
	case UpdateAvailable:
		latest = app.Version
	default:
		return SnapshotFromCheck(app)
	}
	var h *history
	var historyErr UpdateError
	feed, ferr := fetch(feedURL, current, maxFeedBody, updateTimeout)
	if ferr != nil {
		historyErr = *ferr
	} else if parsed, perr := parseHistory(feed, latest); perr != nil {
		historyErr = *perr
	} else {
		h = parsed
	}
	var latestMixer *PublishedMixer
	if h != nil {
		latestMixer = h.publishedMixer(latest)
	}
	if app.Kind == UpdateAvailable {
		var log *Changelog
		if h != nil {
			if l := h.appChangelog(current, app.Version); l.Status.Kind == ChangelogComplete {
				log = &l
			}
		}
		if log == nil {
			l := githubChangelog(listURL, current, app.Version)
			log = &l
		}
		app.Changelog = *log
	}
	return UpdateSnapshot{App: &app, latestMixer: latestMixer, history: h, historyError: historyErr}
}

// githubChangelog is the notes since current, from GitHub's paged release
// list.
func githubChangelog(listURL, current, offered string) Changelog {
	deadline := time.Now().Add(notesDeadline)
	return changelog(current, offered, deadline, func(page uint32) (*reply, *UpdateError) {
		// No page may outlast the notes deadline.
		remaining := time.Until(deadline)
		if remaining <= 0 {
			e := otherError("the release notes took too long")
			return nil, &e
		}
		return fetch(fmt.Sprintf("%s?per_page=%d&page=%d", listURL, perPage, page), current, maxListBody, min(remaining, updateTimeout))
	})
}

func isNewer(a, b string) bool {
	order, ok := core.CompareVersions(a, b)
	return ok && order > 0
}

// inRange is low < version <= high.
func inRange(version, low, high string) bool {
	return isNewer(version, low) && !isNewer(version, high)
}

type feedList[T any] struct {
	Releases []T `json:"releases"`
}

type feedMixer struct {
	Version       string  `json:"version"`
	NotesMarkdown *string `json:"notesMarkdown,default"`
}

type feedApp struct {
	Version         string  `json:"version"`
	NotesMarkdown   *string `json:"notesMarkdown,default"`
	MixerVersion    *string `json:"mixerVersion,default"`
	ProtocolVersion *uint32 `json:"protocolVersion,default"`
}

type feed struct {
	SchemaVersion uint32              `json:"schemaVersion"`
	App           feedList[feedApp]   `json:"app"`
	Mixer         feedList[feedMixer] `json:"mixer"`
}

// history is the feed, reduced to what is published.
type history struct {
	// apps is no newer than the latest published release, newest first.
	apps []feedApp
	// mixers is each mixer version a listed app bundled, newest first.
	mixers []mixerEntry
	// stale: the latest published release is not listed, so what it bundles
	// is unknown and a release before it may be missing too.
	stale bool
}

type mixerEntry struct {
	version string
	// firstApp is the first app release that bundled it.
	firstApp string
	// notes is nil when the feed has no notes for it.
	notes *string
}

func parseHistory(r *reply, latest string) (*history, *UpdateError) {
	bad := func(why string) *UpdateError {
		e := otherError("the update feed " + why)
		return &e
	}
	if r.status != 200 {
		return nil, bad(fmt.Sprintf("answered HTTP %d", r.status))
	}
	var f feed
	if err := sjson.Unmarshal(r.body, &f); err != nil {
		return nil, bad(fmt.Sprintf("is not readable: %v", err))
	}
	if f.SchemaVersion != feedSchema {
		return nil, bad(fmt.Sprintf("has schema %d", f.SchemaVersion))
	}
	if max(len(f.App.Releases), len(f.Mixer.Releases)) > maxFeedReleases {
		return nil, bad("lists too many releases")
	}
	apps := f.App.Releases
	listed := slices.Clone(f.Mixer.Releases)
	for _, app := range apps {
		mixerOK := app.MixerVersion == nil || strictVersion(*app.MixerVersion)
		if !strictVersion(app.Version) || !mixerOK || (app.ProtocolVersion != nil && *app.ProtocolVersion == 0) {
			return nil, bad(fmt.Sprintf("has an invalid release %s", debugString(app.Version)))
		}
	}
	for _, m := range listed {
		if !strictVersion(m.Version) {
			return nil, bad(fmt.Sprintf("has an invalid release %s", debugString(m.Version)))
		}
	}
	newestFirst := func(a, b string) int {
		order, _ := core.CompareVersions(b, a)
		return order
	}
	slices.SortStableFunc(apps, func(a, b feedApp) int { return newestFirst(a.Version, b.Version) })
	slices.SortStableFunc(listed, func(a, b feedMixer) int { return newestFirst(a.Version, b.Version) })
	for i := 1; i < len(apps); i++ {
		if apps[i-1].Version == apps[i].Version {
			return nil, bad("lists a release twice")
		}
	}
	for i := 1; i < len(listed); i++ {
		if listed[i-1].Version == listed[i].Version {
			return nil, bad("lists a release twice")
		}
	}
	apps = slices.DeleteFunc(apps, func(app feedApp) bool { return isNewer(app.Version, latest) })
	stale := len(apps) == 0 || apps[0].Version != latest

	var mixers []mixerEntry
	for i := len(apps) - 1; i >= 0; i-- {
		app := apps[i]
		if app.MixerVersion == nil {
			continue
		}
		version := *app.MixerVersion
		same := func(v string) bool {
			order, ok := core.CompareVersions(v, version)
			return ok && order == 0
		}
		if slices.ContainsFunc(mixers, func(m mixerEntry) bool { return same(m.version) }) {
			continue
		}
		var notes *string
		if j := slices.IndexFunc(listed, func(m feedMixer) bool { return same(m.Version) }); j >= 0 {
			if n := listed[j].NotesMarkdown; n != nil && strings.TrimSpace(*n) != "" {
				text := *n
				notes = &text
			}
		}
		mixers = append(mixers, mixerEntry{version: version, firstApp: app.Version, notes: notes})
	}
	slices.SortStableFunc(mixers, func(a, b mixerEntry) int { return newestFirst(a.version, b.version) })
	return &history{apps: apps, mixers: mixers, stale: stale}, nil
}

// publishedMixer is what the latest release bundles, when the feed lists
// that exact release and names it.
func (h *history) publishedMixer(latest string) *PublishedMixer {
	if len(h.apps) == 0 || h.apps[0].Version != latest || h.apps[0].MixerVersion == nil {
		return nil
	}
	app := h.apps[0]
	return &PublishedMixer{Version: *app.MixerVersion, Protocol: app.ProtocolVersion, AppVersion: app.Version}
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

// appChangelog is complete only when current and the offered release are
// listed and every release in the range has notes.
func (h *history) appChangelog(current, offered string) Changelog {
	var inFeed []feedApp
	for _, app := range h.apps {
		if inRange(app.Version, current, offered) {
			inFeed = append(inFeed, app)
		}
	}
	complete := slices.ContainsFunc(h.apps, func(a feedApp) bool { return a.Version == current }) &&
		slices.ContainsFunc(inFeed, func(a feedApp) bool { return a.Version == offered })
	entries := []ReleaseNotes{}
	for _, app := range inFeed {
		if !hasNotes(app.NotesMarkdown) {
			complete = false
		}
		entries = append(entries, notesEntry(app.Version, app.Version, deref(app.NotesMarkdown)))
	}
	status := ChangelogStatus{Kind: ChangelogPartial}
	if complete {
		status.Kind = ChangelogComplete
	}
	return Changelog{Entries: entries, Status: status}
}

// mixerChangelog is complete only when the installed and offered versions
// are listed, every version in the range has notes, and no release could
// hide another version: the feed is current and names the mixer software of
// every app release. An installed version that is not listed may predate
// the history, so what lies between is not known.
func (h *history) mixerChangelog(installed, offered string) Changelog {
	var inFeed []mixerEntry
	for _, m := range h.mixers {
		if inRange(m.version, installed, offered) {
			inFeed = append(inFeed, m)
		}
	}
	same := func(a, b string) bool {
		order, ok := core.CompareVersions(a, b)
		return ok && order == 0
	}
	complete := !h.stale &&
		!slices.ContainsFunc(h.apps, func(a feedApp) bool { return a.MixerVersion == nil }) &&
		slices.ContainsFunc(h.mixers, func(m mixerEntry) bool { return same(m.version, installed) }) &&
		slices.ContainsFunc(inFeed, func(m mixerEntry) bool { return same(m.version, offered) })
	entries := []ReleaseNotes{}
	for _, m := range inFeed {
		if m.notes == nil {
			complete = false
		}
		entries = append(entries, notesEntry(m.version, m.firstApp, deref(m.notes)))
	}
	status := ChangelogStatus{Kind: ChangelogPartial}
	if complete {
		status.Kind = ChangelogComplete
	}
	return Changelog{Entries: entries, Status: status}
}

func hasNotes(notes *string) bool {
	return notes != nil && strings.TrimSpace(*notes) != ""
}

// notesEntry is version's notes, linking to the release page of appVersion.
func notesEntry(version, appVersion, text string) ReleaseNotes {
	notes, truncated := cutNotes(text)
	return ReleaseNotes{Version: version, URL: releaseURL(appVersion), Notes: notes, Truncated: truncated}
}

// reply is what the server answered, reduced to what the decision needs.
type reply struct {
	status             int
	rateLimitExhausted bool
	body               []byte
}

func fetch(u, current string, maxBody int64, timeout time.Duration) (*reply, *UpdateError) {
	if !strings.HasPrefix(u, "https://") && !allowPlainHTTP {
		e := otherError("refusing a non-HTTPS URL")
		return nil, &e
	}
	client := &http.Client{
		// Covers connecting, the reply and reading its body.
		Timeout: timeout,
		Transport: &http.Transport{
			Proxy:                 http.ProxyFromEnvironment,
			DialContext:           (&net.Dialer{Timeout: timeout}).DialContext,
			TLSHandshakeTimeout:   timeout,
			ResponseHeaderTimeout: timeout,
		},
		// Both hosts answer directly; a redirect elsewhere is not followed.
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	defer client.CloseIdleConnections()
	request, err := http.NewRequest(http.MethodGet, u, nil)
	if err != nil {
		e := otherError(err.Error())
		return nil, &e
	}
	request.Header.Set("User-Agent", "rc2fx-update-check/"+current)
	request.Header.Set("Accept", "application/vnd.github+json")
	request.Header.Set("X-GitHub-Api-Version", "2022-11-28")
	response, err := client.Do(request)
	if err != nil {
		e := classify(err)
		return nil, &e
	}
	defer response.Body.Close()
	exhausted := strings.TrimSpace(response.Header.Get("x-ratelimit-remaining")) == "0"
	body, err := io.ReadAll(io.LimitReader(response.Body, maxBody+1))
	if err != nil {
		e := otherError(fmt.Sprintf("reading the reply: %v", err))
		return nil, &e
	}
	if int64(len(body)) > maxBody {
		e := otherError("the reply is too large")
		return nil, &e
	}
	return &reply{status: response.StatusCode, rateLimitExhausted: exhausted, body: body}, nil
}

func classify(err error) UpdateError {
	var op *net.OpError
	var timeout interface{ Timeout() bool }
	if (errors.As(err, &op) && op.Op == "dial") || (errors.As(err, &timeout) && timeout.Timeout()) {
		return UpdateError{Kind: UpdateOffline}
	}
	return otherError(err.Error())
}

// appVersionOf is `v1.2.3` → `1.2.3`; anything else, including `mixer-v…`,
// is refused. The same rule as the release script's STABLE pattern: three
// numbers, none with a leading zero.
func appVersionOf(tag string) (string, bool) {
	version, ok := strings.CutPrefix(tag, "v")
	if !ok || !strictVersion(version) {
		return "", false
	}
	return version, true
}

func strictVersion(version string) bool {
	parts := strings.Split(version, ".")
	if len(parts) != 3 {
		return false
	}
	for _, p := range parts {
		if p == "" || len(p) > 9 || strings.Trim(p, "0123456789") != "" || (len(p) > 1 && p[0] == '0') {
			return false
		}
	}
	return true
}

type githubRelease struct {
	TagName    string  `json:"tag_name"`
	Draft      bool    `json:"draft,default"`
	Prerelease bool    `json:"prerelease,default"`
	Body       *string `json:"body,default"`
}

// statusError is a non-200 reply as an error: rate limits apart from
// everything else.
func statusError(r *reply) UpdateError {
	switch {
	case r.status == 429:
		return UpdateError{Kind: UpdateRateLimited}
	case r.status == 403 && r.rateLimitExhausted:
		return UpdateError{Kind: UpdateRateLimited}
	}
	return otherError(fmt.Sprintf("GitHub answered HTTP %d", r.status))
}

func evaluate(current string, r *reply) UpdateCheck {
	fail := func(why string) UpdateCheck { return UpdateCheck{Kind: UpdateFailed, Error: otherError(why)} }
	switch r.status {
	case 200:
	case 404:
		return UpdateCheck{Kind: UpdateNoRelease}
	default:
		return UpdateCheck{Kind: UpdateFailed, Error: statusError(r)}
	}
	var release githubRelease
	if err := sjson.Unmarshal(r.body, &release); err != nil {
		return fail(fmt.Sprintf("GitHub's reply is not a release: %v", err))
	}
	if release.Draft || release.Prerelease {
		return fail("GitHub's latest release is not a published stable release")
	}
	latest, ok := appVersionOf(release.TagName)
	if !ok {
		return fail(fmt.Sprintf("the latest release %s is not an app release", debugString(release.TagName)))
	}
	order, ok := core.CompareVersions(latest, current)
	switch {
	case !ok:
		return fail(fmt.Sprintf("cannot compare %s with %s", debugString(latest), debugString(current)))
	case order > 0:
		return UpdateCheck{
			Kind:      UpdateAvailable,
			Version:   latest,
			URL:       releaseURL(latest),
			Changelog: Changelog{Entries: []ReleaseNotes{}, Status: ChangelogStatus{Kind: ChangelogPartial}},
		}
	}
	return UpdateCheck{Kind: UpdateUpToDate, Latest: latest}
}

func releaseURL(version string) string {
	return AppReleasesURL + "/tag/v" + version
}

// cutNotes is text cut to at most maxNotes bytes at a character boundary.
func cutNotes(text string) (string, bool) {
	if len(text) <= maxNotes {
		return text, false
	}
	end := maxNotes
	for end > 0 && !utf8.RuneStart(text[end]) {
		end--
	}
	return text[:end], true
}

// changelog is the notes of every stable app release with current <
// version <= offered, newest first, from the paged release list page(n) (n
// from 1). Complete only when the whole list was read within the limits and
// the offered release was in it: the list is ordered by creation date, not
// by version, so stopping early could miss a release.
func changelog(current, offered string, deadline time.Time, page func(uint32) (*reply, *UpdateError)) Changelog {
	entries := []ReleaseNotes{}
	exhausted := false
	var failure *UpdateError
	for n := uint32(1); n <= maxPages; n++ {
		if !time.Now().Before(deadline) {
			break
		}
		r, err := page(n)
		if err != nil {
			failure = err
			break
		}
		if r.status != 200 {
			e := statusError(r)
			failure = &e
			break
		}
		var releases []githubRelease
		if err := sjson.Unmarshal(r.body, &releases); err != nil {
			e := otherError(fmt.Sprintf("GitHub's release list is not readable: %v", err))
			failure = &e
			break
		}
		for _, release := range releases {
			if release.Draft || release.Prerelease {
				continue
			}
			version, ok := appVersionOf(release.TagName)
			if !ok {
				continue
			}
			above, ok1 := core.CompareVersions(version, current)
			below, ok2 := core.CompareVersions(version, offered)
			if !(ok1 && above > 0 && ok2 && below <= 0) || slices.ContainsFunc(entries, func(e ReleaseNotes) bool { return e.Version == version }) {
				continue
			}
			notes, truncated := cutNotes(deref(release.Body))
			entries = append(entries, ReleaseNotes{Version: version, URL: releaseURL(version), Notes: notes, Truncated: truncated})
		}
		if len(releases) < perPage {
			exhausted = true
			break
		}
	}
	slices.SortStableFunc(entries, func(a, b ReleaseNotes) int {
		order, _ := core.CompareVersions(b.Version, a.Version)
		return order
	})
	status := ChangelogStatus{Kind: ChangelogPartial}
	switch {
	case failure != nil && len(entries) == 0:
		status = ChangelogStatus{Kind: ChangelogUnavailable, Error: *failure}
	case exhausted && slices.ContainsFunc(entries, func(e ReleaseNotes) bool { return e.Version == offered }):
		status.Kind = ChangelogComplete
	}
	return Changelog{Entries: entries, Status: status}
}

// debugString quotes s, escaping quotes, backslashes, \n \r \t \0, and other
// unprintable runes as \u{hex}.
func debugString(s string) string {
	var out strings.Builder
	out.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			out.WriteString(`\"`)
		case '\\':
			out.WriteString(`\\`)
		case '\n':
			out.WriteString(`\n`)
		case '\r':
			out.WriteString(`\r`)
		case '\t':
			out.WriteString(`\t`)
		case 0:
			out.WriteString(`\0`)
		default:
			if r == utf8.RuneError || unicode.IsControl(r) || (!unicode.IsPrint(r) && r != ' ') {
				fmt.Fprintf(&out, `\u{%x}`, r)
			} else {
				out.WriteRune(r)
			}
		}
	}
	out.WriteByte('"')
	return out.String()
}
