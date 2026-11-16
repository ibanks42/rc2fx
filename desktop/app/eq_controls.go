package app

// eq_controls.rs: the EQ page: its heading (stereo sides, EQ files, the
// switch), the graph, the band table with the overall gains, and the
// selected band's popover over the graph. The band helpers that do not draw
// are in edit/eq.go.

import (
	"fmt"
	"slices"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// EQ page sizes: the band table's and the band popover's width.
const (
	BandTableWidth float32 = 252
	PopoverWidth   float32 = 300
	// NarrowTableHeight is the table's height under the graph.
	NarrowTableHeight float32 = 230
)

// popoverBand is the band whose popover is open: which channel, which side
// and which band (by ID, so reordering keeps it).
type popoverBand struct {
	target  string
	channel core.Channel
	band    string
}

func (a *App) eqControls(cx *gk.Cx) gk.Element {
	page := gk.VFlex(s.Flex1, s.MinH0).Gap(px(14)).Child(a.eqHead(cx))
	if a.importError != nil {
		page = page.Child(gk.Div(s.P3, s.TextSm).Rounded(px(10)).Bg(rgb(DangerBg)).
			Child(txt(*a.importError)))
	}
	if a.importDraft != nil {
		page = page.Child(a.importPanel(a.importDraft, cx))
	}
	// The graph and the table fill the rest of the page: side by side, or
	// the table under the graph in a narrow window.
	if a.narrow() {
		return page.Child(gk.VFlex(s.Flex1, s.MinH0).Gap(px(18)).
			Child(gk.Div(s.Flex1, s.WFull).MinH(px(240)).Child(a.eqGraph(cx))).
			Child(a.bandTable(cx).W(gk.Relative(1)).H(px(NarrowTableHeight))))
	}
	return page.Child(gk.HFlex(s.Flex1, s.MinH0).Gap(px(28)).
		Child(gk.Div(s.Flex1, s.MinW0, s.HFull).Child(a.eqGraph(cx))).
		Child(a.bandTable(cx).W(px(BandTableWidth)).Style(s.HFull)))
}

// eqHead is the page's heading: its title (how to edit is its tooltip), the
// stereo sides, the EQ files menu and the switch.
func (a *App) eqHead(cx *gk.Cx) gk.Element {
	target := a.target()
	about := "Up to 32 bands. Drag a point, scroll on it for Q, double-click the graph to add a band, click a point to edit it."
	if target.Channels == 2 {
		about = "Up to 32 bands, on both sides or left and right separately. Drag a point, scroll on it for Q, double-click the graph to add a band, click a point to edit it."
	}
	menu := gk.NewPopupMenu()
	for _, item := range []struct {
		label string
		open  func(cx *gk.Cx)
	}{{"Import EQ file…", a.importEQ}, {"Paste EQ", a.pasteEQ}, {"Export EQ file…", a.exportEQ}} {
		open := item.open
		menu = menu.Item(gk.NewPopupMenuItem(item.label).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if a.fileBusy {
				gk.Quiet(cx)
				return
			}
			open(cx)
		}))
	}
	state := "Off"
	if target.EQ.Enabled {
		state = "On"
	}
	head := gk.HFlex(s.ItemsCenter, s.Gap3).H(px(36)).
		Child(gk.Div(s.FontSemibold, s.WhitespaceNowrap).TextSize(px(20)).Child(txt(core.EffectEQ.Label())).
			ID("eq-about").Tooltip(gk.NewKitTooltip(about)))
	if target.Channels == 2 {
		head = head.Child(gk.Div().Ml(px(8)).Child(a.eqSides(cx)))
	}
	return head.
		Child(gk.Div(s.Flex1)).
		Child(gk.DropdownMenu(gk.Button("eq-files").Ghost().Small().Label("EQ files").DropdownCaret(true).
			Disabled(a.fileBusy), menu)).
		Child(gk.Div().Ml(px(6)).TextSize(px(13.5)).TextColor(rgb(Muted)).Child(txt(state))).
		Child(a.effectSwitch(target, core.EffectEQ))
}

// eqSides is both sides (one EQ for left and right), or each side on its
// own. Rejoining two different sides keeps one of them, so it asks which.
func (a *App) eqSides(cx *gk.Cx) gk.Element {
	linked := a.target().EQ.RightBands == nil
	side := func(id, label string, channel core.Channel) gk.StatefulDivEl {
		return pill(id, label, !linked && a.channel == channel).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			model.Split(a.targetMut())
			a.channel = channel
			a.band = 0
			a.rebuildEditors(cx)
			a.notify(cx)
		})
	}
	var both gk.Element
	if linked {
		both = pill("eq-both", "Both sides", true)
	} else {
		menu := gk.NewPopupMenu()
		for _, choice := range []struct {
			keep  core.Channel
			label string
		}{
			{core.ChannelLeft, "Use the left side for both"},
			{core.ChannelRight, "Use the right side for both"},
		} {
			keep := choice.keep
			menu = menu.Item(gk.NewPopupMenuItem(choice.label).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				model.Link(a.targetMut(), keep)
				a.channel = core.ChannelLeft
				a.band = 0
				a.rebuildEditors(cx)
				a.notify(cx)
			}))
		}
		both = gk.DropdownMenu(gk.Button("eq-both").Ghost().Xsmall().Label("Both sides").DropdownCaret(true), menu)
	}
	return pills().
		Child(both).
		Child(side("eq-left", "Left", core.ChannelLeft)).
		Child(side("eq-right", "Right", core.ChannelRight))
}

// bandTable is every band as a row (number, type, frequency, gain and Q),
// then the overall gains and the add button. Click a value to type it, a
// row's number to turn the band on or off, its type to open its popover;
// drag a row onto another to reorder.
func (a *App) bandTable(cx *gk.Cx) gk.DivEl {
	bands := a.target().EQ.Channel(a.channel)
	off := !a.target().EQ.Enabled
	heading := func(label string) gk.DivEl {
		return gk.Div(s.Flex1, s.MinW0, s.TextRight).FlexBasis(px(0)).Child(txt(label))
	}
	header := bandColumns(gk.HFlex(s.ItemsCenter, s.FontSemibold)).H(px(26)).TextSize(px(10.5)).
		TextColor(rgb(Faint)).
		Child(gk.Div().W(px(22)).Child(txt("#"))).
		Child(gk.Div().W(px(18))).
		Child(heading("FREQ")).
		Child(heading("GAIN")).
		Child(gk.Div(s.TextRight).W(px(42)).Child(txt("Q")))
	list := gk.Div(s.Flex1, s.MinH0).Pt(px(2)).ID("band-rows").OverflowYScroll()
	for index, band := range bands {
		list = list.Child(a.bandRow(index, band))
	}
	if len(bands) == 0 {
		list = list.Child(gk.Div().Px(px(10)).Py(px(8)).TextSize(px(12.5)).LineHeight(gk.Relative(1.45)).
			TextColor(rgb(Faint)).Child(txt("No bands yet. Double-click the graph to add one where you click.")))
	}
	full := len(bands) >= core.MaxBands
	add := gk.Div(s.WhitespaceNowrap).TextSize(px(13)).TextColor(rgb(Muted))
	label := "+ Add band"
	if full {
		label = "32 bands, the most"
		add = add.Opacity(0.5)
	} else {
		add = add.Style(s.CursorPointer).Hover(hover().TextColor(rgb(Text)))
	}
	foot := gk.VFlex(s.BorderT1).Gap(px(8)).Px(px(10)).Pt(px(12)).BorderColor(rgb(surface().Line)).
		Child(a.overallGain("Input gain", edit.ParameterEQInput, off)).
		Child(a.overallGain("Output gain", edit.ParameterEQOutput, off)).
		Child(add.Mt(px(4)).Child(txt(label)).ID("add-band").OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			if full {
				gk.Quiet(cx)
				return
			}
			a.addBand(cx, nil)
		}))
	return gk.VFlex(s.FlexShrink0).Child(header).Child(list).Child(foot)
}

// bandColumns is the table's row layout, shared by its heading and rows.
func bandColumns(row gk.DivEl) gk.DivEl { return row.Gap(px(8)).Px(px(10)) }

// bandRow is one band in the table. The selected band's row is tinted, a
// band that is off has a hollow number, and the value being typed shows its
// text field.
func (a *App) bandRow(index int, band core.Band) gk.Element {
	selected := index == a.band
	row := bandColumns(gk.HFlex(s.ItemsCenter, s.CursorPointer)).H(px(30)).Rounded(px(7)).TextSize(px(13))
	if selected {
		row = row.Bg(white(0.05))
	} else {
		row = row.Hover(hover().Bg(white(0.025)))
	}
	used := edit.Uses(band.BandType)
	text, quiet := Text, Muted
	if !band.Enabled {
		text, quiet = Faint, Dim
	}
	value := func(parameter Parameter, shown string, color uint32) gk.Element {
		cell := gk.Div(s.Flex1, s.MinW0).FlexBasis(px(0))
		if parameter == edit.ParameterQ {
			cell = gk.Div(s.FlexShrink0).W(px(42))
		}
		if !slices.Contains(used, parameter) {
			return cell.Style(s.TextRight).TextColor(rgb(Dim)).Child(txt("–"))
		}
		if selected && a.editingValue != nil && *a.editingValue == parameter {
			if editor := a.editorFor(parameter); editor != nil {
				return cell.Child(gk.Div(s.Border1).Px(px(4)).Rounded(px(5)).Bg(rgb(0x0b0e16)).
					BorderColor(rgb(Accent)).Child(gk.Input(editor.input).Appearance(false)))
			}
		}
		return cell.Child(gk.Div(s.TextRight, s.CursorText, s.WhitespaceNowrap).Px(px(4)).Mx(px(-4)).Rounded(px(5)).
			TextColor(rgb(color)).Hover(hover().Bg(white(0.06))).
			Child(txt(shown)).
			ID(gk.Key(fmt.Sprintf("band-value-%d", int(parameter)), index)).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				gk.StopPropagation(cx)
				a.selectBand(cx, index)
				a.editValue(cx, parameter)
			}))
	}
	iconColor := Muted
	if selected {
		iconColor = Text
	}
	onTip := "Turn this band off"
	if !band.Enabled {
		onTip = "Turn this band on"
	}
	number := gk.Div(s.FlexShrink0, s.CursorPointer).W(px(22)).Child(bandBadge(index, 18, band.Enabled)).
		ID(gk.Key("band-number", index)).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			gk.StopPropagation(cx)
			bands := a.targetMut().EQ.ChannelMut(a.channel)
			if index < len(*bands) {
				(*bands)[index].Enabled = !(*bands)[index].Enabled
			}
			a.notify(cx)
		}).
		Tooltip(gk.NewKitTooltip(onTip))
	kind := gk.Div(s.FlexShrink0, s.CursorPointer).W(px(18)).Child(typeIcon(band.BandType, iconColor)).
		ID(gk.Key("band-type-icon", index)).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			gk.StopPropagation(cx)
			a.openBandPopover(cx, index)
		}).
		Tooltip(gk.NewKitTooltip(band.BandType.Label() + ": click to edit the band"))
	row = row.
		Child(number).
		Child(kind).
		Child(value(edit.ParameterFrequency, edit.ShortHz(band.Frequency), text)).
		Child(value(edit.ParameterBandGain, edit.ShortDB(band.Gain), text)).
		Child(value(edit.ParameterQ, fmt.Sprintf("%.2f", band.Q), quiet))
	stateful := row.ID(gk.Key("band", index)).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
		a.selectBand(cx, index)
		a.notify(cx)
	})
	// The previews are made with the editors (rebuildBandPreviews), one per
	// possible band.
	if index < len(a.bandPreviews) {
		stateful = stateful.OnDrag(gk.DragValue{ID: dragID(dragBand, index), Text: bandSummary(&band)},
			a.bandPreviews[index])
	}
	return stateful.
		CanDrop(gk.DropAccept{IDs: dragIDs(dragBand, core.MaxBands)}).
		DragOver(hover().Bg(white(0.08))).
		OnDrop(func(cx *gk.Cx, d gk.DragValue) {
			kind, from := dragKind(d.ID)
			if kind != dragBand {
				gk.Quiet(cx)
				return
			}
			channel := a.channel
			var selectedID *string
			if current := a.target().EQ.Channel(channel); a.band >= 0 && a.band < len(current) {
				id := current[a.band].ID
				selectedID = &id
			}
			model.PlaceBand(&a.targetMut().EQ, channel, from, index)
			a.band = edit.SelectBandAfterMove(a.target().EQ.Channel(channel), selectedID, a.band)
			a.rebuildEditors(cx)
			a.notify(cx)
		})
}

// overallGain is one of the EQ's overall gains in the table's foot: click
// to type it, scroll on it to step it.
func (a *App) overallGain(label string, parameter Parameter, off bool) gk.Element {
	editor := a.editorFor(parameter)
	value := parameter.Get(a.target(), a.channel, a.band)
	var view gk.Element
	if editor != nil && a.editingValue != nil && *a.editingValue == parameter {
		view = gk.Div(s.Border1).W(px(96)).Px(px(4)).Rounded(px(5)).Bg(rgb(0x0b0e16)).BorderColor(rgb(Accent)).
			Child(gk.Input(editor.input).Appearance(false))
	} else {
		color := Text
		if off {
			color = Muted
		}
		view = gk.Div(s.CursorText, s.WhitespaceNowrap, s.FontMedium).Px(px(4)).Mx(px(-4)).Rounded(px(5)).
			TextColor(rgb(color)).Hover(hover().Bg(white(0.06))).
			Child(txt(parameter.Display(value))).
			ID(gk.Key("overall", int(parameter))).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.editValue(cx, parameter) }).
			OnScroll(func(cx *gk.Cx, e gk.ScrollEvent) {
				var delta float32
				switch d := e.Delta.(type) {
				case gk.ScrollDeltaPixels:
					delta = d.Y
				case gk.ScrollDeltaLines:
					delta = d.Y * 16
				}
				if delta == 0 || editor == nil {
					gk.Quiet(cx)
					return
				}
				step := 0.5
				if delta < 0 {
					step = -0.5
				}
				next := min(max(parameter.Get(a.target(), a.channel, a.band)+step, editor.min), editor.max)
				parameter.Set(a.targetMut(), a.channel, a.band, next)
				a.syncEditorValues(cx)
				a.notify(cx)
			}).
			Tooltip(gk.NewKitTooltip("Click to type, or scroll to change by 0.5 dB."))
	}
	return gk.HFlex(s.ItemsCenter, s.JustifyBetween).TextSize(px(13)).TextColor(rgb(Muted)).
		Child(txt(label)).Child(view)
}

// selectBand makes band index the selected one.
func (a *App) selectBand(cx *gk.Cx, index int) {
	if a.band != index {
		a.band = index
		a.rebuildEditors(cx)
	}
}

// openBandPopover selects band index and opens its popover over the graph.
func (a *App) openBandPopover(cx *gk.Cx, index int) {
	a.selectBand(cx, index)
	bands := a.target().EQ.Channel(a.channel)
	if index < 0 || index >= len(bands) {
		gk.Quiet(cx)
		return
	}
	a.bandPopover = &popoverBand{target: a.target().ID, channel: a.channel, band: bands[index].ID}
	a.notify(cx)
}

// popoverIndex is the band whose popover is open, if it is still the
// selected band of the channel and side on screen.
func (a *App) popoverIndex() (int, bool) {
	p := a.bandPopover
	if p == nil || a.effect != core.EffectEQ || p.target != a.target().ID || p.channel != a.channel {
		return 0, false
	}
	bands := a.target().EQ.Channel(a.channel)
	if a.band < 0 || a.band >= len(bands) || bands[a.band].ID != p.band {
		return 0, false
	}
	return a.band, true
}

// addBand adds a bell, at a point on the graph when given (frequency,
// gain).
func (a *App) addBand(cx *gk.Cx, at *[2]float64) {
	channel := a.channel
	band, err := a.targetMut().EQ.AddBand(channel)
	if err != nil {
		a.message = err.Error()
	} else if at != nil {
		band.Frequency = min(max(at[0], 10), 24000)
		band.Gain = min(max(at[1], -36), 36)
	}
	a.band = max(len(a.target().EQ.Channel(channel))-1, 0)
	a.rebuildEditors(cx)
	a.notify(cx)
}

// selectedBand is the selected band of the active channel, nil when there
// is none.
func (a *App) selectedBand() *core.Band {
	bands := a.targetMut().EQ.ChannelMut(a.channel)
	if a.band < 0 || a.band >= len(*bands) {
		return nil
	}
	return &(*bands)[a.band]
}

// bandPopoverPanel is the selected band's editor, over the graph beside its
// point: its type and filter design, switches, the settings that type uses,
// and removal. A press outside it closes it, as Esc does.
func (a *App) bandPopoverPanel(band core.Band, cx *gk.Cx) gk.DivEl {
	kind := band.BandType
	types := pills()
	for index, option := range edit.Types {
		on := option == kind
		iconColor := Muted
		if on {
			iconColor = Text
		}
		p := gk.HFlex(s.ItemsCenter, s.JustifyCenter, s.CursorPointer).W(px(30)).H(px(26)).Rounded(px(7)).
			Child(typeIcon(option, iconColor))
		if on {
			p = p.Bg(white(0.09))
		} else {
			p = p.Hover(hover().Bg(white(0.04)))
		}
		types = types.Child(p.ID(gk.Key("band-type", index)).
			Tooltip(gk.NewKitTooltip(option.Label())).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if b := a.selectedBand(); b != nil {
					b.BandType = option
				}
				a.rebuildEditors(cx)
				a.notify(cx)
			}))
	}
	toggleRow := func(id, label string, on bool, set func(b *core.Band, v bool)) gk.DivEl {
		return gk.HFlex(s.ItemsCenter, s.Gap2).TextSize(px(13)).TextColor(rgb(Muted)).
			Child(toggle(id, on, true, false).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if b := a.selectedBand(); b != nil {
					set(b, !on)
				}
				a.notify(cx)
			})).
			Child(txt(label))
	}
	design := pills()
	for index, choice := range []struct {
		mode  core.BandMode
		label string
	}{{core.BandModeApoDr, "APO"}, {core.BandModeRlcBt, "RLC"}} {
		mode := choice.mode
		design = design.Child(pill(gk.Key("band-mode", index), choice.label, mode == band.Mode).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if b := a.selectedBand(); b != nil {
					model.SetModel(b, mode)
				}
				a.rebuildEditors(cx)
				a.notify(cx)
			}))
	}
	used := edit.Uses(kind)
	id := band.ID
	label := kind.Label()
	if kind == core.BandTypeOff {
		label = "Off: passes audio through unchanged"
	}
	panel := gk.VFlex(s.Border1, s.ShadowLg).W(px(PopoverWidth)).P(px(16)).Gap(px(14)).Rounded(px(14)).
		Bg(rgb(surface().Raised)).BorderColor(rgb(surface().LineStrong)).
		Child(gk.HFlex(s.ItemsCenter, s.Gap3).
			Child(bandNumber(a.band, 24)).
			Child(gk.Div(s.Flex1, s.MinW0, s.Truncate, s.FontMedium).TextSize(px(13.5)).Child(txt(label))).
			Child(gk.Div(s.CursorPointer).ID("close-band-popover").
				Child(gk.Icon(gk.IconClose).Size(px(14)).TextColor(rgb(Faint))).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.bandPopover = nil
					a.notify(cx)
				}))).
		Child(types).
		Child(gk.HFlex(s.ItemsCenter, s.Gap3).
			Child(gk.Div().ID("band-mode").Child(design).Tooltip(gk.NewKitTooltip(
				"Filter design: how Q becomes the filter's shape. APO matches Equalizer APO (DR), RLC matches analog-style (BT). Imported EQ files set this for you."))).
			Child(gk.Div(s.Flex1)).
			Child(toggleRow("band-enabled", "On", band.Enabled, func(b *core.Band, v bool) { b.Enabled = v })).
			Child(toggleRow("band-solo", "Solo", band.Solo, func(b *core.Band, v bool) { b.Solo = v }))).
		Child(gk.Div().Mb(px(-24)).
			Child(a.controls(func(p Parameter) bool { return slices.Contains(used, p) }, 1, !band.Enabled, cx))).
		Child(gk.HFlex(s.ItemsCenter).
			Child(gk.Div(s.Flex1)).
			Child(gk.Div(s.CursorPointer).TextSize(px(13)).TextColor(rgb(0xc98a8a)).
				Hover(hover().TextColor(rgb(Danger))).
				Child(txt("Remove band")).
				ID("remove-band").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.bandPopover = nil
					a.targetMut().EQ.RemoveBand(a.channel, id)
					a.rebuildEditors(cx)
					a.notify(cx)
				})))
	return panel.Occlude().
		OnMouseDownOut(func(cx *gk.Cx, _ gk.MouseEvent) {
			// The type and design tooltips and the slider drags stay inside;
			// any other press closes it.
			if a.bandPopover == nil {
				gk.Quiet(cx)
				return
			}
			a.bandPopover = nil
			a.notify(cx)
		})
}

// bandNumber is a band's number on its colour.
func bandNumber(index int, size float32) gk.DivEl {
	return gk.HFlex(s.FlexShrink0, s.RoundedFull, s.ItemsCenter, s.JustifyCenter, s.FontSemibold).Size(px(size)).
		Bg(rgb(bandColor(index))).TextSize(px(size * 0.48)).TextColor(rgb(0x0b0d14)).
		Child(txt(fmt.Sprint(index + 1)))
}

// bandBadge is bandNumber for a band that may be off: a hollow ring in its
// colour when off.
func bandBadge(index int, size float32, on bool) gk.DivEl {
	if on {
		return bandNumber(index, size)
	}
	return gk.HFlex(s.FlexShrink0, s.RoundedFull, s.Border1, s.ItemsCenter, s.JustifyCenter, s.FontSemibold).
		Size(px(size)).BorderColor(rgb(bandColor(index))).TextSize(px(size * 0.48)).TextColor(rgb(bandColor(index))).
		Child(txt(fmt.Sprint(index + 1)))
}

// typeIcon is the filter type's shape, as a small line.
func typeIcon(kind core.BandType, color uint32) gk.Element {
	return gk.Div(s.FlexShrink0).W(px(18)).H(px(14)).
		Child(gk.Plot(gk.NewPlotLine(edit.TypeIconPoints(kind, 18, 14), 1.5, rgb(color))))
}

// bandSummary is a dragged band's preview text: its type and frequency.
func bandSummary(band *core.Band) string {
	return fmt.Sprintf("%s · %s", band.BandType.Label(), formatHz(band.Frequency))
}

// bandPreview is DraggedBand's Render: the band chip under the pointer
// while band index is dragged (a view, as a drag preview is). It reads the band when drawn; rebuildBandPreviews and
// notifyBandPreview mark it when the band changes.
func (a *App) bandPreview(index int) gk.Element {
	summary := ""
	if bands := a.target().EQ.Channel(a.channel); index < len(bands) {
		summary = bandSummary(&bands[index])
	}
	return gk.HFlex(s.ItemsCenter, s.Gap2, s.RoundedFull, s.Border1).Pl(px(6)).Pr(px(12)).H(px(34)).
		Bg(rgb(surface().Raised)).BorderColor(rgb(Accent)).TextSize(px(12.5)).TextColor(rgb(Text)).
		Child(bandNumber(index, 22)).
		Child(txt(summary))
}

// rebuildBandPreviews makes the band chips' drag previews once (one view
// per possible band; a view function may not make views) and marks them
// all to be drawn again.
func (a *App) rebuildBandPreviews(cx *gk.Cx) {
	if a.bandPreviews == nil {
		a.bandPreviews = make([]gk.View, core.MaxBands)
		for i := range a.bandPreviews {
			a.bandPreviews[i] = gk.NewView(nil, func(cx *gk.Cx) gk.Element { return a.bandPreview(i) })
		}
		return
	}
	for _, v := range a.bandPreviews {
		gk.Notify(cx, v)
	}
}

// notifyBandPreview marks band index's drag preview after its settings
// changed.
func (a *App) notifyBandPreview(cx *gk.Cx, index int) {
	if index >= 0 && index < len(a.bandPreviews) {
		gk.Notify(cx, a.bandPreviews[index])
	}
}
