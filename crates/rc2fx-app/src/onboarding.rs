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
///
/// It is two soft shadows (blue left, violet right) around an invisible
/// ellipse. GPUI draws a blurred shadow in one cheap pass, whereas filled
/// paths are rasterized again on every frame, and the signature line
/// redraws the window up to 60 times a second.
pub(super) fn backdrop() -> impl IntoElement {
    let glow = |color: u32, offset: f32| BoxShadow {
        color: Hsla::from(rgb(color)).opacity(0.065),
        offset: point(px(offset), px(0.)),
        blur_radius: px(220.),
        spread_radius: px(0.),
        inset: false,
    };
    div()
        .absolute()
        .inset_0()
        .size_full()
        .when(desk::appearance() == desk::Appearance::Dusk, |d| {
            d.child(
                div()
                    .absolute()
                    .left(relative(0.22))
                    .w(relative(0.56))
                    .top(relative(0.14))
                    .h(relative(0.28))
                    .rounded_full()
                    .shadow(vec![glow(0x7194c8, -140.), glow(0x8d7bc4, 140.)]),
            )
        })
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
    /// When the window lost focus; the line is drawn as at that moment until
    /// focus returns, whatever else redraws the window.
    frozen: Option<Instant>,
    _visibility: Subscription,
    _activation: Subscription,
}

/// 30 Hz. The line's noise steps 30 times a second (see
/// `signature::offset`), so faster frames only repaint the same picture.
const FRAME: Duration = Duration::from_micros(33_333);

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
            frozen: None,
            _visibility: cx.observe_window_visibility(window, |line, _, _, cx| {
                line.ticker = None;
                cx.notify();
            }),
            // Decoration rests while the window is in the background.
            _activation: cx.observe_window_activation(window, |line, _, cx| {
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
        if self.active(cx) && window.is_visible() && window.is_window_active() {
            if self.ticker.is_none() {
                self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor().timer(FRAME).await;
                        let keep_running = this.update_in(cx, |line, window, cx| {
                            if !line.active(cx)
                                || !window.is_visible()
                                || !window.is_window_active()
                            {
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
        match (window.is_window_active(), self.frozen) {
            (false, None) => self.frozen = Some(Instant::now()),
            (true, Some(at)) => {
                // Continue from where the line stopped.
                self.epoch += Instant::now().saturating_duration_since(at);
                self.frozen = None;
            }
            _ => {}
        }
        let phase = self
            .frozen
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.epoch)
            .as_secs_f32();
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
