// Package model implements non-UI desktop app state and logic: version
// comparisons, level history, editing helpers, USB observations, and target layout.
package model

import (
	"fmt"

	"github.com/ibanks42/rc2fx/desktop/appupdate"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

const AppVersion = installer.AppVersion

// Compare compares two dotted numeric version strings. Returns ok=false if
// either version cannot be parsed.
func Compare(a, b string) (int, bool) {
	return core.CompareVersions(a, b)
}

func BundledMixerSoftware() (string, bool) {
	release, err := installer.BundledRelease()
	if err != nil {
		return "", false
	}
	return release.Version, true
}

func OfferFits(offer installer.MixerOffer, profile string) bool {
	return offer.Metadata.RequireCompatible(AppVersion, uint32(core.MixerProtocolVersion), profile) == nil
}

// NewestAvailable returns the highest mixer software version this app can
// install (from signed offers or the bundled release). Any supported firmware
// profile counts because flashing can update firmware at the same time.
func NewestAvailable(offers []installer.MixerOffer, bundled *string) (string, bool) {
	var candidates []string
	for _, offer := range offers {
		for _, profile := range offer.Metadata.FirmwareProfiles {
			if OfferFits(offer, profile) {
				candidates = append(candidates, offer.Metadata.Version)
				break
			}
		}
	}
	if bundled != nil {
		candidates = append(candidates, *bundled)
	}
	best, found := "", false
	for _, version := range candidates {
		if _, ok := Compare(version, version); !ok {
			continue
		}
		// Break ties in favor of the later entry.
		if c, _ := Compare(best, version); !found || c <= 0 {
			best, found = version, true
		}
	}
	return best, found
}

func NewestSupportedFirmware(firmwareProfile *string) (string, bool) {
	if firmwareProfile == nil {
		return "", false
	}
	return installer.NewestSupportedFirmware(*firmwareProfile)
}

func AppTooOld(minimum string) bool {
	c, ok := Compare(AppVersion, minimum)
	return ok && c < 0
}

type AppNeed struct {
	Version    string
	MinimumApp string
}

// NeedsNewerApp returns the newest signed mixer offer above available that
// requires a newer desktop app version.
func NeedsNewerApp(offers []installer.MixerOffer, available *string) (AppNeed, bool) {
	var best *installer.MixerTarget
	for i := range offers {
		m := &offers[i].Metadata
		if !AppTooOld(m.MinimumAppVersion) {
			continue
		}
		if available != nil {
			if c, ok := Compare(m.Version, *available); !ok || c <= 0 {
				continue
			}
		}
		if best == nil {
			best = m
		} else if c, ok := Compare(best.Version, m.Version); !ok || c <= 0 {
			best = m
		}
	}
	if best == nil {
		return AppNeed{}, false
	}
	return AppNeed{Version: best.Version, MinimumApp: best.MinimumAppVersion}, true
}

// Standing describes how an installed version compares to the available one.
type Standing int

const (
	StandingUnknown Standing = iota
	StandingCurrent
	StandingUpdateAvailable
	// StandingNewer means the mixer is ahead of this app; never offer a downgrade.
	StandingNewer
)

func StandingOf(installed, available *string) Standing {
	if installed == nil || available == nil {
		return StandingUnknown
	}
	c, ok := Compare(*installed, *available)
	switch {
	case !ok:
		return StandingUnknown
	case c == 0:
		return StandingCurrent
	case c < 0:
		return StandingUpdateAvailable
	default:
		return StandingNewer
	}
}

type Summary struct {
	Runtime       Standing
	Firmware      Standing
	Compatibility Compatibility
}

// Compatibility describes whether the mixer's control protocol matches this app.
type Compatibility int

const (
	CompatibilityUnknown Compatibility = iota
	CompatibilityCompatible
	CompatibilityMixerOlder
	CompatibilityMixerNewer
)

func CompatibilityOf(deviceProtocol *uint32) Compatibility {
	if deviceProtocol == nil {
		return CompatibilityUnknown
	}
	p, own := *deviceProtocol, uint32(core.MixerProtocolVersion)
	switch {
	case p == own:
		return CompatibilityCompatible
	case p < own:
		return CompatibilityMixerOlder
	default:
		return CompatibilityMixerNewer
	}
}

// UpdateAvailable reports whether this app can safely flash a newer runtime
// or firmware without downgrading either component.
func (s Summary) UpdateAvailable() bool {
	currentOrUpdate := s.Firmware == StandingCurrent || s.Firmware == StandingUpdateAvailable
	if s.Compatibility == CompatibilityMixerOlder {
		return s.Runtime != StandingNewer && currentOrUpdate
	}
	return s.Compatibility == CompatibilityCompatible &&
		((s.Runtime == StandingUpdateAvailable && currentOrUpdate) ||
			(s.Runtime == StandingCurrent && s.Firmware == StandingUpdateAvailable))
}

// Blocker explains why this app cannot update the mixer (e.g. protocol
// mismatch or a component that would be downgraded by a combined flash).
func (s Summary) Blocker() (string, bool) {
	if s.Compatibility == CompatibilityMixerOlder {
		if s.UpdateAvailable() {
			return "", false
		}
		return "The RC2FX mixer software on this mixer is older than this app supports, " +
			"and its RØDE firmware is newer or unknown, so this app cannot update it " +
			"safely.", true
	}
	if s.Compatibility == CompatibilityMixerNewer {
		return "The RC2FX mixer software on this mixer was made for a different " +
			"version of this app, so compatibility is unknown. Update the app " +
			"before changing anything on the mixer.", true
	}
	newer := ""
	switch {
	case s.Runtime == StandingNewer && s.Firmware == StandingNewer:
		newer = "newer RC2FX mixer software and RØDE firmware"
	case s.Runtime == StandingNewer:
		newer = "newer RC2FX mixer software"
	case s.Firmware == StandingNewer:
		newer = "newer RØDE firmware"
	}
	if newer != "" {
		return fmt.Sprintf("This mixer has %s than this app can install. The installer "+
			"flashes RC2FX mixer software and RØDE firmware together, so installing "+
			"from this app would replace it with older versions. Update the app first.", newer), true
	}
	return "", false
}

// Inputs collects the known app, mixer, and firmware versions for Settings > Versions.
type Inputs struct {
	App                     string
	InstalledRuntime        *string
	AvailableRuntime        *string
	SignedKnown             bool
	NeedsApp                *AppNeed
	InstalledFirmware       *string
	NewestSupportedFirmware *string
	RodeFirmware            *string
	DeviceProtocol          *uint32
}

// rodeAhead returns RØDE's newest firmware if it is ahead of both the mixer
// and the newest firmware RC2FX supports.
func rodeAhead(i *Inputs) (string, bool) {
	if i.RodeFirmware == nil || i.InstalledFirmware == nil {
		return "", false
	}
	rode := *i.RodeFirmware
	newer := func(than string) bool {
		c, ok := Compare(rode, than)
		return ok && c > 0
	}
	if newer(*i.InstalledFirmware) && (i.NewestSupportedFirmware == nil || newer(*i.NewestSupportedFirmware)) {
		return rode, true
	}
	return "", false
}

func Summarize(i *Inputs) Summary {
	return Summary{
		Runtime:       StandingOf(i.InstalledRuntime, i.AvailableRuntime),
		Firmware:      StandingOf(i.InstalledFirmware, i.NewestSupportedFirmware),
		Compatibility: CompatibilityOf(i.DeviceProtocol),
	}
}

type Part int

const (
	PartApp Part = iota
	PartMixer
	PartFirmware
)

// Tone orders status severity from least to most urgent.
type Tone int

const (
	ToneUnknown Tone = iota
	ToneQuiet
	ToneUpdate
	ToneWarn
	ToneBad
)

type AppStandingKind int

const (
	AppStandingChecking AppStandingKind = iota
	AppStandingUnknown
	AppStandingCurrent
	AppStandingUpdate
)

type AppStanding struct {
	Kind    AppStandingKind
	Latest  string
	Version string
}

type Line struct {
	Part        Part
	Name        string
	Installed   string
	Available   string
	Status      string
	Tone        Tone
	ReadChanges bool
}

type Lead struct {
	Title   string
	Detail  *string
	Changes *Part
}

type Overview struct {
	Lead           Lead
	Lines          []Line
	Details        []string
	OfferInstaller bool
}

type statusTone struct {
	text string
	tone Tone
}

var (
	upToDate       = statusTone{"Up to date", ToneQuiet}
	update         = statusTone{"Update available", ToneUpdate}
	blocked        = statusTone{"Blocked", ToneWarn}
	newerStatus    = statusTone{"Newer", ToneWarn}
	updateAppFirst = statusTone{"Update the app first", ToneWarn}
	unknownStatus  = statusTone{"Unknown", ToneUnknown}
	rodeNewer      = statusTone{"Newer from RØDE", ToneQuiet}
)

func mixerStatus(s Summary, mixer appupdate.MixerUpdateState, signedKnown bool) (statusTone, bool) {
	if s.Compatibility == CompatibilityMixerNewer {
		return statusTone{"Update the app", ToneWarn}, false
	}
	tooOld := s.Compatibility == CompatibilityMixerOlder
	switch mixer.Kind {
	case appupdate.MixerAppUpdateRequired:
		if signedKnown {
			return statusTone{"Needs app update", ToneWarn}, true
		}
		// A signed release may carry it: only that check can say.
		return unknownStatus, true
	case appupdate.MixerNewer:
		return newerStatus, false
	case appupdate.MixerBundled, appupdate.MixerBundledThenApp:
		switch {
		case !s.UpdateAvailable():
			return blocked, true
		case tooOld:
			return statusTone{"Too old", ToneBad}, true
		default:
			return update, true
		}
	default:
		// Current or Unknown.
		if tooOld {
			return statusTone{"Too old", ToneBad}, false
		}
		switch s.Runtime {
		case StandingCurrent:
			return upToDate, false
		case StandingNewer:
			return newerStatus, false
		case StandingUpdateAvailable:
			if s.UpdateAvailable() {
				return update, false
			}
			return blocked, false
		default:
			return unknownStatus, false
		}
	}
}

func firmwareStatus(s Summary) (statusTone, bool) {
	switch s.Firmware {
	case StandingCurrent:
		return upToDate, false
	case StandingUpdateAvailable:
		if s.UpdateAvailable() {
			return update, true
		}
		return blocked, true
	case StandingNewer:
		return newerStatus, false
	default:
		return unknownStatus, false
	}
}

func showVersion(v *string) string {
	if v == nil {
		return "Unknown"
	}
	return *v
}

// OverviewOf builds the Versions section summary. OfferInstaller is driven
// only by signed/bundled availability, never by the unauthenticated mixer feed.
func OverviewOf(i *Inputs, app AppStanding, mixer appupdate.MixerUpdateState) Overview {
	s := Summarize(i)
	var (
		appAvailable string
		appStatus    statusTone
		appRead      bool
	)
	switch app.Kind {
	case AppStandingChecking:
		appAvailable, appStatus = "Unknown", statusTone{"Checking…", ToneUnknown}
	case AppStandingUnknown:
		appAvailable, appStatus = "Unknown", unknownStatus
	case AppStandingCurrent:
		appAvailable, appStatus = app.Latest, upToDate
	case AppStandingUpdate:
		appAvailable, appStatus, appRead = app.Version, update, true
	}
	var (
		mixerText statusTone
		mixerRead bool
	)
	if i.NeedsApp != nil && !s.UpdateAvailable() {
		// What can be installed now comes first; the note below says the rest.
		mixerText, mixerRead = statusTone{"Needs app update", ToneWarn}, false
	} else {
		mixerText, mixerRead = mixerStatus(s, mixer, i.SignedKnown)
		// A newer app is available, and it's what this mixer is waiting for.
		if mixerText == newerStatus && app.Kind == AppStandingUpdate {
			mixerText = updateAppFirst
		}
	}
	rode, rodeOK := rodeAhead(i)
	var (
		firmwareText statusTone
		firmwareRead bool
	)
	if rodeOK && s.Firmware == StandingCurrent {
		// "Up to date" would be wrong: RØDE has newer, RC2FX can't install it.
		firmwareText, firmwareRead = rodeNewer, false
	} else {
		firmwareText, firmwareRead = firmwareStatus(s)
	}
	mixerAvailable := showVersion(i.AvailableRuntime)
	if i.NeedsApp != nil {
		mixerAvailable = i.NeedsApp.Version
	} else if mixer.Kind == appupdate.MixerAppUpdateRequired {
		mixerAvailable = mixer.Latest
	}
	firmwareAvailable := showVersion(i.NewestSupportedFirmware)
	if rodeOK {
		firmwareAvailable = rode
	}
	lines := []Line{
		{
			Part:        PartApp,
			Name:        "App",
			Installed:   i.App,
			Available:   appAvailable,
			Status:      appStatus.text,
			Tone:        appStatus.tone,
			ReadChanges: appRead,
		},
		{
			Part:        PartMixer,
			Name:        "Mixer software",
			Installed:   showVersion(i.InstalledRuntime),
			Available:   mixerAvailable,
			Status:      mixerText.text,
			Tone:        mixerText.tone,
			ReadChanges: mixerRead,
		},
		{
			Part:        PartFirmware,
			Name:        "RØDE firmware",
			Installed:   showVersion(i.InstalledFirmware),
			Available:   firmwareAvailable,
			Status:      firmwareText.text,
			Tone:        firmwareText.tone,
			ReadChanges: firmwareRead,
		},
	}
	var details []string
	if blocker, ok := s.Blocker(); ok {
		details = append(details, blocker)
	}
	if i.SignedKnown && i.NeedsApp == nil {
		latest, appVersion, ok := "", "", false
		switch mixer.Kind {
		case appupdate.MixerBundledThenApp, appupdate.MixerAppUpdateRequired:
			latest, appVersion, ok = mixer.Latest, mixer.AppVersion, true
		}
		if ok {
			details = append(details, fmt.Sprintf(
				"Mixer software %s isn't available for this app. Update to RC2FX %s to get it.",
				latest, appVersion))
		}
	}
	if i.NeedsApp != nil {
		details = append(details, fmt.Sprintf(
			"Mixer software %s needs RC2FX %s or newer. Update the app first.",
			i.NeedsApp.Version, i.NeedsApp.MinimumApp))
	}
	connected := i.InstalledRuntime != nil || i.InstalledFirmware != nil
	if mixer.Kind == appupdate.MixerUnknown && connected && len(details) == 0 {
		details = append(details, "The latest published mixer software is unknown.")
	}
	checkInInstaller := connected &&
		!i.SignedKnown &&
		s.Compatibility != CompatibilityMixerNewer &&
		s.Runtime != StandingNewer &&
		s.Firmware != StandingNewer
	if checkInInstaller {
		details = append(details, "Open the installer to check for signed mixer software.")
	}
	updates := 0
	worst := ToneUnknown
	allQuiet := true
	for _, l := range lines {
		if l.Tone == ToneUpdate {
			updates++
		}
		worst = max(worst, l.Tone)
		if l.Tone != ToneQuiet {
			allQuiet = false
		}
	}
	var (
		flag     string
		flagTone Tone
	)
	switch {
	case worst == ToneBad:
		flag, flagTone = "Needs update", ToneBad
	case worst == ToneWarn:
		flag, flagTone = "Needs attention", ToneWarn
	case worst == ToneUpdate && updates == 1:
		flag, flagTone = "Update available", ToneUpdate
	case worst == ToneUpdate:
		flag, flagTone = fmt.Sprintf("%d updates available", updates), ToneUpdate
	case !connected:
		flag, flagTone = "Not connected", ToneUnknown
	case allQuiet:
		flag, flagTone = "Up to date", ToneQuiet
		if rodeOK {
			flag = "RC2FX up to date"
		}
	default:
		flag, flagTone = "Unknown", ToneUnknown
	}
	waiting := false
	for _, l := range lines {
		if l.Tone == ToneUpdate && l.Part != PartApp {
			waiting = true
			break
		}
	}
	var lead Lead
	switch {
	case app.Kind == AppStandingUpdate:
		lead = Lead{
			Title:   fmt.Sprintf("RC2FX %s is available", app.Version),
			Detail:  new(fmt.Sprintf("You have %s.", i.App)),
			Changes: new(PartApp),
		}
	case app.Kind == AppStandingChecking:
		lead = Lead{Title: "Checking for updates…"}
	case waiting:
		lead = Lead{
			Title:  "A mixer update is ready",
			Detail: new("Mixer software and RØDE firmware install together."),
		}
		for _, l := range lines {
			if l.Tone == ToneUpdate && l.ReadChanges {
				lead.Changes = new(l.Part)
				break
			}
		}
	case flagTone == ToneQuiet:
		if rodeOK {
			lead = Lead{
				Title:  "RC2FX is up to date",
				Detail: new(fmt.Sprintf("RØDE firmware %s is out, but RC2FX doesn't support it yet.", rode)),
			}
		} else {
			lead = Lead{Title: "Everything is up to date"}
		}
	default:
		lead = Lead{Title: flag}
	}
	if rodeOK && flagTone != ToneQuiet {
		details = append(details, fmt.Sprintf(
			"RØDE firmware %s is out, but RC2FX doesn't support it yet, so this app can't install it.", rode))
	}
	return Overview{
		Lead:           lead,
		Lines:          lines,
		Details:        details,
		OfferInstaller: checkInInstaller || s.UpdateAvailable(),
	}
}
