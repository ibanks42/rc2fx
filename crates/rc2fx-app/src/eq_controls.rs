//! The EQ page: the graph, the band
//! row, the selected band's editor below the graph, and the overall gains.

use super::*;
use crate::editor::Parameter;
use gpui_kit::component::tooltip::Tooltip;

/// What each filter type is set by.
pub(super) fn uses(kind: BandType) -> &'static [Parameter] {
    match kind {
        BandType::Bell | BandType::LowShelf | BandType::HighShelf => {
            &[Parameter::Frequency, Parameter::BandGain, Parameter::Q]
        }
        BandType::LowPass | BandType::HighPass | BandType::Notch => {
            &[Parameter::Frequency, Parameter::Q]
        }
        BandType::Off => &[],
    }
}

/// Whether a band's height on the graph is its gain.
pub(super) fn has_gain(kind: BandType) -> bool {
    uses(kind).contains(&Parameter::BandGain)
}

/// The types offered as pills: every type but Off (the band's switch turns
/// a band off; Off as a type only comes from imported files).
const TYPES: [BandType; 6] = [
    BandType::Bell,
    BandType::LowShelf,
    BandType::HighShelf,
    BandType::LowPass,
    BandType::HighPass,
    BandType::Notch,
];

impl Rc2FxApp {
    pub(super) fn eq_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self.target();
        let bands = target.eq.channel(self.channel);
        let mut page = div().child(self.eq_bar(cx));
        if let Some(error) = &self.import_error {
            page = page.child(
                div()
                    .mb(px(12.))
                    .p_3()
                    .rounded(px(10.))
                    .bg(rgb(DANGER_BG))
                    .text_sm()
                    .child(error.clone()),
            );
        }
        if let Some(import) = &self.import {
            page = page.child(div().mb(px(12.)).child(self.import_panel(import, cx)));
        }
        page = page
            .child(self.eq_graph(cx))
            .child(self.band_strip(bands, cx));
        if let Some(band) = bands.get(self.band) {
            page = page.child(self.band_editor(band, cx));
        }
        page.child(
            div()
                .h_flex()
                .items_start()
                .gap(px(56.))
                .pt(px(28.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(ui::section("Overall", None))
                        .child(self.controls(
                            |p| matches!(p, Parameter::EqInput | Parameter::EqOutput),
                            2,
                            !target.eq.enabled,
                            cx,
                        )),
                )
                .child(self.stage_facts(Effect::Eq)),
        )
        .into_any_element()
    }

    /// Above the graph: the stereo sides, how to edit, and EQ files.
    fn eq_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target();
        let file = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_shrink_0()
                .text_size(px(13.))
                .text_color(rgb(MUTED))
                .when(self.file_busy, |d| d.opacity(0.4))
                .when(!self.file_busy, |d| {
                    d.cursor_pointer().hover(|s| s.text_color(rgb(TEXT)))
                })
                .child(label)
        };
        div()
            .h_flex()
            .items_center()
            .gap_3()
            .mb(px(12.))
            .when(target.channels == 2, |bar| bar.child(self.eq_sides(cx)))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(FAINT))
                    .truncate()
                    .child("Drag a point · scroll on it for Q · double-click to add a band"),
            )
            .child(div().flex_1())
            .child(
                file("import-eq", "Import…").on_click(cx.listener(|app, _, window, cx| {
                    if !app.file_busy {
                        app.import_eq(window, cx)
                    }
                })),
            )
            .child(
                file("paste-eq", "Paste").on_click(cx.listener(|app, _, _, cx| {
                    if !app.file_busy {
                        app.paste_eq(cx)
                    }
                })),
            )
            .child(
                file("export-eq", "Export…").on_click(cx.listener(|app, _, window, cx| {
                    if !app.file_busy {
                        app.export_eq(window, cx)
                    }
                })),
            )
    }

    /// Both sides (one EQ for left and right), or each side on its own.
    /// Rejoining two different sides keeps one of them, so it asks which.
    fn eq_sides(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let linked = self.target().eq.right_bands.is_none();
        let side = |id: &'static str, label: &'static str, channel: Channel| {
            ui::pill(id, label, !linked && self.channel == channel).on_click(cx.listener(
                move |app, _, window, cx| {
                    editing::split(app.target_mut());
                    app.channel = channel;
                    app.band = 0;
                    app.rebuild_editors(window, cx);
                    cx.notify();
                },
            ))
        };
        let both = if linked {
            ui::pill("eq-both", "Both sides", true).into_any_element()
        } else {
            let app = cx.entity().downgrade();
            Button::new("eq-both")
                .ghost()
                .xsmall()
                .label("Both sides")
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for (keep, label) in [
                        (Channel::Left, "Use the left side for both"),
                        (Channel::Right, "Use the right side for both"),
                    ] {
                        let app = app.clone();
                        menu =
                            menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                let _ = app.update(cx, |app, cx| {
                                    editing::link(app.target_mut(), keep);
                                    app.channel = Channel::Left;
                                    app.band = 0;
                                    app.rebuild_editors(window, cx);
                                    cx.notify();
                                });
                            }));
                    }
                    menu
                })
                .into_any_element()
        };
        ui::pills()
            .child(both)
            .child(side("eq-left", "Left", Channel::Left))
            .child(side("eq-right", "Right", Channel::Right))
    }

    /// One pill per band (number, type, frequency and gain) and the add
    /// button. Click to select; drag one onto another to reorder.
    fn band_strip(&self, bands: &[Band], cx: &mut Context<Self>) -> impl IntoElement {
        let mut strip = div()
            .h_flex()
            .flex_wrap()
            .gap(px(6.))
            .mt(px(14.))
            .mb(px(18.));
        for (index, band) in bands.iter().enumerate() {
            let selected = index == self.band;
            let summary = band_summary(band);
            strip = strip.child(
                div()
                    .id(("band", index))
                    .h_flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .pl(px(6.))
                    .pr(px(12.))
                    .rounded_full()
                    .border_1()
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .when(selected, |p| {
                        p.bg(hsla(0., 0., 1., 0.07))
                            .border_color(rgb(0x3b4560))
                            .text_color(rgb(TEXT))
                    })
                    .when(!selected, |p| {
                        p.border_color(rgb(surface().line_strong))
                            .text_color(rgb(MUTED))
                            .hover(|s| s.border_color(rgb(0x3a4258)).text_color(rgb(TEXT)))
                    })
                    .when(!band.enabled, |p| p.opacity(0.45))
                    .child(band_number(index, 22.))
                    .child(type_icon(
                        band.band_type,
                        if selected { TEXT } else { MUTED },
                    ))
                    .child(short_hz(band.frequency))
                    .when(has_gain(band.band_type), |p| {
                        p.child(div().text_color(rgb(FAINT)).child(short_db(band.gain)))
                    })
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.band = index;
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    }))
                    .on_drag(DraggedBand { index, summary }, |band, _, _, cx| {
                        cx.new(|_| band.clone())
                    })
                    .drag_over::<DraggedBand>(|style, _, _, _| style.border_color(rgb(ACCENT)))
                    .on_drop(cx.listener(move |app, dragged: &DraggedBand, window, cx| {
                        let channel = app.channel;
                        let selected = app
                            .target()
                            .eq
                            .channel(channel)
                            .get(app.band)
                            .map(|b| b.id.clone());
                        editing::place_band(
                            &mut app.target_mut().eq,
                            channel,
                            dragged.index,
                            index,
                        );
                        if let Some(selected) = selected {
                            app.band = app
                                .target()
                                .eq
                                .channel(channel)
                                .iter()
                                .position(|b| b.id == selected)
                                .unwrap_or(app.band);
                        }
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    })),
            );
        }
        let full = bands.len() >= MAX_BANDS;
        strip.child(
            div()
                .id("add-band")
                .h_flex()
                .items_center()
                .h(px(34.))
                .px(px(14.))
                .rounded_full()
                .border_1()
                .border_dashed()
                .border_color(rgb(surface().line_strong))
                .text_size(px(12.5))
                .text_color(rgb(FAINT))
                .when(full, |p| p.opacity(0.4))
                .when(!full, |p| {
                    p.cursor_pointer().hover(|s| s.text_color(rgb(TEXT)))
                })
                .child(if full { "32 bands" } else { "+ Add band" })
                .on_click(cx.listener(move |app, _, window, cx| {
                    if !full {
                        app.add_band(None, window, cx);
                    }
                })),
        )
    }

    /// Adds a bell, at a point on the graph when given (frequency, gain).
    pub(super) fn add_band(
        &mut self,
        at: Option<(f64, f64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let channel = self.channel;
        match self.target_mut().eq.add_band(channel) {
            Ok(band) => {
                if let Some((frequency, gain)) = at {
                    band.frequency = frequency.clamp(10., 24000.);
                    band.gain = gain.clamp(-36., 36.);
                }
            }
            Err(error) => self.message = error.to_string(),
        }
        self.band = self.target().eq.channel(channel).len().saturating_sub(1);
        self.rebuild_editors(window, cx);
        cx.notify();
    }

    /// The selected band, below the graph: its type and filter design,
    /// switches, the settings that type uses, and removal.
    fn band_editor(&self, band: &Band, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = band.band_type;
        let mut types = ui::pills();
        for (index, option) in TYPES.into_iter().enumerate() {
            let on = option == kind;
            types = types.child(
                ui::pill(("band-type", index), option.label(), on)
                    .child(type_icon(option, if on { TEXT } else { MUTED }))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        let (channel, index) = (app.channel, app.band);
                        if let Some(b) = app.target_mut().eq.channel_mut(channel).get_mut(index) {
                            b.band_type = option;
                        }
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    })),
            );
        }
        let switch = |id: &'static str, label: &'static str, on: bool, set: fn(&mut Band, bool)| {
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(rgb(MUTED))
                .child(ui::toggle(id, on, true, false).on_click(cx.listener(
                    move |app, _, _, cx| {
                        let (channel, index) = (app.channel, app.band);
                        if let Some(b) = app.target_mut().eq.channel_mut(channel).get_mut(index) {
                            set(b, !on);
                        }
                        cx.notify();
                    },
                )))
                .child(label)
        };
        let mut design = ui::pills();
        for (index, (option, label)) in
            [(BandMode::ApoDr, "APO (DR)"), (BandMode::RlcBt, "RLC (BT)")]
                .into_iter()
                .enumerate()
        {
            design = design.child(
                ui::pill(("band-mode", index), label, option == band.mode).on_click(cx.listener(
                    move |app, _, window, cx| {
                        let (channel, index) = (app.channel, app.band);
                        if let Some(b) = app.target_mut().eq.channel_mut(channel).get_mut(index) {
                            editing::set_model(b, option);
                        }
                        app.rebuild_editors(window, cx);
                        cx.notify();
                    },
                )),
            );
        }
        let used = uses(kind);
        let id = band.id.clone();
        let mut editor = div()
            .px(px(20.))
            .pt(px(18.))
            .rounded(px(16.))
            .border_1()
            .border_color(rgb(surface().line))
            .bg(hsla(0., 0., 1., 0.015))
            .child(
                div()
                    .h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(14.))
                    .mb(px(20.))
                    .child(band_number(self.band, 26.))
                    .child(types)
                    .child(div().id("band-mode").child(design).tooltip(|window, cx| {
                        Tooltip::new("Filter design: how Q becomes the filter's shape. Imported EQ files set this for you.")
                            .build(window, cx)
                    }))
                    .child(div().flex_1())
                    .child(switch("band-enabled", "On", band.enabled, |b, v| {
                        b.enabled = v
                    }))
                    .child(switch("band-solo", "Solo", band.solo, |b, v| b.solo = v)),
            )
            .child(self.controls(|p| used.contains(&p), used.len().max(1), !band.enabled, cx));
        if kind == BandType::Off {
            editor = editor.child(ui::hint("This band's type is Off, so it passes audio through unchanged. Pick a type to use it.").mt(px(0.)).mb(px(18.)));
        }
        editor = editor.child(
            div()
                .h_flex()
                .items_center()
                .pb(px(14.))
                .child(div().flex_1())
                .child(
                    div()
                        .id("remove-band")
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(rgb(0xc98a8a))
                        .hover(|s| s.text_color(rgb(DANGER)))
                        .child("Remove band")
                        .on_click(cx.listener(move |app, _, window, cx| {
                            let channel = app.channel;
                            app.target_mut().eq.remove_band(channel, &id);
                            app.rebuild_editors(window, cx);
                            cx.notify();
                        })),
                ),
        );
        editor
    }
}

/// A band's number on its color.
fn band_number(index: usize, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(desk::band_color(index)))
        .h_flex()
        .items_center()
        .justify_center()
        .text_size(px(size * 0.48))
        .font_semibold()
        .text_color(rgb(0x0b0d14))
        .child(format!("{}", index + 1))
}

/// The filter type's shape, as a small line.
pub(super) fn type_icon(kind: BandType, color: u32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let shape = |t: f32| -> f32 {
                match kind {
                    BandType::Bell => (-((t - 0.5) / 0.18).powi(2)).exp(),
                    BandType::LowShelf => 1. / (1. + ((t - 0.5) * 12.).exp()),
                    BandType::HighShelf => 1. / (1. + (-(t - 0.5) * 12.).exp()),
                    BandType::LowPass => {
                        if t < 0.5 {
                            0.75
                        } else {
                            0.75 - 0.75 * ((t - 0.5) / 0.5).powf(1.4)
                        }
                    }
                    BandType::HighPass => {
                        if t > 0.5 {
                            0.75
                        } else {
                            0.75 - 0.75 * ((0.5 - t) / 0.5).powf(1.4)
                        }
                    }
                    BandType::Notch => 0.75 - 0.75 * (-((t - 0.5) / 0.07).powi(2)).exp(),
                    BandType::Off => 0.4,
                }
            };
            let mut path = PathBuilder::stroke(px(1.5));
            for i in 0..=24 {
                let t = i as f32 / 24.;
                let p = point(
                    bounds.origin.x + px(1. + t * (w - 2.)),
                    bounds.origin.y + px(h - 2. - shape(t) * (h - 4.)),
                );
                if i == 0 {
                    path.move_to(p)
                } else {
                    path.line_to(p)
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(color));
            }
        },
    )
    .w(px(18.))
    .h(px(14.))
    .flex_shrink_0()
}

/// "90", "3.5k".
fn short_hz(frequency: f64) -> String {
    if frequency >= 1000. {
        let k = format!("{:.1}", frequency / 1000.);
        format!("{}k", k.trim_end_matches('0').trim_end_matches('.'))
    } else {
        format!("{frequency:.0}")
    }
}

/// "+3.2", "−2.4".
fn short_db(gain: f64) -> String {
    if gain > 0. {
        format!("+{gain:.1}")
    } else {
        format!("{gain:.1}").replace('-', "\u{2212}")
    }
}

fn band_summary(band: &Band) -> String {
    format!(
        "{} · {}",
        band.band_type.label(),
        desk::format_hz(band.frequency)
    )
}

/// A band pill being dragged to a new place; also its preview under the
/// pointer.
#[derive(Clone)]
struct DraggedBand {
    index: usize,
    summary: String,
}

impl Render for DraggedBand {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .pl(px(6.))
            .pr(px(12.))
            .h(px(34.))
            .rounded_full()
            .bg(rgb(surface().raised))
            .border_1()
            .border_color(rgb(ACCENT))
            .text_size(px(12.5))
            .text_color(rgb(TEXT))
            .child(band_number(self.index, 22.))
            .child(self.summary.clone())
    }
}
