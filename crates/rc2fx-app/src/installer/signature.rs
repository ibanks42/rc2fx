//! Decorative onboarding signature, not audio or overall install progress.
//! Decorative motion is driven by onboarding::Signature and the OS motion
//! policy. Measured fill is driven solely by actual operation progress.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    Looking,
    Ready,
    Working(Option<f32>),
    Failed(Option<f32>),
    Complete,
}

fn fraction(progress: Option<&Progress>) -> Option<f32> {
    progress
        .filter(|p| p.total > 0)
        .map(|p| (p.completed as f64 / p.total as f64).clamp(0., 1.) as f32)
}

fn state(installer: &Installer) -> State {
    let m = &installer.model;
    if m.finished && m.run.is_none() {
        State::Complete
    } else if m.run.as_ref().is_some_and(|r| r.failed.is_some()) {
        State::Failed(fraction(installer.progress.as_ref()))
    } else if STEPS
        .into_iter()
        .any(|step| m.state(step) == StepState::Running)
    {
        State::Working(fraction(installer.progress.as_ref()))
    } else if m.running() {
        State::Working(None)
    } else if m.usb().len() == 1 {
        State::Ready
    } else {
        State::Looking
    }
}

pub(crate) fn element(installer: &Installer, phase: f32) -> impl IntoElement + use<> {
    paint(state(installer), phase)
}

pub(crate) fn scene(found: bool, phase: f32) -> impl IntoElement {
    paint(if found { State::Ready } else { State::Looking }, phase)
}

fn offset(u: f32, phase: f32, sample: usize, noise: f32, voice: f32) -> f32 {
    // Same tapered, word-shaped carrier and grain as the approved HTML.
    // Clamp the taper before fractional powf (sin(pi) can be negative in f32).
    let taper = (std::f32::consts::PI * u).sin().max(0.).powf(1.2);
    let word = (std::f32::consts::TAU * (u * 3.4 - phase * 0.12))
        .sin()
        .max(0.)
        .powf(1.6);
    let carrier = (u * 140. - phase * 8.).sin() * 0.6 + (u * 61. + phase * 3.).sin() * 0.4;
    let seed = ((sample as f32 + (phase * 30.).floor() * 7.) * 12.9898).sin() * 43758.547;
    let grain = (seed - seed.floor() - 0.5) * noise * 0.6;
    taper * (voice * word * carrier + grain)
}

fn paint(state: State, phase: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            if width < 4. || height < 2. || !phase.is_finite() {
                return;
            }
            let mid = height / 2.;
            let fill = match state {
                State::Complete => Some(1.),
                State::Working(value) | State::Failed(value) => value,
                State::Looking | State::Ready => None,
            };
            let (noise, voice) = match state {
                State::Looking => (0.6, 0.),
                State::Complete => (0., 1.),
                _ => (1., 1.),
            };
            let failed = matches!(state, State::Failed(_));
            // Peaks and the widest glow stroke must stay inside the slot,
            // whatever its height; the line is never clipped.
            let headroom = (mid - 6.).max(0.);
            let amplitude = (width * 130. / 1584.).min(70.).min(headroom * 0.8);
            let steps = (width / 1.5).ceil().max(2.) as usize;
            let points: Vec<_> = (0..=steps)
                .map(|i| {
                    let u = i as f32 / steps as f32;
                    let y = if failed && u > fill.unwrap_or(0.) {
                        0.
                    } else {
                        (offset(u, phase, i, noise, voice) * amplitude).clamp(-headroom, headroom)
                    };
                    (
                        u,
                        point(
                            bounds.origin.x + px(u * width),
                            bounds.origin.y + px(mid - y),
                        ),
                    )
                })
                .collect();
            let stroke = |width: f32, end: f32| {
                let mut path = PathBuilder::stroke(px(width));
                for (i, (_, p)) in points.iter().take_while(|(u, _)| *u <= end).enumerate() {
                    if i == 0 {
                        path.move_to(*p);
                    } else {
                        path.line_to(*p);
                    }
                }
                path.build().ok()
            };
            let grey = if state == State::Looking { 0.25 } else { 0.45 };
            for (width, alpha) in [(8. / 2., grey * 0.25), (2.5 / 2., grey)] {
                if let Some(path) = stroke(width, 1.) {
                    window.paint_path(
                        path,
                        Hsla::from(rgb(if failed { WARN } else { 0xf4efe6 })).opacity(alpha),
                    );
                }
            }
            if let Some(fill) = fill.filter(|f| *f > 0.) {
                let (start, end) = if failed {
                    (WARN, DANGER)
                } else {
                    (ACCENT, VIOLET)
                };
                for (width, alpha) in [(11., 0.12), (4.5, 0.25), (1.5, 1.)] {
                    if let Some(path) = stroke(width, fill) {
                        window.paint_path(
                            path,
                            linear_gradient(
                                90.,
                                linear_color_stop(Hsla::from(rgb(start)).opacity(alpha), 0.),
                                linear_color_stop(Hsla::from(rgb(end)).opacity(alpha), 1.),
                            ),
                        );
                    }
                }
            }
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::{Progress, fraction, offset};
    #[test]
    fn progress_is_per_operation_and_unknown_is_not_zero() {
        assert_eq!(fraction(None), None);
        let mut p = Progress::default();
        assert_eq!(fraction(Some(&p)), None);
        p.total = 100;
        assert_eq!(fraction(Some(&p)), Some(0.));
        p.completed = 37;
        assert_eq!(fraction(Some(&p)), Some(0.37));
        p.completed = 200;
        assert_eq!(fraction(Some(&p)), Some(1.));
        p.total = 0;
        assert_eq!(fraction(Some(&p)), None);
        p.completed = u64::MAX;
        p.total = u64::MAX;
        assert_eq!(fraction(Some(&p)), Some(1.));
    }
    #[test]
    fn noisy_signature_has_finite_tapered_endpoints() {
        for phase in [0., 0.016_667, 3.2, 1234.5] {
            for (noise, voice) in [(0.6, 0.), (1., 1.), (0., 1.)] {
                for i in 0..=600 {
                    let y = offset(i as f32 / 600., phase, i, noise, voice);
                    assert!(y.is_finite());
                    if i == 0 || i == 600 {
                        assert!(y.abs() < 1e-5);
                    }
                }
            }
        }
    }
}
