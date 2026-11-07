//! Desk visual language: theme, channel list rows and the signal-flow lane.

use super::*;

/// A band's color: eight steps from RC2FX cyan to violet, repeating. Amber
/// and red stay reserved for problems.
pub(super) fn band_color(index: usize) -> u32 {
    let t = (index % 8) as f32 / 7.;
    let mix = |from: u32, to: u32, shift: u32| {
        let a = ((from >> shift) & 0xff) as f32;
        let b = ((to >> shift) & 0xff) as f32;
        ((a + (b - a) * t).round() as u32) << shift
    };
    mix(ACCENT, VIOLET, 16) | mix(ACCENT, VIOLET, 8) | mix(ACCENT, VIOLET, 0)
}

/// The window's surfaces for one appearance. Text, accent and problem colors
/// are shared (main.rs).
pub(super) struct Surface {
    pub bg: u32,
    pub sunken: u32,
    pub panel: u32,
    pub raised: u32,
    pub raised_hover: u32,
    pub selected: u32,
    pub line: u32,
    pub line_strong: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Appearance {
    /// Deep blue-grey, the default.
    Dusk,
    /// Neutral dark grey.
    Graphite,
    /// Near black.
    Black,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::Dusk, Self::Graphite, Self::Black];

    pub fn key(self) -> &'static str {
        match self {
            Self::Dusk => "dusk",
            Self::Graphite => "graphite",
            Self::Black => "black",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dusk => "Dusk",
            Self::Graphite => "Graphite",
            Self::Black => "Black",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }

    fn surface(self) -> &'static Surface {
        match self {
            Self::Dusk => &DUSK,
            Self::Graphite => &GRAPHITE,
            Self::Black => &BLACK,
        }
    }
}

const DUSK: Surface = Surface {
    bg: 0x131622,
    sunken: 0x0f121b,
    panel: 0x181c2a,
    raised: 0x1f2434,
    raised_hover: 0x272d40,
    selected: 0x1e2438,
    line: 0x232839,
    line_strong: 0x2e3448,
};
const GRAPHITE: Surface = Surface {
    bg: 0x15171c,
    sunken: 0x101216,
    panel: 0x1a1d23,
    raised: 0x22252c,
    raised_hover: 0x2a2e36,
    selected: 0x252830,
    line: 0x262930,
    line_strong: 0x31353e,
};
const BLACK: Surface = Surface {
    bg: 0x050608,
    sunken: 0x030405,
    panel: 0x0b0d11,
    raised: 0x12151b,
    raised_hover: 0x1a1e26,
    selected: 0x141820,
    line: 0x16191f,
    line_strong: 0x22262e,
};

/// The current appearance, as an index into `Appearance::ALL`. Read on every
/// render, written only by `set_appearance`.
static APPEARANCE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub(super) fn appearance() -> Appearance {
    let i = APPEARANCE.load(std::sync::atomic::Ordering::Relaxed) as usize;
    Appearance::ALL[i.min(Appearance::ALL.len() - 1)]
}

pub(super) fn surface() -> &'static Surface {
    appearance().surface()
}

/// Switches the window's surfaces and the components' theme to match.
pub(super) fn set_appearance(appearance: Appearance, cx: &mut App) {
    let i = Appearance::ALL
        .iter()
        .position(|a| *a == appearance)
        .unwrap_or(0);
    APPEARANCE.store(i as u8, std::sync::atomic::Ordering::Relaxed);
    apply_theme(cx);
}

pub(super) fn apply_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.font_size = px(ui::ROOT_TEXT);
        let c = |hex: u32| -> Hsla { rgb(hex).into() };
        theme.colors.background = c(surface().bg);
        theme.colors.foreground = c(TEXT);
        theme.colors.border = c(surface().line);
        theme.colors.input = c(surface().line_strong);
        theme.colors.muted = c(surface().raised);
        theme.colors.muted_foreground = c(MUTED);
        // Hovered menu and list items; distinct from the popover behind them.
        theme.colors.accent = c(surface().raised_hover);
        theme.colors.accent_foreground = c(TEXT);
        theme.colors.popover = c(surface().panel);
        theme.colors.popover_foreground = c(TEXT);
        // Switches, checkboxes, tabs and focus use `primary`: RC2FX cyan.
        theme.colors.primary = c(ACCENT);
        theme.colors.primary_hover = c(ACCENT_HOVER);
        theme.colors.primary_active = c(ACCENT_ACTIVE);
        theme.colors.primary_foreground = c(surface().bg);
        // Primary buttons are white pills; cyan marks processing.
        theme.colors.button_primary = c(TEXT);
        theme.colors.button_primary_hover = c(PRIMARY_HOVER);
        theme.colors.button_primary_active = c(PRIMARY_ACTIVE);
        theme.colors.button_primary_foreground = c(surface().bg);
        theme.colors.button = c(surface().raised);
        theme.colors.button_hover = c(surface().raised_hover);
        theme.colors.button_active = c(surface().line_strong);
        theme.colors.button_foreground = c(TEXT);
        theme.colors.secondary = c(surface().raised);
        theme.colors.secondary_hover = c(surface().raised_hover);
        theme.colors.secondary_active = c(surface().line_strong);
        theme.colors.secondary_foreground = c(TEXT);
        theme.colors.ring = c(ACCENT);
        theme.colors.caret = c(ACCENT);
        theme.colors.selection = rgba(0x5ce1e640).into();
        theme.colors.list_active = c(surface().selected);
        theme.colors.switch = c(surface().line_strong);
        theme.colors.slider_bar = c(ACCENT);
        theme.colors.slider_thumb = c(TEXT);
        theme.colors.scrollbar_thumb = c(surface().line_strong);
        theme.radius = px(8.);
        theme.radius_lg = px(14.);
    });
}

/// Peak amplitude as a 0..1 meter position over a 60 dB range.
pub(super) fn level(peak: f64) -> f32 {
    ((20. * peak.max(1e-6).log10() + 60.) / 60.).clamp(0., 1.) as f32
}

pub(super) fn format_hz(frequency: f64) -> String {
    if frequency >= 1000. {
        format!("{:.1} kHz", frequency / 1000.)
    } else {
        format!("{frequency:.0} Hz")
    }
}

/// Effects in signal order, without suppression on targets that cannot use it.
/// A stage card being dragged to a new place in the chain; also its
/// preview under the pointer.
#[derive(Clone)]
pub(super) struct DraggedStage(Effect);

impl Render for DraggedStage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(rgb(surface().raised))
            .border_1()
            .border_color(rgb(ACCENT))
            .text_sm()
            .text_color(rgb(TEXT))
            .child(self.0.label())
    }
}

/// Two columns of dots marking something as draggable.
pub(super) fn grip() -> impl IntoElement {
    let column = || {
        div()
            .v_flex()
            .gap(px(2.))
            .children((0..3).map(|_| div().size(px(2.)).rounded_full().bg(rgb(MUTED))))
    };
    div()
        .h_flex()
        .flex_shrink_0()
        .gap(px(2.))
        .px(px(3.))
        .cursor_grab()
        .child(column())
        .child(column())
}

/// A short effect name for the channel list.
pub(super) fn short_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::Highpass => "HPF",
        Effect::Suppression => "Noise",
        Effect::Eq => "EQ",
        Effect::Compressor => "Comp",
        Effect::Gain => "Gain",
        Effect::Limiter => "Limiter",
    }
}

pub(super) fn visible_effects(target: &Target) -> Vec<Effect> {
    let mut effects: Vec<_> = target
        .order
        .iter()
        .copied()
        .filter(|effect| target.supports_suppression() || *effect != Effect::Suppression)
        .collect();
    if target.is_chat_output() {
        // The voice branch is before the stereo sum, not a movable output stage.
        effects.retain(|effect| *effect != Effect::Suppression);
        effects.insert(0, Effect::Suppression);
    }
    effects
}

fn stage_summary(target: &Target, effect: Effect, channel: Channel) -> String {
    match effect {
        Effect::Suppression => format!(
            "{:.0}% blend{}",
            target.suppression.wet * 100.,
            if target.suppression.gate.active() {
                " · gate"
            } else {
                ""
            }
        ),
        Effect::Gain => format!("{:+.1} dB", target.gain.db),
        Effect::Highpass => format_hz(target.highpass.frequency),
        Effect::Eq => {
            let count = target.eq.channel(channel).len();
            format!("{count} band{}", if count == 1 { "" } else { "s" })
        }
        Effect::Compressor => format!(
            "{:.0} dB · {:.1}:1",
            target.compressor.threshold, target.compressor.ratio
        ),
        Effect::Limiter => format!("{:.1} dBFS", target.limiter.ceiling),
    }
}

/// A notice above the page: rounded, quiet,
/// and amber or red only through its callers' border and title colors.
pub(super) fn card() -> Div {
    div()
        .v_flex()
        .gap_2()
        .mt(px(14.))
        .px(px(20.))
        .py(px(14.))
        .rounded(px(14.))
        .bg(hsla(0., 0., 1., 0.02))
        .border_1()
        .border_color(rgb(surface().line_strong))
}

impl Rc2FxApp {
    /// The channel's live line with its reported levels, then its six stages
    /// in processing order. A stage
    /// shows whether it is on and its key value; drag one onto another to
    /// move it in the chain.
    pub(super) fn flow_lane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target();
        let fresh = self.fresh_meter();
        let readout = |label: &'static str, peak: f64| {
            div().h_flex().gap(px(6.)).child(label).child(
                div()
                    .font_medium()
                    .text_color(rgb(MUTED))
                    .child(ui::dbfs(peak)),
            )
        };
        let line = div()
            .relative()
            .h(px(92.))
            .mt(px(6.))
            .mx(px(-48.))
            .child(
                div()
                    .size_full()
                    .px(px(40.))
                    .when(!self.reduced_motion, |d| {
                        d.child(self.indicator(
                            line_overlay::Source::Path(target.id.clone()),
                            fresh.is_some(),
                            live_line::CHANNEL,
                        ))
                    }),
            )
            .child(
                div()
                    .absolute()
                    .right(px(48.))
                    .top(px(6.))
                    .h_flex()
                    .gap(px(18.))
                    .text_size(px(12.))
                    .text_color(rgb(FAINT))
                    .map(|d| match fresh {
                        Some((m, _)) => d
                            .child(readout("In", m.input_peak))
                            .child(readout("Out", m.output_peak)),
                        None => d.child("No fresh level"),
                    }),
            );
        let mut lane = div()
            .h_flex()
            .w_full()
            .border_t_1()
            .border_b_1()
            .border_color(rgb(surface().line));
        let effects = visible_effects(target);
        let count = effects.len();
        for (index, effect) in effects.into_iter().enumerate() {
            let on = target.effect_enabled(effect) && !target.bypass;
            let selected = self.effect == effect;
            let cell = div()
                .id(("stage", effect as usize))
                .group("stage-cell")
                .relative()
                .flex_1()
                .flex_basis(px(0.))
                .min_w_0()
                .px(px(16.))
                .pt(px(15.))
                .pb(px(14.))
                .cursor_pointer()
                .when(index + 1 < count, |c| {
                    c.border_r_1().border_color(rgb(surface().line))
                })
                .when(!selected, |c| c.hover(|s| s.bg(hsla(0., 0., 1., 0.025))))
                .child(
                    div()
                        .absolute()
                        .left(px(2.))
                        .top(px(22.))
                        .opacity(0.)
                        .group_hover("stage-cell", |s| s.opacity(1.))
                        .child(grip()),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .whitespace_nowrap()
                        .text_size(px(12.5))
                        .text_color(rgb(if selected { TEXT } else { FAINT }))
                        .child(ui::dot(on, 6.))
                        .child(div().min_w_0().truncate().child(effect.label())),
                )
                .child(div().mt(px(7.)).truncate().text_size(px(16.)).map(|v| {
                    if on {
                        v.font_semibold()
                            .child(stage_summary(target, effect, self.channel))
                    } else {
                        v.font_medium()
                            .text_color(rgb(ui::DIM))
                            .child(if target.bypass { "Bypassed" } else { "Off" })
                    }
                }))
                .when(selected, |c| {
                    c.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom(px(-1.))
                            .h(px(2.))
                            .bg(ui::gradient()),
                    )
                })
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.effect = effect;
                    app.rebuild_editors(window, cx);
                    cx.notify();
                }))
                // Drag a stage onto another to move it there in the chain.
                .on_drag(DraggedStage(effect), |stage, _, _, cx| {
                    cx.new(|_| stage.clone())
                })
                .drag_over::<DraggedStage>(|style, _, _, _| style.bg(rgb(surface().raised_hover)))
                .on_drop(cx.listener(move |app, dragged: &DraggedStage, _, cx| {
                    editing::place_effect(app.target_mut(), dragged.0, effect);
                    cx.notify();
                }));
            lane = lane.child(cell);
        }
        div().child(line).child(lane)
    }
}
