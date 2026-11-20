package app

import (
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"
)

const settingsColumn float32 = 760

func settingsSection(title string, aside gk.Element, body gk.DivEl) gk.DivEl {
	head := gk.HFlex(s.ItemsCenter, s.JustifyBetween).MinH(px(28)).Mb(px(6)).
		Child(gk.Div(s.FontSemibold).TextSize(px(12)).TextColor(rgb(Faint)).Child(txt(strings.ToUpper(title))))
	if aside != nil {
		head = head.Child(aside)
	} else {
		head = head.Child(gk.Div())
	}
	return gk.VFlex().Child(head).Child(body.Style(s.VFlex))
}

func row(label, controls gk.Element) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.BorderB1).Gap(px(24)).MinH(px(56)).Py(px(16)).
		BorderColor(rgb(surface().Line)).
		Child(gk.Div(s.Flex1, s.MinW0, s.FontMedium).TextSize(px(15)).Child(label)).
		Child(gk.Div(s.FlexShrink0).Child(controls))
}

func labeled(title, detail string) gk.DivEl {
	d := gk.VFlex().Gap(px(3)).Child(txt(title))
	if detail != "" {
		d = d.Child(gk.Div(s.FontNormal).TextSize(px(13.5)).LineHeight(gk.Relative(1.45)).
			TextColor(rgb(Muted)).Child(txt(detail)))
	}
	return d
}

func ghost(id gk.ID, label string) gk.ButtonEl {
	return gk.Button(id).Ghost().H(px(38)).Px(px(16)).Rounded(float32(19)).Style(s.Border1).
		BorderColor(rgb(surface().LineStrong)).Label(label)
}

// setSettingsOpen opens Settings (refreshing presets and SSH status on entry)
// or closes the active overlay.
func (a *App) setSettingsOpen(open bool) {
	if !open {
		a.nav.CloseOverlay()
		return
	}
	if a.nav.ShowSettings() {
		a.listPresets()
		a.readSSH()
	}
}

func (a *App) mixerSection(cx *gk.Cx) gk.DivEl {
	connected := "Not connected"
	if a.session.Connected {
		connected = "Connected"
	}
	rows := gk.Div().
		Child(row(labeled("RØDECaster", connected),
			ghost("disconnect", "Disconnect").Disabled(!a.session.Connected && !a.link.Connecting()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.disconnect(cx) }))).
		Child(row(labeled("RC2FX on this mixer",
			"Update RC2FX to a newer version, or remove it and put RØDE’s stock firmware back. Your settings are saved first."),
			gk.HFlex(s.Gap2).
				Child(ghost("update-rc2fx", "Update RC2FX").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openTask(cx, taskUpdate) })).
				Child(ghost("remove-rc2fx", "Remove RC2FX").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openTask(cx, taskRemove) }))))
	if processing, ok := a.processingRow(cx); ok {
		rows = rows.Child(processing)
	}
	return settingsSection("Mixer", nil, rows)
}

// showDiagnostics toggles the Diagnostics subpage and resets the shared scroll container.
func (a *App) showDiagnostics(cx *gk.Cx, open bool) {
	a.nav.SetDiagnostics(open)
	a.contentScroll.SetOffset(cx, gk.GpuiPoint{})
}

func (a *App) aboutSection(cx *gk.Cx) gk.DivEl {
	rows := gk.Div().Child(row(
		labeled("Licenses", "RC2FX is MIT licensed. Third-party notices and credits are included."),
		ghost("open-licenses", "View licenses").OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { openLicenses(cx) })))
	return settingsSection("About", nil, rows)
}

func (a *App) filesSection(cx *gk.Cx) gk.DivEl {
	return settingsSection("Files", nil, gk.Div().Child(
		row(labeled("Settings file", "Open a settings file into your draft, or save this draft to share or keep."),
			gk.HFlex(s.Gap2).
				Child(ghost("open-settings-file", "Open…").
					Disabled(a.fileBusy || a.pending != nil || a.presets.writing() || a.link.Connecting() || a.transitionBusy()).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openDraft(cx) })).
				Child(ghost("save-settings-file", "Save…").Disabled(a.fileBusy).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.saveDraft(cx) }))).
			Style(s.BorderB0)))
}

func (a *App) appearanceSection(cx *gk.Cx) gk.DivEl {
	current := appearance()
	choices := gk.HFlex(s.Gap2)
	for index, ap := range AppearanceAll {
		note := "Near black"
		switch ap {
		case AppearanceDusk:
			note = "Deep blue-gray · default"
		case AppearanceGraphite:
			note = "Neutral dark gray"
		}
		choices = choices.Child(choice(gk.Key("appearance", index), ap.Label(), note, ap == current).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				setAppearance(cx, ap)
				rememberAppearance(ap)
				// Refresh all windows so the events window picks up the new theme too.
				gk.Refresh(cx)
			}))
	}
	return settingsSection("Appearance", nil, gk.Div(s.BorderB1).Py(px(16)).BorderColor(rgb(surface().Line)).
		Child(labeled("Colors", "").TextSize(px(15)).Style(s.FontMedium).Mb(px(14))).
		Child(choices))
}

func (a *App) diagnosticsTools(cx *gk.Cx) gk.DivEl {
	return settingsSection("Troubleshooting", nil, gk.Div().
		Child(row(labeled("Events", "What the mixer reported: overruns, path changes and recoveries, newest first. Opens in its own window."),
			ghost("open-events", "Open events").OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openEvents(cx) }))).
		Child(row(labeled("Show telemetry", "Levels, delay, counters and timing for every processed path, shown below."),
			toggle("show-telemetry", a.telemetryOpen, false, false).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.telemetryOpen = !a.telemetryOpen
				rememberTelemetry(a.telemetryOpen)
				a.notify(cx)
			}))).
		Child(row(labeled("Diagnostics file", "The mixer's state and recent timing history, for reporting a problem."),
			ghost("export-diagnostics", "Export diagnostics…").
				Disabled(a.diagnosticsPending || a.fileBusy || !a.session.Connected).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.requestDiagnostics(cx) })).
			Style(s.BorderB0)))
}

func (a *App) diagnosticsEntry(cx *gk.Cx) gk.DivEl {
	return settingsSection("Diagnostics", nil, gk.Div().Child(row(labeled("Diagnostics", ""),
		ghost("open-diagnostics", "Open").OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.showDiagnostics(cx, true)
			a.notify(cx)
		}))))
}

func (a *App) diagnosticsPage(cx *gk.Cx) gk.DivEl {
	page := gk.VFlex(s.WFull, s.MxAuto, s.Pb6).Gap(px(40)).MaxW(px(settingsColumn)).Pt(px(18)).
		Child(gk.VFlex().Gap(px(10)).
			Child(gk.Div(s.FlexShrink0, s.CursorPointer).TextSize(px(13.5)).TextColor(rgb(Muted)).
				Hover(hover().TextColor(rgb(Text))).Child(txt("‹ Settings")).
				ID("diagnostics-back").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.showDiagnostics(cx, false)
					a.notify(cx)
				})).
			Child(gk.Div(s.FontBold).TextSize(px(TitleText)).LineHeight(gk.Relative(1)).Child(txt("Diagnostics")))).
		Child(a.diagnosticsTools(cx))
	if a.telemetryOpen {
		mono := gk.KitTheme(cx).MonoFontFamily(cx)
		snapshot := a.telemetrySnapshot()
		page = page.Child(telemetrySection(&snapshot, mono, a.telemetryAside(cx),
			gk.Switch("telemetry-details").Small().Checked(a.telemetryDetails).Label("Technical details").
				OnClick(func(cx *gk.Cx, on bool) {
					a.telemetryDetails = on
					a.notify(cx)
				})))
	}
	return page
}

func (a *App) settingsPage(cx *gk.Cx) gk.Element {
	if a.nav.Diagnostics() {
		return a.diagnosticsPage(cx)
	}
	return gk.VFlex(s.WFull, s.MxAuto, s.Pb6).Gap(px(40)).MaxW(px(settingsColumn)).Pt(px(18)).
		Child(gk.Div(s.FontBold).TextSize(px(TitleText)).LineHeight(gk.Relative(1)).Child(txt("Settings"))).
		Child(a.mixerSection(cx)).
		Child(a.versionsSection(cx)).
		Child(a.aboutSection(cx)).
		Child(a.presetPanel(cx)).
		Child(a.filesSection(cx)).
		Child(a.appearanceSection(cx)).
		Child(a.developerSection(cx)).
		Child(a.diagnosticsEntry(cx))
}
