// Package appupdate checks published RC2FX app and mixer releases and verifies
// downloaded app update packages against signed TUF metadata.
package appupdate

// Release checks use unauthenticated HTTPS only. GitHub's latest release is the
// authority for what is published; the docs site's update feed provides release
// notes and the mixer version bundled by each app release. If the feed is
// unavailable, app notes fall back to GitHub's release list and the published
// mixer version stays unknown.
//
// Only stable `vMAJOR.MINOR.PATCH` app releases are accepted (drafts,
// prereleases, and legacy `mixer-v` tags are rejected). Feed entries newer than
// GitHub's latest release are ignored; lower entries are trusted as generated
// from GitHub's release list. Results are informational only and do not govern
// what the installer can flash.

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

// Always point at the official repo even in fork builds.
const (
	AppReleasesURL  = "https://github.com/ibanks42/rc2fx/releases"
	latestAPI       = "https://api.github.com/repos/ibanks42/rc2fx/releases/latest"
	listAPI         = "https://api.github.com/repos/ibanks42/rc2fx/releases"
	UpdateFeedURL   = "https://ibanks42.github.io/rc2fx/updates.json"
	AppHistoryURL   = "https://ibanks42.github.io/rc2fx/updates/app"
	MixerHistoryURL = "https://ibanks42.github.io/rc2fx/updates/mixer"

	// Breaking feed schema changes get a new URL; unknown fields are ignored.
	feedSchema      = 1
	maxFeedBody     = 4 * 1024 * 1024
	maxFeedReleases = 2000 // per list
	updateTimeout   = 10 * time.Second
	maxReleaseBody  = 1024 * 1024
	perPage         = 100
	maxPages        = 5
	maxListBody     = 4 * 1024 * 1024
	notesDeadline   = 30 * time.Second
	maxNotes        = 64 * 1024 // per release, cut on a UTF-8 boundary
)

// Overridden in tests for local HTTP servers.
var allowPlainHTTP = false

type UpdateCheckKind int

const (
	// UpdateUpToDate means no newer release exists. Latest can still be older
	// than a local dev build.
	UpdateUpToDate UpdateCheckKind = iota
	UpdateAvailable
	UpdateNoRelease
	UpdateFailed
)

type UpdateCheck struct {
	Kind      UpdateCheckKind
	Latest    string
	Version   string
	URL       string
	Changelog Changelog
	Error     UpdateError
}

// Changelog holds release notes for versions in (installed, offered], newest first.
type Changelog struct {
	Entries []ReleaseNotes
	Status  ChangelogStatus
}

type ReleaseNotes struct {
	Version string
	// Release page URL constructed from the validated tag.
	URL string
	// Raw Markdown from the feed or GitHub release body; never render as HTML.
	Notes     string
	Truncated bool
}

type ChangelogStatusKind int

const (
	ChangelogComplete ChangelogStatusKind = iota
	// ChangelogPartial means some releases in the range may be missing (paging
	// cap, deadline, failed later page, or offered release absent).
	ChangelogPartial
	ChangelogUnavailable
)

type ChangelogStatus struct {
	Kind  ChangelogStatusKind
	Error UpdateError
}

type UpdateErrorKind int

const (
	UpdateOffline UpdateErrorKind = iota
	UpdateRateLimited
	UpdateOther
)

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

// UpdateSnapshot caches published release metadata so MixerUpdate can be
// queried offline whenever a mixer's reported version changes.
type UpdateSnapshot struct {
	// App is nil until an update check completes.
	App          *UpdateCheck
	latestMixer  *PublishedMixer
	history      *history
	historyError UpdateError
}

// PublishedMixer describes the mixer runtime bundled by the latest app release.
type PublishedMixer struct {
	Version    string
	Protocol   *uint32
	AppVersion string
}

type MixerUpdate struct {
	State MixerUpdateState
	// Notes for (installed, offered], newest first; nil if no update is offered.
	Changelog *Changelog
}

type MixerUpdateStateKind int

const (
	MixerUnknown MixerUpdateStateKind = iota
	MixerCurrent
	// MixerNewer means the mixer is ahead of both bundled and published
	// versions; downgrades are not offered.
	MixerNewer
	// MixerBundled means the app bundles a newer mixer version than installed.
	MixerBundled
	// MixerBundledThenApp means the app bundles a newer version and a newer
	// app release carries an even newer mixer version.
	MixerBundledThenApp
	// MixerAppUpdateRequired means only a newer app release has a newer mixer.
	MixerAppUpdateRequired
)

// MixerUpdateState is informational only; installer eligibility is determined
// solely from the bundled payload and target firmware.
type MixerUpdateState struct {
	Kind       MixerUpdateStateKind
	Version    string
	Latest     string
	AppVersion string
}

// SnapshotFromCheck wraps an app check with no mixer history.
func SnapshotFromCheck(app UpdateCheck) UpdateSnapshot {
	err := otherError("the update feed was not read")
	if app.Kind == UpdateFailed {
		err = app.Error
	}
	return UpdateSnapshot{App: &app, historyError: err}
}

// UncheckedSnapshot returns a snapshot before any network check has run.
func UncheckedSnapshot() UpdateSnapshot {
	return UpdateSnapshot{historyError: otherError("no update check has finished")}
}

func optionalVersion(v *string) (string, bool) {
	if v == nil || !comparable(*v) {
		return "", false
	}
	return *v, true
}

// MixerUpdate compares the mixer's installed version against the bundled and
// latest published versions without making network requests.
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

// CheckForUpdates queries GitHub and the update feed for newer app/mixer
// releases and release notes since current (e.g. "0.1.0"). Blocks on network
// I/O (up to ~50s in the fallback path), so call off the UI goroutine.
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
	// Skip the feed if GitHub didn't tell us the latest release.
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

func githubChangelog(listURL, current, offered string) Changelog {
	deadline := time.Now().Add(notesDeadline)
	return changelog(current, offered, deadline, func(page uint32) (*reply, *UpdateError) {
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

// inRange reports whether low < version <= high.
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

// history is the validated feed filtered down to published releases.
type history struct {
	apps   []feedApp    // <= latest published release, newest first
	mixers []mixerEntry // unique mixer versions bundled by apps, newest first
	// stale is true when the latest published release is missing from the feed.
	stale bool
}

type mixerEntry struct {
	version  string
	firstApp string // first app release that bundled this mixer version
	notes    *string
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

// mixerChangelog is marked complete only when the feed is fresh, every listed
// app maps a mixer version, both endpoints are present, and all intermediate
// entries have notes.
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

func notesEntry(version, appVersion, text string) ReleaseNotes {
	notes, truncated := cutNotes(text)
	return ReleaseNotes{Version: version, URL: releaseURL(appVersion), Notes: notes, Truncated: truncated}
}

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
		Timeout: timeout,
		Transport: &http.Transport{
			Proxy:                 http.ProxyFromEnvironment,
			DialContext:           (&net.Dialer{Timeout: timeout}).DialContext,
			TLSHandshakeTimeout:   timeout,
			ResponseHeaderTimeout: timeout,
		},
		// Don't follow redirects off the expected hosts.
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

// appVersionOf parses strict "vMAJOR.MINOR.PATCH" tags (no leading zeros),
// matching the release script's STABLE pattern and rejecting mixer-v* tags.
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

// cutNotes truncates text to at most maxNotes bytes on a UTF-8 boundary.
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

// changelog collects notes for stable releases in (current, offered], newest
// first, from 1-indexed pages. Because GitHub orders releases by creation date
// rather than version, the result is only marked complete if all pages are read
// and offered appears in the list.
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
