package app

// The Diagnostics page's TELEMETRY section, shown when
// Show telemetry is on. It answers whether RC2FX is keeping up, in words a
// person reads at a glance: a row of status badges, four yes/no checks, then
// one line per processed channel. Technical details opens the raw figures,
// each with an explanation. Only audible problems are flagged, and only while
// they are recent: older ones become history, and the mixer starting is left
// out. Built from a plain snapshot so it renders the same with preview data.
// What it says (statuses, checks, figures) is in app/telemetry (view.go);
// this file draws it.

import (
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// TelemetrySnapshot is the figures the TELEMETRY section shows.
type TelemetrySnapshot = telemetry.Snapshot

// toneColor is a telemetry tone's color.
func toneColor(t telemetry.Tone) uint32 {
	switch t {
	case telemetry.ToneAccent:
		return Accent
	case telemetry.ToneWarn:
		return Warn
	case telemetry.ToneDanger:
		return Danger
	case telemetry.ToneFaint:
		return Faint
	}
	return Text
}

// telemetryValue is a figure's text in its color.
func telemetryValue(text string, color uint32) gk.DivEl {
	return gk.Div(s.FlexShrink0, s.TextSm).TextColor(rgb(color)).Child(txt(text))
}

// telemetryDot is a plain status dot (ui.go's dot is the gradient one).
func telemetryDot(color uint32) gk.DivEl {
	return gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(8)).Bg(rgb(color))
}

func checkRow(check telemetry.Check, first bool) gk.DivEl {
	row := gk.HFlex(s.ItemsCenter, s.Gap3, s.Py2)
	if !first {
		row = row.Style(s.BorderT1).BorderColor(rgb(surface().Line))
	}
	answerColor := Text
	if check.State == telemetry.CheckOff {
		answerColor = Faint
	}
	answer := gk.HFlex(s.FlexShrink0, s.Gap1, s.TextXs).
		Child(gk.Div().TextColor(rgb(answerColor)).Child(txt(check.Answer)))
	if check.Problem != "" {
		answer = answer.Child(gk.Div().TextColor(rgb(Warn)).Child(txt("· " + check.Problem)))
	}
	if check.History != "" {
		answer = answer.Child(gk.Div().TextColor(rgb(Faint)).Child(txt("· " + check.History)))
	}
	return row.
		Child(telemetryDot(toneColor(check.State.Tone()))).
		Child(gk.Div(s.Flex1, s.MinW0, s.Truncate, s.TextSm, s.FontMedium).Child(txt(check.Label))).
		Child(answer)
}

// badge is a small rounded badge: a quiet name ("" for none), then its
// value.
func badge(name, text string, color uint32) gk.TagEl {
	row := gk.HFlex(s.ItemsCenter, s.Gap1, s.TextXs)
	if name != "" {
		row = row.Child(gk.Div().TextColor(rgb(Muted)).Child(txt(name)))
	}
	return gk.TagSecondary().Small().RoundedFull().
		Child(row.Child(gk.Div(s.FontSemibold).TextColor(rgb(color)).Child(txt(text))))
}

// badges is the status, how long audio has run, the format, and anything
// unusual.
func badges(engine *core.EngineStatus, routing *core.RoutingStatus, snapshot *TelemetrySnapshot) gk.DivEl {
	status := telemetry.StatusOf(engine, routing, snapshot)
	color := toneColor(status.Tone())
	row := gk.HFlex(s.FlexWrap, s.ItemsCenter, s.Gap2).
		Child(gk.TagSecondary().Small().RoundedFull().Child(
			gk.HFlex(s.ItemsCenter, s.Gap1p5, s.TextXs, s.FontSemibold).TextColor(rgb(color)).
				Child(telemetryDot(color)).
				Child(txt(status.Label()))))
	for _, b := range telemetry.Badges(engine, routing, snapshot) {
		row = row.Child(badge(b.Name, b.Text, toneColor(b.Tone)))
	}
	return row
}

// Column widths of the channel table: name, delay, issues; the two meters
// share what is left.
const (
	nameWidth   float32 = 170
	delayWidth  float32 = 64
	issuesWidth float32 = 150
)

func headerCell(text string) gk.DivEl {
	return gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(text))
}

// telemetryLevel is a level bar from −60 to 0 dBFS, red at or over full
// scale, and its value. (colors.go's level is the overview's meter scale.)
func telemetryLevel(peak float64, mono string) gk.DivEl {
	width, over := telemetry.LevelWidth(peak)
	color, valueColor := Accent, Text
	if over {
		color, valueColor = Danger, Danger
	}
	return gk.HFlex(s.ItemsCenter, s.Gap2, s.Flex1, s.MinW0).
		Child(gk.Div(s.Flex1, s.RoundedFull).MinW(px(48)).H(px(6)).Bg(rgb(surface().Sunken)).
			Child(gk.Div(s.HFull, s.RoundedFull).W(gk.Relative(width)).Bg(rgb(color)))).
		Child(gk.Div(s.FlexShrink0, s.TextRight, s.TextXs).W(px(44)).FontFamily(mono).
			TextColor(rgb(valueColor)).Child(txt(telemetry.DBValue(peak))))
}

func channelRow(path *telemetry.PathView, sampleRate uint32, now time.Time, mono string) gk.DivEl {
	meter := &path.Meter
	delay, delayColor := telemetry.Delay(meter, sampleRate), Text
	if delay == "" {
		delay, delayColor = "—", Faint
	}
	var issueCell gk.DivEl
	if text, recent, ok := telemetry.Issues(path, now); ok {
		color := Faint
		if recent {
			color = Warn
		}
		issueCell = gk.Div().Child(gk.Div(s.Truncate).ID("telemetry-issues-" + meter.ID).
			TextColor(rgb(color)).Child(txt(text)).
			Tooltip(gk.NewKitTooltip(text)))
	} else {
		issueCell = gk.Div().Child(gk.Div().TextColor(rgb(Faint)).Child(txt("—")))
	}
	name := gk.HFlex(s.FlexShrink0, s.ItemsCenter, s.Gap2).W(px(nameWidth)).
		Child(gk.Div(s.MinW0, s.Truncate, s.TextSm, s.FontSemibold).Child(txt(path.Name)))
	if label := telemetry.FormatLabel(path.Channels); label != "" {
		name = name.Child(gk.TagSecondary().Outline().Small().Child(txt(label)))
	}
	return gk.HFlex(s.ItemsCenter, s.Gap4, s.Py2, s.BorderT1).BorderColor(rgb(surface().Line)).
		Child(name).
		Child(telemetryLevel(meter.InputPeak, mono)).
		Child(telemetryLevel(meter.OutputPeak, mono)).
		Child(gk.Div(s.FlexShrink0, s.TextXs).W(px(delayWidth)).TextColor(rgb(delayColor)).Child(txt(delay))).
		Child(gk.HFlex(s.FlexShrink0, s.JustifyEnd, s.TextXs).W(px(issuesWidth)).Child(issueCell))
}

func channelTable(snapshot *TelemetrySnapshot, sampleRate uint32, mono string) gk.DivEl {
	table := gk.VFlex().Child(gk.HFlex(s.ItemsCenter, s.Gap4, s.Pb1).
		Child(headerCell("Channel").W(px(nameWidth)).Style(s.FlexShrink0)).
		Child(headerCell("Input dB").Style(s.Flex1)).
		Child(headerCell("Output dB").Style(s.Flex1)).
		Child(headerCell("Delay").W(px(delayWidth)).Style(s.FlexShrink0)).
		Child(headerCell("Issues").W(px(issuesWidth)).Style(s.FlexShrink0, s.TextRight)))
	for i := range snapshot.Paths {
		table = table.Child(channelRow(&snapshot.Paths[i], sampleRate, snapshot.Now, mono))
	}
	return table
}

// factRow is one figure; hovering anywhere on the row explains it, so no
// icon has to follow labels of different lengths.
func factRow(fact telemetry.Fact, mono string) gk.Element {
	row := gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.Gap3, s.Px2, s.Py1, s.RoundedMd).
		Hover(hover().Bg(rgb(surface().Raised))).
		Child(gk.Div(s.MinW0, s.Truncate, s.TextXs, s.TextDecoration1, s.TextDecorationSolid).
			TextColor(rgb(Muted)).TextDecorationColor(rgb(surface().LineStrong)).Child(txt(fact.Label))).
		Child(gk.Div(s.FlexShrink0, s.TextXs).FontFamily(mono).TextColor(rgb(Text)).Child(txt(fact.Value)))
	return row.ID("telemetry-fact-" + fact.Label).
		Tooltip(gk.NewKitTooltipElement(gk.Div(s.Py1).W(px(InfoWidth)).Child(txt(fact.Help))))
}

// factGrid is the facts in two columns, reading down the left first.
func factGrid(facts []telemetry.Fact, mono string) gk.DivEl {
	leftFacts, rightFacts := telemetry.FactColumns(facts)
	left := gk.VFlex(s.Flex1, s.MinW0)
	for _, f := range leftFacts {
		left = left.Child(factRow(f, mono))
	}
	right := gk.VFlex(s.Flex1, s.MinW0)
	for _, f := range rightFacts {
		right = right.Child(factRow(f, mono))
	}
	return gk.HFlex(s.ItemsStart, s.Gap6).Child(left).Child(right)
}

// telemetrySection is the TELEMETRY section; aside holds its header
// controls and toggle opens or closes Technical details.
func telemetrySection(snapshot *TelemetrySnapshot, mono string, aside gk.DivEl, toggle gk.Element) gk.DivEl {
	sampleRate := uint32(48_000)
	if snapshot.HasEngine {
		sampleRate = snapshot.Engine.SampleRate
	}
	body := gk.VFlex(s.Gap3, s.Py3)
	if snapshot.HasEngine {
		body = body.Child(badges(&snapshot.Engine, &snapshot.Routing, snapshot))
	}
	if snapshot.Notice != "" {
		body = body.Child(telemetryValue(snapshot.Notice, Muted))
	}
	if len(snapshot.Paths) > 0 {
		checks := telemetry.Checks(snapshot)
		list := gk.VFlex(s.Px4, s.Py1, s.RoundedLg).Bg(rgb(surface().Raised))
		for i, c := range checks {
			list = list.Child(checkRow(c, i == 0))
		}
		body = body.Child(list).
			Child(gk.Div(s.Pt2, s.TextXs, s.FontSemibold).TextColor(rgb(Muted)).Child(txt("Channels"))).
			Child(channelTable(snapshot, sampleRate, mono))
	}
	if len(snapshot.Idle) > 0 {
		body = body.Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(telemetry.IdleLine(snapshot.Idle))))
	}
	closing := gk.VFlex(s.Gap2, s.Py2, s.BorderT1).BorderColor(rgb(surface().Line)).
		Child(gk.HFlex().Child(toggle))
	if snapshot.Details && snapshot.HasEngine {
		closing = closing.Child(factGrid(telemetry.Facts(&snapshot.Engine, snapshot.Paths), mono))
	}
	return settingsSection("TELEMETRY", aside, body.Child(closing))
}

// telemetrySnapshot is everything the section shows, read from the app now.
func (a *App) telemetrySnapshot() TelemetrySnapshot {
	snapshot := TelemetrySnapshot{Now: time.Now(), Details: a.telemetryDetails}
	if !a.session.Connected {
		snapshot.Notice = telemetry.NoticeOffline
		return snapshot
	}
	state := a.session.Saved
	if state == nil {
		snapshot.Notice = telemetry.NoticeWaiting
		return snapshot
	}
	snapshot.HasEngine = true
	snapshot.Engine, snapshot.Routing = state.Engine, state.Routing
	if a.meter != nil {
		snapshot.Epoch = a.meter.DiagnosticsEpoch
	}
	if !a.streamConnected {
		snapshot.Notice = telemetry.NoticeInterrupted
		return snapshot
	}
	target := func(id string) *core.Target {
		for i := range a.session.Draft {
			if a.session.Draft[i].ID == id {
				return &a.session.Draft[i]
			}
		}
		return nil
	}
	name := func(id string) string {
		if t := target(id); t != nil {
			return a.targetName(t)
		}
		return id
	}
	channels := func(id string) uint8 {
		if t := target(id); t != nil {
			return t.Channels
		}
		return 0
	}
	// Stock passthrough paths carry untouched audio: only processed ones.
	if a.meter != nil {
		for i := range a.meter.Targets {
			id := a.meter.Targets[i].ID
			if !state.Config.IsSelected(id) {
				continue
			}
			meter, synthetic := a.freshMeterFor(id)
			if meter == nil {
				snapshot.Idle = append(snapshot.Idle, name(id))
				continue
			}
			snapshot.Synthetic = snapshot.Synthetic || synthetic
			shown := telemetry.CloneTargetMeter(meter)
			a.startup.Settle(&shown)
			snapshot.Paths = append(snapshot.Paths, telemetry.PathView{
				Name:     name(meter.ID),
				Channels: channels(meter.ID),
				Meter:    shown,
				Recent:   a.recentFaults.Path(meter.ID),
			})
		}
	}
	snapshot.Xruns = a.recentFaults.Xruns()
	if len(snapshot.Paths) == 0 {
		snapshot.Idle = nil
		if state.Engine.Status != core.EnginePhaseRunning {
			snapshot.Notice = telemetry.NoticeNotRunning
		} else {
			snapshot.Notice = telemetry.NoticeNoFresh
		}
	}
	return snapshot
}
