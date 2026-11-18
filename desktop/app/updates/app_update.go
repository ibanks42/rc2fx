// Package updates is the non-drawing half of the update check and the
// Versions section: what the last update check found,
// which links may open, and the untrusted release notes made safe to
// render. Nothing here imports gpuikit.dev.
//
// Checking GitHub for a newer desktop app only notifies. Copies Velopack
// installed download and install an update when the user asks (package
// selfupdate); other copies link to the release page.
package updates

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// RodeNotesURL is RØDE's published notes for the Pro II and the Duo.
const RodeNotesURL = "https://update.rode.com/notes/rcp2/"

// check is the last result, and whether the user asked for it.
type check struct {
	snapshot installer.UpdateSnapshot
	manual   bool
}

// AppUpdate is what the update checks found. The zero value has checked
// nothing.
type AppUpdate struct {
	// Checking: a check is running.
	Checking bool
	// last is the last result. A failed check at startup stays quiet; one
	// the user asked for says what went wrong.
	last *check
	// signed is the latest authenticated mixer releases. The feed in last
	// can't say what this app may install; only these can. nil until a
	// check answers.
	signed *[]installer.MixerOffer
	// rode is the firmware RØDE publishes, from each model's manifest. A
	// model has none until a check answers for it; a failed check keeps the
	// last answer.
	rode []installer.Release
}

// Finish records a finished check of the app: its result and whether it
// was asked for, and that no check is under way.
func (u *AppUpdate) Finish(result installer.UpdateSnapshot, manual bool) {
	u.Checking = false
	u.last = &check{snapshot: result, manual: manual}
}

func (u *AppUpdate) result() *installer.UpdateCheck {
	if u.last == nil {
		return nil
	}
	return u.last.snapshot.App
}

// Standing is where this app stands, for the Versions dialog.
func (u *AppUpdate) Standing() model.AppStanding {
	if u.Checking {
		return model.AppStanding{Kind: model.AppStandingChecking}
	}
	if r := u.result(); r != nil {
		switch r.Kind {
		case installer.UpdateUpToDate:
			return model.AppStanding{Kind: model.AppStandingCurrent, Latest: r.Latest}
		case installer.UpdateAvailable:
			return model.AppStanding{Kind: model.AppStandingUpdate, Version: r.Version}
		}
	}
	return model.AppStanding{Kind: model.AppStandingUnknown}
}

// MixerUpdate compares a mixer with what this app can install and, once a
// check has answered, published. It needs no network, so it works before
// the first check.
func (u *AppUpdate) MixerUpdate(installed, available *string) installer.MixerUpdate {
	if u.last != nil {
		return u.last.snapshot.MixerUpdate(installed, available)
	}
	unchecked := installer.UncheckedSnapshot()
	return unchecked.MixerUpdate(installed, available)
}

// Signed is the latest authenticated mixer releases; ok false until a check
// answers.
func (u *AppUpdate) Signed() ([]installer.MixerOffer, bool) {
	if u.signed == nil {
		return nil, false
	}
	return *u.signed, true
}

// SetSigned records the latest authenticated mixer releases.
func (u *AppUpdate) SetSigned(offers []installer.MixerOffer) { u.signed = &offers }

// RodeFirmware is RØDE's newest stable firmware for the mixer's model, once
// its firmware is known.
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

// SetRode replaces one model's list of RØDE releases.
func (u *AppUpdate) SetRode(m firmware.Model, releases []installer.Release) {
	kept := u.rode[:0:0]
	for _, r := range u.rode {
		if r.Model != m.ID() {
			kept = append(kept, r)
		}
	}
	u.rode = append(kept, releases...)
}

// Status is the status line under "Check for updates"; ok false shows
// nothing.
func (u *AppUpdate) Status() (string, bool) {
	if u.Checking {
		return "Checking…", true
	}
	if u.last == nil || u.last.snapshot.App == nil {
		return "", false
	}
	r, manual := u.last.snapshot.App, u.last.manual
	switch r.Kind {
	case installer.UpdateUpToDate:
		return fmt.Sprintf("Up to date (latest release %s)", r.Latest), true
	case installer.UpdateAvailable:
		return fmt.Sprintf("RC2FX %s is available", r.Version), true
	case installer.UpdateNoRelease:
		return "No release published yet", manual
	case installer.UpdateFailed:
		return r.Error.Error(), manual
	}
	return "", false
}

// ReleaseURL is the release page to offer, only for a GitHub page of this
// project.
func (u *AppUpdate) ReleaseURL() (string, bool) {
	r := u.result()
	if r == nil || r.Kind != installer.UpdateAvailable {
		return "", false
	}
	return SafeURL(r.URL)
}

// Offered is the offered version and its changes, when an update is
// available.
func (u *AppUpdate) Offered() (string, *installer.Changelog, bool) {
	r := u.result()
	if r == nil || r.Kind != installer.UpdateAvailable {
		return "", nil, false
	}
	return r.Version, &r.Changelog, true
}

// SignedMixers asks the signed repository what this app could install. ok
// false when it can't say; the last answer stands, and the installer can
// always ask again. work is the installer's working folder ("" unknown).
// Blocking.
func SignedMixers(work string) ([]installer.MixerOffer, bool) {
	if !installer.MixerConfigured() || work == "" {
		return nil, false
	}
	// Its own folder: the engine holds its folder's lock for a whole
	// request, network included, and the installer page must not wait for
	// this check.
	reply, err := installer.DownloadOnly(filepath.Join(work, "update-check")).Handle(installer.Request{Kind: installer.RequestMixerReleases})
	if err != nil || reply.Kind != installer.ReplyMixerReleases {
		return nil, false
	}
	return reply.Offers, true
}

// HistoryNote is what the changes view says about missing notes; ok false
// when complete.
func HistoryNote(status installer.ChangelogStatus) (string, bool) {
	switch status.Kind {
	case installer.ChangelogComplete:
		return "", false
	case installer.ChangelogPartial:
		return "Some release notes could not be loaded. The full update history is online.", true
	}
	return fmt.Sprintf("Release notes could not be loaded (%s). The full update history is online.", status.Error.Error()), true
}

// SafeURL lets only this project's release pages and update history, and
// RØDE's notes, open. A history link may name a version: `#v1.2.3`.
func SafeURL(url string) (string, bool) {
	release := url == installer.AppReleasesURL || strings.HasPrefix(url, installer.AppReleasesURL+"/")
	history := false
	for _, page := range []string{installer.AppHistoryURL, installer.MixerHistoryURL} {
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
