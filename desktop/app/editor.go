package app

// The selected stage's editor: its heading and switch, a value
// and slider per parameter, the noise suppression page and what the mixer
// reports beside them. The parameters and figures are in edit/parameter.go.

import (
	"fmt"
	"math"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// Parameter is a control an effect's editor shows (edit.Parameter).
type Parameter = edit.Parameter

// Reported is what the mixer reports for the selected
// path (reportedLookahead in app.go makes one).
type Reported struct {
	latencySamples *uint32
	liveLookahead  *uint8
}

func (r Reported) edit() edit.Reported {
	return edit.Reported{LatencySamples: r.latencySamples, LiveLookahead: r.liveLookahead}
}

// ParameterEditor is one control's kit state: its text field and slider,
// made by rebuildEditors and released by the next.
type ParameterEditor struct {
	parameter Parameter
	label     string
	input     gk.InputState
	slider    gk.SliderState
	min, max  float64
	log       bool
	// decimals is the decimal places its step needs.
	decimals int
}

// fraction is where value sits along this editor's slider, 0 to 1, on its
// scale.
func (e *ParameterEditor) fraction(value float64) float32 {
	return edit.Fraction(value, e.min, e.max, e.log)
}

// rebuildEditors makes the selected stage's controls again: after the
// channel, the stage, the band or what they show changed.
func (a *App) rebuildEditors(cx *gk.Cx) {
	a.editorEpoch++
	epoch := a.editorEpoch
	a.draggingBand = false
	a.editingValue = nil
	a.channel = model.ActiveChannel(a.target(), a.channel)
	// The old editors' views are given back, but only once the view has
	// been described without them (retire).
	for _, editor := range a.editors {
		a.retire(editor.input, editor.slider)
	}
	a.editors = nil
	for _, sub := range a.subscriptions {
		a.retire(sub)
	}
	a.subscriptions = nil
	if a.effect == core.EffectSuppression && !a.target().SupportsSuppression() {
		a.effect = core.EffectEQ
	}
	a.band = min(a.band, max(len(a.target().EQ.Channel(a.channel))-1, 0))
	target := a.target()
	parameters := edit.Parameters(a.effect, target.Suppression.Gate.Advanced, len(target.EQ.Channel(a.channel)) > 0)
	for _, parameter := range parameters {
		d := parameter.Describe()
		index := len(a.editors)
		decimals := edit.Decimals(d.Step)
		value := parameter.Get(a.target(), a.channel, a.band)
		input := gk.NewInputState(cx).DefaultValue(edit.Format(value, decimals)).
			Min(d.Min).Max(d.Max).Step(d.Step).Build()
		scale := gk.SliderScaleLinear
		if d.Log {
			scale = gk.SliderScaleLogarithmic
		}
		slider := gk.NewSliderState(cx).Min(float32(d.Min)).Max(float32(d.Max)).Step(float32(d.Step)).
			DefaultValue(float32(value)).Scale(scale).Build()
		a.subscriptions = append(a.subscriptions, input.OnInputEvent(cx, func(cx *gk.Cx, e gk.InputEvent) {
			if a.editorEpoch != epoch {
				gk.Quiet(cx)
				return
			}
			switch e.(type) {
			// Typing ends with Enter or leaving the field.
			case gk.InputEventBlur, gk.InputEventPressEnter:
				a.editingValue = nil
				a.notify(cx)
				return
			case gk.InputEventChange:
			default:
				gk.Quiet(cx)
				return
			}
			value, ok := edit.ParseValue(input.Value(cx))
			if !ok || index >= len(a.editors) {
				gk.Quiet(cx)
				return
			}
			editor := a.editors[index]
			if math.IsInf(value, 0) || math.IsNaN(value) || value < editor.min || value > editor.max {
				gk.Quiet(cx)
				return
			}
			editor.parameter.Set(a.target(), a.channel, a.band, value)
			editor.slider.SetValue(cx, float32(value))
			a.notifyBandPreview(cx, a.band)
			a.notify(cx)
		}))
		a.subscriptions = append(a.subscriptions, slider.OnSliderEvent(cx, func(cx *gk.Cx, e gk.SliderEvent) {
			change, ok := e.(gk.SliderEventChange)
			if a.editorEpoch != epoch || !ok || index >= len(a.editors) {
				gk.Quiet(cx)
				return
			}
			editor := a.editors[index]
			value := float64(gk.SliderValueNumber(change.Value))
			editor.parameter.Set(a.target(), a.channel, a.band, value)
			editor.input.SetValue(cx, edit.Format(value, editor.decimals))
			a.notifyBandPreview(cx, a.band)
			a.notify(cx)
		}))
		a.editors = append(a.editors, ParameterEditor{
			parameter: parameter,
			label:     d.Label,
			input:     input,
			slider:    slider,
			min:       d.Min,
			max:       d.Max,
			log:       d.Log,
			decimals:  decimals,
		})
	}
	a.rebuildBandPreviews(cx)
}

// syncEditorValues puts the target's current values in every control.
func (a *App) syncEditorValues(cx *gk.Cx) {
	for _, editor := range a.editors {
		value := editor.parameter.Get(a.target(), a.channel, a.band)
		editor.input.SetValue(cx, edit.Format(value, editor.decimals))
		editor.slider.SetValue(cx, float32(value))
	}
	a.notifyBandPreview(cx, a.band)
}

// renderEditor is the selected stage: its heading and switch, its
// controls, and what the mixer reports beside them. The EQ lays out its own
// page.
func (a *App) renderEditor(cx *gk.Cx) gk.Element {
	effect := a.effect
	if effect == core.EffectEQ {
		return a.eqControls(cx)
	}
	target := a.target()
	off := !target.EffectEnabled(effect)
	var body gk.Element
	switch effect {
	case core.EffectSuppression:
		body = a.suppressionControls(cx)
	case core.EffectCompressor, core.EffectLimiter:
		columns := 2
		if a.narrow() {
			columns = 1
		}
		body = a.controls(func(Parameter) bool { return true }, columns, off, cx)
	default:
		body = a.controls(func(Parameter) bool { return true }, 1, off, cx)
	}
	controls := gk.Div(s.Flex1, s.MinW0)
	if hint, ok := a.firstChannelHint(); ok {
		controls = controls.Child(hint)
	}
	controls = controls.Child(a.stageHead(effect, cx)).Child(body)
	// The rail has the channel's levels; only what is particular to a stage
	// stays beside it, or under it in a narrow window.
	f, ok := a.stageFacts(effect)
	switch {
	case !ok:
		return controls
	case a.narrow():
		return gk.VFlex().Gap(px(28)).Child(controls).Child(f)
	}
	return gk.HFlex(s.ItemsStart).Gap(px(40)).Child(controls).Child(f)
}

// firstChannelHint is the one line the channel opened from a finished
// install shows above its suppression switch: what to do, then, once
// suppression is on, what to listen for. Dismissed or completed, it never
// shows again.
func (a *App) firstChannelHint() (gk.DivEl, bool) {
	if a.firstHintDone || a.firstHintTarget == "" || a.effect != core.EffectSuppression ||
		a.target().ID != a.firstHintTarget {
		return gk.DivEl{}, false
	}
	on := a.target().Suppression.Enabled
	text, label := "Switch on noise suppression below, then talk into the microphone.", "Skip"
	if on {
		text, label = "Now talk. You should hear the room noise drop in your headphones.", "Got it"
	}
	return gk.HFlex(s.ItemsCenter, s.Gap3, s.RoundedXl, s.Border1).MaxW(px(620)).Mb(px(22)).
		Px(px(16)).Py(px(10)).Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line)).
		Child(gk.Div(s.Flex1, s.MinW0, s.TextSm).TextColor(rgb(Text)).Child(txt(text))).
		Child(gk.Button("first-channel-hint").Ghost().Small().Label(label).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.firstHintTarget, a.firstHintDone = "", true
				if !a.firstHintSaved {
					a.firstHintSaved = true
					rememberFirstChannelHint()
				}
				a.notify(cx)
			})), true
}

// noteFirstChannelHint records the hint as completed once suppression is
// on for its channel: it shows its second line until dismissed or left,
// and never again after.
func (a *App) noteFirstChannelHint() {
	if a.firstHintDone || a.firstHintSaved || a.firstHintTarget == "" {
		return
	}
	for i := range a.session.Draft {
		if t := &a.session.Draft[i]; t.ID == a.firstHintTarget && t.Suppression.Enabled {
			a.firstHintSaved = true
			rememberFirstChannelHint()
			return
		}
	}
}

// narrow is whether the page is too narrow for a channel's editor to keep
// its columns side by side.
func (a *App) narrow() bool { return a.pageWidth > 0 && a.pageWidth < NarrowEditor }

// savedTargets is the saved settings' channels while connected, nil
// otherwise: what editing's suppression slot checks read.
func (a *App) savedTargets() []core.Target {
	if a.session.Saved == nil || !a.session.Connected {
		return nil
	}
	return a.session.Saved.Config.Targets
}

// suppressionBlocker is the other channel holding the one suppression slot
// this one needs, if any.
func (a *App) suppressionBlocker() *core.Target { return a.suppressionBlockerFor(a.effect) }

// suppressionBlockerFor is suppressionBlocker for effect's switch.
func (a *App) suppressionBlockerFor(effect core.Effect) *core.Target {
	target := a.target()
	if effect != core.EffectSuppression || target.Suppression.Enabled {
		return nil
	}
	return model.SuppressionBlocker(a.session.Draft, a.savedTargets(), target.ID)
}

// stageHead is a stage's title and switch, then one line on what it does.
func (a *App) stageHead(effect core.Effect, cx *gk.Cx) gk.DivEl {
	target := a.target()
	on := target.EffectEnabled(effect)
	blocker := a.suppressionBlocker()
	sw := a.effectSwitch(target, effect)
	var description string
	switch {
	case effect == core.EffectHighpass:
		description = "Cuts rumble and handling noise below the cutoff."
	case effect == core.EffectSuppression && target.IsChatOutput():
		description = "Chat voices: combines routed microphones into one mono denoiser before the stereo output effects. Smart Pads and other playback bypass suppression and the voice gate."
	case effect == core.EffectSuppression:
		description = "Removes background noise from the voice, on the mixer. Limited to one Mic or Chat Mix."
	case effect == core.EffectEQ && target.Channels == 2:
		description = "Up to 32 bands. Link both sides, or tune left and right separately."
	case effect == core.EffectEQ:
		description = "Up to 32 bands."
	case effect == core.EffectCompressor:
		description = "Evens out loud and quiet moments. Above the threshold, the level rises 1 dB for every ratio dB in."
	case effect == core.EffectGain:
		description = "A fixed level change at this point in the chain."
	case effect == core.EffectLimiter:
		description = "A sample-peak ceiling the channel never exceeds."
	}
	state := "Off"
	if on {
		state = "On"
	}
	about := gk.Div().MaxW(px(620)).Mb(px(22)).TextSize(px(13.5)).LineHeight(gk.Relative(1.5)).
		TextColor(rgb(Faint)).Child(txt(description))
	if blocker != nil {
		ownerID := blocker.ID
		needsRelease := false
		for i := range a.session.Draft {
			if a.session.Draft[i].ID == ownerID && a.session.Draft[i].Suppression.Enabled {
				needsRelease = true
				break
			}
		}
		disabled := !a.session.Connected || a.pending != nil || a.routingPending != nil || a.presets.busy() ||
			a.transitionBusy()
		// Edits save on their own once they stop: the slot is free as soon
		// as turning suppression off on the other channel is saved.
		line := fmt.Sprintf("%s holds the suppression slot until turning its suppression off is saved. Enable it here after that.",
			a.targetName(blocker))
		if needsRelease {
			line = fmt.Sprintf("%s uses the suppression slot. Turn its suppression off, then enable it here once that is saved.",
				a.targetName(blocker))
		}
		about = about.Child(gk.Div(s.Mt2).TextColor(rgb(Warn)).Child(txt(line)))
		if needsRelease {
			about = about.Child(ghost("release-suppression", "Release suppression slot").Disabled(disabled).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if !a.session.Connected || a.pending != nil || a.routingPending != nil || a.presets.busy() ||
						a.transitionBusy() {
						gk.Quiet(cx)
						return
					}
					model.ReleaseSuppression(a.session.Draft, ownerID)
					a.notify(cx)
				}))
		}
	}
	return gk.Div().
		Child(gk.HFlex(s.ItemsCenter).Gap(px(14)).Mb(px(6)).
			Child(gk.Div(s.FontSemibold).TextSize(px(20)).Child(txt(effect.Label()))).
			Child(gk.Div(s.Flex1)).
			Child(gk.Div().TextSize(px(13.5)).TextColor(rgb(Muted)).Child(txt(state))).
			Child(sw)).
		Child(about)
}

// controls is the controls keep accepts, columns to a row.
func (a *App) controls(keep func(Parameter) bool, columns int, off bool, cx *gk.Cx) gk.DivEl {
	var items []gk.Element
	for i := range a.editors {
		if keep(a.editors[i].parameter) {
			items = append(items, a.control(&a.editors[i], off, cx))
		}
	}
	return grid(columns, items)
}

// editorFor is the control of parameter, nil when the stage has none.
func (a *App) editorFor(parameter Parameter) *ParameterEditor {
	for i := range a.editors {
		if a.editors[i].parameter == parameter {
			return &a.editors[i]
		}
	}
	return nil
}

// control is one control: its name, its value (click to type), and a thin
// slider with the default marked. Double-clicking the slider restores the
// default.
func (a *App) control(editor *ParameterEditor, off bool, cx *gk.Cx) gk.DivEl {
	parameter := editor.parameter
	value := parameter.Get(a.target(), a.channel, a.band)
	fraction := editor.fraction(value)
	def := editor.fraction(parameter.DefaultValue(a.target()))
	var valueView gk.Element
	if a.editingValue != nil && *a.editingValue == parameter {
		valueView = gk.Div(s.Border1, s.FontSemibold).W(px(112)).Px(px(6)).Mx(px(-6)).Rounded(px(6)).
			Bg(rgb(0x0b0e16)).BorderColor(rgb(Accent)).TextSize(px(15)).
			Child(gk.Input(editor.input).Appearance(false).Style(s.Px0))
	} else {
		color := Text
		if off {
			color = Muted
		}
		valueView = gk.Div(s.CursorText, s.WhitespaceNowrap, s.FontSemibold).Px(px(6)).Py(px(2)).Mx(px(-6)).
			My(px(-2)).Rounded(px(6)).TextSize(px(15)).TextColor(rgb(color)).
			Hover(hover().Bg(white(0.05))).
			Child(txt(parameter.Display(value))).
			ID(gk.Key("value", int(parameter))).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.editValue(cx, parameter) })
	}
	thumb := uint32(0xffffff)
	if off {
		thumb = 0x9aa2b2
	}
	return gk.Div().Mb(px(24)).
		Child(gk.HFlex(s.ItemsCenter, s.JustifyBetween, s.Gap3).Mb(px(10)).
			Child(gk.Div(s.WhitespaceNowrap).TextSize(px(13.5)).TextColor(rgb(Muted)).Child(txt(editor.label))).
			Child(valueView)).
		Child(gk.Div(s.Relative, s.WFull).H(px(24)).
			Child(sliderTrack(fraction, def, off)).
			Child(gk.Slider(editor.slider).AccessibilityLabel(editor.label).Bg(gk.HSLA(0, 0, 0, 0)).
				TextColor(rgb(thumb))).
			// Read in the capture phase: the slider owns the press.
			CaptureAnyMouseUp(func(cx *gk.Cx, e gk.MouseEvent) {
				if e.Button == gk.ButtonLeft && e.ClickCount == 2 {
					a.resetValue(cx, parameter)
					return
				}
				gk.Quiet(cx)
			}).
			ID(gk.Key("slider", int(parameter))))
}

// editValue turns a value into a text field, with its number selected.
func (a *App) editValue(cx *gk.Cx, parameter Parameter) {
	editor := a.editorFor(parameter)
	if editor == nil {
		gk.Quiet(cx)
		return
	}
	value := parameter.Get(a.target(), a.channel, a.band)
	editor.input.SetValue(cx, edit.Format(value, editor.decimals))
	editor.input.Focus(cx)
	editor.input.SelectAll(cx)
	a.editingValue = &parameter
	a.notify(cx)
}

// resetValue restores a control's default (the core's defaults for a new
// channel or band).
func (a *App) resetValue(cx *gk.Cx, parameter Parameter) {
	value := parameter.DefaultValue(a.target())
	parameter.Set(a.target(), a.channel, a.band, value)
	a.syncEditorValues(cx)
	a.notify(cx)
}

// suppressionControls is noise suppression: the model, its lookahead and
// blend, then the voice gate, as one slider or its separate settings.
func (a *App) suppressionControls(cx *gk.Cx) gk.Element {
	target := a.target()
	off := !target.Suppression.Enabled
	custom := target.Suppression.Gate.Advanced
	page := gk.Div()
	{
		lookahead := target.Suppression.Lookahead
		reported := a.reportedLookahead().edit()
		choices := pills()
		for index, choice := range core.Lookaheads {
			cost := edit.LookaheadCost(reported, choice, lookahead)
			name := "On"
			if choice == 0 {
				name = "Off"
			}
			label := name
			if cost != "now" {
				label = name + " · " + cost
			}
			choices = choices.Child(pill(gk.Key("lookahead", index), label, choice == lookahead).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.target().Suppression.SelectLookahead(choice)
					a.notify(cx)
				}))
		}
		figure := edit.LookaheadFigure(reported, lookahead)
		note := "One hop of context: can sound cleaner at the start of words, and adds 5 ms to this channel."
		if lookahead == 0 {
			note = "Lowest delay. Turn on for slightly cleaner word onsets, at one hop (5 ms) more delay."
		}
		if figure != "" {
			note = fmt.Sprintf("%s Reported delay: %s.", note, figure)
		}
		page = page.Child(gk.Div().Mb(px(24)).
			Child(gk.HFlex(s.ItemsCenter, s.JustifyBetween).
				Child(gk.Div().TextSize(px(13.5)).TextColor(rgb(Muted)).Child(txt("Lookahead"))).
				Child(choices)).
			Child(hint(note).Mt(px(8))))
	}
	gateMode := pills().
		Child(pill("voice-gate-simple", "Simple", !custom).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.target().Suppression.Gate.SetAdvanced(false)
			a.rebuildEditors(cx)
			a.notify(cx)
		})).
		Child(pill("voice-gate-custom", "Custom", custom).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.target().Suppression.Gate.SetAdvanced(true)
			a.rebuildEditors(cx)
			a.notify(cx)
		}))
	if blend := a.editorFor(edit.ParameterBlend); blend != nil {
		page = page.Child(a.control(blend, off, cx).
			Child(hint("How much of the cleaned voice you hear. Lower blends the original back in.")))
	}
	page = page.Child(gk.Div().Mt(px(34)).Child(section("Voice gate", gateMode)))
	if custom {
		columns := 2
		if a.narrow() {
			columns = 1
		}
		page = page.Child(a.controls(func(p Parameter) bool { return p != edit.ParameterBlend }, columns, off, cx))
	} else if strength := a.editorFor(edit.ParameterGateStrength); strength != nil {
		note := "Turns the channel down when the model hears no voice. One slider sets threshold, reduction and timing together."
		if target.Suppression.Gate.Strength == 0 {
			note = "Off. Raise it to turn the channel down between words."
		}
		page = page.Child(a.control(strength, off, cx).Child(hint(note)))
	}
	return page
}

// stageFacts is what the mixer reports that is particular to a stage,
// beside it: the compressor's gain reduction and curve, and suppression's
// voice, gate and delay. The channel's levels are in the rail; ok is false
// for the stages with nothing more.
func (a *App) stageFacts(effect core.Effect) (gk.DivEl, bool) {
	m, synthetic := a.freshMeter()
	live := m != nil
	peak := func(f func(*core.TargetMeter) float64) float64 {
		if m == nil {
			return 0
		}
		return f(m)
	}
	var missed uint64
	var latency uint32
	if m != nil {
		missed, latency = m.MissedDeadlines, m.LatencySamples
	}
	target := a.target()
	switch effect {
	case core.EffectSuppression:
		voice := peak(func(m *core.TargetMeter) float64 { return m.Voice })
		gate := "off"
		if target.Suppression.Gate.Active() {
			gate = fmt.Sprintf("open · %.0f%%", peak(func(m *core.TargetMeter) float64 { return m.GateOpen })*100)
		}
		delay := "not reported"
		if latency > 0 {
			delay = fmt.Sprintf("%.1f ms", float64(latency)/48)
		}
		return facts(live, synthetic, []Fact{
			newFact("Voice heard", fmt.Sprintf("%.0f%%", voice*100)).withBar(float32(voice)),
			newFact("Gate", gate),
			newFact("Delay on this channel", delay),
			newFact("Missed frames", fmt.Sprint(missed)),
		}, nil), true
	case core.EffectCompressor:
		reduction := math.Abs(peak(func(m *core.TargetMeter) float64 { return m.GainReduction }))
		var livePeak *float64
		if m != nil {
			livePeak = &m.InputPeak
		}
		return facts(live, synthetic, []Fact{
			newFact("Gain reduction", fmt.Sprintf("%.1f dB", reduction)).withBar(float32(reduction / 24)),
		}, compressorCurve(target.Compressor, livePeak)), true
	}
	return gk.DivEl{}, false
}

// sliderTrack is the slider's track, its gradient fill up to the value and
// a tick at the default, as divs under the
// toolkit slider, whose own track is clear. The control is 24 px tall, so
// the 4 px bars sit 10 px down and the 12 px tick 6 px down.
func sliderTrack(fraction, def float32, off bool) gk.Element {
	bar := func() gk.DivEl {
		return gk.Div(s.Absolute, s.Left0).Top(px(10)).H(px(4)).Rounded(px(2))
	}
	fill := bar().W(gk.Relative(min(max(fraction, 0), 1)))
	if off {
		fill = fill.Bg(rgb(0x3a4152))
	} else {
		fill = fill.Bg(gradient())
	}
	return gk.Div(s.Absolute, s.Inset0).
		Child(bar().W(gk.Relative(1)).Bg(rgb(0x232838))).
		Child(gk.Div(s.Absolute).Left(gk.Relative(def)).Ml(px(-1)).Top(px(6)).W(px(2)).H(px(12)).
			Rounded(px(1)).Bg(rgb(Dim))).
		Child(fill)
}

// compressorCurve is the compressor's static curve, calculated from its
// settings, with the reported input level (nil for none) marked on it.
func compressorCurve(compressor core.Compressor, inputPeak *float64) gk.Element {
	threshold := compressor.Threshold
	const size, margin float32 = 220, 24
	span := size - 2*margin
	at := func(xDB, yDB float64) [2]float32 {
		return [2]float32{
			margin + float32((xDB+60)/60)*span,
			size - margin - float32((yDB+60)/60)*span,
		}
	}
	line := func(from, to [2]float32, color uint32) gk.PlotPart {
		return gk.NewPlotLine([][2]float32{from, to}, 1, rgb(color))
	}
	parts := []gk.PlotPart{
		line(at(-60, -60), at(0, -60), surface().Line),
		line(at(0, -60), at(0, 0), surface().Line),
		line(at(0, 0), at(-60, 0), surface().Line),
		line(at(-60, 0), at(-60, -60), surface().Line),
		line(at(-60, -60), at(0, 0), surface().LineStrong),
		line(at(threshold, -60), at(threshold, 0), surface().LineStrong),
	}
	curve := make([][2]float32, 0, 61)
	for i := 0; i <= 60; i++ {
		db := -60 + float64(i)
		curve = append(curve, at(db, edit.CompressorOut(compressor, db)))
	}
	parts = append(parts, gk.NewPlotLine(curve, 2.2, gradient()))
	if inputPeak != nil {
		db := edit.LiveInputDB(*inputPeak)
		p := at(db, edit.CompressorOut(compressor, db))
		parts = append(parts, gk.NewPlotQuad(p[0]-5, p[1]-5, 10, 10, rgb(0xffffff), gk.QuadStyle{CornerRadius: 5}))
	}
	box := gk.Div(s.Relative).Size(px(size)).
		Child(gk.Div(s.Absolute, s.Inset0).Child(gk.Plot(parts...)))
	label := func(text string) gk.DivEl {
		return gk.Div(s.Absolute).TextSize(px(11)).TextColor(rgb(Faint)).Child(txt(text))
	}
	box = box.
		Child(label("in").Left(px(margin)).Bottom(px(4))).
		Child(label("out").Right(px(margin)).Top(px(4))).
		Child(label(edit.ParameterThreshold.Display(threshold)).
			Left(px(margin + float32((threshold+60)/60)*span + 5)).Bottom(px(margin + 4)))
	return gk.VFlex(s.Gap2).Mt(px(6)).Child(box)
}

// retire queues kit state for release. A view may still show it in the frame
// being built, and releasing state a view shows stops the app, so it is
// given back two polls later (releaseRetired), after the view that showed it
// has been redrawn without it.
func (a *App) retire(handles ...any) {
	a.retired = append(a.retired, handles...)
}

// releaseRetired runs once per poll: it releases what was retired two polls
// ago and moves the newer ones a step on.
func (a *App) releaseRetired(cx *gk.Cx) {
	for _, h := range a.retiring {
		gk.Release(cx, h)
	}
	a.retiring, a.retired = a.retired, nil
}
