package app

// Separate window displaying the mixer's recent diagnostic audio events.
// The main window owns the connection and EventLog; this window just renders it.

import (
	"fmt"
	"slices"
	"strconv"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

type EventLog = telemetry.EventLog

func newEventLog() EventLog { return telemetry.NewEventLog() }

func kindColor(kind core.DiagnosticsKind) uint32 {
	switch telemetry.KindToneOf(kind) {
	case telemetry.KindToneDanger:
		return Danger
	case telemetry.KindToneMuted:
		return Muted
	}
	return Warn
}

func kindChip(kind core.DiagnosticsKind) gk.DivEl {
	color := kindColor(kind)
	bg := surface().Raised
	if color == Danger {
		bg = DangerBg
	}
	return gk.Div(s.Px2, s.RoundedMd, s.TextXs, s.Truncate).Py(px(2)).Bg(rgb(bg)).TextColor(rgb(color)).
		Child(txt(telemetry.KindLabel(kind)))
}

func resetControl(id, label string, confirming, connected bool, ask, cancel, reset func(cx *gk.Cx)) gk.DivEl {
	if confirming {
		return gk.HFlex(s.ItemsCenter, s.Gap2).
			Child(gk.Div(s.TextXs).TextColor(rgb(Muted)).Child(txt("Reset counters and events on the mixer?"))).
			Child(gk.Button(gk.Key(id, 0)).Outline().Small().Label("Cancel").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { cancel(cx) })).
			Child(gk.Button(gk.Key(id, 1)).Danger().Small().Label("Reset").Disabled(!connected).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { reset(cx) }))
	}
	return gk.Div().Child(gk.Button(gk.Key(id, 2)).Danger().Outline().Small().Icon(gk.IconUndo2).Label(label).
		Disabled(!connected).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { ask(cx) }))
}

type eventsView struct {
	log   *gk.Model[EventLog]
	app   *App // nil in preview builds
	view  gk.View
	ticks gk.Timer
}

func (v *eventsView) init(cx *gk.Cx) {
	// Tick every second so relative timestamps ("12 s ago") stay fresh.
	v.ticks = gk.Every(cx, time.Second, func(cx *gk.Cx) { gk.Notify(cx, v.view) })
}

func (v *eventsView) header(log *EventLog) gk.DivEl {
	setConfirm := func(value bool) func(cx *gk.Cx) {
		return func(cx *gk.Cx) { v.log.Update(cx, func(l *EventLog) { l.ConfirmReset = value }) }
	}
	reset := func(cx *gk.Cx) {
		if v.app != nil {
			v.app.resetDiagnostics(cx)
		}
	}
	return gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.Gap4).
		Child(gk.VFlex(s.Gap1).
			Child(gk.Div(s.Text2xl, s.FontSemibold).Child(txt("Events"))).
			Child(gk.Div(s.TextXs).TextColor(rgb(Muted)).Child(txt(fmt.Sprintf(
				"%s · newest first · the last %d kept", telemetry.SinceLabel(log.Since, log.Epoch), telemetry.Capacity))))).
		Child(resetControl("events-reset", "Reset diagnostics…", log.ConfirmReset,
			log.Connected && v.app != nil, setConfirm(true), setConfirm(false), reset))
}

func (v *eventsView) filters(log *EventLog) gk.DivEl {
	chips := gk.HFlex(s.FlexWrap, s.ItemsCenter, s.Gap2)
	for index, group := range telemetry.GroupAll {
		shown := !log.Hidden[group]
		count := log.Count(group)
		bg, color, countColor := surface().Bg, Faint, Faint
		if shown {
			bg, color = surface().Selected, Text
			if count > 0 {
				countColor = Text
			}
		}
		chips = chips.Child(gk.HFlex(s.ItemsCenter, s.Gap2, s.Px3, s.Py1, s.RoundedFull, s.CursorPointer, s.TextXs).
			Bg(rgb(bg)).TextColor(rgb(color)).
			Child(txt(group.Label())).
			Child(gk.Div().TextColor(rgb(countColor)).Child(txt(strconv.Itoa(count)))).
			ID(gk.Key("event-filter", index)).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				v.log.Update(cx, func(l *EventLog) { l.ToggleGroup(group) })
			}))
	}
	startup := log.StartupCount()
	return chips.Child(gk.Div(s.Flex1)).Child(gk.Switch("event-startup").Small().Checked(log.ShowStartup).
		Label(fmt.Sprintf("Startup events (%d)", startup)).
		Tooltip("Events from the engine's first 30 seconds of audio. Bursts while it starts are expected.").
		OnClick(func(cx *gk.Cx, on bool) {
			v.log.Update(cx, func(l *EventLog) { l.ShowStartup = on })
		}))
}

func timeCell(at, now time.Time, mono string) gk.DivEl {
	return gk.VFlex(s.FlexShrink0).W(px(132)).
		Child(gk.Div(s.TextXs).FontFamily(mono).Child(txt(telemetry.Clock(at)))).
		Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(telemetry.Ago(at, now))))
}

func (v *eventsView) table(log *EventLog, mono string) gk.DivEl {
	now := time.Now()
	header := gk.HFlex(s.Gap3, s.Px4, s.Py2, s.BorderB1, s.TextXs).BorderColor(rgb(surface().Line)).
		TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0).W(px(132)).Child(txt("Time"))).
		Child(gk.Div(s.FlexShrink0).W(px(150)).Child(txt("Kind"))).
		Child(gk.Div(s.FlexShrink0).W(px(140)).Child(txt("Path"))).
		Child(gk.Div(s.Flex1).Child(txt("Message")))
	var rows []gk.DivEl
	all := log.Rows()
	for i := range all {
		row := &all[i]
		if !log.Visible(row) {
			continue
		}
		if row.Lost {
			plural := "s"
			if row.Count == 1 {
				plural = ""
			}
			rows = append(rows, gk.HFlex(s.Gap3, s.Px4, s.Py2, s.BorderB1, s.TextSm).
				BorderColor(rgb(surface().Line)).Bg(rgb(DangerBg)).
				Child(gk.Div(s.FlexShrink0).W(px(132))).
				Child(gk.Div(s.Flex1).TextColor(rgb(Danger)).
					Child(txt(fmt.Sprintf("%d event%s lost before they could be read", row.Count, plural)))))
			continue
		}
		seq := row.Seq
		expanded := log.Expanded != nil && *log.Expanded == seq
		pathText, pathColor := "-", Faint
		if row.Path != nil {
			pathText, pathColor = *row.Path, Text
		}
		line := gk.HFlex(s.ItemsStart, s.Gap3, s.CursorPointer, s.TextSm).
			Child(timeCell(row.At, now, mono)).
			Child(gk.Div(s.FlexShrink0).W(px(150)).Child(kindChip(row.Kind))).
			Child(gk.Div(s.FlexShrink0, s.Truncate).W(px(140)).TextColor(rgb(pathColor)).Child(txt(pathText))).
			Child(gk.Div(s.Flex1, s.MinW0).Child(txt(row.Message)))
		if len(row.Details) > 0 {
			label := "Details"
			if expanded {
				label = "Hide"
			}
			line = line.Child(gk.Div(s.FlexShrink0, s.TextXs).TextColor(rgb(Faint)).Child(txt(label)))
		}
		cell := gk.VFlex(s.Gap2, s.Px4, s.Py2, s.BorderB1).BorderColor(rgb(surface().Line)).
			Child(line.ID(gk.Key("event", int(seq))).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				v.log.Update(cx, func(l *EventLog) { l.ToggleExpanded(seq) })
			}))
		if expanded && len(row.Details) > 0 {
			lines := gk.VFlex(s.Gap1, s.Px3, s.Py2, s.RoundedLg, s.TextXs).Ml(px(132 + 12)).
				Bg(rgb(surface().Sunken)).TextColor(rgb(Muted))
			for _, d := range row.Details {
				lines = lines.Child(gk.Div().Child(txt(d)))
			}
			details := lines.ID(gk.Key("event-details", int(seq)))
			if row.Kind == core.DiagnosticsKindXrun {
				details = details.Tooltip(gk.NewKitTooltip(telemetry.XrunNote))
			}
			cell = cell.Child(details)
		}
		rows = append(rows, cell)
	}
	empty := len(rows) == 0
	if !empty {
		rows[len(rows)-1] = rows[len(rows)-1].Style(s.BorderB0)
	}
	table := gk.VFlex(s.BorderT1).BorderColor(rgb(surface().Line)).Child(header)
	for _, r := range rows {
		table = table.Child(r)
	}
	if empty {
		message := "No events in the chosen groups."
		switch {
		case !log.Connected:
			message = "Connect to the mixer to read its events."
		case len(all) == 0:
			message = "No events yet."
		}
		table = table.Child(gk.Div(s.Px4, s.Py4, s.TextSm).TextColor(rgb(Muted)).Child(txt(message)))
	}
	return table
}

func (v *eventsView) render(cx *gk.Cx) gk.Element {
	mono := gk.KitTheme(cx).MonoFontFamily(cx)
	log := v.log.Get()
	page := gk.VFlex(s.Gap4).Child(v.header(&log)).Child(v.filters(&log))
	if log.Error != "" {
		page = page.Child(gk.Div(s.TextSm).TextColor(rgb(Warn)).Child(txt(log.Error)))
	}
	return gk.Div(s.SizeFull, s.Px6, s.Py5).Bg(rgb(surface().Bg)).TextColor(rgb(Text)).
		ID("events-scroll").OverflowYScroll().
		Child(page.Child(v.table(&log, mono)))
}

// openEventsWindow opens the diagnostic events window or focuses it if already open.
func openEventsWindow(cx *gk.Cx, existing *gk.Window, log *gk.Model[EventLog], app *App,
	closed func(cx *gk.Cx)) *gk.Window {
	if existing != nil && slices.Contains(gk.Windows(cx), *existing) {
		gk.ActivateWindow(cx, *existing)
		return existing
	}
	v := &eventsView{log: log, app: app}
	v.view = gk.NewView(v.init, v.render)
	w := gk.OpenWindowWith(cx, windowIdentity(gk.WindowConfig{
		Width:     1040,
		Height:    720,
		MinWidth:  760,
		MinHeight: 420,
		Title:     "RC2FX diagnostic events",
	}), v.view)
	gk.OnWindowClosed(cx, w, func(cx *gk.Cx, _ gk.Window) {
		gk.Release(cx, v.ticks)
		if closed != nil {
			closed(cx)
		}
	})
	return &w
}
