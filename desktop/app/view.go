package app

// The window's drawing: the current page, the sidebar, the footer and the
// notices.

import (
	"fmt"
	"slices"
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// channelNav is the channel list: processed channels first, each with its
// live line and effects; then the rest by kind; and the filter at the foot.
func (a *App) channelNav(cx *gk.Cx) gk.Element {
	editing := a.nav.channelInFront()
	sources := a.showSources()
	sourceOf := func(id string) *core.StockSource {
		for _, src := range sources {
			if src.TargetID == id {
				return src
			}
		}
		return nil
	}
	list := gk.Div(s.Flex1, s.MinH0).Px(px(12)).Pt(px(6)).Pb(px(12)).ID("target-scroll").OverflowYScroll()
	// Processed channels.
	processed := a.processedIndexes()
	count := len(processed)
	list = list.Child(navHeading("Processing", &count))
	for _, index := range processed {
		target := &a.session.Draft[index]
		selected := editing && a.inputs.selected == nil && index == a.selected
		if !selected && editing && a.inputs.selected != nil {
			if src := sourceOf(target.ID); src != nil && src.ID == *a.inputs.selected {
				selected = true
			}
		}
		list = list.Child(a.processedRow(index, selected, cx))
	}
	// The rest, by kind: show sources in the show's order, then the mixer's
	// other paths.
	groups := map[model.Group][]gk.Element{}
	for _, source := range sources {
		index := slices.IndexFunc(a.session.Draft, func(t core.Target) bool { return t.ID == source.TargetID })
		if index >= 0 && slices.Contains(processed, index) {
			continue
		}
		selected := editing && ((a.inputs.selected != nil && *a.inputs.selected == source.ID) ||
			(a.inputs.selected == nil && index >= 0 && index == a.selected))
		if source.Fader == nil {
			continue
		}
		group := model.GroupSources
		if index >= 0 && a.session.Draft[index].Kind == core.TargetKindMic {
			group = model.GroupMicrophones
		}
		id := source.ID
		groups[group] = append(groups[group],
			quietRow(gk.Key("source", int(source.SourceID)), model.DisplayName(source.Name),
				sourceDetail(source, ""), selected).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.inputs.selected = &id
					if index >= 0 {
						a.selected = index
					}
					a.band = 0
					a.channel = core.ChannelLeft
					a.importDraft = nil
					a.importError = nil
					a.nav.showChannel()
					a.rebuildEditors(cx)
					a.notify(cx)
				}))
	}
	for index := range a.session.Draft {
		target := &a.session.Draft[index]
		if slices.Contains(processed, index) || sourceOf(target.ID) != nil {
			continue
		}
		group := model.GroupOf(target)
		selected := editing && a.inputs.selected == nil && index == a.selected
		if group != model.GroupHeadphones && group != model.GroupOutputs {
			continue
		}
		groups[group] = append(groups[group],
			quietRow(gk.Key("target", index), a.targetName(target), "", selected).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openChannel(cx, index) }))
	}
	for _, group := range model.GroupAll {
		rows := groups[group]
		if len(rows) == 0 {
			continue
		}
		list = list.Child(navHeading(group.Label(), nil)).Children(rows...)
	}
	if a.inputs.error != nil {
		list = list.Child(gk.Div(s.Pt2).Px(px(12)).TextSize(px(12)).TextColor(rgb(Warn)).
			Child(txt("Last confirmed show · refresh unavailable: " + *a.inputs.error)))
	} else if a.inputs.inventory == nil && a.session.Connected {
		list = list.Child(gk.Div(s.Pt2).Px(px(12)).TextSize(px(12)).TextColor(rgb(Faint)).
			Child(txt("Loading show inputs…")))
	}
	overview := a.nav.homeInFront()
	settings := a.nav.settings()
	return gk.VFlex(s.FlexShrink0, s.HFull, s.BorderR1).W(px(SidebarWidth)).
		BorderColor(rgb(surface().Line)).
		Child(gk.Div().Px(px(12)).Pt(px(14)).
			Child(pageRow("overview", gk.IconLayoutDashboard, "Overview", overview).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openOverview(cx) }))).
		Child(list).
		Child(gk.Div(s.BorderT1).Px(px(12)).Py(px(10)).BorderColor(rgb(surface().Line)).
			Child(pageRow("settings", gk.IconSettings, "Settings", settings).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.setSettingsOpen(true)
					a.notify(cx)
				})))
}

// footer is the window's foot: a problem worth reading (the connection or
// its USB link, unsaved edits, an engine fault) or else how many channels
// are processed. Routine progress messages aren't shown; ok false when
// there's nothing.
func (a *App) footer() (gk.DivEl, bool) {
	status, color := a.status()
	var line string
	var lineColor uint32
	shown := true
	switch {
	case a.streamError != nil:
		line, lineColor = *a.streamError, Warn
	case a.usb.NeedsAttention():
		line, lineColor = a.usb.Label(), Warn
	case color != Accent && color != Muted:
		line, lineColor = status, color
	default:
		shown = false
	}
	row := gk.HFlex(s.ItemsCenter, s.BorderT1).Gap(px(8)).MinH(px(30)).Px(px(20)).Py(px(6)).
		TextSize(px(12.5)).BorderColor(rgb(surface().Line))
	if shown {
		return row.TextColor(rgb(lineColor)).
			Child(gk.Div(s.RoundedFull, s.FlexShrink0).Size(px(7)).Bg(rgb(lineColor))).
			Child(gk.Div(s.Flex1, s.MinW0, s.Truncate).Child(txt(line))), true
	}
	processed := len(a.processedIndexes())
	if processed == 0 {
		return gk.DivEl{}, false
	}
	plural := "s"
	if processed == 1 {
		plural = ""
	}
	return row.TextColor(rgb(Muted)).
		Child(gk.Div(s.RoundedFull, s.FlexShrink0).Size(px(7)).Bg(gradient())).
		Child(txt(fmt.Sprintf("%d channel%s processed", processed, plural))), true
}

// processedRow is a processed channel: its name, its first effects (or what
// is wrong), its live line, and a dot while it has edits not on the mixer.
func (a *App) processedRow(index int, selected bool, cx *gk.Cx) gk.StatefulDivEl {
	target := &a.session.Draft[index]
	var on []string
	for _, e := range visibleEffects(target) {
		if target.EffectEnabled(e) {
			on = append(on, shortEffect(e))
		}
	}
	var detail string
	problem := false
	if note, ok := a.pathNote(target.ID); ok {
		detail, problem = note, true
	} else if target.Bypass {
		detail = "Bypassed"
	} else if len(on) == 0 {
		detail = "No effects on"
	} else if len(on) > 2 {
		detail = fmt.Sprintf("%s +%d", strings.Join(on[:2], " · "), len(on)-2)
	} else {
		detail = strings.Join(on, " · ")
	}
	edited := false
	if saved := a.session.Saved; saved != nil {
		for _, t := range saved.Config.Targets {
			if t.ID == target.ID {
				edited = !t.Equal(*target)
				break
			}
		}
	}
	meter, _ := a.freshMeterFor(target.ID)
	fresh := meter != nil
	nameColor, detailColor := Muted, Faint
	if selected {
		nameColor = Text
	}
	if problem {
		detailColor = Warn
	}
	line := gk.Div(s.FlexShrink0).W(px(46)).H(px(18))
	if !a.reducedMotion {
		line = line.Child(a.indicator(linePath(target.ID), fresh, liveLineMini))
	}
	row := gk.HFlex(s.Relative, s.ItemsCenter, s.CursorPointer).Gap(px(11)).H(px(48)).Px(px(12)).
		Rounded(px(10)).TextSize(px(14)).TextColor(rgb(nameColor))
	if selected {
		row = row.Bg(white(0.07))
	} else {
		row = row.Hover(hover().Bg(white(0.035)).TextColor(rgb(Text)))
	}
	row = row.Child(dot(true, 7)).
		Child(gk.Div(s.Flex1, s.MinW0).
			Child(gk.Div(s.Truncate).Child(txt(a.targetName(target)))).
			Child(gk.Div(s.Truncate).Mt(px(1)).TextSize(px(11.5)).TextColor(rgb(detailColor)).Child(txt(detail)))).
		Child(line)
	if edited {
		row = row.Child(gk.Div(s.Absolute, s.RoundedFull).Right(px(5)).Top(px(21)).Size(px(6)).Bg(rgb(Text)))
	}
	return row.ID(gk.Key("processed", index)).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openChannel(cx, index) })
}

func (a *App) engineErrorCard(cx *gk.Cx) (gk.DivEl, bool) {
	if a.session.Saved == nil {
		return gk.DivEl{}, false
	}
	notice, ok := telemetry.EngineError(a.session.Saved)
	if !ok {
		return gk.DivEl{}, false
	}
	return card().BorderColor(rgb(Warn)).
		Child(gk.Div(s.FontSemibold).TextColor(rgb(Warn)).Child(txt(telemetry.EngineErrorNoticeTitle))).
		Child(gk.Div(s.TextSm).Child(txt(notice.Message))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).
			Child(txt("Last reported recovery status: " + notice.RecoveryDetail))).
		Child(gk.Div().Child(gk.Button("export-error-diagnostics").Label("Export diagnostics…").
			Disabled(a.diagnosticsPending || a.fileBusy || !a.session.Connected).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.requestDiagnostics(cx) }))), true
}

// conflictCard shows only when local edits can't be placed on the mixer's
// settings.
func (a *App) conflictCard(cx *gk.Cx) gk.DivEl {
	return card().BorderColor(rgb(Warn)).
		Child(gk.Div(s.FontSemibold).TextColor(rgb(Warn)).Child(txt("Your edits don't fit the mixer's settings"))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).
			Child(txt("They name channels the mixer no longer has. Reload the mixer's settings to continue editing."))).
		Child(gk.Div().Child(gk.Button("reload").Primary().Rounded(float32(18)).Px(px(16)).
			Label("Reload mixer settings").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.session.ReloadSaved()
				a.rebuildEditors(cx)
				a.notify(cx)
			})))
}

// navHeading is a sidebar heading, with a count when given.
func navHeading(label string, count *int) gk.DivEl {
	d := gk.HFlex(s.ItemsCenter, s.FontSemibold).Px(px(12)).Pt(px(18)).Pb(px(8)).TextSize(px(11)).
		TextColor(rgb(Faint)).Child(txt(strings.ToUpper(label))).Child(gk.Div(s.Flex1))
	if count != nil {
		d = d.Child(gk.Div(s.FontNormal).TextColor(rgb(Dim)).Child(txt(fmt.Sprint(*count))))
	}
	return d
}

// pageRow is a page in the sidebar (Overview, Settings): an icon and its
// name.
func pageRow(id string, icon gk.IconName, label string, selected bool) gk.StatefulDivEl {
	iconColor := Muted
	if selected {
		iconColor = Text
	}
	r := gk.HFlex(s.ItemsCenter, s.CursorPointer, s.FontMedium).Gap(px(11)).H(px(40)).Px(px(12)).
		Rounded(px(10)).TextSize(px(14)).TextColor(rgb(Text))
	if selected {
		r = r.Bg(white(0.07))
	} else {
		r = r.Hover(hover().Bg(white(0.035)))
	}
	return r.Child(gk.Icon(icon).Size(px(16)).TextColor(rgb(iconColor))).Child(txt(label)).ID(id)
}

// quietRow is an unprocessed channel in the sidebar: a hollow dot, its name
// and where it sits (its fader).
func quietRow(id gk.ID, name, meta string, selected bool) gk.StatefulDivEl {
	color := Muted
	if selected {
		color = Text
	}
	r := gk.HFlex(s.ItemsCenter, s.CursorPointer).Gap(px(11)).H(px(40)).Px(px(12)).Rounded(px(10)).
		TextSize(px(14)).TextColor(rgb(color))
	if selected {
		r = r.Bg(white(0.07))
	} else {
		r = r.Hover(hover().Bg(white(0.035)).TextColor(rgb(Text)))
	}
	return r.Child(dot(false, 7)).
		Child(gk.Div(s.Flex1, s.MinW0, s.Truncate).Child(txt(name))).
		Child(gk.Div(s.FlexShrink0).TextSize(px(12)).TextColor(rgb(Dim)).Child(txt(meta))).
		ID(id)
}

// view is the root view's function.
func (a *App) view(cx *gk.Cx) gk.Element { return a.renderPage(cx) }

func (a *App) renderPage(cx *gk.Cx) gk.Element {
	a.renderGeneration++
	// A guided task stays in front, connected or not, until it is
	// dismissed: the window never jumps away from it as the mixer
	// restarts.
	if a.installer != nil {
		return a.taskScreen(cx)
	}
	if !a.session.Connected {
		return a.connectionScreen(cx)
	}
	target := a.target().Clone()
	// A channel's editor fills the window to its foot; the other pages keep
	// some room below their last section.
	editorPage := a.nav.channelInFront() && a.unprocessedSource() == nil
	bottom := PageBottom
	if editorPage {
		bottom = EditorBottom
	}
	content := gk.VFlex(s.Gap4, s.Flex1, s.MinW0, s.HFull).Px(px(PageX)).Pt(px(PageTop)).Pb(px(bottom)).
		ID("content-scroll").TrackScroll(a.contentScroll).OverflowYScroll()
	// Anything that needs a decision comes before editing.
	if c, ok := a.engineErrorCard(cx); ok {
		content = content.Child(c)
	}
	content = content.Child(a.restartPanel(cx))
	if a.session.Conflict {
		content = content.Child(a.conflictCard(cx))
	}
	if c, ok := a.selectionNotice(cx); ok {
		content = content.Child(c)
	}
	if c, ok := a.mixerUpdateNotice(cx); ok {
		content = content.Child(c)
	}
	var rail gk.Element
	switch {
	case a.nav.settings():
		content = content.Child(a.settingsPage(cx))
	case a.nav.home():
		content = content.Child(a.homePage(cx))
	default:
		if source := a.unprocessedSource(); source != nil {
			content = content.Child(a.unprocessedPanel(source))
		} else {
			// Every channel shows its whole editor; the header's Processing
			// switch is what turns it on.
			// The rail is beside the page (below); the editor fills what is
			// left of the window, down to a height it still fits in.
			rail = a.chainRail(&target, cx)
			content = content.Child(gk.VFlex(s.Flex1, s.MinW0).MinH(px(EditorMinHeight)).Child(a.renderEditor(cx)))
		}
	}
	page := gk.Div(s.Relative, s.Flex1, s.MinW0, s.HFull).Child(content).
		// The page's width at the last prepaint, for the editor's narrow
		// layout (one frame late after a resize, as the graph is).
		OnChildrenPrepainted(func(cx *gk.Cx, children []gk.Bounds) {
			if len(children) == 0 || children[0].Size.Width-2*PageX == a.pageWidth {
				gk.Quiet(cx)
				return
			}
			a.pageWidth = children[0].Size.Width - 2*PageX
		})
	body := gk.HFlex(s.Flex1, s.MinH0).Child(a.channelNav(cx))
	if rail != nil {
		body = body.Child(rail)
	}
	body = body.Child(page)
	root := gk.VFlex(s.SizeFull).FontFamily("Inter").Bg(rgb(surface().Bg)).TextColor(rgb(Text)).
		TrackFocus(a.focus).
		OnKeyDown(func(cx *gk.Cx, e gk.KeyEvent) {
			away := !a.nav.homeInFront()
			// Ctrl+Z / Ctrl+Shift+Z (Cmd on macOS) undo and redo edits,
			// except while a text field has the keys.
			if e.Key == "z" && e.Modifiers&(gk.ModControl|gk.ModPlatform) != 0 && a.editingValue == nil {
				if e.Modifiers&gk.ModShift != 0 {
					a.redo(cx)
				} else {
					a.undo(cx)
				}
				return
			}
			if e.Key == "escape" && a.bandPopover != nil {
				// Esc closes the band's popover first.
				a.bandPopover = nil
				a.notify(cx)
				return
			}
			if e.Key == "escape" && away {
				a.openOverview(cx)
			}
		}).
		Child(body)
	if f, ok := a.footer(); ok {
		root = root.Child(f)
	}
	return root.ID("app")
}
