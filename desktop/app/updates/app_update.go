// Package updates tracks update check results, URL allowlisting, and
// markdown sanitization for release notes.
package updates

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// RodeNotesURL is RØDE's release notes page for the Pro II and Duo.
const RodeNotesURL = "https://update.rode.com/notes/rcp2/"

type check struct {
	snapshot appupdate.UpdateSnapshot
	manual   bool
}

// AppUpdate stores the latest app, mixer, and RØDE firmware update check results.
type AppUpdate struct {
	Checking bool
	// Startup failures stay silent; manual checks surface errors in Status.
	last *check
	// Authenticated mixer offers (nil until a check succeeds).
	signed *[]installer.MixerOffer
	// Latest RØDE firmware releases per model.
	rode []installer.Release
}

func (u *AppUpdate) Finish(result appupdate.UpdateSnapshot, manual bool) {
	u.Checking = false
	u.last = &check{snapshot: result, manual: manual}
}

func (u *AppUpdate) result() *appupdate.UpdateCheck {
	if u.last == nil {
		return nil
	}
	return u.last.snapshot.App
}

func (u *AppUpdate) Standing() model.AppStanding {
	if u.Checking {
		return model.AppStanding{Kind: model.AppStandingChecking}
	}
	if r := u.result(); r != nil {
		switch r.Kind {
		case appupdate.UpdateUpToDate:
			return model.AppStanding{Kind: model.AppStandingCurrent, Latest: r.Latest}
		case appupdate.UpdateAvailable:
			return model.AppStanding{Kind: model.AppStandingUpdate, Version: r.Version}
		}
	}
	return model.AppStanding{Kind: model.AppStandingUnknown}
}

// MixerUpdate compares installed against available mixer software using the
// latest snapshot if present, or an offline snapshot otherwise.
func (u *AppUpdate) MixerUpdate(installed, available *string) appupdate.MixerUpdate {
	if u.last != nil {
		return u.last.snapshot.MixerUpdate(installed, available)
	}
	unchecked := appupdate.UncheckedSnapshot()
	return unchecked.MixerUpdate(installed, available)
}

func (u *AppUpdate) Signed() ([]installer.MixerOffer, bool) {
	if u.signed == nil {
		return nil, false
	}
	return *u.signed, true
}

func (u *AppUpdate) SetSigned(offers []installer.MixerOffer) { u.signed = &offers }

// RodeFirmware returns RØDE's newest stable firmware version for the model
// matching firmwareProfile.
func (u *AppUpdate) RodeFirmware(firmwareProfile *string) (string, bool) {
	if firmwareProfile == nil {
		return "", false
	}
	profile := *firmwareProfile
	var found *firmware.Model
	for _, m := range firmware.ModelAll {
		if rest, ok := strings.CutPrefix(profile, m.ID()); ok && strings.HasPrefix(rest, "-") {
			found = &m
			break
		}
	}
	if found == nil {
		return "", false
	}
	for _, r := range u.rode {
		if r.Model == found.ID() && r.Channel == "stable" {
			return r.Version, true
		}
	}
	return "", false
}

// SetRode replaces the cached RØDE release list for model m.
func (u *AppUpdate) SetRode(m firmware.Model, releases []installer.Release) {
	kept := u.rode[:0:0]
	for _, r := range u.rode {
		if r.Model != m.ID() {
			kept = append(kept, r)
		}
	}
	u.rode = append(kept, releases...)
}

func (u *AppUpdate) Status() (string, bool) {
	if u.Checking {
		return "Checking…", true
	}
	if u.last == nil || u.last.snapshot.App == nil {
		return "", false
	}
	r, manual := u.last.snapshot.App, u.last.manual
	switch r.Kind {
	case appupdate.UpdateUpToDate:
		return fmt.Sprintf("Up to date (latest release %s)", r.Latest), true
	case appupdate.UpdateAvailable:
		return fmt.Sprintf("RC2FX %s is available", r.Version), true
	case appupdate.UpdateNoRelease:
		return "No release published yet", manual
	case appupdate.UpdateFailed:
		return r.Error.Error(), manual
	}
	return "", false
}

func (u *AppUpdate) ReleaseURL() (string, bool) {
	r := u.result()
	if r == nil || r.Kind != appupdate.UpdateAvailable {
		return "", false
	}
	return SafeURL(r.URL)
}

func (u *AppUpdate) Offered() (string, *appupdate.Changelog, bool) {
	r := u.result()
	if r == nil || r.Kind != appupdate.UpdateAvailable {
		return "", nil, false
	}
	return r.Version, &r.Changelog, true
}

// SignedMixers fetches the signed mixer release list (blocking).
func SignedMixers(work string) ([]installer.MixerOffer, bool) {
	if !installer.MixerConfigured() || work == "" {
		return nil, false
	}
	// Use a separate work dir so this check doesn't contend on the main
	// installer lock while downloading.
	reply, err := installer.DownloadOnly(filepath.Join(work, "update-check")).Handle(installer.Request{Kind: installer.RequestMixerReleases})
	if err != nil || reply.Kind != installer.ReplyMixerReleases {
		return nil, false
	}
	return reply.Offers, true
}

func HistoryNote(status appupdate.ChangelogStatus) (string, bool) {
	switch status.Kind {
	case appupdate.ChangelogComplete:
		return "", false
	case appupdate.ChangelogPartial:
		return "Some release notes could not be loaded. The full update history is online.", true
	}
	return fmt.Sprintf("Release notes could not be loaded (%s). The full update history is online.", status.Error.Error()), true
}

// SafeURL allows only this project's release/history pages (with optional #vX.Y.Z
// anchor) and RØDE's firmware notes page.
func SafeURL(url string) (string, bool) {
	release := url == appupdate.AppReleasesURL || strings.HasPrefix(url, appupdate.AppReleasesURL+"/")
	history := false
	for _, page := range []string{appupdate.AppHistoryURL, appupdate.MixerHistoryURL} {
		if rest, ok := strings.CutPrefix(url, page); ok {
			if rest == "" {
				history = true
			} else if v, ok := strings.CutPrefix(rest, "#v"); ok {
				if _, ok := model.Compare(v, v); ok {
					history = true
				}
			}
		}
	}
	if release || history || url == RodeNotesURL {
		return url, true
	}
	return "", false
}
