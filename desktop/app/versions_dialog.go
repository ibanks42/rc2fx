package app

// The Versions section of Settings, and the changelog
// that What's new opens.
//
// Notes come from the network, so they are treated as untrusted: raw HTML
// and image syntax are escaped (updates.SafeMarkdown), and links open only
// when updates.SafeURL allows them. (The text view cannot yet be given an
// image source that loads nothing; see the TODO on changesView.)

import (
	"fmt"
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
	"github.com/ibanks42/rc2fx/desktop/app/updates"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

func versionsDialogToneColor(tone model.Tone) uint32 {
	switch tone {
	case model.ToneQuiet:
		return Muted
	case model.ToneUpdate:
		return Accent
	case model.ToneWarn:
		return Warn
	case model.ToneBad:
		return Danger
	}
	return Faint
}

// flag is a status: a small dot and its words, in the status's color.
func flag(text string, tone model.Tone) gk.DivEl {
	color := versionsDialogToneColor(tone)
	return gk.HFlex(s.ItemsCenter, s.FlexShrink0, s.FontMedium, s.WhitespaceNowrap).Gap(px(6)).
		TextSize(px(13)).TextColor(rgb(color)).
		Child(gk.Div(s.RoundedFull).Size(px(7)).Bg(rgb(color))).
		Child(txt(text))
}

// styledDialog is the shell every RC2FX dialog shares.
func styledDialog(width float32) gk.DialogEl {
	return gk.Dialog(s.P6, s.Border1).W(width).Rounded(px(20)).Bg(rgb(surface().Panel)).
		BorderColor(rgb(surface().LineStrong)).TextColor(rgb(Text)).FontFamily("Inter")
}

// versionsPill is a button in the Versions section (ui.go has the app's own
// pill).
func versionsPill(button gk.ButtonEl) gk.ButtonEl {
	return button.H(px(34)).Px(px(16)).Rounded(float32(17))
}

func large(button gk.ButtonEl) gk.ButtonEl {
	return button.Large().H(px(44)).Style(s.Px5).Rounded(float32(22))
}

// changeLink is one footer link of the changes dialog.
type changeLink struct{ label, url string }

// Changes is what the changes dialog shows for one part.
type Changes struct {
	// eyebrow is a small line above the title.
	eyebrow string
	title   string
	// entryPrefix starts each entry's heading.
	entryPrefix string
	changelog   *installer.Changelog
	// note is said above the notes instead of the changelog's own status;
	// "" for none.
	note string
	// available: entries newer than this need an app update first; nil for
	// none.
	available *string
	links     []changeLink
}

// changeEntry is one release's notes, made safe when the dialog opens.
type changeEntry struct {
	heading   string
	notes     string
	empty     bool
	truncated bool
}

// changesDialog is the open changes dialog: one gk.Built made on first use
// (a handler), drawing open.
type changesDialog struct {
	built gk.Built[gk.DialogEl]
	made  bool
	theme gk.BaseTheme
	open  *openChanges
}

type openChanges struct {
	spec    Changes
	note    string
	entries []changeEntry
}

func (a *App) openChangesDialog(cx *gk.Cx, spec Changes) {
	note := spec.note
	if note == "" && spec.changelog != nil {
		note, _ = updates.HistoryNote(spec.changelog.Status)
	}
	var entries []changeEntry
	if spec.changelog != nil {
		for _, entry := range spec.changelog.Entries {
			entries = append(entries, changeEntry{
				heading:   updates.EntryHeading(spec.entryPrefix, entry, spec.available),
				notes:     updates.SafeMarkdown(entry.Notes),
				empty:     strings.TrimSpace(entry.Notes) == "",
				truncated: entry.Truncated,
			})
		}
	}
	d := &a.appUpdate.changes
	d.open = &openChanges{spec: spec, note: note, entries: entries}
	// A copy of the theme as it is now: the global itself cannot be handed
	// to a text view's style. Taken again at each opening (the dialog is
	// closed here), so an appearance change shows.
	if d.made {
		gk.Release(cx, d.theme)
	}
	d.theme = gk.NewBaseThemeGlobal(cx)
	if !d.made {
		d.built = gk.NewBuilt(cx, a.changesView)
		d.made = true
	}
	gk.OpenDialog(cx, d.built)
}

// changesView draws the open changes dialog.
//
// TODO(B1 image source): each notes view should get an image source that
// returns an empty image, so no note could make the app fetch one. The Go
// BaseTextViewEl has no image source call; SafeMarkdown already escapes
// every `![` outside code, which is what kept images out in the first place.
func (a *App) changesView(cx *gk.Cx) gk.DialogEl {
	d := &a.appUpdate.changes
	open := d.open
	if open == nil {
		open = &openChanges{}
	}
	spec := open.spec
	style := func() gk.BaseTextViewStyle {
		return gk.NewBaseTextViewStyleFromTheme(d.theme).WithForeground(rgb(Muted)).
			WithLink(rgb(Accent)).WithCodeBackground(rgb(surface().Raised))
	}
	body := gk.VFlex(s.Gap5, s.Border1).MaxH(px(420)).P(px(18)).Rounded(px(12)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line)).TextSize(px(13.5)).
		ID("changes").OverflowYScroll()
	if open.note != "" {
		body = body.Child(gk.Div().TextColor(rgb(Faint)).Child(txt(open.note)))
	}
	for i, entry := range open.entries {
		item := gk.VFlex(s.Gap1).Child(gk.Div(s.FontSemibold).TextSize(px(15)).Child(txt(entry.heading)))
		if entry.empty {
			item = item.Child(gk.Div().TextColor(rgb(Muted)).Child(txt("No notes for this release.")))
		} else {
			item = item.Child(gk.BaseTextViewMarkdown(gk.Key("change-notes", i), entry.notes).
				Style(style()).Selectable(true).
				OnLinkClick(func(cx *gk.Cx, e gk.Event83) {
					if url, ok := updates.SafeURL(e.A0); ok {
						gk.OpenURL(cx, url)
					}
				}))
		}
		if entry.truncated {
			item = item.Child(gk.Div().TextColor(rgb(Faint)).Child(txt("Notes shortened; the update history has the rest.")))
		}
		body = body.Child(item)
	}
	footer := gk.DialogFooter(s.Pt5)
	for i, l := range spec.links {
		if button, ok := link(gk.Key("change-link", i), l.label, l.url); ok {
			footer = footer.Child(large(button).Style(s.Border1))
		}
	}
	footer = footer.Child(gk.DialogClose().Trigger(gk.Button("change-close").Primary().Large().
		H(px(44)).Style(s.Px5).Rounded(float32(22)).FontSemibold().Label("Close")))
	return styledDialog(720).
		Title(gk.VFlex(s.Gap1).
			Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(spec.eyebrow))).
			Child(gk.Div(s.TextXl, s.FontSemibold).Child(txt(spec.title)))).
		Child(body).
		Footer(footer)
}

// versionInputs is the mixer's software, its firmware, and the newest
// firmware this app supports for it; nil for unknown.
func (a *App) versionInputs() (runtime, firmware, newestFirmware *string) {
	d := a.deviceVersions
	if d == nil {
		return nil, nil, nil
	}
	if v, ok := model.NewestSupportedFirmware(d.FirmwareProfile); ok {
		newestFirmware = &v
	}
	return d.Runtime, d.Firmware, newestFirmware
}

// availableMixerSoftware is the newest mixer software this app could
// install right now.
func (a *App) availableMixerSoftware() *string {
	signed, _ := a.appUpdate.Signed()
	if v, ok := model.NewestAvailable(signed, a.bundledMixerSoftware); ok {
		return &v
	}
	return nil
}

// versionsOverview is the versions as Settings and the Versions dialog show
// them.
func (a *App) versionsOverview() model.Overview {
	runtime, firmware, newestFirmware := a.versionInputs()
	available := a.availableMixerSoftware()
	mixer := a.appUpdate.MixerUpdate(runtime, available)
	signed, signedKnown := a.appUpdate.Signed()
	var needsApp *model.AppNeed
	if need, ok := model.NeedsNewerApp(signed, available); ok {
		needsApp = &need
	}
	var profile, rode *string
	var protocol *uint32
	if a.deviceVersions != nil {
		profile, protocol = a.deviceVersions.FirmwareProfile, a.deviceVersions.Protocol
	}
	if v, ok := a.appUpdate.RodeFirmware(profile); ok {
		rode = &v
	}
	return model.OverviewOf(&model.Inputs{
		App:                     model.AppVersion,
		InstalledRuntime:        runtime,
		AvailableRuntime:        available,
		SignedKnown:             signedKnown,
		NeedsApp:                needsApp,
		InstalledFirmware:       firmware,
		NewestSupportedFirmware: newestFirmware,
		RodeFirmware:            rode,
		DeviceProtocol:          protocol,
	}, a.appUpdate.Standing(), mixer.State)
}

// versionsSection is the Versions section: what matters first, then each
// part.
func (a *App) versionsSection(cx *gk.Cx) gk.DivEl {
	overview := a.versionsOverview()
	lead := overview.Lead
	release, hasRelease := a.appUpdate.ReleaseURL()
	actions := gk.HFlex(s.FlexWrap, s.ItemsCenter, s.Gap2)
	acting := false
	updateState := a.selfUpdate.State()
	offered := hasRelease || a.selfUpdate.Previewing()
	// A started or finished download, or a failed check, stays even if
	// today's check found nothing.
	inPlace := a.selfUpdate.Supported() && (offered || a.selfUpdate.StaysShown())
	updateNote := ""
	if inPlace {
		button, note := a.updateButton(updateState)
		actions = actions.Child(button)
		updateNote = note
	}
	// A copy that cannot update itself downloads from the release page.
	download := !inPlace
	hasPrimary := inPlace
	if hasRelease && (!inPlace || updateState.Kind == selfupdate.StateFailed) {
		var button gk.ButtonEl
		if download {
			hasPrimary = true
			button = versionsPill(gk.Button("versions-download").Primary()).FontSemibold().Label("Download")
		} else {
			button = versionsPill(gk.Button("versions-download").Ghost().Style(s.Border1)).
				BorderColor(rgb(surface().LineStrong)).Label("Release page")
		}
		actions = actions.Child(button.Icon(gk.IconExternalLink).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { gk.OpenURL(cx, release) }))
		acting = true
	}
	acting = acting || inPlace
	if overview.OfferInstaller {
		// Only opens Update RC2FX; its Before you start says what happens.
		var button gk.ButtonEl
		if hasPrimary {
			button = versionsPill(gk.Button("versions-installer").Ghost().Style(s.Border1)).
				BorderColor(rgb(surface().LineStrong))
		} else {
			button = versionsPill(gk.Button("versions-installer").Primary()).FontSemibold()
		}
		actions = actions.Child(button.Label("Update RC2FX").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openTask(cx, taskUpdate) }))
		acting = true
	}
	if lead.Changes != nil {
		actions = actions.Child(a.whatsNew(gk.Key("versions-changes", 0), *lead.Changes))
		acting = true
	}
	tiles := gk.HFlex(s.ItemsStretch, s.Gap2)
	for _, line := range overview.Lines {
		elsewhere := line.ReadChanges && (lead.Changes == nil || *lead.Changes != line.Part)
		tiles = tiles.Child(a.versionTile(line, elsewhere))
	}
	failure := ""
	if a.appUpdate.Standing().Kind == model.AppStandingUnknown {
		failure, _ = a.appUpdate.Status()
	}
	heading := gk.VFlex(s.Gap1).Child(gk.Div(s.FontSemibold).TextSize(px(SubheadingText)).Child(txt(lead.Title)))
	if lead.Detail != nil {
		heading = heading.Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).Child(txt(*lead.Detail)))
	}
	body := gk.VFlex(s.Gap3, s.Pt3).Child(heading)
	if acting {
		body = body.Child(actions)
	}
	if updateNote != "" {
		body = body.Child(gk.Div(s.TextXs).TextColor(rgb(Muted)).Child(txt(updateNote)))
	}
	body = body.Child(tiles)
	for _, note := range overview.Details {
		body = body.Child(gk.Div(s.TextXs).LineHeight(gk.Relative(1.45)).TextColor(rgb(Muted)).Child(txt(note)))
	}
	if failure != "" {
		body = body.Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(failure)))
	}
	check := gk.Button("versions-check").Ghost().Small().Label("Check for updates").
		Disabled(a.appUpdate.Checking).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.checkAppUpdate(cx, true) })
	return settingsSection("Versions", check, body)
}

// updateButton is the in-place update action, which keeps one width through
// its states, and what to say beside it.
func (a *App) updateButton(state selfupdate.State) (gk.ButtonEl, string) {
	button := versionsPill(gk.Button("versions-update").Primary()).FontSemibold().MinW(px(184))
	switch state.Kind {
	case selfupdate.StateDownloading:
		return button.Label(fmt.Sprintf("Downloading %d%%", state.Percent)).Icon(gk.IconLoaderCircle).Disabled(true),
			"You can keep working while it downloads."
	case selfupdate.StateVerifying:
		return button.Label("Checking update").Icon(gk.IconLoaderCircle).Disabled(true),
			fmt.Sprintf("Checking RC2FX %s against its signed release.", state.Version)
	case selfupdate.StateReady:
		reason, blocked := a.installerRestartBlocker()
		note := fmt.Sprintf("RC2FX %s is downloaded. RC2FX restarts to install it.", state.Version)
		if blocked {
			note = fmt.Sprintf("RC2FX %s is downloaded. %s", state.Version, reason)
		}
		return button.Label("Restart to update").Icon(gk.IconRefreshCw).Disabled(blocked).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.restartToUpdate(cx) }), note
	}
	// Idle or Failed.
	note := "It downloads in the background. RC2FX restarts only when you choose."
	if state.Kind == selfupdate.StateFailed {
		note = state.Message
	}
	// A kept package that could not be checked is only checked again.
	label, icon := "Download update", gk.IconArrowDown
	if a.selfUpdate.Retrying() {
		label, icon = "Check again", gk.IconRefreshCw
	}
	return button.Label(label).Icon(icon).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.downloadAppUpdate(cx) }), note
}

func (a *App) whatsNew(id gk.ID, part model.Part) gk.ButtonEl {
	return versionsPill(gk.Button(id).Ghost().Style(s.Border1)).BorderColor(rgb(surface().LineStrong)).
		Label("What's new").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openChanges(cx, part) })
}

// versionTile is one part: its name, version and standing.
func (a *App) versionTile(line model.Line, whatsNew bool) gk.DivEl {
	version := line.Installed
	if line.Tone == model.ToneUpdate {
		version = fmt.Sprintf("%s → %s", line.Installed, line.Available)
	}
	tile := gk.VFlex(s.Flex1, s.MinW0, s.Gap1, s.P3, s.RoundedLg).Bg(rgb(surface().Raised)).
		Child(gk.Div(s.TextXs).TextColor(rgb(Muted)).Child(txt(line.Name))).
		Child(gk.Div(s.FontSemibold).TextSize(px(SubheadingText)).Child(txt(version))).
		Child(flag(line.Status, line.Tone))
	if whatsNew {
		tile = tile.Child(gk.HFlex(s.Pt1).Child(a.whatsNew(gk.Key("versions-changes", int(line.Part)+1), line.Part)))
	}
	return tile
}

func (a *App) openChanges(cx *gk.Cx, part model.Part) {
	runtime, firmware, newestFirmware := a.versionInputs()
	or := func(v *string) string {
		if v == nil {
			return "Unknown"
		}
		return *v
	}
	var spec Changes
	switch part {
	case model.PartApp:
		version, changelog, ok := a.appUpdate.Offered()
		if !ok {
			return
		}
		links := []changeLink{{"All app updates", fmt.Sprintf("%s#v%s", installer.AppHistoryURL, version)}}
		if url, ok := a.appUpdate.ReleaseURL(); ok {
			links = append(links, changeLink{"Download", url})
		}
		spec = Changes{
			eyebrow:     fmt.Sprintf("RC2FX %s is available", version),
			title:       fmt.Sprintf("What's new since %s", model.AppVersion),
			entryPrefix: "RC2FX",
			changelog:   changelog,
			links:       links,
		}
	case model.PartMixer:
		available := a.availableMixerSoftware()
		update := a.appUpdate.MixerUpdate(runtime, available)
		newest := or(available)
		if update.Changelog != nil && len(update.Changelog.Entries) > 0 {
			newest = update.Changelog.Entries[0].Version
		}
		spec = Changes{
			eyebrow:     fmt.Sprintf("Mixer software %s", newest),
			title:       fmt.Sprintf("What's new since %s", or(runtime)),
			entryPrefix: "Mixer software",
			changelog:   update.Changelog,
			available:   available,
			links:       []changeLink{{"All mixer updates", fmt.Sprintf("%s#v%s", installer.MixerHistoryURL, newest)}},
		}
	case model.PartFirmware:
		spec = Changes{
			eyebrow:     fmt.Sprintf("You have %s. Nothing changes until you install it.", or(firmware)),
			title:       fmt.Sprintf("RØDE firmware %s", or(newestFirmware)),
			entryPrefix: "Version",
			note:        "RØDE publishes the notes for this firmware on its website.",
			links:       []changeLink{{"RØDE's release notes", updates.RodeNotesURL}},
		}
	default:
		return
	}
	a.openChangesDialog(cx, spec)
}
