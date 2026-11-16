package app

// desk.rs: the desk visual language: the theme, the notice card, the grip
// and the signal-flow lane. The surfaces are in appearance.go, the
// non-drawing helpers in desk_logic.go and colors.go.

import (
	"fmt"
	"os"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// loadThemes puts RC2FX's three themes into the kit's registry (theme.go),
// once, before the first applyTheme.
func loadThemes(cx *gk.Cx) {
	if err := gk.ThemeRegistryGlobalMut(cx).LoadThemesFromStr(cx, themeSetJSON()); err != nil {
		fmt.Fprintf(os.Stderr, "rc2fx: the app's theme was refused: %s\n", err)
	}
}

// setAppearance switches the window's surfaces and the components' theme to
// match.
func setAppearance(cx *gk.Cx, a Appearance) {
	storeAppearance(a)
	applyTheme(cx)
}

// applyTheme is desk.rs apply_theme: the components' colours, font size and
// radii for the current appearance, by applying its theme from the registry
// (theme.go says why it is not written field by field).
func applyTheme(cx *gk.Cx) {
	name := themeName(appearance())
	for _, t := range gk.ThemeRegistryGlobal(cx).Themes(cx) {
		if t.V0 == name {
			gk.ThemeGlobalMut(cx).ApplyConfig(cx, t.V1)
			return
		}
	}
	fmt.Fprintf(os.Stderr, "rc2fx: theme %q is not loaded\n", name)
}

// card is a notice above the page: rounded, quiet, and amber or red only
// through its callers' border and title colors.
func card() gk.DivEl {
	return gk.VFlex(s.Gap2, s.Border1).Mt(px(14)).Px(px(20)).Py(px(14)).Rounded(px(14)).
		Bg(white(0.02)).BorderColor(rgb(surface().LineStrong))
}

// grip is two columns of dots marking something as draggable.
func grip() gk.Element {
	column := func() gk.DivEl {
		c := gk.VFlex().Gap(px(3))
		for range 3 {
			c = c.Child(gk.Div(s.RoundedFull).Size(px(3)).Bg(rgb(Faint)))
		}
		return c
	}
	return gk.HFlex(s.FlexShrink0, s.CursorGrab).Gap(px(3)).Px(px(4)).Py(px(4)).Child(column()).Child(column())
}

// stagePreview is DraggedStage's Render: the stage card under the pointer
// while it is dragged (a view, as a drag preview is).
func (a *App) stagePreview(label string) gk.View {
	return gk.NewView(nil, func(cx *gk.Cx) gk.Element {
		return gk.Div(s.Px3, s.Py2, s.RoundedLg, s.Border1, s.TextSm).Bg(rgb(surface().Raised)).
			BorderColor(rgb(Accent)).TextColor(rgb(Text)).Child(txt(label))
	})
}

// chainRail is the channel's own column, full height between the sidebar
// and its editor: its name, Processing switch and what it is, its stages in
// signal order (a dot for on, and each one's key value), and its levels at
// the foot. Click a stage to edit it (its switch is on its page); drag one
// onto another to move it in the chain.
func (a *App) chainRail(target *core.Target, cx *gk.Cx) gk.Element {
	rail := gk.VFlex(s.FlexShrink0, s.HFull, s.BorderR1).W(px(RailWidth)).Px(px(12)).Pt(px(PageTop + 4)).
		Pb(px(18)).Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line)).
		Child(a.railHeader(target, cx))
	stages := gk.VFlex().Gap(px(2)).Mt(px(18)).ID("rail-stages").OverflowYScroll()
	for _, effect := range visibleEffects(target) {
		stages = stages.Child(a.railStage(target, effect))
	}
	rail = rail.Child(gk.Div(s.Flex1, s.MinH0).Child(stages))
	m, _ := a.freshMeter()
	levels := gk.VFlex().Gap(px(7)).Px(px(12)).Pt(px(12)).
		Child(railMeter("In", m, func(m *core.TargetMeter) float64 { return m.InputPeak })).
		Child(railMeter("Out", m, func(m *core.TargetMeter) float64 { return m.OutputPeak }).Mt(px(4)))
	if m != nil && m.Clipped > 0 {
		levels = levels.Child(gk.HFlex(s.JustifyBetween).Mt(px(4)).TextSize(px(12)).TextColor(rgb(Warn)).
			Child(txt("Clipped")).Child(txt(fmt.Sprint(m.Clipped))))
	}
	return rail.Child(levels)
}

// railHeader is the channel's name and Processing switch, and one line on
// what it is (mono or stereo, its fader) or what is wrong with its path.
func (a *App) railHeader(target *core.Target, cx *gk.Cx) gk.Element {
	var fader *uint8
	for _, src := range a.showSources() {
		if src.TargetID == target.ID {
			fader = src.Fader
			break
		}
	}
	processed := a.processed(target.ID)
	id := target.ID
	canSwitch := a.canSwitchPath(target.ID)
	sw := toggle("channel-processing", processed, true, !canSwitch)
	if canSwitch {
		sw = sw.OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.switchPath(cx, id, !processed) })
	}
	about := "Stereo"
	if target.Channels == 1 {
		about = "Mono"
	}
	if fader != nil {
		about = fmt.Sprintf("%s · Fader %d", about, *fader)
	}
	line := gk.Div(s.Truncate).Px(px(12)).Mt(px(4)).TextSize(px(12)).TextColor(rgb(Faint)).Child(txt(about))
	if note, ok := a.pathNote(target.ID); ok {
		line = gk.Div().Px(px(12)).Mt(px(4)).TextSize(px(12)).LineHeight(gk.Relative(1.4)).TextColor(rgb(Warn)).
			Child(txt(note))
	} else if a.selectionBusy() {
		line = line.TextColor(rgb(Warn)).Child(txt(" · changing…"))
	}
	return gk.Div().
		Child(gk.HFlex(s.ItemsCenter, s.Gap3).Pl(px(12)).Pr(px(4)).
			Child(gk.Div(s.Flex1, s.MinW0, s.Truncate, s.FontBold).TextSize(px(18)).Child(txt(a.targetName(target)))).
			Child(gk.Div(s.FlexShrink0).ID("channel-processing-wrap").Child(sw).
				Tooltip(gk.NewKitTooltip("Processing: run this channel through RC2FX on the mixer.")))).
		Child(line).
		Child(a.liveBar(target))
}

// railMeter is a level at the rail's foot: its label and reading in dBFS
// over a thin bar, or a dash without a fresh reading.
func railMeter(label string, m *core.TargetMeter, peak func(*core.TargetMeter) float64) gk.DivEl {
	value, color, fraction := "–", Dim, float32(0)
	if m != nil {
		value, color, fraction = dbfs(peak(m)), Text, level(peak(m))
	}
	return gk.VFlex(s.Gap2).
		Child(gk.HFlex(s.JustifyBetween).TextSize(px(12)).TextColor(rgb(Faint)).
			Child(txt(label)).
			Child(gk.Div(s.WhitespaceNowrap).TextColor(rgb(color)).Child(txt(value)))).
		Child(gk.Div(s.WFull).H(px(3)).Rounded(px(2)).Bg(rgb(0x232838)).
			Child(gk.Div(s.HFull).W(gk.Relative(fraction)).Rounded(px(2)).Bg(gradient())))
}

// railStage is one stage in the rail: a dot for on, its name and key value.
// The selected stage is marked.
func (a *App) railStage(target *core.Target, effect core.Effect) gk.Element {
	on := target.EffectEnabled(effect) && !target.Bypass
	selected := a.effect == effect
	row := gk.HFlex(s.Relative, s.ItemsCenter, s.CursorPointer).Gap(px(10)).H(px(46)).Px(px(12)).
		Rounded(px(9)).Group("stage-row")
	if selected {
		row = row.Bg(white(0.06))
	} else {
		row = row.Hover(hover().Bg(white(0.03)))
	}
	nameColor, valueColor := Muted, Dim
	if on {
		nameColor, valueColor = Text, Muted
	}
	value := "Off"
	switch {
	case on:
		value = stageSummary(target, effect, a.channel)
	case target.Bypass:
		value = "Bypassed"
	}
	// The grip shows at the row's end while it is hovered: drag the row to
	// move the stage.
	row = row.
		Child(dot(on, 6)).
		Child(gk.Div(s.Flex1, s.MinW0).
			Child(gk.Div(s.Truncate).TextSize(px(13.5)).TextColor(rgb(nameColor)).Child(txt(effect.Label()))).
			Child(gk.Div(s.Truncate).TextSize(px(12)).TextColor(rgb(valueColor)).Child(txt(value)))).
		Child(gk.Div(s.FlexShrink0).Opacity(0).GroupHover("stage-row", hover().Opacity(1)).Child(grip()))
	return row.ID(gk.Key("stage", effectIndex(effect))).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.effect = effect
			a.rebuildEditors(cx)
			a.notify(cx)
		}).
		// Drag a stage onto another to move it there in the chain.
		OnDrag(gk.DragValue{ID: dragID(dragStage, effectIndex(effect)), Text: effect.Label()},
			a.stagePreviews[effect]).
		CanDrop(gk.DropAccept{IDs: dragIDs(dragStage, len(core.EffectAll))}).
		DragOver(hover().Bg(rgb(surface().RaisedHover))).
		OnDrop(func(cx *gk.Cx, d gk.DragValue) {
			kind, from := dragKind(d.ID)
			if kind != dragStage || from >= len(core.EffectAll) {
				gk.Quiet(cx)
				return
			}
			model.PlaceEffect(a.targetMut(), core.EffectAll[from], effect)
			a.notify(cx)
		})
}

// effectSwitch is a stage's on/off switch, in its page's heading.
// Suppression's is disabled while another channel holds the one suppression
// slot (the page says how to release it).
func (a *App) effectSwitch(target *core.Target, effect core.Effect) gk.StatefulDivEl {
	on := target.EffectEnabled(effect)
	sw := toggle("effect-enable", on, false, a.suppressionBlockerFor(effect) != nil)
	if a.suppressionBlockerFor(effect) != nil {
		return sw
	}
	return sw.OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
		model.EnableEffect(a.session.Draft, a.savedTargets(), a.selected, effect, !on)
		a.notify(cx)
	})
}
