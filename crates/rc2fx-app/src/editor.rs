use super::*;
use gpui_kit::component::{
    input::InputEvent,
    slider::{Slider, SliderEvent, SliderScale, SliderState},
};

/// The mixer's own report for one path: the latency it reports there and the
/// lookahead it runs there. Either is None while the app has no fresh report
/// (offline, or nothing applied yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Reported {
    pub latency_samples: Option<u32>,
    pub live_lookahead: Option<u8>,
}

impl Reported {
    /// What the mixer would report for `choice`: its reported figure moved by
    /// whole hops when the choice differs from the live one, so a draft is
    /// shown as the exact latency it would bring.
    pub fn after_applying(self, choice: u8) -> Option<u32> {
        let live = self.live_lookahead?;
        let latency = self.latency_samples?;
        let steps = i64::from(choice) - i64::from(live);
        Some((i64::from(latency) + steps * i64::from(rc2fx_core::DFN_HOP_SAMPLES)).max(0) as u32)
    }

    /// A choice's cost against `reference` (what the mixer runs, or the draft
    /// while it has no report), in samples: +240 for one hop more, −240 for
    /// one less.
    pub fn relative(self, choice: u8, reference: u8) -> i32 {
        (i64::from(choice) - i64::from(reference)) as i32 * rc2fx_core::DFN_HOP_SAMPLES as i32
    }
}

/// The figure beside the lookahead choices: the mixer's reported latency, and
/// what it would be once a different draft is applied. Empty when there is no
/// report to state (the cards show the relative cost then).
fn lookahead_figure(reported: Reported, draft: u8) -> String {
    let Some(after) = reported.after_applying(draft) else {
        return String::new();
    };
    let ms = after as f64 / 48.;
    if reported.live_lookahead == Some(draft) {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.1} ms after applying")
    }
}

/// A card's cost line: "now" for the reference choice, "+5.0 ms" or "−5.0 ms"
/// for the other, and empty while neither a report nor a draft says which way.
fn lookahead_cost(reported: Reported, choice: u8, draft: u8) -> String {
    let reference = reported.live_lookahead.unwrap_or(draft);
    let relative = reported.relative(choice, reference);
    if relative == 0 {
        "now".into()
    } else {
        let ms = f64::from(relative) / 48.;
        format!("{ms:+.1} ms").replace('-', "\u{2212}")
    }
}

/// What a model trades: its character, then whether it is slower than the
/// quickest model.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Parameter {
    Cutoff,
    Blend,
    GateStrength,
    GateThreshold,
    GateReduction,
    GateAttack,
    GateHold,
    GateRelease,
    Gain,
    Threshold,
    Ratio,
    Attack,
    Release,
    Makeup,
    Ceiling,
    LimiterRelease,
    EqInput,
    EqOutput,
    Frequency,
    BandGain,
    Q,
}

pub(super) struct ParameterEditor {
    pub(super) parameter: Parameter,
    label: &'static str,
    input: Entity<InputState>,
    slider: Entity<SliderState>,
    min: f64,
    max: f64,
    log: bool,
    /// Decimal places its step needs.
    pub(super) decimals: usize,
}

/// Decimal places that show a value to `step`.
fn decimals(step: f64) -> usize {
    if step >= 1. {
        0
    } else if step >= 0.1 {
        1
    } else {
        2
    }
}

impl ParameterEditor {
    /// Where `value` sits along this editor's slider, 0 to 1, on its scale.
    pub(super) fn fraction(&self, value: f64) -> f32 {
        let value = value.clamp(self.min, self.max);
        let span = if self.log && self.min > 0. {
            (value / self.min).ln() / (self.max / self.min).ln()
        } else {
            (value - self.min) / (self.max - self.min)
        };
        if span.is_finite() { span as f32 } else { 0. }
    }
}

/// A frequency as the editor shows it: "80 Hz", "3.5 kHz", "12.5 kHz".
fn hz(value: f64) -> String {
    if value >= 1000. {
        let k = value / 1000.;
        let text = if k >= 10. {
            format!("{k:.1}")
        } else {
            format!("{k:.2}")
        };
        format!("{} kHz", text.trim_end_matches('0').trim_end_matches('.'))
    } else {
        format!("{value:.0} Hz")
    }
}

impl Parameter {
    /// A value with its unit, the way the editor shows it.
    pub(super) fn display(self, value: f64) -> String {
        let (_, unit, _, _, step, _) = self.description();
        let text = match self {
            Self::Cutoff | Self::Frequency => hz(value),
            Self::Ratio => format!("{value:.1}:1"),
            Self::Q => format!("{value:.2}"),
            _ => {
                let decimals = decimals(step);
                let number = format!("{value:.decimals$}");
                let signed = if unit == "dB" && value > 0. {
                    format!("+{number}")
                } else {
                    number
                };
                match unit {
                    "%" => format!("{signed}%"),
                    "" => signed,
                    unit => format!("{signed} {unit}"),
                }
            }
        };
        text.replace('-', "\u{2212}")
    }

    /// What a new channel or band starts with (the core's own defaults).
    /// The gate's separate settings return to what its strength stands for.
    pub(super) fn default_value(self, target: &Target) -> f64 {
        let fresh = Target::new("default", "default", target.kind, target.channels);
        let band = Band::new("default");
        let gate = GateShape::for_strength(target.suppression.gate.strength);
        match self {
            Self::Cutoff => fresh.highpass.frequency,
            Self::Blend => fresh.suppression.wet * 100.,
            Self::GateStrength => fresh.suppression.gate.strength * 100.,
            Self::GateThreshold => gate.threshold * 100.,
            Self::GateReduction => gate.reduction_db,
            Self::GateAttack => gate.attack_ms,
            Self::GateHold => gate.hold_ms,
            Self::GateRelease => gate.release_ms,
            Self::Gain => fresh.gain.db,
            Self::Threshold => fresh.compressor.threshold,
            Self::Ratio => fresh.compressor.ratio,
            Self::Attack => fresh.compressor.attack,
            Self::Release => fresh.compressor.release,
            Self::Makeup => fresh.compressor.makeup,
            Self::Ceiling => fresh.limiter.ceiling,
            Self::LimiterRelease => fresh.limiter.release,
            Self::EqInput => fresh.eq.input_gain_db,
            Self::EqOutput => fresh.eq.output_gain_db,
            Self::Frequency => band.frequency,
            Self::BandGain => band.gain,
            Self::Q => band.q,
        }
    }

    fn description(self) -> (&'static str, &'static str, f64, f64, f64, bool) {
        match self {
            Self::Cutoff => ("Cutoff", "Hz", 20., 1000., 1., true),
            Self::Blend => ("Blend", "%", 0., 100., 1., false),
            Self::GateStrength => ("Strength", "%", 0., 100., 1., false),
            Self::GateThreshold => ("Threshold", "%", 0., 100., 1., false),
            Self::GateReduction => ("Reduction", "dB", 0., 60., 0.5, false),
            Self::GateAttack => ("Attack", "ms", 0.5, 50., 0.1, true),
            Self::GateHold => ("Hold", "ms", 0., 2000., 1., false),
            Self::GateRelease => ("Release", "ms", 30., 2000., 1., true),
            Self::Gain => ("Output gain", "dB", -60., 12., 0.1, false),
            Self::Threshold => ("Threshold", "dB", -60., 0., 0.5, false),
            Self::Ratio => ("Ratio", ":1", 1., 20., 0.1, false),
            Self::Attack => ("Attack", "ms", 0.1, 200., 0.1, true),
            Self::Release => ("Release", "ms", 10., 2000., 1., true),
            Self::Makeup => ("Makeup", "dB", 0., 24., 0.1, false),
            Self::Ceiling => ("Ceiling", "dBFS", -24., 0., 0.1, false),
            Self::LimiterRelease => ("Release", "ms", 10., 2000., 1., true),
            Self::EqInput => ("Input gain", "dB", -36., 36., 0.1, false),
            Self::EqOutput => ("Output gain", "dB", -36., 36., 0.1, false),
            Self::Frequency => ("Frequency", "Hz", 10., 24000., 0.1, true),
            Self::BandGain => ("Gain", "dB", -36., 36., 0.1, false),
            Self::Q => ("Q", "", 0.1, 100., 0.01, true),
        }
    }
    fn get(self, target: &Target, channel: Channel, band: usize) -> f64 {
        match self {
            Self::Cutoff => target.highpass.frequency,
            Self::Blend => target.suppression.wet * 100.,
            Self::GateStrength => target.suppression.gate.strength * 100.,
            Self::GateThreshold => target.suppression.gate.threshold * 100.,
            Self::GateReduction => target.suppression.gate.reduction,
            Self::GateAttack => target.suppression.gate.attack,
            Self::GateHold => target.suppression.gate.hold,
            Self::GateRelease => target.suppression.gate.release,
            Self::Gain => target.gain.db,
            Self::Threshold => target.compressor.threshold,
            Self::Ratio => target.compressor.ratio,
            Self::Attack => target.compressor.attack,
            Self::Release => target.compressor.release,
            Self::Makeup => target.compressor.makeup,
            Self::Ceiling => target.limiter.ceiling,
            Self::LimiterRelease => target.limiter.release,
            Self::EqInput => target.eq.input_gain_db,
            Self::EqOutput => target.eq.output_gain_db,
            Self::Frequency => target
                .eq
                .channel(channel)
                .get(band)
                .map_or(1000., |b| b.frequency),
            Self::BandGain => target.eq.channel(channel).get(band).map_or(0., |b| b.gain),
            Self::Q => target.eq.channel(channel).get(band).map_or(1., |b| b.q),
        }
    }
    fn set(self, target: &mut Target, channel: Channel, band: usize, value: f64) {
        match self {
            Self::Cutoff => target.highpass.frequency = value,
            Self::Blend => target.suppression.wet = value / 100.,
            Self::GateStrength => target.suppression.gate.strength = value / 100.,
            Self::GateThreshold => target.suppression.gate.threshold = value / 100.,
            Self::GateReduction => target.suppression.gate.reduction = value,
            Self::GateAttack => target.suppression.gate.attack = value,
            Self::GateHold => target.suppression.gate.hold = value,
            Self::GateRelease => target.suppression.gate.release = value,
            Self::Gain => target.gain.db = value,
            Self::Threshold => target.compressor.threshold = value,
            Self::Ratio => target.compressor.ratio = value,
            Self::Attack => target.compressor.attack = value,
            Self::Release => target.compressor.release = value,
            Self::Makeup => target.compressor.makeup = value,
            Self::Ceiling => target.limiter.ceiling = value,
            Self::LimiterRelease => target.limiter.release = value,
            Self::EqInput => target.eq.input_gain_db = value,
            Self::EqOutput => target.eq.output_gain_db = value,
            Self::Frequency => {
                if let Some(b) = target.eq.channel_mut(channel).get_mut(band) {
                    b.frequency = value
                }
            }
            Self::BandGain => {
                if let Some(b) = target.eq.channel_mut(channel).get_mut(band) {
                    b.gain = value
                }
            }
            Self::Q => {
                if let Some(b) = target.eq.channel_mut(channel).get_mut(band) {
                    b.q = value
                }
            }
        }
    }
}

impl Rc2FxApp {
    pub(super) fn rebuild_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_epoch = self.editor_epoch.wrapping_add(1);
        let epoch = self.editor_epoch;
        self.dragging_band = false;
        self.editing_value = None;
        self.channel = editing::active_channel(self.target(), self.channel);
        self.editors.clear();
        self.subscriptions.clear();
        if self.effect == Effect::Suppression && !self.target().supports_suppression() {
            self.effect = Effect::Eq;
        }
        self.band = self.band.min(
            self.target()
                .eq
                .channel(self.channel)
                .len()
                .saturating_sub(1),
        );
        let parameters = match self.effect {
            Effect::Highpass => vec![Parameter::Cutoff],
            Effect::Suppression => {
                if self.target().suppression.gate.advanced {
                    vec![
                        Parameter::Blend,
                        Parameter::GateThreshold,
                        Parameter::GateReduction,
                        Parameter::GateAttack,
                        Parameter::GateHold,
                        Parameter::GateRelease,
                    ]
                } else {
                    vec![Parameter::Blend, Parameter::GateStrength]
                }
            }
            Effect::Gain => vec![Parameter::Gain],
            Effect::Compressor => vec![
                Parameter::Threshold,
                Parameter::Ratio,
                Parameter::Attack,
                Parameter::Release,
                Parameter::Makeup,
            ],
            Effect::Limiter => vec![Parameter::Ceiling, Parameter::LimiterRelease],
            Effect::Eq => {
                if self.target().eq.channel(self.channel).is_empty() {
                    vec![Parameter::EqInput, Parameter::EqOutput]
                } else {
                    vec![
                        Parameter::Frequency,
                        Parameter::BandGain,
                        Parameter::Q,
                        Parameter::EqInput,
                        Parameter::EqOutput,
                    ]
                }
            }
        };
        for parameter in parameters {
            let (label, _, min, max, step, log) = parameter.description();
            let index = self.editors.len();
            let decimals = decimals(step);
            let value = parameter.get(self.target(), self.channel, self.band);
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(format!("{value:.decimals$}"))
                    .min(min)
                    .max(max)
                    .step(step)
            });
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(min as f32)
                    .max(max as f32)
                    .step(step as f32)
                    .default_value(value as f32)
                    .scale(if log {
                        SliderScale::Logarithmic
                    } else {
                        SliderScale::Linear
                    })
            });
            self.subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |app, state, event, window, cx| {
                    if app.editor_epoch != epoch {
                        return;
                    }
                    match event {
                        // Typing ends with Enter or leaving the field.
                        InputEvent::Blur | InputEvent::PressEnter { .. } => {
                            app.editing_value = None;
                            cx.notify();
                            return;
                        }
                        InputEvent::Change => {}
                        InputEvent::Focus => return,
                    }
                    let Ok(value) = state.read(cx).value().parse::<f64>() else {
                        return;
                    };
                    let Some(editor) = app.editors.get(index) else {
                        return;
                    };
                    if !value.is_finite() || !(editor.min..=editor.max).contains(&value) {
                        return;
                    }
                    let parameter = editor.parameter;
                    let slider = editor.slider.clone();
                    let band = app.band;
                    let channel = app.channel;
                    parameter.set(app.target_mut(), channel, band, value);
                    slider.update(cx, |slider, cx| slider.set_value(value as f32, window, cx));
                    cx.notify();
                },
            ));
            self.subscriptions.push(cx.subscribe_in(
                &slider,
                window,
                move |app, _, event, window, cx| {
                    if app.editor_epoch != epoch {
                        return;
                    }
                    let SliderEvent::Change(value) = event else {
                        return;
                    };
                    let Some(editor) = app.editors.get(index) else {
                        return;
                    };
                    let parameter = editor.parameter;
                    let input = editor.input.clone();
                    let decimals = editor.decimals;
                    let band = app.band;
                    let channel = app.channel;
                    let value = value.start() as f64;
                    parameter.set(app.target_mut(), channel, band, value);
                    input.update(cx, |input, cx| {
                        input.set_value(format!("{value:.decimals$}"), window, cx)
                    });
                    cx.notify();
                },
            ));
            self.editors.push(ParameterEditor {
                parameter,
                label,
                input,
                slider,
                min,
                max,
                log,
                decimals,
            });
        }
    }

    pub(super) fn sync_editor_values(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for editor in &self.editors {
            let value = editor.parameter.get(self.target(), self.channel, self.band);
            let decimals = editor.decimals;
            editor.input.update(cx, |input, cx| {
                input.set_value(format!("{value:.decimals$}"), window, cx)
            });
            editor
                .slider
                .update(cx, |slider, cx| slider.set_value(value as f32, window, cx));
        }
    }

    /// The selected stage: its heading
    /// and switch, its controls, and what the mixer reports beside them. The
    /// EQ lays out its own page.
    pub(super) fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let effect = self.effect;
        if effect == Effect::Eq {
            return div()
                .pt(px(10.))
                .child(self.stage_head(effect, cx))
                .child(self.eq_controls(cx))
                .into_any_element();
        }
        let target = self.target();
        let off = !target.effect_enabled(effect);
        let body = match effect {
            Effect::Suppression => self.suppression_controls(cx).into_any_element(),
            Effect::Compressor | Effect::Limiter => {
                self.controls(|_| true, 2, off, cx).into_any_element()
            }
            _ => self.controls(|_| true, 1, off, cx).into_any_element(),
        };
        div()
            .h_flex()
            .items_start()
            .gap(px(56.))
            .pt(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.stage_head(effect, cx))
                    .child(body),
            )
            .child(self.stage_facts(effect))
            .into_any_element()
    }

    /// Whether another channel holds the one suppression slot this one needs.
    fn suppression_blocker(&self) -> Option<&Target> {
        let target = self.target();
        if self.effect != Effect::Suppression || target.suppression.enabled {
            return None;
        }
        editing::suppression_blocker(
            &self.session.draft,
            self.session
                .saved
                .as_ref()
                .filter(|_| self.session.connected)
                .map(|s| s.config.targets.as_slice()),
            &target.id,
        )
    }

    /// A stage's title and switch, then one line on what it does.
    fn stage_head(&self, effect: Effect, cx: &mut Context<Self>) -> Div {
        let target = self.target();
        let on = target.effect_enabled(effect);
        let blocker = self.suppression_blocker();
        let mut toggle = ui::toggle("effect-enable", on, false, blocker.is_some());
        if blocker.is_none() {
            toggle = toggle.on_click(cx.listener(move |app, _, _, cx| {
                let saved = app
                    .session
                    .saved
                    .as_ref()
                    .filter(|_| app.session.connected)
                    .map(|s| s.config.targets.as_slice());
                editing::enable_effect(&mut app.session.draft, saved, app.selected, effect, !on);
                cx.notify();
            }));
        }
        let description = match effect {
            Effect::Highpass => "Cuts rumble and handling noise below the cutoff.".to_owned(),
            Effect::Suppression if target.is_chat_output() => "Chat voices: combines routed microphones into one mono denoiser before the stereo output effects. Smart Pads and other playback bypass suppression and the voice gate.".to_owned(),
            Effect::Suppression => "Removes background noise from the voice, on the mixer. Limited to one Mic or Chat Mix.".to_owned(),
            Effect::Eq if target.channels == 2 => "Up to 32 bands. Link both sides, or tune left and right separately.".to_owned(),
            Effect::Eq => "Up to 32 bands.".to_owned(),
            Effect::Compressor => "Evens out loud and quiet moments. Above the threshold, the level rises 1 dB for every ratio dB in.".to_owned(),
            Effect::Gain => "A fixed level change at this point in the chain.".to_owned(),
            Effect::Limiter => "A sample-peak ceiling the channel never exceeds.".to_owned(),
        };
        div()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .gap(px(14.))
                    .mb(px(6.))
                    .child(div().text_size(px(20.)).font_semibold().child(effect.label()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(rgb(MUTED))
                            .child(if on { "On" } else { "Off" }),
                    )
                    .child(toggle),
            )
            .child(
                div()
                    .max_w(px(620.))
                    .mb(px(26.))
                    .text_size(px(13.5))
                    .line_height(relative(1.5))
                    .text_color(rgb(FAINT))
                    .child(description)
                    .when_some(blocker, |d, other| {
                        let owner_id = other.id.clone();
                        let needs_release = self.session.draft.iter().any(|target| {
                            target.id == owner_id && target.suppression.enabled
                        });
                        let disabled = !self.session.connected
                            || self.pending.is_some()
                            || self.routing_pending.is_some()
                            || self.presets.busy()
                            || self.transition_busy();
                        d.child(
                            div().mt_2().text_color(rgb(WARN)).child(if needs_release {
                                format!("{} uses the suppression slot. Turn its suppression off and save before enabling it here.", self.target_name(other))
                            } else {
                                "Save to mixer to release the suppression slot before enabling it here.".to_owned()
                            }),
                        )
                        .when(needs_release, |d| {
                            d.child(
                                settings::ghost("release-suppression", "Release suppression slot")
                                    .disabled(disabled)
                                    .on_click(cx.listener(move |app, _, _, cx| {
                                        if !app.session.connected
                                            || app.pending.is_some()
                                            || app.routing_pending.is_some()
                                            || app.presets.busy()
                                            || app.transition_busy()
                                        {
                                            return;
                                        }
                                        editing::release_suppression(&mut app.session.draft, &owner_id);
                                        cx.notify();
                                    })),
                            )
                        })
                    }),
            )
    }

    /// The controls `keep` accepts, `columns` to a row.
    pub(super) fn controls(
        &self,
        keep: impl Fn(Parameter) -> bool,
        columns: usize,
        off: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let items = self
            .editors
            .iter()
            .filter(|editor| keep(editor.parameter))
            .map(|editor| self.control(editor, off, cx).into_any_element())
            .collect();
        ui::grid(columns, items)
    }

    /// One control: its name, its value (click to type), and a thin slider
    /// with the default marked. Double-clicking the slider restores the
    /// default.
    pub(super) fn control(
        &self,
        editor: &ParameterEditor,
        off: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let parameter = editor.parameter;
        let value = parameter.get(self.target(), self.channel, self.band);
        let fraction = editor.fraction(value);
        let default = editor.fraction(parameter.default_value(self.target()));
        let value_view = if self.editing_value == Some(parameter) {
            div()
                .w(px(112.))
                .px(px(6.))
                .mx(px(-6.))
                .rounded(px(6.))
                .bg(rgb(0x0b0e16))
                .border_1()
                .border_color(rgb(ACCENT))
                .text_size(px(15.))
                .font_semibold()
                .child(Input::new(&editor.input).appearance(false))
                .into_any_element()
        } else {
            div()
                .id(("value", parameter as usize))
                .px(px(6.))
                .py(px(2.))
                .mx(px(-6.))
                .my(px(-2.))
                .rounded(px(6.))
                .cursor_text()
                .whitespace_nowrap()
                .text_size(px(15.))
                .font_semibold()
                .text_color(rgb(if off { MUTED } else { TEXT }))
                .hover(|s| s.bg(hsla(0., 0., 1., 0.05)))
                .child(parameter.display(value))
                .on_click(
                    cx.listener(move |app, _, window, cx| app.edit_value(parameter, window, cx)),
                )
                .into_any_element()
        };
        div()
            .mb(px(24.))
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .mb(px(10.))
                    .child(
                        div()
                            .whitespace_nowrap()
                            .text_size(px(13.5))
                            .text_color(rgb(MUTED))
                            .child(editor.label),
                    )
                    .child(value_view),
            )
            .child(
                div()
                    .id(("slider", parameter as usize))
                    .relative()
                    .h(px(24.))
                    .w_full()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                paint_track(window, bounds, fraction, default, off)
                            },
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .child(
                        Slider::new(&editor.slider)
                            .accessibility_label(editor.label)
                            .bg(transparent_black())
                            .text_color(rgb(if off { 0x9aa2b2 } else { 0xffffff })),
                    )
                    // Read in the capture phase: the slider owns the press.
                    .capture_any_mouse_up(cx.listener(
                        move |app, event: &MouseUpEvent, window, cx| {
                            if event.button == MouseButton::Left && event.click_count == 2 {
                                app.reset_value(parameter, window, cx);
                            }
                        },
                    )),
            )
    }

    /// Turns a value into a text field, with its number selected.
    fn edit_value(&mut self, parameter: Parameter, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editors.iter().find(|e| e.parameter == parameter) else {
            return;
        };
        let value = parameter.get(self.target(), self.channel, self.band);
        let decimals = editor.decimals;
        editor.input.update(cx, |input, cx| {
            input.set_value(format!("{value:.decimals$}"), window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.editing_value = Some(parameter);
        cx.notify();
    }

    /// Restores a control's default (the core's defaults for a new channel
    /// or band).
    fn reset_value(&mut self, parameter: Parameter, window: &mut Window, cx: &mut Context<Self>) {
        let value = parameter.default_value(self.target());
        let (channel, band) = (self.channel, self.band);
        parameter.set(self.target_mut(), channel, band, value);
        self.sync_editor_values(window, cx);
        cx.notify();
    }

    /// Noise suppression: the model, its lookahead and blend, then the voice
    /// gate, as one slider or its separate settings.
    fn suppression_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target();
        let off = !target.suppression.enabled;
        let custom = target.suppression.gate.advanced;
        let mut page = div();
        {
            let lookahead = target.suppression.lookahead;
            let reported = self.reported_lookahead();
            let mut choices = ui::pills();
            for (index, choice) in rc2fx_core::LOOKAHEADS.into_iter().enumerate() {
                let cost = lookahead_cost(reported, choice, lookahead);
                let name = if choice == 0 { "Off" } else { "On" };
                let label = if cost == "now" {
                    name.to_owned()
                } else {
                    format!("{name} · {cost}")
                };
                choices = choices.child(
                    ui::pill(("lookahead", index), label, choice == lookahead).on_click(
                        cx.listener(move |app, _, _, cx| {
                            app.target_mut().suppression.select_lookahead(choice);
                            cx.notify();
                        }),
                    ),
                );
            }
            let figure = lookahead_figure(reported, lookahead);
            let mut note = if lookahead == 0 {
                "Lowest delay. Turn on for slightly cleaner word onsets, at one hop (5 ms) more delay.".to_owned()
            } else {
                "One hop of context: can sound cleaner at the start of words, and adds 5 ms to this channel.".to_owned()
            };
            if !figure.is_empty() {
                note = format!("{note} Reported delay: {figure}.");
            }
            page = page.child(
                div()
                    .mb(px(24.))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .text_color(rgb(MUTED))
                                    .child("Lookahead"),
                            )
                            .child(choices),
                    )
                    .child(ui::hint(note).mt(px(8.))),
            );
        }
        let gate_mode = ui::pills()
            .child(
                ui::pill("voice-gate-simple", "Simple", !custom).on_click(cx.listener(
                    |app, _, window, cx| {
                        app.target_mut().suppression.gate.set_advanced(false);
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    },
                )),
            )
            .child(
                ui::pill("voice-gate-custom", "Custom", custom).on_click(cx.listener(
                    |app, _, window, cx| {
                        app.target_mut().suppression.gate.set_advanced(true);
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    },
                )),
            );
        if let Some(blend) = self
            .editors
            .iter()
            .find(|e| e.parameter == Parameter::Blend)
        {
            page = page.child(self.control(blend, off, cx).child(ui::hint(
                "How much of the cleaned voice you hear. Lower blends the original back in.",
            )));
        }
        page = page.child(div().mt(px(34.)).child(ui::section(
            "Voice gate",
            Some(gate_mode.into_any_element()),
        )));
        if custom {
            page = page.child(self.controls(|p| p != Parameter::Blend, 2, off, cx));
        } else if let Some(strength) = self
            .editors
            .iter()
            .find(|e| e.parameter == Parameter::GateStrength)
        {
            page = page.child(self.control(strength, off, cx).child(ui::hint(
                if target.suppression.gate.strength == 0. {
                    "Off. Raise it to turn the channel down between words."
                } else {
                    "Turns the channel down when the model hears no voice. One slider sets threshold, reduction and timing together."
                },
            )));
        }
        page
    }

    /// What the mixer reports for this channel, beside the selected stage.
    pub(super) fn stage_facts(&self, effect: Effect) -> Div {
        let fresh = self.fresh_meter();
        let live = fresh.is_some();
        let synthetic = fresh.is_some_and(|(_, s)| s);
        let m = fresh.map(|(m, _)| m);
        let peak = |f: fn(&TargetMeter) -> f64| m.map_or(0., f);
        let input = ui::Fact::new("Input", ui::dbfs(peak(|m| m.input_peak)))
            .bar(desk::level(peak(|m| m.input_peak)));
        let output = ui::Fact::new("Output", ui::dbfs(peak(|m| m.output_peak)))
            .bar(desk::level(peak(|m| m.output_peak)));
        let clipped = ui::Fact::new("Clipped", format!("{}", m.map_or(0, |m| m.clipped)));
        let target = self.target();
        let (rows, extra) = match effect {
            Effect::Suppression => {
                let latency = m.map_or(0, |m| m.latency_samples);
                (
                    vec![
                        ui::Fact::new("Voice heard", format!("{:.0}%", peak(|m| m.voice) * 100.))
                            .bar(peak(|m| m.voice) as f32),
                        ui::Fact::new(
                            "Gate",
                            if target.suppression.gate.active() {
                                format!("open · {:.0}%", peak(|m| m.gate_open) * 100.)
                            } else {
                                "off".into()
                            },
                        ),
                        ui::Fact::new(
                            "Delay on this channel",
                            if latency > 0 {
                                format!("{:.1} ms", f64::from(latency) / 48.)
                            } else {
                                "not reported".into()
                            },
                        ),
                        ui::Fact::new(
                            "Missed frames",
                            format!("{}", m.map_or(0, |m| m.missed_deadlines)),
                        ),
                        clipped,
                    ],
                    None,
                )
            }
            Effect::Compressor => (
                vec![
                    ui::Fact::new(
                        "Gain reduction",
                        format!("{:.1} dB", peak(|m| m.gain_reduction).abs()),
                    )
                    .bar((peak(|m| m.gain_reduction).abs() / 24.) as f32),
                    input,
                    output,
                ],
                Some(
                    compressor_curve(&target.compressor, m.map(|m| m.input_peak))
                        .into_any_element(),
                ),
            ),
            Effect::Limiter => (vec![output, clipped], None),
            _ => (vec![input, output, clipped], None),
        };
        ui::facts(live, synthetic, rows, extra)
    }
}

/// The slider's track, its gradient fill up to the value and a tick at the
/// default. Painted under the toolkit slider, whose own track is clear.
fn paint_track(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    fraction: f32,
    default: f32,
    off: bool,
) {
    let width = f32::from(bounds.size.width);
    let mid = bounds.origin.y + bounds.size.height / 2.;
    let bar = |from: f32, to: f32| {
        Bounds::new(
            point(bounds.origin.x + px(from * width), mid - px(2.)),
            size(px(((to - from) * width).max(0.)), px(4.)),
        )
    };
    window.paint_quad(quad(
        bar(0., 1.),
        px(2.),
        rgb(0x232838),
        px(0.),
        transparent_black(),
        BorderStyle::Solid,
    ));
    window.paint_quad(quad(
        Bounds::new(
            point(bounds.origin.x + px(default * width - 1.), mid - px(6.)),
            size(px(2.), px(12.)),
        ),
        px(1.),
        rgb(ui::DIM),
        px(0.),
        transparent_black(),
        BorderStyle::Solid,
    ));
    let fill: Background = if off {
        rgb(0x3a4152).into()
    } else {
        ui::gradient()
    };
    window.paint_quad(quad(
        bar(0., fraction.clamp(0., 1.)),
        px(2.),
        fill,
        px(0.),
        transparent_black(),
        BorderStyle::Solid,
    ));
}

/// The compressor's static curve, calculated from its settings, with the
/// reported input level marked on it.
fn compressor_curve(compressor: &Compressor, input_peak: Option<f64>) -> impl IntoElement {
    let (threshold, ratio, makeup) = (
        compressor.threshold,
        compressor.ratio.max(1.),
        compressor.makeup,
    );
    let out = move |db: f64| {
        (if db < threshold {
            db
        } else {
            threshold + (db - threshold) / ratio
        } + makeup)
            .min(0.)
    };
    let live = input_peak.map(|p| (20. * p.max(1e-9).log10()).clamp(-60., 0.));
    let size_px = 220.;
    let margin = 24.;
    div().v_flex().gap_2().mt(px(6.)).child(
        div()
            .relative()
            .size(px(size_px))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let span = size_px - 2. * margin;
                        let at = |x_db: f64, y_db: f64| {
                            point(
                                bounds.origin.x + px(margin + ((x_db + 60.) / 60.) as f32 * span),
                                bounds.origin.y
                                    + px(size_px - margin - ((y_db + 60.) / 60.) as f32 * span),
                            )
                        };
                        let line = |from: Point<Pixels>, to: Point<Pixels>, width: f32| {
                            let mut path = PathBuilder::stroke(px(width));
                            path.move_to(from);
                            path.line_to(to);
                            path.build().ok()
                        };
                        for (a, b) in [
                            ((-60., -60.), (0., -60.)),
                            ((0., -60.), (0., 0.)),
                            ((0., 0.), (-60., 0.)),
                            ((-60., 0.), (-60., -60.)),
                        ] {
                            if let Some(p) = line(at(a.0, a.1), at(b.0, b.1), 1.) {
                                window.paint_path(p, rgb(surface().line));
                            }
                        }
                        if let Some(p) = line(at(-60., -60.), at(0., 0.), 1.) {
                            window.paint_path(p, rgb(surface().line_strong));
                        }
                        if let Some(p) = line(at(threshold, -60.), at(threshold, 0.), 1.) {
                            window.paint_path(p, rgb(surface().line_strong));
                        }
                        let mut curve = PathBuilder::stroke(px(2.2));
                        for i in 0..=60 {
                            let db = -60. + i as f64;
                            let p = at(db, out(db));
                            if i == 0 {
                                curve.move_to(p)
                            } else {
                                curve.line_to(p)
                            }
                        }
                        if let Ok(path) = curve.build() {
                            window.paint_path(path, ui::gradient());
                        }
                        if let Some(db) = live {
                            let p = at(db, out(db));
                            window.paint_quad(quad(
                                Bounds::new(p - point(px(5.), px(5.)), size(px(10.), px(10.))),
                                px(5.),
                                rgb(0xffffff),
                                px(0.),
                                transparent_black(),
                                BorderStyle::Solid,
                            ));
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .child(
                div()
                    .absolute()
                    .left(px(margin))
                    .bottom(px(4.))
                    .text_size(px(11.))
                    .text_color(rgb(FAINT))
                    .child("in"),
            )
            .child(
                div()
                    .absolute()
                    .right(px(margin))
                    .top(px(4.))
                    .text_size(px(11.))
                    .text_color(rgb(FAINT))
                    .child("out"),
            )
            .child(
                div()
                    .absolute()
                    .left(px(margin
                        + ((threshold + 60.) / 60.) as f32 * (size_px - 2. * margin)
                        + 5.))
                    .bottom(px(margin + 4.))
                    .text_size(px(11.))
                    .text_color(rgb(FAINT))
                    .child(Parameter::Threshold.display(threshold)),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{Reported, lookahead_cost, lookahead_figure};

    fn reported(latency_samples: Option<u32>, live_lookahead: Option<u8>) -> Reported {
        Reported {
            latency_samples,
            live_lookahead,
        }
    }

    #[test]
    fn a_draft_states_the_latency_it_would_bring_from_the_mixers_own_report() {
        // The mixer runs LA0 and reports 920 samples (19.2 ms): the helper
        // build's total, model plus lead plus one mixer period.
        let la0 = reported(Some(920), Some(0));
        assert_eq!(lookahead_figure(la0, 0), "19.2 ms");
        assert_eq!(lookahead_figure(la0, 1), "24.2 ms after applying");
        // And the other way: running LA1 at 1160 (24.2 ms).
        let la1 = reported(Some(1160), Some(1));
        assert_eq!(lookahead_figure(la1, 1), "24.2 ms");
        assert_eq!(lookahead_figure(la1, 0), "19.2 ms after applying");
        // One hop is exactly 240 samples in either figure.
        assert_eq!(la0.after_applying(1), Some(1160));
        assert_eq!(la1.after_applying(0), Some(920));
    }

    #[test]
    fn without_a_report_only_the_relative_cost_is_stated() {
        let offline = reported(None, None);
        assert_eq!(lookahead_figure(offline, 0), "");
        assert_eq!(lookahead_figure(offline, 1), "");
        // Offline the draft itself is the reference: more context costs 5 ms,
        // the lowest latency saves it.
        assert_eq!(lookahead_cost(offline, 0, 0), "now");
        assert_eq!(lookahead_cost(offline, 1, 0), "+5.0 ms");
        assert_eq!(lookahead_cost(offline, 0, 1), "\u{2212}5.0 ms");
        // A live report decides the reference even when a draft differs.
        let live = reported(Some(920), Some(0));
        assert_eq!(lookahead_cost(live, 1, 1), "+5.0 ms");
        assert_eq!(lookahead_cost(live, 0, 0), "now");
        let live = reported(Some(1160), Some(1));
        assert_eq!(lookahead_cost(live, 0, 1), "\u{2212}5.0 ms");
        assert_eq!(lookahead_cost(live, 1, 0), "now");
        // A report without a latency (the path reports 0 while idle) still
        // states the relative cost.
        let idle = reported(None, Some(0));
        assert_eq!(lookahead_cost(idle, 1, 0), "+5.0 ms");
        assert_eq!(lookahead_figure(idle, 1), "");
    }
}
