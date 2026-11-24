package app

import (
	"cmp"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/home"
	"github.com/ibanks42/rc2fx/desktop/app/levels"
	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// processedIndexes returns draft indices of paths currently processed by the mixer.
func (a *App) processedIndexes() []int {
	state := a.session.Saved
	if state == nil {
		return nil
	}
	var indexes []int
	for i, t := range a.session.Draft {
		if state.Config.IsSelected(t.ID) {
			indexes = append(indexes, i)
		}
	}
	return indexes
}

// homeHeadline returns the overview headline and subtext. System and routing
// states take precedence (matching the status line). "All clear" requires a
// fresh meter reading for every processed path, and audio warnings outrank
// missing audio.
func (a *App) homeHeadline(processed []int) (string, string) {
	if label, _, ok := a.systemStatus(); ok {
		detail := ""
		if !a.streamConnected {
			detail = "Mixer processing is independent of this app; it keeps running."
		} else if a.session.Saved != nil {
			if _, engineError := telemetry.EngineError(a.session.Saved); engineError {
				detail = "The notice above says what happened and what to do."
			}
		}
		return label, detail
	}
	if label, _, ok := a.routingStatus(); ok {
		return label, ""
	}
	plural := func(n int) string {
		if n == 1 {
			return ""
		}
		return "s"
	}
	if len(processed) == 0 {
		return "No channels processed.", "Choose a channel on the left to add RC2FX processing."
	}
	for _, i := range processed {
		if warning, ok := a.audioWarningFor(a.session.Draft[i].ID); ok {
			return "Needs a look.", warning.Label()
		}
	}
	fresh := 0
	for _, i := range processed {
		if meter, _ := a.freshMeterFor(a.session.Draft[i].ID); meter != nil {
			fresh++
		}
	}
	if fresh < len(processed) {
		return "Waiting for audio.", fmt.Sprintf("No fresh levels from %d of %d processed channel%s.",
			len(processed)-fresh, len(processed), plural(len(processed)))
	}
	return "All clear.", fmt.Sprintf("RC2FX is processing %d channel%s.", len(processed), plural(len(processed)))
}

// homeKey captures the time-dependent parts of the Home view (headline and
// meter freshness) so the poll loop can detect when meters go stale.
func (a *App) homeKey() HomeKey {
	processed := a.processedIndexes()
	headline, detail := a.homeHeadline(processed)
	var fresh []bool
	isFresh := func(id string) bool {
		meter, _ := a.freshMeterFor(id)
		return meter != nil
	}
	for _, i := range processed {
		fresh = append(fresh, isFresh(a.session.Draft[i].ID))
	}
	if a.meter != nil {
		for _, t := range a.meter.Targets {
			fresh = append(fresh, isFresh(t.ID))
		}
	}
	return HomeKey{V0: headline, V1: detail, V2: fresh}
}

// animatedPage reports whether a page containing live level indicators is visible.
func (a *App) animatedPage() bool {
	return a.session.Connected && a.nav.BasePageInFront() && a.installer == nil
}

// levelBar embeds the indicator view for an overview bar, falling back to a
// static draw until the poll loop creates the view.
func (a *App) levelBar(targetID string, bar levels.Bar) gk.Element {
	if el, ok := a.embed(indicatorKey{targetID, levels.BarShape(bar)}); ok {
		return el
	}
	a.lineSlots.inlineBars = a.renderGeneration + 1
	return levelBarFill(bar, a.overviewLevel(targetID))
}

// indicator embeds the animated indicator view for source, or draws a static
// fallback until the view exists (never both). The inline fallback has no
// measured size, so it is blank for the ~50 ms until the poll creates the view.
func (a *App) indicator(source lineSource, fresh bool, style LineStyle) gk.Element {
	if el, ok := a.embed(indicatorKey{source.path, levels.LineShape(style)}); ok {
		return el
	}
	lvl, state := float32(0), levels.LineStateUnavailable
	if fresh {
		lvl, state = levelOf(a.levels.Get(source.path), time.Now()), levels.LineStateLive
	}
	return livePlot(0, 0, 0, lvl, state, style)
}

func (a *App) openChannel(cx *gk.Cx, index int) {
	a.inputs.selected = nil
	a.selected = index
	a.band = 0
	a.channel = core.ChannelLeft
	a.importDraft = nil
	a.importError = nil
	a.nav.ShowChannel()
	a.rebuildEditors(cx)
	a.notify(cx)
}

// overviewLevel returns a channel's meter level in [0, 1], or nil if stale.
func (a *App) overviewLevel(targetID string) *float32 {
	if meter, _ := a.freshMeterFor(targetID); meter == nil {
		return nil
	}
	l := levelOf(a.levels.Get(targetID), time.Now())
	return &l
}

// overviewChip returns the summary label shown below a channel card and whether
// the channel is processed.
func (a *App) overviewChip(targetID string) (string, bool) {
	i := slices.IndexFunc(a.session.Draft, func(t core.Target) bool { return t.ID == targetID })
	if i < 0 {
		return "No RC2FX path", false
	}
	target := &a.session.Draft[i]
	if !a.processed(targetID) {
		return "Off", false
	}
	if target.Bypass {
		return "Bypassed", true
	}
	var on []string
	for _, e := range visibleEffects(target) {
		if target.EffectEnabled(e) {
			on = append(on, shortEffect(e))
		}
	}
	if len(on) == 0 {
		return "No effects on", true
	}
	return strings.Join(on, " · "), true
}

func (a *App) openSource(cx *gk.Cx, sourceID string) {
	var target string
	found := false
	for _, src := range a.showSources() {
		if src.ID == sourceID {
			target, found = src.TargetID, true
			break
		}
	}
	if !found {
		return
	}
	if index := slices.IndexFunc(a.session.Draft, func(t core.Target) bool { return t.ID == target }); index >= 0 {
		a.openChannel(cx, index)
	}
	a.inputs.selected = &sourceID
	a.notify(cx)
}

func (a *App) homePage(cx *gk.Cx) gk.Element {
	processed := a.processedIndexes()
	headline, detail := a.homeHeadline(processed)
	var sub string
	switch {
	case headline == "All clear.":
		sub = "Click any channel to change its effects."
	case headline == "No channels processed.":
		sub = "Nothing is processed yet. Pick a channel to add effects."
	case detail == "":
		sub = headline
	default:
		sub = headline + " " + detail
	}
	var faders []*core.StockSource
	for _, src := range a.showSources() {
		if src.Fader != nil {
			faders = append(faders, src)
		}
	}
	slices.SortStableFunc(faders, func(x, y *core.StockSource) int { return cmp.Compare(*x.Fader, *y.Fader) })
	strips := gk.HFlex(s.ItemsStretch).Gap(px(10))
	for _, source := range faders {
		chip, on := a.overviewChip(source.TargetID)
		id := source.ID
		strips = strips.Child(overviewCard(on).Style(s.Flex1, s.MinW0, s.Flex, s.FlexCol, s.ItemsCenter).
			Gap(px(8)).Pt(px(14)).Pb(px(12)).Px(px(8)).
			Child(verticalMeter(a.levelBar(source.TargetID, levels.BarVertical))).
			Child(cardName(model.DisplayName(source.Name), on).Mt(px(4))).
			Child(gk.Div().TextSize(px(11.5)).TextColor(rgb(Faint)).Child(txt(fmt.Sprintf("Fader %d", *source.Fader)))).
			Child(chipLabel(chip, on)).
			ID(gk.Key("strip", int(source.SourceID))).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openSource(cx, id) }))
	}
	// Show processed inputs that aren't assigned to any fader in this show so
	// they don't silently disappear from the overview.
	onFaders := make([]string, 0, len(faders))
	for _, src := range faders {
		onFaders = append(onFaders, src.TargetID)
	}
	elsewhere, anyElsewhere := gk.HFlex(s.FlexWrap).Gap(px(10)), false
	outputs := gk.HFlex(s.FlexWrap).Gap(px(10))
	for index := range a.session.Draft {
		target := &a.session.Draft[index]
		group := model.GroupOf(target)
		output := group == model.GroupHeadphones || group == model.GroupOutputs
		if !output && !home.ProcessedOffFaders(target.ID, slices.Contains(processed, index), onFaders) {
			continue
		}
		chip, on := a.overviewChip(target.ID)
		card := overviewCard(on).Style(s.Flex, s.FlexCol).W(px(232)).Gap(px(8)).Px(px(14)).Py(px(11)).
			Child(gk.HFlex(s.ItemsCenter).Gap(px(12)).
				Child(cardName(a.targetName(target), on)).
				Child(horizontalMeter(a.levelBar(target.ID, levels.BarHorizontal)))).
			Child(gk.HFlex().Child(chipLabel(chip, on))).
			ID(gk.Key("output", index)).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openChannel(cx, index) })
		if output {
			outputs = outputs.Child(card)
		} else {
			anyElsewhere = true
			elsewhere = elsewhere.Child(card)
		}
	}
	page := gk.VFlex(s.WFull, s.MxAuto, s.Pb6).MaxW(px(1180)).
		Child(gk.Div(s.FontBold).TextSize(px(HeadingText)).LineHeight(gk.Relative(1.1)).Child(txt("Your mixer"))).
		Child(gk.Div().Mt(px(6)).Mb(px(SectionGap)).TextSize(px(15)).TextColor(rgb(Muted)).Child(txt(sub))).
		Child(strips)
	if anyElsewhere {
		page = page.Child(sectionLabel("PROCESSED, NOT IN THIS SHOW")).Child(elsewhere)
	}
	return page.Child(sectionLabel("HEADPHONES AND OUTPUTS")).Child(outputs)
}

func sectionLabel(text string) gk.DivEl {
	return gk.Div(s.FontSemibold).Mt(px(SectionGap)).Mb(px(10)).TextSize(px(11)).TextColor(rgb(Faint)).
		Child(txt(text))
}

func overviewCard(processed bool) gk.DivEl {
	border := surface().Line
	if processed {
		border = 0x2f4a5a
	}
	return gk.Div(s.CursorPointer, s.Border1).Rounded(px(12)).Bg(rgb(surface().Panel)).
		BorderColor(rgb(border)).
		Hover(hover().Bg(rgb(surface().Raised)).BorderColor(rgb(surface().LineStrong)))
}

func cardName(name string, processed bool) gk.DivEl {
	d := gk.HFlex(s.ItemsCenter, s.MaxWFull, s.MinW0, s.FontMedium).Gap(px(6)).TextSize(px(13.5))
	if processed {
		d = d.Child(dot(true, 6))
	}
	return d.Child(gk.Div(s.MinW0, s.Truncate).Child(txt(name)))
}

func chipLabel(text string, processed bool) gk.DivEl {
	d := gk.Div(s.MaxWFull, s.Truncate, s.RoundedFull).Px(px(9)).Py(px(1)).TextSize(px(11))
	if processed {
		d = d.Bg(gk.HSLA(183.0/360, 0.73, 0.63, 0.12)).TextColor(rgb(Text))
	} else {
		d = d.Style(s.Border1).BorderColor(rgb(surface().LineStrong)).TextColor(rgb(Faint))
	}
	return d.Child(txt(text))
}

func verticalMeter(bar gk.Element) gk.DivEl {
	return gk.Div(s.Relative, s.OverflowHidden).W(px(10)).H(px(MeterHeight)).Rounded(px(6)).
		Bg(rgb(surface().Sunken)).Child(bar)
}

// horizontalMeter uses Relative so the bar fill can position inside it.
func horizontalMeter(bar gk.Element) gk.DivEl {
	return gk.Div(s.Flex1, s.Relative, s.OverflowHidden).MinW(px(40)).H(px(6)).Rounded(px(4)).
		Bg(rgb(surface().Sunken)).Child(bar)
}
