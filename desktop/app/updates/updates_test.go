package updates

import (
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func changelog(status appupdate.ChangelogStatus) appupdate.Changelog {
	return appupdate.Changelog{
		Entries: []appupdate.ReleaseNotes{{
			Version: "0.2.0",
			URL:     appupdate.AppReleasesURL + "/tag/v0.2.0",
			Notes:   "- Faster meters",
		}},
		Status: status,
	}
}

func after(result appupdate.UpdateCheck, manual bool) AppUpdate {
	var u AppUpdate
	u.Finish(appupdate.SnapshotFromCheck(result), manual)
	return u
}

func TestIncompleteHistoryIsNeverPresentedAsComplete(t *testing.T) {
	if _, ok := HistoryNote(appupdate.ChangelogStatus{Kind: appupdate.ChangelogComplete}); ok {
		t.Fatal("complete history has a note")
	}
	if n, _ := HistoryNote(appupdate.ChangelogStatus{Kind: appupdate.ChangelogPartial}); !strings.Contains(n, "could not be loaded") {
		t.Fatal(n)
	}
	offline := appupdate.UpdateError{Kind: appupdate.UpdateOffline}
	n, _ := HistoryNote(appupdate.ChangelogStatus{Kind: appupdate.ChangelogUnavailable, Error: offline})
	if !strings.Contains(n, "could not be loaded") || !strings.Contains(n, offline.Error()) {
		t.Fatal(n)
	}
}

func TestOnlyProjectReleasePagesOpen(t *testing.T) {
	safe := func(url string) bool { _, ok := SafeURL(url); return ok }
	for url, want := range map[string]bool{
		appupdate.AppReleasesURL:                      true,
		appupdate.AppReleasesURL + "/tag/v0.2.0":      true,
		"https://github.com/ibanks42/rc2fx/releasesX": false,
		"https://evil.example/":                       false,
		appupdate.AppHistoryURL:                       true,
		appupdate.MixerHistoryURL + "#v1.2.3":         true,
		appupdate.MixerHistoryURL + "#v1.2.3x":        false,
		appupdate.AppHistoryURL + ".evil.example":     false,
		RodeNotesURL:                    true,
		"https://update.rode.com/other": false,
		"javascript:alert(1)":           false,
	} {
		if safe(url) != want {
			t.Fatalf("%s: %v", url, !want)
		}
	}
}

func TestTheOfferedChangelogIsExposed(t *testing.T) {
	a := after(appupdate.UpdateCheck{
		Kind:      appupdate.UpdateAvailable,
		Version:   "0.2.0",
		URL:       appupdate.AppReleasesURL + "/tag/v0.2.0",
		Changelog: changelog(appupdate.ChangelogStatus{Kind: appupdate.ChangelogComplete}),
	}, false)
	v, c, ok := a.Offered()
	if !ok || v != "0.2.0" || len(c.Entries) != 1 {
		t.Fatalf("%v %s %+v", ok, v, c)
	}
	n := after(appupdate.UpdateCheck{Kind: appupdate.UpdateNoRelease}, true)
	if _, _, ok := n.Offered(); ok {
		t.Fatal("no release offered something")
	}
}

func TestRodeFirmwareIsRodesNewestStableForTheMixersModel(t *testing.T) {
	release := func(m firmware.Model, channel, version string) installer.Release {
		return installer.Release{Model: m.ID(), Channel: channel, Version: version}
	}
	p := func(s string) *string { return &s }
	get := func(u *AppUpdate, profile *string) string { v, _ := u.RodeFirmware(profile); return v }
	u := after(appupdate.UpdateCheck{Kind: appupdate.UpdateNoRelease}, false)
	if v, ok := u.RodeFirmware(p("rodecaster-pro-ii-1.7.7")); ok {
		t.Fatal(v)
	}
	u.SetRode(firmware.ModelProIi, []installer.Release{
		release(firmware.ModelProIi, "beta", "1.8.1"),
		release(firmware.ModelProIi, "stable", "1.8.0"),
		release(firmware.ModelProIi, "stable", "1.7.7"),
	})
	if v := get(&u, p("rodecaster-pro-ii-1.7.7")); v != "1.8.0" {
		t.Fatal(v)
	}
	// The Duo is answered only from its own manifest.
	if _, ok := u.RodeFirmware(p("rodecaster-duo-1.7.7")); ok {
		t.Fatal("duo answered from the Pro II's list")
	}
	u.SetRode(firmware.ModelDuo, []installer.Release{release(firmware.ModelDuo, "stable", "1.7.9")})
	if v := get(&u, p("rodecaster-duo-1.7.7")); v != "1.7.9" {
		t.Fatal(v)
	}
	// A later answer for one model replaces only that model's list.
	u.SetRode(firmware.ModelProIi, []installer.Release{release(firmware.ModelProIi, "stable", "1.8.2")})
	if v := get(&u, p("rodecaster-pro-ii-1.7.7")); v != "1.8.2" {
		t.Fatal(v)
	}
	if v := get(&u, p("rodecaster-duo-1.7.7")); v != "1.7.9" {
		t.Fatal(v)
	}
	if _, ok := u.RodeFirmware(p("unknown-1.7.7")); ok {
		t.Fatal("unknown model answered")
	}
	if _, ok := u.RodeFirmware(nil); ok {
		t.Fatal("no profile answered")
	}
}

func TestStartupFailuresAreQuietAndManualOnesReadable(t *testing.T) {
	for _, e := range []appupdate.UpdateError{{Kind: appupdate.UpdateOffline}, {Kind: appupdate.UpdateRateLimited}} {
		quiet := after(appupdate.UpdateCheck{Kind: appupdate.UpdateFailed, Error: e}, false)
		if s, ok := quiet.Status(); ok {
			t.Fatal(s)
		}
		loud := after(appupdate.UpdateCheck{Kind: appupdate.UpdateFailed, Error: e}, true)
		if s, ok := loud.Status(); !ok || s != e.Error() {
			t.Fatal(s)
		}
	}
	quiet := after(appupdate.UpdateCheck{Kind: appupdate.UpdateNoRelease}, false)
	if s, ok := quiet.Status(); ok {
		t.Fatal(s)
	}
	loud := after(appupdate.UpdateCheck{Kind: appupdate.UpdateNoRelease}, true)
	if s, ok := loud.Status(); !ok || s != "No release published yet" {
		t.Fatal(s)
	}
}

func TestOnlyAnAvailableReleaseOnThisProjectIsLinked(t *testing.T) {
	ok := appupdate.AppReleasesURL + "/tag/v0.2.0"
	a := after(appupdate.UpdateCheck{
		Kind: appupdate.UpdateAvailable, Version: "0.2.0", URL: ok,
		Changelog: changelog(appupdate.ChangelogStatus{Kind: appupdate.ChangelogComplete}),
	}, false)
	if url, _ := a.ReleaseURL(); url != ok {
		t.Fatal(url)
	}
	if s, _ := a.Status(); s != "RC2FX 0.2.0 is available" {
		t.Fatal(s)
	}
	elsewhere := after(appupdate.UpdateCheck{
		Kind: appupdate.UpdateAvailable, Version: "0.2.0", URL: "https://example.com/rc2fx",
		Changelog: changelog(appupdate.ChangelogStatus{Kind: appupdate.ChangelogPartial}),
	}, true)
	if url, linked := elsewhere.ReleaseURL(); linked {
		t.Fatal(url)
	}
	current := after(appupdate.UpdateCheck{Kind: appupdate.UpdateUpToDate, Latest: "0.1.0"}, false)
	if url, linked := current.ReleaseURL(); linked {
		t.Fatal(url)
	}
	if s, _ := current.Status(); !strings.Contains(s, "Up to date") {
		t.Fatal(s)
	}
}

func TestCheckingIsShownWhileRunning(t *testing.T) {
	a := AppUpdate{Checking: true}
	if s, ok := a.Status(); !ok || s != "Checking…" {
		t.Fatal(s)
	}
	var idle AppUpdate
	if s, ok := idle.Status(); ok {
		t.Fatal(s)
	}
}

func TestNotesCannotCarryHTMLOrImages(t *testing.T) {
	for in, want := range map[string]string{
		`<img src="https://evil.example/x.png">`:   `\<img src="https://evil.example/x.png">`,
		"a ![pixel](https://evil.example/x.png) b": `a \![pixel](https://evil.example/x.png) b`,
		"<script>x</script>":                       `\<script>x\</script>`,
	} {
		if got := SafeMarkdown(in); got != want {
			t.Fatalf("%q: %q", in, got)
		}
	}
}

func TestOrdinaryMarkdownIsUnchanged(t *testing.T) {
	notes := "### Features\n\n- *(app)* Show **bold** and [a link](https://example.com)\n"
	if got := SafeMarkdown(notes); got != notes {
		t.Fatal(got)
	}
}

func TestCodeStaysLiteralAndEscapesAreKept(t *testing.T) {
	for in, want := range map[string]string{
		"use `<T>` here <b>":     "use `<T>` here \\<b>",
		"```\n<div>\n```\n<div>": "```\n<div>\n```\n\\<div>",
		"already \\<safe":        "already \\<safe",
		"a lone ` <b>":           "a lone ` \\<b>",
	} {
		if got := SafeMarkdown(in); got != want {
			t.Fatalf("%q: %q", in, got)
		}
	}
}

func TestVersionsThisAppCannotInstallAreMarked(t *testing.T) {
	entry := func(version string) appupdate.ReleaseNotes {
		return appupdate.ReleaseNotes{Version: version, URL: appupdate.AppReleasesURL + "/tag/v" + version}
	}
	v := "1.0.0"
	if got := EntryHeading("Mixer software", entry("1.1.0"), &v); got != "Mixer software 1.1.0 · needs an app update" {
		t.Fatal(got)
	}
	if got := EntryHeading("Mixer software", entry("1.0.0"), &v); got != "Mixer software 1.0.0" {
		t.Fatal(got)
	}
	if got := EntryHeading("RC2FX", entry("1.1.0"), nil); got != "RC2FX 1.1.0" {
		t.Fatal(got)
	}
}
