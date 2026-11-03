//! The level indicator: the design's
//! glowing line. Its height follows a path's reported output level (peak-
//! meter ballistics, `LevelHistory::display`); its shape is decorative, not
//! an audio waveform, and the UI says so. This module only paints; the
//! static form redraws with its view, the animated form is driven by the
//! overlay (line_overlay.rs).

use super::*;
use level_history::LevelHistory;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum LineState {
    /// Fresh readings: the RC2FX gradient at the displayed level.
    Live,
    /// No fresh readings: a flat, dim line, labelled by the caller as
    /// unavailable so it can't be read as measured silence.
    Unavailable,
}

/// The sizes of one indicator.
#[derive(Clone, Copy)]
pub(super) struct LineStyle {
    /// The line's largest half-height, as a fraction of the element's height.
    pub amplitude: f32,
    pub stroke: f32,
    /// How far the glow spreads, in multiples of the stroke; 0 for none.
    pub glow: f32,
}

/// The channel editor's line under the channel name.
pub(super) const CHANNEL: LineStyle = LineStyle {
    amplitude: 0.34,
    stroke: 2.5,
    glow: 2.,
};
/// The sidebar's small line beside each processed channel.
pub(super) const MINI: LineStyle = LineStyle {
    amplitude: 0.42,
    stroke: 1.6,
    glow: 0.,
};

/// The displayed level of `history` at `now`, 0 to 1 on the meter scale.
pub(super) fn level_of(history: Option<&LevelHistory>, now: Instant) -> f32 {
    history.map_or(0., |h| h.display(now, desk::level))
}

/// A still indicator: for reduced motion, for a window without the overlay,
/// and for the unavailable state. `level` is from `level_of`.
pub(super) fn still_line(level: f32, state: LineState, style: LineStyle) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint_line(window, bounds, 0., level, state, style),
    )
    .size_full()
}

/// The line's points across `width`: (x, offset from the middle), one every
/// 3 px. None if any point isn't finite: an invalid line is not drawn,
/// rather than drawn with a substituted point.
fn offsets(width: f32, phase: f32, amplitude: f32) -> Option<Vec<(f32, f32)>> {
    let steps = (width / 3.).ceil().max(2.) as usize;
    (0..=steps)
        .map(|i| {
            let u = i as f32 / steps as f32;
            // Tapered at the ends, like the design. The shape doesn't fall to
            // zero between the ends, so it draws no silence that wasn't
            // measured. sin(π) is slightly negative in f32, and a negative
            // base would make powf NaN and the whole path invalid.
            let taper = (std::f32::consts::PI * u).sin().max(0.).powf(1.1);
            let carrier = ((u * 140. - phase * 8.).sin() * 0.6
                + (u * 61. + phase * 3.).sin() * 0.4)
                * (0.65 + 0.35 * (u * 6.3 - phase * 1.1).sin());
            let (x, offset) = (u * width, taper * amplitude * carrier);
            (x.is_finite() && offset.is_finite()).then_some((x, offset))
        })
        .collect()
}

/// Paints the indicator in `bounds`. `phase` (seconds) moves the decorative
/// shape; `level` scales its height.
pub(super) fn paint_line(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    phase: f32,
    level: f32,
    state: LineState,
    style: LineStyle,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width < 4. || height < 2. {
        return;
    }
    let mid = height / 2.;
    let amplitude = match state {
        LineState::Live => level.clamp(0., 1.) * style.amplitude * height,
        LineState::Unavailable => 0.,
    };
    let Some(offsets) = offsets(width, phase, amplitude) else {
        return;
    };
    let points: Vec<Point<Pixels>> = offsets
        .into_iter()
        .map(|(x, offset)| point(bounds.origin.x + px(x), bounds.origin.y + px(mid - offset)))
        .collect();
    let stroke = |width: f32| {
        let mut builder = PathBuilder::stroke(px(width));
        for (i, p) in points.iter().enumerate() {
            if i == 0 {
                builder.move_to(*p);
            } else {
                builder.line_to(*p);
            }
        }
        builder.build().ok()
    };
    match state {
        LineState::Live => {
            let gradient = |alpha: f32| -> Background {
                linear_gradient(
                    90.,
                    linear_color_stop(Hsla::from(rgb(ACCENT)).opacity(alpha), 0.),
                    linear_color_stop(Hsla::from(rgb(VIOLET)).opacity(alpha), 1.),
                )
            };
            if style.glow > 0. {
                for (spread, alpha) in [(7. / 3., 0.12), (1., 0.25)] {
                    if let Some(glow) = stroke(style.stroke * style.glow * spread) {
                        window.paint_path(glow, gradient(alpha));
                    }
                }
            }
            if let Some(path) = stroke(style.stroke) {
                window.paint_path(path, gradient(1.));
            }
        }
        LineState::Unavailable => {
            if let Some(path) = stroke(style.stroke) {
                window.paint_path(path, Hsla::from(rgb(FAINT)).opacity(0.5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CHANNEL, MINI, offsets};

    #[test]
    fn every_point_is_finite_and_both_ends_meet_the_middle() {
        for style in [CHANNEL, MINI] {
            for height in [34., 170.] {
                // Levels from silence to full scale, as `paint_line` scales them.
                for level in [0., 0.5, 1.] {
                    let amplitude = level * style.amplitude * height;
                    for width in [4., 300., 899., 1280.] {
                        for phase in [0., 0.016_667, 1.7, 1234.5] {
                            let points = offsets(width, phase, amplitude)
                                .expect("a valid line for valid inputs");
                            let (first, last) = (points[0], points[points.len() - 1]);
                            assert_eq!((first.0, last.0), (0., width));
                            assert!(first.1.abs() < 1e-3 && last.1.abs() < 1e-3, "tapered ends");
                            assert!(points.iter().all(|(_, y)| y.abs() <= amplitude + 1e-3));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_invalid_line_is_rejected_not_patched() {
        assert!(offsets(300., f32::NAN, 20.).is_none());
        assert!(offsets(300., 0., f32::INFINITY).is_none());
    }
}
