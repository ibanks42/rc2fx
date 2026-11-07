//! Onboarding has its own animation lifetime. It does not use Home's cache
//! or overlay. Its decorative motion follows the system motion policy.
use super::*;
use std::sync::{Arc, OnceLock};

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

/// The halo's design: two soft glows (blue left, violet right) around an
/// invisible pill, at this window size.
const HALO_WINDOW: (f32, f32) = (1320., 860.);
const HALO_GLOWS: [(u32, f32); 2] = [(0x7194c8, -140.), (0x8d7bc4, 140.)];
const HALO_OPACITY: f32 = 0.065;
const HALO_BLUR: f32 = 220.;
/// Window pixels per image pixel. A blur this wide has no fine detail.
const HALO_CELL: f32 = 8.;

/// A glow in the halo image: the pill, offset and blurred, as coverage from 0
/// to 1 for each image pixel.
fn halo_glow(offset: f32, width: usize, height: usize) -> Vec<f32> {
    let (window_width, window_height) = HALO_WINDOW;
    let (left, right) = (0.22 * window_width + offset, 0.78 * window_width + offset);
    let radius = 0.14 * window_height;
    let middle = 0.14 * window_height + radius;
    let inside = |x: usize, y: usize| {
        let (x, y) = ((x as f32 + 0.5) * HALO_CELL, (y as f32 + 0.5) * HALO_CELL);
        let dx = x - x.clamp(left + radius, right - radius);
        f32::from(dx.hypot(y - middle) <= radius)
    };
    let sigma = HALO_BLUR / HALO_CELL;
    let reach = (3. * sigma) as isize;
    let kernel: Vec<f32> = (-reach..=reach)
        .map(|i| (-0.5 * (i as f32 / sigma).powi(2)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    // One axis at a time: blurring in x then y is the same as blurring in both.
    let blur = |source: &[f32], length: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut blurred = vec![0.; source.len()];
        for line in 0..source.len() / length {
            for position in 0..length {
                let sum: f32 = kernel
                    .iter()
                    .zip(position as isize - reach..)
                    .filter(|(_, i)| (0..length as isize).contains(i))
                    .map(|(weight, i)| weight * source[at(line, i as usize)])
                    .sum();
                blurred[at(line, position)] = sum / total;
            }
        }
        blurred
    };
    let mask: Vec<f32> = (0..width * height)
        .map(|i| inside(i % width, i / width))
        .collect();
    let rows = blur(&mask, width, &|row, x| row * width + x);
    blur(&rows, height, &|column, y| y * width + column)
}

/// The halo's pixels, BGRA with straight alpha: the left glow under the right.
fn halo_pixels(width: usize, height: usize) -> Vec<u8> {
    let color = |value: u32| [16, 8, 0].map(|shift| ((value >> shift) & 0xff) as f32);
    let [(below_color, below), (above_color, above)] = HALO_GLOWS;
    let (below_color, above_color) = (color(below_color), color(above_color));
    let below = halo_glow(below, width, height);
    let above = halo_glow(above, width, height);
    below
        .iter()
        .zip(&above)
        .flat_map(|(below, above)| {
            let (below, above) = (below * HALO_OPACITY, above * HALO_OPACITY);
            let alpha = above + below * (1. - above);
            let channel = |i: usize| {
                let mixed = above_color[i] * above + below_color[i] * below * (1. - above);
                (mixed / alpha.max(f32::EPSILON)).round() as u8
            };
            [
                channel(2),
                channel(1),
                channel(0),
                (alpha * 255.).round() as u8,
            ]
        })
        .collect()
}

/// The halo, computed once. As a live blurred shadow it covered the whole
/// window with per-pixel blur maths on every frame: more than half the GPU
/// time of these screens on an integrated GPU, for a picture that never
/// changes. An image costs one texture read a pixel.
fn halo() -> Arc<RenderImage> {
    static HALO: OnceLock<Arc<RenderImage>> = OnceLock::new();
    HALO.get_or_init(|| {
        let width = (HALO_WINDOW.0 / HALO_CELL).ceil() as usize;
        let height = (HALO_WINDOW.1 / HALO_CELL).ceil() as usize;
        let pixels =
            image::RgbaImage::from_raw(width as u32, height as u32, halo_pixels(width, height))
                .expect("the halo fills its image");
        Arc::new(RenderImage::new([image::Frame::new(pixels)]))
    })
    .clone()
}

/// The halo belongs to the window, never to a clipped line-sized canvas.
pub(super) fn backdrop() -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .size_full()
        .when(desk::appearance() == desk::Appearance::Dusk, |d| {
            d.child(
                img(ImageSource::Render(halo()))
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
        })
}

pub(super) fn headline(first: &'static str, second: &'static str) -> Div {
    div()
        .v_flex()
        .items_center()
        .text_center()
        .text_size(px(ui::DISPLAY_TEXT))
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
        // It keeps moving behind other windows: during an install, a still
        // line would look as if the install had stopped.
        if self.active(cx) && window.is_visible() {
            if self.ticker.is_none() {
                self.ticker = Some(cx.spawn_in(window, async move |this, cx| {
                    loop {
                        cx.background_executor().timer(FRAME).await;
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

#[cfg(test)]
mod tests {
    use super::{HALO_OPACITY, halo_pixels};

    #[test]
    fn the_halo_is_faint_and_strongest_around_the_middle() {
        let (width, height) = (165, 108);
        let pixels = halo_pixels(width, height);
        assert_eq!(pixels.len(), width * height * 4);
        let alpha = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        let peak = pixels.chunks_exact(4).map(|p| p[3]).max().unwrap();
        // Two glows at most, each no stronger than its opacity.
        assert!(f32::from(peak) <= 2. * HALO_OPACITY * 255.);
        let centre = alpha(width / 2, height * 28 / 100);
        assert!(centre > 0 && centre > alpha(0, height - 1));
    }

    #[test]
    fn each_side_of_the_halo_takes_its_own_color() {
        let (width, height) = (165, 108);
        let pixels = halo_pixels(width, height);
        let blue_to_red = |x: usize| {
            let pixel = &pixels[(height * 28 / 100 * width + x) * 4..][..4];
            f32::from(pixel[0]) / f32::from(pixel[2])
        };
        assert!(blue_to_red(width / 5) > blue_to_red(width * 4 / 5));
    }
}
