//! Home's animated level indicators: the glowing lines and the overview's bars.
//! On by default; `RC2FX_ANIMATED_LINE=0` opens the window exactly as before,
//! with Home's indicators drawn still.
//!
//! Why an overlay: GPUI renders the window root on every frame, and
//! notifying a view also dirties its ancestors, so animating inside the app
//! would re-render the whole app on every frame. Instead the window holds a
//! thin `Shell` with two siblings: the app, rendered `.cached()` while Home
//! is in front (every other page renders as before), and this overlay, the
//! only view notified on each animation tick. That the cached app is then
//! reused, and the frame rate, are unverified: check with
//! `RC2FX_RENDER_STATS=1`. The tick aims at 60 Hz, or 30 Hz while the window is
//! not focused; it doesn't guarantee either. Presenting a frame costs a weak
//! GPU far more than anything drawn in it, so a tick that would show nothing
//! new draws no frame (`Slots::unchanged`): a silent mixer costs almost nothing.
//! The indicators stay live beside whatever is in front, and stop while hidden.
//!
//! Who draws an indicator (never both, never neither): in each render, Home
//! claims a slot for the overlay only for an indicator that can animate
//! (overlay on, reduced motion off, fresh readings) and draws every other
//! indicator still itself. Claims belong to the app render that made them
//! (`Slots::generation`): any app render without Home, or with different
//! claims, replaces them. The overlay refuses geometry it can't trust
//! (missing, or from another scale): it paints nothing for that slot, latches
//! the refusal for the current window size and scale, and asks the app to
//! render once more, in which Home draws that indicator still. Only a change
//! of window size or scale releases the latch.

use super::*;
use level_bar::Bar;
use live_line::{LineState, LineStyle};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};

const FRAME: Duration = Duration::from_micros(16_667);
const UNFOCUSED_FRAME: Duration = Duration::from_micros(33_333);
/// A line lower than this, in pixels either side of its middle, looks flat.
const FLAT: f32 = 0.5;
/// A bar's level changing by less than this moves it less than a pixel.
const BAR_STEP: f32 = 0.004;

/// Whether the window uses the shell and overlay. Read once.
pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RC2FX_ANIMATED_LINE").map_or(true, |v| v != "0"))
}

fn stats_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RC2FX_RENDER_STATS").is_ok_and(|v| v == "1"))
}

/// Render counts, summed and logged every 5 s with `RC2FX_RENDER_STATS=1`.
/// They show how often each view renders, not layout, paint or CPU cost.
pub(super) static SHELL_RENDERS: AtomicU64 = AtomicU64::new(0);
pub(super) static APP_RENDERS: AtomicU64 = AtomicU64::new(0);
static OVERLAY_RENDERS: AtomicU64 = AtomicU64::new(0);
static OVERLAY_PAINTS: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

pub(super) fn count(counter: &AtomicU64) {
    if stats_enabled() {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// The background task that logs the counters, or None when they're off.
/// Dropping the task (with the shell) stops it.
fn stats_logger(cx: &mut App) -> Option<Task<()>> {
    stats_enabled().then(|| {
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let take = |c: &AtomicU64| c.swap(0, Ordering::Relaxed);
                eprintln!(
                    "rc2fx render-stats per 5 s: shell={} app={} overlay-renders={} overlay-paints={} ticks={}",
                    take(&SHELL_RENDERS),
                    take(&APP_RENDERS),
                    take(&OVERLAY_RENDERS),
                    take(&OVERLAY_PAINTS),
                    take(&TICKS),
                );
            }
        })
    })
}

/// What an indicator shows.
#[derive(Clone, PartialEq)]
pub(super) enum Source {
    /// One path's reported output peak.
    Path(String),
}

/// How an indicator is drawn.
#[derive(Clone, Copy)]
pub(super) enum Shape {
    Line(LineStyle),
    Bar(Bar),
}

struct Claim {
    source: Source,
    shape: Shape,
    geometry: Option<(Bounds<Pixels>, ContentMask<Pixels>, f32)>,
}

/// The window's size and scale: a change is what can make refused geometry
/// trustworthy again.
pub(super) type Layout = (gpui_kit::Size<Pixels>, f32);

/// Home's claims for the overlay, from one app render.
#[derive(Default)]
pub(super) struct Slots {
    generation: u64,
    claims: Vec<Claim>,
    /// A claim's geometry was refused in this frame; the overlay asks the app
    /// to render once more, and Home then draws that indicator still.
    refused: bool,
    /// Indicators whose geometry was refused, with the layout it happened
    /// in. Home draws them still, without claiming, until the layout changes,
    /// so a refusal can't turn into a retry loop. At most one per indicator.
    latched: Vec<(Source, Layout)>,
    /// How many claims the last paint found on screen, and in which render's
    /// claims: none on screen parks the ticker.
    painted: (u64, usize),
    /// Each claim's level in the last paint, for `unchanged`.
    drawn: Vec<Option<f32>>,
}

impl Slots {
    /// Whether painting `levels` (one per claim) would look like the last
    /// paint: every line was and stays flat, whatever its phase, and no bar
    /// has moved.
    fn unchanged(&self, levels: &[Option<f32>]) -> bool {
        let same = |claim: &Claim, drawn: Option<f32>, level: Option<f32>| match claim.shape {
            Shape::Line(style) => {
                let height = claim
                    .geometry
                    .map_or(f32::INFINITY, |(bounds, ..)| f32::from(bounds.size.height));
                [drawn, level]
                    .iter()
                    .all(|l| l.unwrap_or(1.) * style.amplitude * height < FLAT)
            }
            Shape::Bar(_) => match (drawn, level) {
                (Some(drawn), Some(level)) => (drawn - level).abs() < BAR_STEP,
                (drawn, level) => drawn == level,
            },
        };
        self.drawn.len() == self.claims.len()
            && levels.len() == self.claims.len()
            && self
                .claims
                .iter()
                .zip(self.drawn.iter().zip(levels))
                .all(|(claim, (drawn, level))| same(claim, *drawn, *level))
    }
}

pub(super) type SharedSlots = Rc<RefCell<Slots>>;

/// Starts an app render's claims, dropping the previous render's, and
/// releases refusals made in another layout. Called by Home before it
/// claims; app renders without Home leave claims that no longer match the
/// generation.
pub(super) fn begin(slots: &SharedSlots, generation: u64, layout: Layout) {
    let mut s = slots.borrow_mut();
    s.generation = generation;
    s.claims.clear();
    s.latched.retain(|(_, at)| *at == layout);
}

/// Whether the overlay refused this indicator in the current layout; Home
/// then draws it still instead of claiming it.
pub(super) fn latched(slots: &SharedSlots, source: &Source) -> bool {
    slots.borrow().latched.iter().any(|(s, _)| s == source)
}

/// Whether any indicator is drawn still because its geometry was refused.
pub(super) fn any_latched(slots: &SharedSlots) -> bool {
    !slots.borrow().latched.is_empty()
}

/// Claims a slot for the overlay and returns the element that records its
/// geometry during prepaint. It paints nothing itself.
pub(super) fn claim(slots: &SharedSlots, source: Source, shape: Shape) -> impl IntoElement {
    let index = {
        let mut s = slots.borrow_mut();
        s.claims.push(Claim {
            source,
            shape,
            geometry: None,
        });
        s.claims.len() - 1
    };
    let slots = slots.clone();
    canvas(
        move |bounds, window, _| {
            if let Some(claim) = slots.borrow_mut().claims.get_mut(index) {
                claim.geometry = Some((bounds, window.content_mask(), window.scale_factor()));
            }
        },
        |_, _, _, _| {},
    )
    .size_full()
}

fn claimed(slots: &Slots, generation: u64) -> bool {
    slots.generation == generation && !slots.claims.is_empty()
}

pub(super) struct LineOverlay {
    app: WeakEntity<Rc2FxApp>,
    slots: SharedSlots,
    /// The animation's clock: shapes move with time since this instant.
    epoch: Instant,
    /// The one shared ticker; runs only while some claim can animate.
    ticker: Option<Task<()>>,
    /// The time between ticks, kept up to date by each render.
    frame: Rc<Cell<Duration>>,
    _visibility: Subscription,
}

impl LineOverlay {
    fn new(
        app: WeakEntity<Rc2FxApp>,
        slots: SharedSlots,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A parked overlay is woken by the window becoming visible as well as
        // by any frame (it renders in every frame the window draws).
        Self {
            app,
            slots,
            epoch: Instant::now(),
            ticker: None,
            frame: Rc::new(Cell::new(FRAME)),
            _visibility: cx.observe_window_visibility(window, |_, _, _, cx| cx.notify()),
        }
    }
}

impl LineOverlay {
    /// Whether this tick would change nothing on screen.
    fn unchanged(&self, cx: &App) -> bool {
        let Some(app) = self.app.upgrade() else {
            return false;
        };
        let app = app.read(cx);
        let slots = self.slots.borrow();
        let now = Instant::now();
        let levels: Vec<_> = slots
            .claims
            .iter()
            .map(|claim| claim_level(app, claim, now))
            .collect();
        slots.unchanged(&levels)
    }
}

/// What this claim shows now: a line's level, or a bar's reading if it has one.
fn claim_level(app: &Rc2FxApp, claim: &Claim, now: Instant) -> Option<f32> {
    let Source::Path(id) = &claim.source;
    match claim.shape {
        Shape::Line(_) => Some(live_line::level_of(app.levels.get(id), now)),
        Shape::Bar(_) => app.overview_level(id),
    }
}

impl Render for LineOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        count(&OVERLAY_RENDERS);
        let Some(app) = self.app.upgrade() else {
            self.ticker = None;
            return div().into_any_element();
        };
        // Read-only: reading the app neither notifies nor subscribes to it.
        let (generation, reduced) = {
            let app = app.read(cx);
            (app.render_generation, app.reduced_motion)
        };
        // At most a frame late: a cached app renders during prepaint, after
        // this. Paint, which runs after every prepaint, decides what to draw.
        let (any, refused) = {
            let mut s = self.slots.borrow_mut();
            (claimed(&s, generation), std::mem::take(&mut s.refused))
        };
        if refused {
            // Outside the draw: render the app again so Home claims afresh
            // or draws its indicators still.
            let app = app.downgrade();
            cx.defer(move |cx| {
                let _ = app.update(cx, |_, cx| cx.notify());
            });
        }
        // Parked when the last paint of these claims found none on screen
        // (until a frame or app render says otherwise).
        let on_screen = {
            let s = self.slots.borrow();
            s.painted.0 != generation || s.painted.1 > 0
        };
        // Visibility is GPUI-reported; what counts as hidden is
        // platform-dependent.
        let animate = any && on_screen && !reduced && window.is_visible();
        self.frame.set(if window.is_window_active() {
            FRAME
        } else {
            UNFOCUSED_FRAME
        });
        if !animate {
            self.ticker = None;
        } else if self.ticker.is_none() {
            let frame = self.frame.clone();
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    // One frame per tick (a target, not a measured frame
                    // rate); a late tick is not made up.
                    cx.background_executor().timer(frame.get()).await;
                    count(&TICKS);
                    let tick = this.update(cx, |overlay, cx| {
                        if !overlay.unchanged(cx) {
                            cx.notify();
                        }
                    });
                    if tick.is_err() {
                        break;
                    }
                }
            }));
        }
        let ticking = self.ticker.is_some();
        let slots = self.slots.clone();
        let epoch = self.epoch;
        let app = app.downgrade();
        // Noninteractive: a bare canvas has no hitbox, focus or key context.
        div()
            .absolute()
            .inset_0()
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        let Some(app) = app.upgrade() else { return };
                        let app = app.read(cx);
                        let mut s = slots.borrow_mut();
                        if !claimed(&s, app.render_generation) {
                            return;
                        }
                        count(&OVERLAY_PAINTS);
                        let now = Instant::now();
                        let phase = now.saturating_duration_since(epoch).as_secs_f32();
                        let scale = window.scale_factor();
                        let layout = (window.viewport_size(), scale);
                        let mut refused = Vec::new();
                        let mut on_screen = 0;
                        let drawn = s
                            .claims
                            .iter()
                            .map(|claim| claim_level(app, claim, now))
                            .collect();
                        s.drawn = drawn;
                        for claim in &s.claims {
                            let trusted = claim.geometry.filter(|(_, _, at)| *at == scale);
                            let Some((bounds, mask, _)) = trusted else {
                                refused.push(claim.source.clone());
                                continue;
                            };
                            if bounds.intersect(&mask.bounds).is_empty() {
                                continue;
                            }
                            on_screen += 1;
                            let Source::Path(id) = &claim.source;
                            let style = match claim.shape {
                                Shape::Line(style) => style,
                                // Shows a reading going stale before Home
                                // next renders.
                                Shape::Bar(bar) => {
                                    let level = app.overview_level(id);
                                    window.with_content_mask(Some(mask), |window| {
                                        level_bar::paint(window, bounds, bar, level)
                                    });
                                    continue;
                                }
                            };
                            let level = live_line::level_of(app.levels.get(id), now);
                            window.with_content_mask(Some(mask), |window| {
                                live_line::paint_line(
                                    window,
                                    bounds,
                                    phase,
                                    level,
                                    LineState::Live,
                                    style,
                                )
                            });
                        }
                        if !refused.is_empty() {
                            s.refused = true;
                            for source in refused {
                                if !s.latched.iter().any(|(l, _)| *l == source) {
                                    s.latched.push((source, layout));
                                }
                            }
                        }
                        s.painted = (app.render_generation, on_screen);
                        // Claims made after this view rendered: one more frame
                        // lets render start the ticker.
                        if !ticking && on_screen > 0 && !app.reduced_motion && window.is_visible() {
                            window.request_animation_frame();
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

/// The window's root while the overlay is on: the app, cached while Home or
/// a channel's editor is in front, and the overlay beside it. The app entity is created once here,
/// whichever way it renders.
pub(super) struct Shell {
    app: Entity<Rc2FxApp>,
    overlay: Entity<LineOverlay>,
    _stats: Option<Task<()>>,
}

impl Shell {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = cx.new(|cx| Rc2FxApp::new(window, cx));
        let slots = app.read(cx).line_slots.clone();
        let weak = app.downgrade();
        let overlay = cx.new(|cx| LineOverlay::new(weak, slots, window, cx));
        Self {
            app,
            overlay,
            _stats: stats_logger(cx),
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        count(&SHELL_RENDERS);
        // Other pages keep the app's ordinary every-frame rendering, so their
        // time-dependent displays behave exactly as without the overlay.
        let app = if self.app.read(cx).animated_page() {
            self.app
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element()
        } else {
            self.app.clone().into_any_element()
        };
        div()
            .relative()
            .size_full()
            .child(app)
            .child(self.overlay.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(shape: Shape, drawn: Option<f32>) -> Slots {
        Slots {
            claims: vec![Claim {
                source: Source::Path("mic-1".into()),
                shape,
                geometry: Some((
                    Bounds::new(point(px(0.), px(0.)), size(px(100.), px(40.))),
                    ContentMask {
                        bounds: Bounds::default(),
                    },
                    1.,
                )),
            }],
            drawn: vec![drawn],
            ..Slots::default()
        }
    }

    fn line() -> Shape {
        Shape::Line(live_line::MINI)
    }

    #[test]
    fn a_flat_line_needs_no_frame_but_a_moving_one_does() {
        assert!(slots(line(), Some(0.)).unchanged(&[Some(0.)]));
        // Same level, but the shape moves with time unless it is flat.
        assert!(!slots(line(), Some(0.5)).unchanged(&[Some(0.5)]));
    }

    #[test]
    fn a_line_going_flat_or_rising_draws_that_change() {
        assert!(!slots(line(), Some(0.5)).unchanged(&[Some(0.)]));
        assert!(!slots(line(), Some(0.)).unchanged(&[Some(0.5)]));
    }

    #[test]
    fn a_new_set_of_claims_always_draws() {
        let mut slots = slots(line(), Some(0.));
        slots.drawn.clear();
        assert!(!slots.unchanged(&[Some(0.)]));
    }
}
