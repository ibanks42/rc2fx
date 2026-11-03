//! Onboarding has its own animation lifetime. It does not use Home's cache
//! or overlay. Its decorative motion follows the system motion policy.
use super::*;

pub(super) fn background() -> Background {
    let (top, bottom) = match desk::appearance() {
        desk::Appearance::Dusk => (0x131622, 0x0b0d14),
        desk::Appearance::Graphite => (0x171a20, 0x0c0d11),
        desk::Appearance::Black => (0x08090c, 0x030405),
    };
    linear_gradient(
        180.,
        linear_color_stop(rgb(top), 0.),
        linear_color_stop(rgb(bottom), 1.),
    )
}

/// The halo belongs to the window, never to a clipped line-sized canvas.
pub(super) fn backdrop() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |bounds, _, window, _| {
            if desk::appearance() != desk::Appearance::Dusk {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let center = point(
                bounds.origin.x + px(width * 0.5),
                bounds.origin.y + px(height * 0.28),
            );
            // An actual elliptical radial falloff, rather than a blurred rectangle.
            // Each translucent ellipse adds only the next opacity increment; the
            // outer edge begins at zero, with no widget-sized clipping boundary.
            let mut previous = 0.;
            for ring in 1..=48 {
                let t = ring as f32 / 49.;
                let radius = 1. - t;
                let opacity = 0.12 * t * t;
                let alpha = (opacity - previous) / (1. - previous);
                previous = opacity;
                let rx = width * 0.48 * radius;
                let ry = height * 0.34 * radius;
                let at = |x: f32, y: f32| point(center.x + px(x), center.y + px(y));
                let k = 0.552_284_8;
                let mut ellipse = PathBuilder::fill();
                ellipse.move_to(at(rx, 0.));
                ellipse.cubic_bezier_to(at(0., ry), at(rx, k * ry), at(k * rx, ry));
                ellipse.cubic_bezier_to(at(-rx, 0.), at(-k * rx, ry), at(-rx, k * ry));
                ellipse.cubic_bezier_to(at(0., -ry), at(-rx, -k * ry), at(-k * rx, -ry));
                ellipse.cubic_bezier_to(at(rx, 0.), at(k * rx, -ry), at(rx, -k * ry));
                ellipse.close();
                if let Ok(path) = ellipse.build() {
                    window.paint_path(
                        path,
                        linear_gradient(
                            90.,
                            linear_color_stop(Hsla::from(rgb(0x7194c8)).opacity(alpha), 0.),
                            linear_color_stop(Hsla::from(rgb(0x8d7bc4)).opacity(alpha), 1.),
                        ),
                    );
                }
            }
        },
    )
    .absolute()
    .inset_0()
    .size_full()
}

pub(super) fn headline(first: &'static str, second: &'static str) -> Div {
    div()
        .v_flex()
        .items_center()
        .text_center()
        .text_size(px(64.))
        .line_height(relative(0.98))
        .font_weight(FontWeight::EXTRA_BOLD)
        .child(first)
        .child(div().text_color(rgb(FAINT)).child(second))
}

pub(super) struct Signature {
    app: WeakEntity<Rc2FxApp>,
    epoch: Instant,
    ticker: Option<Task<()>>,
    _visibility: Subscription,
}

impl Signature {
    pub(super) fn new(
        app: WeakEntity<Rc2FxApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            app,
            epoch: Instant::now(),
            ticker: None,
            _visibility: cx.observe_window_visibility(window, |line, _, _, cx| {
                line.ticker = None;
                cx.notify();
            }),
        }
    }

    fn active(&self, cx: &App) -> bool {
        self.app.upgrade().is_some_and(|app| {
            let app = app.read(cx);
            !app.reduced_motion
                && (!app.session.connected || app.installer_page)
                && app
                    .installer
                    .as_ref()
                    .is_none_or(|installer| !installer.needs_access())
        })
    }
}

impl Render for Signature {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active(cx) && window.is_visible() {
            if self.ticker.is_none() {
                self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor()
                            .timer(Duration::from_micros(16_667))
                            .await;
                        let keep_running = this.update_in(cx, |line, window, cx| {
                            if !line.active(cx) || !window.is_visible() {
                                line.ticker = None;
                                return false;
                            }
                            cx.notify();
                            true
                        });
                        if !matches!(keep_running, Ok(true)) {
                            break;
                        }
                    }
                }));
            }
        } else {
            self.ticker = None;
        }
        let phase = self.epoch.elapsed().as_secs_f32();
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        let app = app.read(cx);
        if let Some(installer) = app.installer.as_ref() {
            installer::signature::element(installer, phase).into_any_element()
        } else {
            installer::signature::scene(connection::mixer_found(&app.message), phase)
                .into_any_element()
        }
    }
}
