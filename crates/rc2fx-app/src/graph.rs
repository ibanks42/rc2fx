use super::*;
use crate::editor::Parameter;

const FMIN: f64 = 10.;
const FMAX: f64 = 24000.;
/// The graph's range above and below 0 dB; bands beyond it sit at the edge.
const DB: f64 = 18.;
fn frequency_x(frequency: f64) -> f64 {
    (frequency / FMIN).ln() / (FMAX / FMIN).ln()
}
fn x_frequency(x: f64) -> f64 {
    FMIN * (FMAX / FMIN).powf(x.clamp(0., 1.))
}

const CURVE_POINTS: usize = 241;

/// The selected EQ's response on the graph axis, kept until the EQ changes.
/// Meter updates and hovering redraw the window often; the curve rarely moves.
pub(super) struct EqCurve {
    eq: EqSettings,
    channel: Channel,
    db: Rc<[f64]>,
}

impl Rc2FxApp {
    /// Response in dB at evenly spaced points across the log frequency axis.
    pub(super) fn eq_curve(&self) -> Rc<[f64]> {
        let eq = &self.target().eq;
        let channel = self.channel;
        let mut cache = self.eq_curve.borrow_mut();
        if let Some(curve) = cache.as_ref()
            && curve.channel == channel
            && curve.eq == *eq
        {
            return curve.db.clone();
        }
        let frequencies = (0..CURVE_POINTS)
            .map(|i| x_frequency(i as f64 / (CURVE_POINTS - 1) as f64))
            .collect::<Vec<_>>();
        let db: Rc<[f64]> = rc2fx_core::eq::response_curve(eq, channel, &frequencies, true).into();
        *cache = Some(EqCurve {
            eq: eq.clone(),
            channel,
            db: db.clone(),
        });
        db
    }

    /// Moves the selected band to a point on the graph: frequency across,
    /// and gain up and down for the types that have one.
    fn move_band(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((frequency, gain)) = self.graph_point(position) else {
            return;
        };
        let (index, channel) = (self.band, self.channel);
        if let Some(band) = self.target_mut().eq.channel_mut(channel).get_mut(index) {
            band.frequency = frequency;
            if eq_controls::has_gain(band.band_type) {
                band.gain = gain;
            }
        }
        self.sync_editor_values(window, cx);
        cx.notify();
    }

    /// The frequency and gain at a window position over the graph.
    fn graph_point(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let bounds = self.graph_bounds.get();
        let width = f64::from(f32::from(bounds.size.width));
        let height = f64::from(f32::from(bounds.size.height));
        if width <= 0. || height <= 0. {
            return None;
        }
        let x = f64::from(f32::from(position.x - bounds.origin.x)) / width;
        let y = f64::from(f32::from(position.y - bounds.origin.y)) / height;
        let gain = ((0.5 - y) * 2. * DB).clamp(-36., 36.);
        Some((x_frequency(x), (gain * 10.).round() / 10.))
    }

    /// The response, its bands as numbered points, and the grid. Drag a
    /// point; scroll on it for Q; double-click empty space to add a band.
    pub(super) fn eq_graph(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let curve = self.eq_curve();
        let bounds_cell = self.graph_bounds.clone();
        let height = |gain: f64| 0.5 - gain.clamp(-DB, DB) / (2. * DB);
        let graph = canvas(
            move |bounds, _, _| bounds_cell.set(bounds),
            move |bounds, _, window, _| {
                let width = f32::from(bounds.size.width);
                let tall = f32::from(bounds.size.height);
                let at = |x: f64, y: f64| {
                    point(
                        bounds.origin.x + px(x as f32 * width),
                        bounds.origin.y + px(y as f32 * tall),
                    )
                };
                let mut rule = |from: Point<Pixels>, to: Point<Pixels>, color: u32| {
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(from);
                    line.line_to(to);
                    if let Ok(path) = line.build() {
                        window.paint_path(path, rgb(color));
                    }
                };
                for frequency in GRID_HZ {
                    let x = frequency_x(frequency);
                    rule(at(x, 0.), at(x, 1.), 0x1c2133);
                }
                for db in [12., 6., -6., -12.] {
                    rule(at(0., height(db)), at(1., height(db)), 0x181d2c);
                }
                rule(at(0., 0.5), at(1., 0.5), surface().line_strong);
                let last = (curve.len().max(2) - 1) as f64;
                let points: Vec<_> = curve
                    .iter()
                    .enumerate()
                    .map(|(i, db)| at(i as f64 / last, height(*db)))
                    .collect();
                let mut area = PathBuilder::fill();
                area.move_to(at(0., 0.5));
                for p in &points {
                    area.line_to(*p);
                }
                area.line_to(at(1., 0.5));
                area.close();
                if let Ok(path) = area.build() {
                    window.paint_path(path, rgba(0x7aa8f01f));
                }
                let mut line = PathBuilder::stroke(px(2.5));
                for (i, p) in points.iter().enumerate() {
                    if i == 0 {
                        line.move_to(*p)
                    } else {
                        line.line_to(*p)
                    }
                }
                if let Ok(path) = line.build() {
                    window.paint_path(path, ui::gradient());
                }
            },
        )
        .absolute()
        .inset_0();
        let label = |text: String| {
            div()
                .absolute()
                .text_size(px(10.5))
                .text_color(rgb(ui::DIM))
                .child(text)
        };
        let mut plot = div()
            .id("eq-plot")
            .relative()
            .h(px(260.))
            .w_full()
            .rounded(px(14.))
            .border_1()
            .border_color(rgb(surface().line))
            .bg(hsla(0.62, 0.4, 0.03, 0.35))
            .cursor_crosshair()
            .child(graph)
            .children(GRID_HZ.into_iter().map(|frequency| {
                let x = frequency_x(frequency) as f32;
                let text = if frequency >= 1000. {
                    format!("{}k", frequency / 1000.)
                } else {
                    format!("{frequency}")
                };
                let l = label(text).bottom(px(8.));
                if frequency >= 20000. {
                    l.right(px(6.))
                } else {
                    l.left(relative(x)).ml(px(5.))
                }
            }))
            .children([12., 6., -6., -12.].map(|db: f64| {
                label(format!("{db:+.0}").replace('-', "\u{2212}"))
                    .left(px(6.))
                    .top(relative(height(db) as f32))
                    .mt(px(-15.))
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, event: &MouseDownEvent, window, cx| {
                    // Points stop their own presses, so this is empty space.
                    if event.click_count == 2
                        && app.target().eq.channel(app.channel).len() < MAX_BANDS
                        && let Some(at) = app.graph_point(event.position)
                    {
                        app.add_band(Some(at), window, cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|app, event: &MouseMoveEvent, window, cx| {
                if app.dragging_band && event.pressed_button == Some(MouseButton::Left) {
                    app.move_band(event.position, window, cx);
                } else if app.dragging_band {
                    app.dragging_band = false;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    app.dragging_band = false;
                    cx.notify();
                }),
            );
        for (index, band) in self.target().eq.channel(self.channel).iter().enumerate() {
            let selected = index == self.band;
            let color = desk::band_color(index);
            let x = frequency_x(band.frequency) as f32;
            let y = height(if eq_controls::has_gain(band.band_type) {
                band.gain
            } else {
                0.
            }) as f32;
            let radius = if selected { 11. } else { 9. };
            if selected {
                plot = plot.child(
                    div()
                        .absolute()
                        .left(relative(x))
                        .top(relative(y))
                        .ml(px(-17.))
                        .mt(px(-17.))
                        .size(px(34.))
                        .rounded_full()
                        .bg(rgba((color << 8) | 0x24)),
                );
            }
            plot = plot.child(
                div()
                    .id(("eq-point", index))
                    .absolute()
                    .left(relative(x))
                    .top(relative(y))
                    .ml(px(-radius))
                    .mt(px(-radius))
                    .size(px(radius * 2.))
                    .rounded_full()
                    .border_2()
                    .border_color(rgb(color))
                    .bg(rgb(if selected { color } else { surface().sunken }))
                    .when(!band.enabled, |p| p.opacity(0.4))
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .font_bold()
                    .text_color(rgb(if selected { 0x0b0d14 } else { color }))
                    .cursor_grab()
                    .child(format!("{}", index + 1))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, window, cx| {
                            cx.stop_propagation();
                            if app.band != index {
                                app.band = index;
                                app.rebuild_editors(window, cx);
                            }
                            app.dragging_band = true;
                            cx.notify();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(
                        move |app, event: &ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(px(16.)).y;
                            if delta == px(0.) {
                                return;
                            }
                            cx.stop_propagation();
                            if app.band != index {
                                app.band = index;
                                app.rebuild_editors(window, cx);
                            }
                            let channel = app.channel;
                            if let Some(b) = app.target_mut().eq.channel_mut(channel).get_mut(index)
                            {
                                let factor = if delta < px(0.) { 1.08 } else { 1. / 1.08 };
                                b.q = ((b.q * factor).clamp(0.1, 100.) * 100.).round() / 100.;
                            }
                            app.sync_editor_values(window, cx);
                            cx.notify();
                        },
                    )),
            );
            if selected && self.dragging_band {
                let mut text = Parameter::Frequency.display(band.frequency);
                if eq_controls::has_gain(band.band_type) {
                    text = format!("{text} · {}", Parameter::BandGain.display(band.gain));
                }
                plot = plot.child(
                    div()
                        .absolute()
                        .left(relative(x))
                        .top(relative(y))
                        .ml(px(-90.))
                        .mt(px(-46.))
                        .w(px(180.))
                        .h_flex()
                        .justify_center()
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(8.))
                                .bg(rgb(0x0b0e16))
                                .border_1()
                                .border_color(rgb(surface().line_strong))
                                .text_size(px(12.))
                                .whitespace_nowrap()
                                .child(format!("{text} · Q {:.2}", band.q)),
                        ),
                );
            }
        }
        plot
    }
}

/// Grid lines and labels across the frequency axis.
const GRID_HZ: [f64; 10] = [
    20., 50., 100., 200., 500., 1000., 2000., 5000., 10000., 20000.,
];

#[cfg(test)]
mod tests {
    use super::{frequency_x, x_frequency};
    #[test]
    fn log_axis_round_trip_and_clamps() {
        for f in [10., 20., 100., 1000., 20000., 24000.] {
            assert!((x_frequency(frequency_x(f)) - f).abs() < 1e-8);
        }
        assert_eq!(x_frequency(-1.), 10.);
        assert_eq!(x_frequency(2.), 24000.);
    }
}
