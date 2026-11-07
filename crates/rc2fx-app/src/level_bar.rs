//! The overview's level bars. This module only paints: the overlay draws a bar
//! every frame while it can animate (line_overlay.rs), and Home draws it still
//! otherwise, the same way.

use super::*;

/// The shortest bar with a fresh reading, as a fraction of its track.
const LOWEST: f32 = 0.02;
/// The stub shown without a fresh reading.
const STUB: f32 = 6.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Bar {
    /// A fader's level: filled from the bottom.
    Vertical,
    /// An output's level: filled from the left.
    Horizontal,
}

impl Bar {
    /// The part of `track` to fill for `level`; None level is the stub.
    fn fill(self, track: Bounds<Pixels>, level: Option<f32>) -> Bounds<Pixels> {
        let length = |whole: Pixels| match level {
            Some(level) => whole * level.clamp(LOWEST, 1.),
            None => px(STUB),
        };
        match self {
            Self::Vertical => {
                let height = length(track.size.height);
                Bounds::new(
                    point(track.origin.x, track.origin.y + track.size.height - height),
                    size(track.size.width, height),
                )
            }
            Self::Horizontal => Bounds::new(
                track.origin,
                size(length(track.size.width), track.size.height),
            ),
        }
    }

    fn background(self) -> Background {
        match self {
            Self::Vertical => linear_gradient(
                0.,
                linear_color_stop(rgb(ACCENT), 0.),
                linear_color_stop(rgb(VIOLET), 1.),
            ),
            Self::Horizontal => ui::gradient(),
        }
    }

    fn radius(self) -> Pixels {
        match self {
            Self::Vertical => px(6.),
            Self::Horizontal => px(4.),
        }
    }
}

/// Paints the bar in its `track`. `level` is 0 to 1, or None without a fresh
/// reading.
pub(super) fn paint(window: &mut Window, track: Bounds<Pixels>, bar: Bar, level: Option<f32>) {
    let background = match level {
        Some(_) => bar.background(),
        None => rgb(ui::DIM).into(),
    };
    window.paint_quad(fill(bar.fill(track, level), background).corner_radii(bar.radius()));
}

/// A bar drawn with its view, for when the overlay can't animate it.
pub(super) fn still(bar: Bar, level: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |track, _, window, _| paint(window, track, bar, level),
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::{Bar, LOWEST, STUB};
    use gpui_kit::{Bounds, point, px, size};

    fn track() -> Bounds<gpui_kit::Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(100.), px(200.)))
    }

    #[test]
    fn a_vertical_bar_rises_from_the_bottom_of_its_track() {
        let fill = Bar::Vertical.fill(track(), Some(0.5));
        assert_eq!(fill.size, size(px(100.), px(100.)));
        assert_eq!(fill.origin, point(px(10.), px(120.)));
        assert_eq!(fill.bottom(), track().bottom());
    }

    #[test]
    fn a_horizontal_bar_grows_from_the_left_of_its_track() {
        let fill = Bar::Horizontal.fill(track(), Some(0.25));
        assert_eq!(fill.origin, track().origin);
        assert_eq!(fill.size, size(px(25.), px(200.)));
    }

    #[test]
    fn levels_stay_inside_the_track_and_silence_stays_visible() {
        let full = Bar::Vertical.fill(track(), Some(2.));
        assert_eq!(full, track());
        let silent = Bar::Horizontal.fill(track(), Some(0.));
        assert_eq!(silent.size.width, px(100.) * LOWEST);
    }

    #[test]
    fn no_reading_is_a_fixed_stub_at_the_start_of_the_track() {
        assert_eq!(Bar::Vertical.fill(track(), None).size.height, px(STUB));
        assert_eq!(Bar::Horizontal.fill(track(), None).size.width, px(STUB));
    }
}
