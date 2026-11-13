//! The helper's own wake-up: the handshake between `Helper::queue` and
//! `await_frame`, the forecast it sleeps on, and a real helper thread on a
//! clock the tests own.
//!
//! Everything here is host-only: no mixer, no real device, no timing claims.
use super::*;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// A model of the handshake: every interleaving of the atomic steps of
// `Helper::queue` (and `Drop`) with those of `await_frame` and the frame.
// ---------------------------------------------------------------------------

/// Frames the modelled callback hands off.
const MODEL_FRAMES: u8 = 3;
/// When the modelled callbacks started, for `must_wake`.
const MODEL_BEGUN: u64 = 1_000_000_000;

/// What the modelled helper publishes in `wake_at`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Sleep {
    Awake,
    /// A timer the callback may leave the helper to: at the latest self-start.
    Near,
    /// A timer later than that.
    Far,
    Parked,
}
impl Sleep {
    fn wake_at(self) -> u64 {
        match self {
            Sleep::Awake => AWAKE,
            Sleep::Near => MODEL_BEGUN + LATEST_SELF_START_NS,
            Sleep::Far => MODEL_BEGUN + LATEST_SELF_START_NS + 1,
            Sleep::Parked => PARKED,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unpark {
    /// `must_wake`, as `Helper::queue` calls it.
    AsWritten,
    Never,
    /// Only a parked helper, whatever its timer.
    OnlyParked,
}
/// The code the model runs. `SOUND` is the code as written; each test of a
/// broken variant changes one field.
#[derive(Clone, Copy, Debug)]
struct Rules {
    /// The helper reads `state` after publishing a timed sleep, and a park.
    look_before_sleep: bool,
    look_before_park: bool,
    unpark: Unpark,
    /// The callback stores QUEUED before it reads `wake_at`.
    store_first: bool,
    /// SeqCst on the callback's store of QUEUED, and on the helper's stores
    /// of `wake_at`. Without it a store may still be in its thread's store
    /// buffer, unseen by the other thread, when that thread's next load runs.
    callback_seqcst: bool,
    helper_seqcst: bool,
    /// The pipeline may be dropped at any time between two callbacks, with a
    /// frame still queued or in the helper's hands, and before all frames
    /// were handed off. Otherwise only once the last frame is done.
    stop_early: bool,
    /// `Drop` unparks the helper after it stores the stop.
    stop_unparks: bool,
}
const SOUND: Rules = Rules {
    look_before_sleep: true,
    look_before_park: true,
    unpark: Unpark::AsWritten,
    store_first: true,
    callback_seqcst: true,
    helper_seqcst: true,
    stop_early: false,
    stop_unparks: true,
};
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum HelperAt {
    /// Frame done, IDLE stored: the forecast gives a timer or none.
    Plan,
    TimedStore,
    TimedLook,
    TimedClock,
    TimedSleep,
    ParkStore,
    ParkLook,
    ParkSleep,
    Run,
    Finish,
    Exited,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CallbackAt {
    /// `Pipeline::begin`: hands off only once the helper is idle.
    Begin,
    First,
    Second,
    Decide,
    /// `Drop`.
    Stop,
    Done,
}
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct World {
    // Memory, as both threads see it.
    queued: bool,
    wake_at: Sleep,
    stop: bool,
    /// The helper thread's unpark token.
    token: bool,
    // Stores not yet visible to the other thread.
    callback_pending: bool,
    helper_pending: Option<Sleep>,
    helper: HelperAt,
    timer: Sleep,
    blocked: bool,
    callback: CallbackAt,
    /// What the callback read from `wake_at`.
    read: Sleep,
    handed: u8,
    ran: u8,
    /// Frames the callback left to the helper's own timer, and that ran.
    left_alone: u8,
    fault: Option<&'static str>,
}
impl World {
    fn new() -> Self {
        Self {
            queued: false,
            wake_at: Sleep::Awake,
            stop: false,
            token: false,
            callback_pending: false,
            helper_pending: None,
            helper: HelperAt::ParkStore,
            timer: Sleep::Parked,
            blocked: false,
            callback: CallbackAt::Begin,
            read: Sleep::Awake,
            handed: 0,
            ran: 0,
            left_alone: 0,
            fault: None,
        }
    }
    fn publish(&mut self, rules: &Rules, value: Sleep) {
        if rules.helper_seqcst {
            self.wake_at = value;
        } else {
            // In order: an older store reaches memory first.
            if let Some(older) = self.helper_pending.replace(value) {
                self.wake_at = older;
            }
        }
    }
    /// The sleep itself: with the token set it returns at once.
    fn sleep(&mut self, again: HelperAt) {
        // A thread that blocks has long since emptied its store buffer.
        if let Some(value) = self.helper_pending.take() {
            self.wake_at = value;
        }
        if self.token {
            self.token = false;
            self.helper = again;
        } else {
            self.blocked = true;
        }
    }
    /// Every state one atomic step away.
    fn steps(&self, rules: &Rules) -> Vec<World> {
        let mut next = Vec::new();
        let mut push = |change: &dyn Fn(&mut World)| {
            let mut world = self.clone();
            change(&mut world);
            next.push(world);
        };
        // Store buffers drain at any time.
        if self.callback_pending {
            push(&|w| {
                w.callback_pending = false;
                w.queued = true;
            });
        }
        if let Some(value) = self.helper_pending {
            push(&|w| {
                w.helper_pending = None;
                w.wake_at = value;
            });
        }
        match self.helper {
            HelperAt::Plan => {
                push(&|w| w.helper = HelperAt::ParkStore);
                for timer in [Sleep::Near, Sleep::Far] {
                    push(&|w| {
                        w.timer = timer;
                        w.helper = HelperAt::TimedStore;
                    });
                }
            }
            HelperAt::TimedStore => push(&|w| {
                w.publish(rules, w.timer);
                w.helper = if rules.look_before_sleep {
                    HelperAt::TimedLook
                } else {
                    HelperAt::TimedClock
                };
            }),
            HelperAt::TimedLook => push(&|w| {
                w.helper = if w.stop {
                    HelperAt::Exited
                } else if w.queued {
                    HelperAt::Run
                } else {
                    HelperAt::TimedClock
                };
            }),
            // The timer's time has come, or not yet.
            HelperAt::TimedClock => {
                push(&|w| w.helper = HelperAt::ParkStore);
                push(&|w| w.helper = HelperAt::TimedSleep);
            }
            HelperAt::TimedSleep if !self.blocked => {
                push(&|w| w.sleep(HelperAt::TimedStore));
                // The timer's time came between the two clock reads: no
                // sleep at all (`Kernel::sleep_until`), and a token is kept.
                push(&|w| w.helper = HelperAt::TimedStore);
            }
            HelperAt::TimedSleep => {
                // The timer ends it; so does an unpark.
                push(&|w| {
                    w.blocked = false;
                    w.helper = HelperAt::TimedStore;
                });
                if self.token {
                    push(&|w| {
                        w.token = false;
                        w.blocked = false;
                        w.helper = HelperAt::TimedStore;
                    });
                }
            }
            HelperAt::ParkStore => push(&|w| {
                w.publish(rules, Sleep::Parked);
                w.helper = if rules.look_before_park {
                    HelperAt::ParkLook
                } else {
                    HelperAt::ParkSleep
                };
            }),
            HelperAt::ParkLook => push(&|w| {
                w.helper = if w.stop {
                    HelperAt::Exited
                } else if w.queued {
                    HelperAt::Run
                } else {
                    HelperAt::ParkSleep
                };
            }),
            HelperAt::ParkSleep if !self.blocked => push(&|w| w.sleep(HelperAt::ParkStore)),
            // An unpark ends a park. It may also end for no reason: the helper
            // then looks again, and no frame may run because of it. Nothing
            // relies on that to get a frame started: a helper parked with a
            // frame queued and no unpark to come is `stuck`, whatever leaves
            // that state.
            HelperAt::ParkSleep => {
                if self.token {
                    push(&|w| {
                        w.token = false;
                        w.blocked = false;
                        w.helper = HelperAt::ParkStore;
                    });
                }
                push(&|w| {
                    w.blocked = false;
                    w.helper = HelperAt::ParkStore;
                });
            }
            HelperAt::Run => push(&|w| {
                w.publish(rules, Sleep::Awake);
                if !w.queued {
                    w.fault = Some("ran with no frame queued");
                }
                w.ran += 1;
                if w.ran != w.handed {
                    w.fault = Some("ran a frame twice or out of order");
                }
                w.helper = HelperAt::Finish;
            }),
            HelperAt::Finish => push(&|w| {
                w.queued = false;
                w.helper = HelperAt::Plan;
            }),
            HelperAt::Exited => {}
        }
        let store = |w: &mut World| {
            w.handed += 1;
            if rules.callback_seqcst {
                w.queued = true;
            } else {
                w.callback_pending = true;
            }
        };
        let load = |w: &mut World| w.read = w.wake_at;
        match self.callback {
            CallbackAt::Begin if self.handed == MODEL_FRAMES => {
                // Dropped only once the last frame is done, as the mixer's
                // pipeline is (`Pipeline::counters` waits for it), or at any
                // time.
                if rules.stop_early
                    || (!self.queued && !self.callback_pending && self.ran == self.handed)
                {
                    push(&|w| w.callback = CallbackAt::Stop);
                }
            }
            CallbackAt::Begin => {
                if !self.queued && !self.callback_pending {
                    push(&|w| w.callback = CallbackAt::First);
                }
                if rules.stop_early {
                    push(&|w| w.callback = CallbackAt::Stop);
                }
            }
            CallbackAt::First => push(&|w| {
                if rules.store_first {
                    store(w);
                } else {
                    load(w);
                }
                w.callback = CallbackAt::Second;
            }),
            CallbackAt::Second => push(&|w| {
                if rules.store_first {
                    load(w);
                } else {
                    store(w);
                }
                w.callback = CallbackAt::Decide;
            }),
            CallbackAt::Decide => push(&|w| {
                let wake = match rules.unpark {
                    Unpark::AsWritten => must_wake(w.read.wake_at(), MODEL_BEGUN),
                    Unpark::Never => false,
                    Unpark::OnlyParked => w.read == Sleep::Parked,
                };
                if wake {
                    w.token = true;
                } else if w.read == Sleep::Near {
                    w.left_alone += 1;
                }
                w.callback = CallbackAt::Begin;
            }),
            CallbackAt::Stop => push(&|w| {
                w.stop = true;
                if rules.stop_unparks {
                    w.token = true;
                }
                w.callback = CallbackAt::Done;
            }),
            CallbackAt::Done => {}
        }
        next
    }
    /// A state nothing but luck gets out of: the callback has done all it
    /// will do for the frame it handed off, and the helper sleeps on.
    fn stuck(&self) -> Option<&'static str> {
        let settled = !self.callback_pending
            && self.helper_pending.is_none()
            && matches!(self.callback, CallbackAt::Begin | CallbackAt::Done);
        if !(settled && self.blocked && !self.token) {
            return None;
        }
        match self.helper {
            HelperAt::ParkSleep if self.queued => Some("lost wake: parked with a frame queued"),
            HelperAt::ParkSleep if self.stop => Some("parked through the stop"),
            HelperAt::TimedSleep if self.stop && self.timer == Sleep::Far => {
                Some("asleep on a far timer through the stop")
            }
            HelperAt::TimedSleep if self.queued && self.timer == Sleep::Far => {
                Some("late start: asleep past the latest self-start with a frame queued")
            }
            _ => None,
        }
    }
}
struct Explored {
    states: usize,
    faults: HashSet<&'static str>,
    /// States with nothing left to do.
    ends: Vec<World>,
    /// The most frames in one run that the helper started from its own timer.
    left_alone: u8,
}
fn explore(rules: &Rules) -> Explored {
    let mut seen = HashSet::new();
    let mut pending = vec![World::new()];
    let mut explored = Explored {
        states: 0,
        faults: HashSet::new(),
        ends: Vec::new(),
        left_alone: 0,
    };
    while let Some(world) = pending.pop() {
        if !seen.insert(world.clone()) {
            continue;
        }
        explored.states += 1;
        if let Some(fault) = world.fault.or(world.stuck()) {
            explored.faults.insert(fault);
            continue;
        }
        let next = world.steps(rules);
        if next.is_empty() {
            explored.left_alone = explored.left_alone.max(world.left_alone);
            explored.ends.push(world);
        }
        pending.extend(next);
    }
    explored
}

/// In every interleaving of the handoff with the helper going to sleep on a
/// timer or parking, each frame runs once and in order, none waits for a wake
/// that does not come or for a timer set too late, and the stop is noticed.
#[test]
fn no_interleaving_of_the_handoff_and_the_helpers_sleep_loses_or_repeats_a_frame() {
    let explored = explore(&SOUND);
    assert!(explored.faults.is_empty(), "{:?}", explored.faults);
    // Not vacuous: thousands of states, every run ends with the helper gone
    // and all frames run, and some runs left every frame to the timer.
    assert!(explored.states > 1_000, "{}", explored.states);
    assert!(!explored.ends.is_empty());
    for end in &explored.ends {
        assert_eq!(
            (end.helper, end.callback, end.ran, end.queued),
            (HelperAt::Exited, CallbackAt::Done, MODEL_FRAMES, false),
            "{end:?}"
        );
    }
    assert_eq!(explored.left_alone, MODEL_FRAMES);
}

/// The model tells each broken handshake from the sound one, so it is the
/// rules that keep the frames safe and not a model that sees nothing.
#[test]
fn the_model_finds_each_broken_handshake() {
    const LOST: &str = "lost wake: parked with a frame queued";
    const LATE: &str = "late start: asleep past the latest self-start with a frame queued";
    let cases: [(&str, Rules, &str); 7] = [
        (
            "parks without looking again",
            Rules {
                look_before_park: false,
                ..SOUND
            },
            LOST,
        ),
        (
            "sleeps on its timer without looking again",
            Rules {
                look_before_sleep: false,
                ..SOUND
            },
            LATE,
        ),
        (
            "the callback never wakes",
            Rules {
                unpark: Unpark::Never,
                ..SOUND
            },
            LOST,
        ),
        (
            "the callback wakes only a parked helper",
            Rules {
                unpark: Unpark::OnlyParked,
                ..SOUND
            },
            LATE,
        ),
        (
            "the callback reads wake_at before it stores QUEUED",
            Rules {
                store_first: false,
                ..SOUND
            },
            LOST,
        ),
        (
            "the callback's store is not SeqCst",
            Rules {
                callback_seqcst: false,
                ..SOUND
            },
            LOST,
        ),
        (
            "the helper's stores are not SeqCst",
            Rules {
                helper_seqcst: false,
                ..SOUND
            },
            LOST,
        ),
    ];
    for (what, rules, fault) in cases {
        let explored = explore(&rules);
        assert!(
            explored.faults.contains(fault),
            "{what}: {:?}",
            explored.faults
        );
    }
}

/// The pipeline dropped at any moment between two callbacks: with a frame
/// queued and left to the helper's timer, with one in the helper's hands, or
/// before any. The helper always leaves, out of either sleep; every frame it
/// started was a queued one, once and in order; and the only frame that may
/// be left is the last one handed off, still queued and never started.
#[test]
fn a_stop_at_any_moment_ends_the_helper_and_starts_no_frame_twice() {
    let explored = explore(&Rules {
        stop_early: true,
        ..SOUND
    });
    assert!(explored.faults.is_empty(), "{:?}", explored.faults);
    assert!(explored.states > 1_000, "{}", explored.states);
    let (mut all_ran, mut one_left, mut none_handed) = (false, false, false);
    for end in &explored.ends {
        assert_eq!(
            (end.helper, end.callback),
            (HelperAt::Exited, CallbackAt::Done),
            "{end:?}"
        );
        let left = end.handed - end.ran;
        assert!(left <= 1 && (left == 1) == end.queued, "{end:?}");
        all_ran |= left == 0 && end.handed == MODEL_FRAMES;
        one_left |= left == 1;
        none_handed |= end.handed == 0;
    }
    assert!(all_ran && one_left && none_handed);
    // It is the stop's own unpark that gets the helper out of either sleep:
    // without it the model finds a helper that sleeps on.
    for stop_early in [false, true] {
        let explored = explore(&Rules {
            stop_early,
            stop_unparks: false,
            ..SOUND
        });
        for fault in [
            "parked through the stop",
            "asleep on a far timer through the stop",
        ] {
            assert!(explored.faults.contains(fault), "{:?}", explored.faults);
        }
    }
}

// ---------------------------------------------------------------------------
// The decision and the forecast.
// ---------------------------------------------------------------------------

#[test]
fn the_callback_wakes_a_parked_helper_and_one_whose_timer_is_too_late() {
    let begun = 5_000_000_000;
    assert!(!must_wake(AWAKE, begun));
    assert!(must_wake(PARKED, begun));
    assert!(!must_wake(begun + 1, begun));
    assert!(!must_wake(begun + LATEST_SELF_START_NS, begun));
    assert!(must_wake(begun + LATEST_SELF_START_NS + 1, begun));
    // A timer that is already due is not waited for either: the helper is up.
    assert!(!must_wake(begun - PERIOD_NS, begun));
    // A callback with no start stamp leaves nothing to a timer.
    assert!(must_wake(begun, 0));
    assert!(must_wake(PARKED, u64::MAX));
    // The bound leaves the helper its frame before a callback half a period
    // on gives up waiting, and the margin fits a usual callback under it.
    const USUAL_FRAME_NS: u64 = 240_000;
    const {
        assert!(LATEST_SELF_START_NS + USUAL_FRAME_NS <= PERIOD_NS / 2 + HELPER_WAIT_NS);
        assert!(WAKE_MARGIN_NS < IRREGULAR_NS);
        assert!(WAKE_MARGIN_NS + PERIOD_NS / 3 < LATEST_SELF_START_NS);
    }
}

const T0: u64 = 50_000_000_000;
/// A callback's usual length here.
const LENGTH: u64 = 700_000;

/// Feeds `count` callbacks a period apart from `first`, each ending
/// `LENGTH + stray(k)` after its start; returns the last forecast.
fn feed(
    forecast: &mut Forecast,
    first: u64,
    count: u64,
    stray: impl Fn(u64) -> i64,
) -> Option<u64> {
    let mut last = None;
    for k in 0..count {
        let begun = first + k * PERIOD_NS;
        last = forecast.observe(begun, (begun + LENGTH).wrapping_add_signed(stray(k)));
    }
    last
}

#[test]
fn the_forecast_sets_a_timer_only_after_regular_callbacks() {
    let mut forecast = Forecast::default();
    // The first callback is the anchor; the next FORECAST_WARMUP - 1 only
    // build the streak.
    for k in 0..u64::from(FORECAST_WARMUP) {
        let begun = T0 + k * PERIOD_NS;
        assert_eq!(
            forecast.observe(begun, begun + LENGTH),
            None,
            "callback {k}"
        );
    }
    assert_eq!(forecast.deviation, Some(0));
    // From then on: one period after the last end, plus the margin.
    for k in u64::from(FORECAST_WARMUP)..200 {
        let begun = T0 + k * PERIOD_NS;
        let ended = begun + LENGTH;
        assert_eq!(
            forecast.observe(begun, ended),
            Some(ended + PERIOD_NS + WAKE_MARGIN_NS),
            "callback {k}"
        );
    }
}

#[test]
fn the_forecast_follows_jitter_by_an_eighth_and_reports_the_deviation() {
    let mut forecast = Forecast::default();
    feed(&mut forecast, T0, 40, |_| 0);
    let begun = T0 + 40 * PERIOD_NS;
    // 80 us late: the anchor moves 10 us.
    let wake_at = forecast.observe(begun, begun + LENGTH + 80_000).unwrap();
    assert_eq!(forecast.deviation, Some(80_000));
    assert_eq!(
        wake_at,
        begun + LENGTH + 10_000 + PERIOD_NS + WAKE_MARGIN_NS
    );
    // The next, on the old grid, is 10 us early for the moved anchor.
    let begun = begun + PERIOD_NS;
    let wake_at = forecast.observe(begun, begun + LENGTH).unwrap();
    assert_eq!(forecast.deviation, Some(-10_000));
    assert_eq!(wake_at, begun + LENGTH + 8_750 + PERIOD_NS + WAKE_MARGIN_NS);
    // Jitter within the band never breaks the streak.
    let stray = |k: u64| (k * 7919 % 400_001) as i64 - 200_000;
    for k in 0..500 {
        let begun = begun + (k + 1) * PERIOD_NS;
        let ended = (begun + LENGTH).wrapping_add_signed(stray(k) / 4);
        assert!(forecast.observe(begun, ended).is_some(), "callback {k}");
    }
}

#[test]
fn anything_irregular_starts_the_forecast_over() {
    let warm = |forecast: &mut Forecast, first: u64| {
        assert!(feed(forecast, first, 40, |_| 0).is_some());
        first + 40 * PERIOD_NS
    };
    let cold = |forecast: &mut Forecast, from: u64| {
        // As after a first callback: a full warm-up before the next timer.
        for k in 1..u64::from(FORECAST_WARMUP) {
            let begun = from + k * PERIOD_NS;
            assert_eq!(
                forecast.observe(begun, begun + LENGTH),
                None,
                "callback {k}"
            );
        }
        let begun = from + u64::from(FORECAST_WARMUP) * PERIOD_NS;
        assert!(forecast.observe(begun, begun + LENGTH).is_some());
    };
    // A callback that ends late, or early, by more than the band.
    for stray in [IRREGULAR_NS as i64 + 1, -(IRREGULAR_NS as i64) - 1] {
        let mut forecast = Forecast::default();
        let next = warm(&mut forecast, T0).wrapping_add_signed(stray);
        assert_eq!(forecast.observe(next, next + LENGTH), None);
        assert_eq!(forecast.deviation, None);
        cold(&mut forecast, next);
    }
    // At the band's edge it is still regular.
    let mut forecast = Forecast::default();
    let next = warm(&mut forecast, T0) + IRREGULAR_NS;
    assert!(forecast.observe(next, next + LENGTH).is_some());
    // A skipped period, a doubled one, and a long stop.
    for gap in [2 * PERIOD_NS, PERIOD_NS / 2, 5_000 * PERIOD_NS] {
        let mut forecast = Forecast::default();
        let next = warm(&mut forecast, T0) - PERIOD_NS + gap;
        assert_eq!(forecast.observe(next, next + LENGTH), None, "gap {gap}");
        cold(&mut forecast, next);
    }
}

#[test]
fn the_forecast_takes_a_callbacks_own_end_stamp_or_the_one_before() {
    // The callback has not stored its end when the helper finishes (a short
    // frame): the helper reads the end of the callback before, and takes
    // this one to be as long. On a steady grid that is the same forecast.
    let mut forecast = Forecast::default();
    let mut last = None;
    for k in 0..40 {
        let begun = T0 + k * PERIOD_NS;
        let stale = if k == 0 {
            0
        } else {
            begun - PERIOD_NS + LENGTH
        };
        last = forecast.observe(begun, stale);
        // One frame later than with the callback's own stamp: the first has
        // none at all.
        assert_eq!(last.is_some(), k > u64::from(FORECAST_WARMUP), "frame {k}");
    }
    let grid = |begun: u64| begun + LENGTH + PERIOD_NS + WAKE_MARGIN_NS;
    assert_eq!(last, Some(grid(T0 + 39 * PERIOD_NS)));
    // The two mixed, as the race goes: the streak holds.
    for k in 40..80 {
        let begun = T0 + k * PERIOD_NS;
        let ended = begun + LENGTH - if k % 3 == 0 { PERIOD_NS } else { 0 };
        assert_eq!(
            forecast.observe(begun, ended),
            Some(grid(begun)),
            "frame {k}"
        );
        assert_eq!(forecast.deviation, Some(0));
    }
    // The callback before was 100 us longer, and this one's end is not
    // known: taken to be as long.
    let begun = T0 + 80 * PERIOD_NS;
    forecast.observe(begun, begun + LENGTH + 100_000);
    let begun = begun + PERIOD_NS;
    forecast.observe(begun, begun - PERIOD_NS + LENGTH + 100_000);
    assert_eq!(forecast.deviation, Some(100_000 - 100_000 / 8));

    // No stamp of either callback is no sample, and the forecast is over:
    // an older stamp, a later callback's (one that found the helper busy
    // ran in between, or the helper ran through it), or none.
    for ended in [
        |begun: u64| begun - 2 * PERIOD_NS + LENGTH,
        |begun: u64| begun + PERIOD_NS + LENGTH,
        |_: u64| 0,
    ] {
        let mut forecast = Forecast::default();
        assert!(feed(&mut forecast, T0, 40, |_| 0).is_some());
        let begun = T0 + 40 * PERIOD_NS;
        assert_eq!(forecast.observe(begun, ended(begun)), None);
        assert_eq!((forecast.anchor, forecast.streak), (0, 0));
    }
    // After a callback that did not hand off (the helper was busy), the
    // latest stamp is that callback's: not the frame's before, two periods
    // back.
    let mut forecast = Forecast::default();
    assert!(feed(&mut forecast, T0, 40, |_| 0).is_some());
    let begun = T0 + 41 * PERIOD_NS;
    assert_eq!(forecast.observe(begun, begun - PERIOD_NS + LENGTH), None);
    // Nothing without a start stamp, and nothing in a pipeline nobody stamps:
    // the helper parks after every frame, as before.
    let mut forecast = Forecast::default();
    for k in 1..100 {
        assert_eq!(forecast.observe(0, T0 + k * PERIOD_NS), None);
        assert_eq!(forecast.observe(T0 + k * PERIOD_NS, 0), None);
    }
}

/// When callbacks usually end so late that the next would have to wake the
/// helper anyway, it sets no timer and parks, exactly as before.
#[test]
fn no_timer_is_set_that_the_callback_would_override() {
    let fits = LATEST_SELF_START_NS - WAKE_MARGIN_NS;
    for (length, timer) in [(fits, true), (fits + 1, false), (PERIOD_NS * 3 / 4, false)] {
        let mut forecast = Forecast::default();
        let mut last = None;
        for k in 0..100 {
            let begun = T0 + k * PERIOD_NS;
            last = forecast.observe(begun, begun + length);
        }
        assert_eq!(last.is_some(), timer, "length {length}");
        if let Some(wake_at) = last {
            // And the callback it is for agrees.
            assert!(!must_wake(wake_at, T0 + 100 * PERIOD_NS));
        }
    }
}

// ---------------------------------------------------------------------------
// A real helper thread on the tests' clock.
// ---------------------------------------------------------------------------

/// The longest a test waits for the helper before it calls the frame lost.
const PATIENCE: Duration = Duration::from_secs(20);

/// The time the tests give the helper, and whether it is in one of its
/// sleeps: past its last look at `state`, so that a frame handed off now is
/// one it comes by in that sleep, and not one it finds queued before it.
#[derive(Default)]
struct Clock {
    now: AtomicU64,
    napping: AtomicBool,
}
impl Clock {
    fn at(now: u64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicU64::new(now),
            napping: AtomicBool::new(false),
        })
    }
}
/// A clock only the test moves. The helper's timed sleep ends when the test
/// fires its timer (`Rig::advance`), or on an unpark; its park is a real one.
struct Stepped {
    clock: Arc<Clock>,
}
impl Stepped {
    fn nap(&self) {
        self.clock.napping.store(true, Ordering::SeqCst);
        thread::park();
        self.clock.napping.store(false, Ordering::SeqCst);
    }
}
impl Pace for Stepped {
    fn now(&self) -> u64 {
        self.clock.now.load(Ordering::SeqCst)
    }
    fn sleep_until(&self, wake_at: u64) {
        if self.now() < wake_at {
            self.nap();
        }
    }
    fn park(&self) {
        self.nap();
    }
}
fn empty_handoff() -> Handoff {
    Handoff {
        seq: 0,
        frames: FRAMES,
        cycle_start: 0,
        started: 0,
        native_success: true,
        jobs: Vec::new(),
        work: Vec::new(),
        counters: Counters::default(),
        done_seq: 0,
        helper: None,
    }
}
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::yield_now();
    }
}
/// A helper and the callback's side of it, with empty frames.
struct Rig {
    helper: Helper,
    handoff: Box<UnsafeCell<Handoff>>,
    clock: Arc<Clock>,
    seq: u64,
    /// With the bench gauges on: every handoff is stamped.
    gauged: bool,
    /// The callback's end is published before the handoff, so that the
    /// helper always finds it and each test has one outcome. The real
    /// callback publishes it just after: the racing test does too.
    end_first: bool,
}
impl Rig {
    fn new(at: u64) -> Self {
        let clock = Clock::at(at);
        Self::with(
            Stepped {
                clock: clock.clone(),
            },
            clock,
        )
    }
    fn with<P: Pace + Send + 'static>(pace: P, clock: Arc<Clock>) -> Self {
        Self {
            helper: Helper::start_with(pace).unwrap(),
            handoff: Box::new(UnsafeCell::new(empty_handoff())),
            clock,
            seq: 0,
            gauged: false,
            end_first: true,
        }
    }
    fn shared(&self) -> &Shared {
        &self.helper.shared
    }
    fn now(&self) -> u64 {
        self.clock.now.load(Ordering::SeqCst)
    }
    /// Waits for the helper to finish its frame and settle into its sleep;
    /// returns what it published: PARKED, or a timer still ahead. Into the
    /// sleep itself (`Clock::napping`): what it publishes it publishes before
    /// its last look at `state`, and a frame handed off between the two it
    /// rightly starts at once, as one it found queued.
    fn asleep(&self) -> u64 {
        let shared = self.shared();
        let settled = || {
            let wake_at = shared.wake_at.load(Ordering::SeqCst);
            (self.helper.idle()
                && self.clock.napping.load(Ordering::SeqCst)
                && (wake_at == PARKED || wake_at > self.now()))
            .then_some(wake_at)
        };
        until("the helper never went to sleep", || settled().is_some());
        settled().unwrap()
    }
    /// Moves the clock, and fires the helper's timer if that passes it.
    fn advance(&self, to: u64) {
        assert!(to >= self.now());
        let shared = self.shared();
        let wake_at = shared.wake_at.load(Ordering::SeqCst);
        self.clock.now.store(to, Ordering::SeqCst);
        if wake_at != AWAKE && wake_at != PARKED && wake_at <= to {
            self.helper.thread.as_ref().unwrap().thread().unpark();
            until("the helper slept through its timer", || {
                shared.wake_at.load(Ordering::SeqCst) != wake_at
            });
        }
    }
    /// The handoff of a callback that started at `begun`, as it is.
    fn queue(&mut self, begun: u64) -> Woke {
        until("the helper never finished its frame", || self.helper.idle());
        self.seq += 1;
        let handoff = unsafe { &mut *self.handoff.get() };
        assert_eq!(
            handoff.done_seq,
            self.seq - 1,
            "the frame before was not run"
        );
        handoff.seq = self.seq;
        handoff.started = begun;
        let queued_at = if self.gauged { self.now() } else { 0 };
        if self.end_first {
            self.shared().ended.store(self.now(), Ordering::Relaxed);
        }
        let woke = self.helper.queue(self.handoff.get(), begun, queued_at);
        self.shared().ended.store(self.now(), Ordering::Relaxed);
        woke
    }
    /// A whole callback that started at `begun` and ends at `at`, against a
    /// helper that has settled: the clock runs to `at`, the frame is handed
    /// off, and the clock runs on to the helper's timer if it was left to it.
    /// Returns what the handoff did and how and when the helper started.
    fn callback(&mut self, begun: u64, at: u64) -> (Woke, Start, u64) {
        self.asleep();
        self.advance(at);
        self.asleep();
        let woke = self.queue(begun);
        if woke == Woke::No {
            let wake_at = self.shared().wake_at.load(Ordering::SeqCst);
            assert!(wake_at != PARKED && wake_at != AWAKE, "left to no timer");
            self.advance(wake_at);
        }
        until("the frame was never run", || self.helper.idle());
        let (seq, start, started) = *self.shared().ran.lock().unwrap().last().unwrap();
        assert_eq!(seq, self.seq);
        (woke, start, started)
    }
    /// `count` regular callbacks from `first`, `stray(k)` off their usual end.
    fn regular(
        &mut self,
        first: u64,
        count: u64,
        stray: impl Fn(u64) -> i64,
    ) -> Vec<(Woke, Start, u64)> {
        (0..count)
            .map(|k| {
                let begun = first + k * PERIOD_NS;
                self.callback(begun, (begun + LENGTH).wrapping_add_signed(stray(k)))
            })
            .collect()
    }
    /// Every frame ran once, in the order it was handed off.
    fn assert_ran_once_in_order(&self) {
        until("the last frame was never run", || self.helper.idle());
        let ran: Vec<u64> = self
            .shared()
            .ran
            .lock()
            .unwrap()
            .iter()
            .map(|frame| frame.0)
            .collect();
        assert_eq!(ran, (1..=self.seq).collect::<Vec<_>>());
        assert_eq!(unsafe { &*self.handoff.get() }.done_seq, self.seq);
    }
    /// Drops the helper, which joins its thread; a join that hangs fails.
    fn stop(self) {
        let (done, joined) = std::sync::mpsc::channel();
        let Rig {
            helper, handoff, ..
        } = self;
        let dropper = thread::spawn(move || {
            drop(helper);
            done.send(()).unwrap();
        });
        joined
            .recv_timeout(PATIENCE)
            .expect("the helper did not stop");
        dropper.join().unwrap();
        drop(handoff);
    }
}
/// Deterministic jitter within +-`span` ns.
fn jitter(k: u64, span: i64) -> i64 {
    let x = (k as u32)
        .wrapping_mul(2_654_435_761)
        .wrapping_add(0x9e37_79b9);
    i64::from(x >> 8) % (2 * span + 1) - span
}

/// With callbacks a period apart, their ends jittering by less than the
/// margin, the callback wakes the helper for the warm-up frames and never
/// again: every later frame starts from the helper's own timer, after its
/// handoff and within the margin and the jitter of it.
#[test]
fn regular_handoffs_need_no_wake_after_the_warm_up() {
    let mut rig = Rig::new(T0);
    // Together under half the margin, as the ends' spread is on the mixer.
    let span = (WAKE_MARGIN_NS * 7 / 20) as i64;
    let start_span = (WAKE_MARGIN_NS / 10) as i64;
    let frames = 400;
    let mut wakes_after_warm_up = 0;
    for k in 0..frames {
        let begun = (T0 + k * PERIOD_NS).wrapping_add_signed(jitter(k + 1_000, start_span));
        let at = (begun + LENGTH).wrapping_add_signed(jitter(k, span));
        let (woke, start, started) = rig.callback(begun, at);
        if k <= u64::from(FORECAST_WARMUP) {
            assert_eq!(woke, Woke::Parked, "frame {k}");
            assert_eq!(start, Start::Parked { expired: false }, "frame {k}");
            assert_eq!(started, at, "frame {k}");
        } else {
            wakes_after_warm_up += u64::from(woke != Woke::No);
            assert_eq!(
                start,
                Start::Slept(started),
                "frame {k}: not from its timer"
            );
            let latency = started - at;
            assert!(
                latency <= WAKE_MARGIN_NS + 2 * (span + start_span) as u64,
                "frame {k}: started {latency} ns after its handoff"
            );
            assert!(started <= begun + LATEST_SELF_START_NS, "frame {k}");
        }
    }
    assert_eq!(wakes_after_warm_up, 0);
    rig.assert_ran_once_in_order();
    rig.stop();
}

/// Handoffs that come early, late, in bursts, after skipped periods and after
/// a long stop: each frame runs once and in order, and whenever the helper
/// would not have started in time by itself the callback wakes it, so that it
/// starts at the handoff as it always did.
#[test]
fn early_late_burst_and_skipped_handoffs_each_run_once_and_start_in_time() {
    let mut rig = Rig::new(T0);
    let warm = u64::from(FORECAST_WARMUP) + 4;
    let none = |_: u64| 0;
    let all_woken = |frames: &[(Woke, Start, u64)]| {
        frames
            .iter()
            .all(|frame| frame.0 == Woke::Parked && frame.1 == Start::Parked { expired: false })
    };
    let mut first = T0;
    // After anything irregular: woken for the warm-up frames (one fewer if
    // the irregular callback itself already was on the new grid), then on the
    // timer, for good.
    let warm_up = |rig: &mut Rig, first: &mut u64| {
        let frames = rig.regular(*first, warm, none);
        let (woken, timed) = frames.split_at(FORECAST_WARMUP as usize);
        assert!(all_woken(woken), "{woken:?}");
        let on_timer = |frame: &(Woke, Start, u64)| {
            frame.0 == Woke::No && matches!(frame.1, Start::Slept(at) if at == frame.2)
        };
        assert!(all_woken(&timed[..1]) || on_timer(&timed[0]), "{timed:?}");
        assert!(timed[1..].iter().all(on_timer), "{timed:?}");
        *first += warm * PERIOD_NS;
    };
    warm_up(&mut rig, &mut first);

    // A little early, a little late (inside the margin): the timer covers
    // both, no wake.
    let margin = WAKE_MARGIN_NS as i64;
    for stray in [-margin * 3 / 4, margin * 3 / 4, -margin, margin * 17 / 20] {
        let begun = first;
        let at = (begun + LENGTH).wrapping_add_signed(stray);
        let (woke, start, started) = rig.callback(begun, at);
        assert_eq!(woke, Woke::No, "stray {stray}");
        assert_eq!(start, Start::Slept(started), "stray {stray}");
        assert!(started >= at && started <= begun + LATEST_SELF_START_NS);
        // Back on the grid before the next.
        first += PERIOD_NS;
        rig.regular(first, 12, none);
        first += 12 * PERIOD_NS;
    }

    // Late by more than the margin: the timer finds nothing, the helper
    // parks, and the callback wakes it as it always did. Still regular, so
    // the next frame is the timer's again.
    let (begun, at) = (first, first + LENGTH + WAKE_MARGIN_NS + 25_000);
    assert_eq!(
        rig.callback(begun, at),
        (Woke::Parked, Start::Parked { expired: true }, at)
    );
    first += PERIOD_NS;
    let (woke, start, _) = rig.callback(first, first + LENGTH);
    assert_eq!(woke, Woke::No);
    assert!(matches!(start, Start::Slept(_)));
    first += PERIOD_NS;

    // A callback that starts late (a late cycle) and so ends far past the
    // timer: woken from the park, and the forecast starts over.
    let (begun, at) = (first + 900_000, first + 900_000 + LENGTH);
    assert_eq!(
        rig.callback(begun, at),
        (Woke::Parked, Start::Parked { expired: true }, at)
    );
    // The cycles after it are back on the grid, the first of them short.
    first += PERIOD_NS;
    warm_up(&mut rig, &mut first);

    // A callback half a period early (a doubled cycle): the helper's timer
    // is far off, so the callback wakes it out of the timed sleep and the
    // frame starts at the handoff, not at the timer.
    let (begun, at) = (first - PERIOD_NS / 2, first - PERIOD_NS / 2 + LENGTH);
    let timer = rig.asleep();
    assert!(timer > at + PERIOD_NS / 2 && timer != PARKED);
    assert_eq!(
        rig.callback(begun, at),
        (Woke::Late, Start::Slept(timer), at)
    );
    warm_up(&mut rig, &mut first);

    // Skipped periods: nothing for three periods, then on the grid again.
    first += 3 * PERIOD_NS;
    let (woke, start, started) = rig.callback(first, first + LENGTH);
    assert_eq!(
        (woke, start, started),
        (
            Woke::Parked,
            Start::Parked { expired: true },
            first + LENGTH
        )
    );
    first += PERIOD_NS;
    warm_up(&mut rig, &mut first);

    // A burst: callbacks back to back, as fast as the helper finishes, each
    // handed off without waiting for it to settle. Whatever it was doing,
    // every frame runs.
    rig.asleep();
    rig.advance(first + LENGTH);
    for k in 0..200 {
        if rig.queue(first + k) == Woke::No {
            // Left to a timer, if the helper was not simply still awake:
            // fire it.
            let wake_at = rig.shared().wake_at.load(Ordering::SeqCst);
            if wake_at != AWAKE && wake_at != PARKED {
                rig.advance(wake_at.max(rig.now()));
            }
        }
    }
    rig.assert_ran_once_in_order();
    first += 3 * PERIOD_NS;
    warm_up(&mut rig, &mut first);

    // The engine stops for ten seconds and comes back.
    first += 4_286 * PERIOD_NS + 123_456;
    let (woke, start, _) = rig.callback(first, first + LENGTH);
    assert_eq!(
        (woke, start),
        (Woke::Parked, Start::Parked { expired: true })
    );
    first += PERIOD_NS;
    warm_up(&mut rig, &mut first);

    rig.assert_ran_once_in_order();
    rig.stop();
}

/// A helper stops promptly out of its timed sleep and out of its park, and a
/// new one (the engine restarted) starts cold: woken for its warm-up frames,
/// then on its timer.
#[test]
fn the_helper_stops_from_either_sleep_and_a_restart_starts_cold() {
    // Parked from the start, never handed a frame.
    let rig = Rig::new(T0);
    assert_eq!(rig.asleep(), PARKED);
    rig.stop();
    let mut first = T0;
    for generation in 0..3 {
        let mut rig = Rig::new(first);
        let frames = rig.regular(first, 40, |k| jitter(k, 50_000));
        for (k, frame) in frames.iter().enumerate() {
            assert_eq!(
                frame.0 != Woke::No,
                k <= FORECAST_WARMUP as usize,
                "generation {generation}, frame {k}"
            );
        }
        rig.assert_ran_once_in_order();
        first += 40 * PERIOD_NS;
        let timer = rig.asleep();
        match generation {
            // Asleep on its timer.
            0 => assert!(timer != PARKED && timer > rig.now()),
            // Parked after a timer that found nothing.
            1 => {
                rig.advance(timer + 1);
                assert_eq!(rig.asleep(), PARKED);
            }
            // With a frame handed off and left to the timer: a helper told
            // to stop starts no frame, its pipeline being on its way out.
            _ => {
                rig.advance(first + LENGTH);
                assert_eq!(rig.queue(first), Woke::No);
                let shared = Arc::clone(&rig.helper.shared);
                rig.stop();
                assert_eq!(shared.ran.lock().unwrap().len(), 40);
                assert_eq!(shared.state.load(Ordering::SeqCst), QUEUED);
                continue;
            }
        }
        rig.stop();
        first += 1_000 * PERIOD_NS;
    }
}

/// Where the timed sleep is on time only for a real-time thread (the mixer's
/// kernel), a helper that has not been made one sets no timer: it parks after
/// every frame and every handoff wakes it, exactly as before, however regular
/// the callbacks are.
#[test]
fn a_helper_that_is_not_real_time_yet_sets_no_timer() {
    struct Strict(Stepped);
    impl Pace for Strict {
        fn now(&self) -> u64 {
            self.0.now()
        }
        fn sleep_until(&self, _: u64) {
            panic!("a timed sleep on a thread that is not real-time");
        }
        fn park(&self) {
            self.0.park();
        }
        fn needs_realtime(&self) -> bool {
            true
        }
    }
    let clock = Clock::at(T0);
    let mut rig = Rig::with(
        Strict(Stepped {
            clock: clock.clone(),
        }),
        clock,
    );
    for k in 0..100 {
        let begun = T0 + k * PERIOD_NS;
        assert_eq!(rig.asleep(), PARKED, "frame {k}");
        let (woke, start, started) = rig.callback(begun, begun + LENGTH);
        assert_eq!(
            (woke, start, started),
            (
                Woke::Parked,
                Start::Parked { expired: false },
                begun + LENGTH
            ),
            "frame {k}"
        );
    }
    rig.assert_ran_once_in_order();
    rig.stop();
    // The mixer's own pace is such a one; under test it is not, so that the
    // tests reach its timed sleep without a real-time thread.
    assert!(
        !Kernel.needs_realtime()
            && !Stepped {
                clock: Arc::default()
            }
            .needs_realtime()
    );
}

/// The sleeps the helper takes while nothing is handed off: one timed sleep
/// to its timer, then one park, and no more; wakes with nothing queued (an
/// older wake's token, a spurious return) send it back to the same sleep
/// without running anything.
#[test]
fn an_idle_helper_sleeps_and_does_not_spin() {
    struct Counted {
        inner: Stepped,
        naps: Arc<[AtomicU64; 2]>,
    }
    impl Pace for Counted {
        fn now(&self) -> u64 {
            self.inner.now()
        }
        fn sleep_until(&self, wake_at: u64) {
            self.naps[0].fetch_add(1, Ordering::SeqCst);
            self.inner.sleep_until(wake_at);
        }
        fn park(&self) {
            self.naps[1].fetch_add(1, Ordering::SeqCst);
            self.inner.park();
        }
    }
    let clock = Clock::at(T0);
    let naps = Arc::new([const { AtomicU64::new(0) }; 2]);
    let mut rig = Rig::with(
        Counted {
            inner: Stepped {
                clock: clock.clone(),
            },
            naps: naps.clone(),
        },
        clock,
    );
    let read = || [0, 1].map(|nap| naps[nap].load(Ordering::SeqCst));
    rig.regular(T0, 40, |_| 0);
    let timer = rig.asleep();
    assert!(timer != PARKED);
    let thread = rig.helper.thread.as_ref().unwrap().thread().clone();
    // One timed sleep for each frame left to the timer and one for this
    // wait; one park at the start and one after each warm-up frame but the
    // last.
    let warm_up = u64::from(FORECAST_WARMUP);
    until("the timed sleep", || read()[0] == 40 - warm_up);
    let before = read();
    assert_eq!(before, [40 - warm_up, 1 + warm_up]);
    // Five wakes for nothing, well before the timer: five more timed sleeps.
    for wake in 1..=5 {
        thread.unpark();
        until("back to its timed sleep", || read()[0] == before[0] + wake);
    }
    assert_eq!(rig.shared().wake_at.load(Ordering::SeqCst), timer);
    assert_eq!(read()[1], before[1]);
    // The timer, with nothing queued: one park.
    rig.advance(timer);
    assert_eq!(rig.asleep(), PARKED);
    until("the park", || read()[1] == before[1] + 1);
    for wake in 1..=5 {
        thread.unpark();
        until("back to its park", || read()[1] == before[1] + 1 + wake);
    }
    // Ten periods on it is still there, having run nothing since.
    rig.advance(timer + 10 * PERIOD_NS);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(read(), [before[0] + 5, before[1] + 6]);
    assert_eq!(rig.shared().ran.lock().unwrap().len(), 40);
    rig.assert_ran_once_in_order();
    rig.stop();
}

/// A real helper thread, raced into its sleep by handoffs that come as soon
/// as each frame is back. The clock is the test's. After each frame the
/// helper dawdles for up to a microsecond before it turns to its sleep, so
/// that the handoff lands before, in the middle of and after its last look
/// at `state`.
///
/// `timers_fire`: a timed sleep is a short real one, as if the timer could
/// fire at any moment. Otherwise it lasts until the test fires the timer,
/// and the test fires only a timer the callback may leave a frame to: a
/// helper asleep on a later one, with a frame queued, is never woken.
fn race(timers_fire: bool, frames: u64) {
    struct Racing {
        clock: Arc<Clock>,
        noise: AtomicU64,
        timers_fire: bool,
    }
    impl Pace for Racing {
        fn now(&self) -> u64 {
            self.clock.now.load(Ordering::SeqCst)
        }
        fn sleep_until(&self, _: u64) {
            if self.timers_fire {
                thread::park_timeout(Duration::from_micros(20));
            } else {
                thread::park();
            }
        }
        fn park(&self) {
            thread::park();
        }
        fn rest(&self) {
            let mut noise = self.noise.load(Ordering::Relaxed);
            noise ^= noise << 13;
            noise ^= noise >> 7;
            noise ^= noise << 17;
            self.noise.store(noise, Ordering::Relaxed);
            for _ in 0..noise % 90 {
                std::hint::black_box(noise);
            }
        }
    }
    let clock = Clock::at(T0);
    let mut rig = Rig::with(
        Racing {
            clock: clock.clone(),
            noise: AtomicU64::new(0x2545_f491_4f6c_dd1d),
            timers_fire,
        },
        clock,
    );
    rig.end_first = false;
    let helper = rig.helper.thread.as_ref().unwrap().thread().clone();
    const LATE: u64 = 56;
    let mut begun = T0;
    let mut woke = [0u64; 3];
    let mut noise = 0x9e37_79b9_7f4a_7c15_u64;
    for k in 0..frames {
        // The frame in flight: wait for it, firing the helper's timer if it
        // is asleep on one that it was rightly left to.
        let deadline = Instant::now() + PATIENCE;
        let mut spins = 0u32;
        while !rig.helper.idle() {
            spins += 1;
            assert!(
                !spins.is_multiple_of(4096) || Instant::now() < deadline,
                "frame {k} was lost"
            );
            let wake_at = rig.shared().wake_at.load(Ordering::SeqCst);
            if !timers_fire && wake_at != AWAKE && !must_wake(wake_at, begun) {
                helper.unpark();
            }
            std::hint::spin_loop();
        }
        // 64 callbacks: 17 warm-up frames that find the helper parking and
        // 14 on its timer; one half a period early, whose timer is too late
        // (woken out of the timed sleep); warm-up and timer again on that
        // grid; one a little late (the timer finds nothing and the helper
        // parks); and 7 irregular ones (parking).
        let phase = k % 64;
        begun += match phase {
            31 => PERIOD_NS / 2,
            LATE => PERIOD_NS + WAKE_MARGIN_NS + 20_000,
            57.. => PERIOD_NS + 2 * IRREGULAR_NS * (phase % 2) + 1,
            _ => PERIOD_NS,
        };
        // The helper is on its way to sleep: hand off somewhere along it. The
        // late callback's time comes first, and its timer, so that the timer
        // may already have found nothing.
        noise ^= noise << 13;
        noise ^= noise >> 7;
        noise ^= noise << 17;
        if phase == LATE {
            rig.clock.now.store(begun + LENGTH, Ordering::SeqCst);
            if !timers_fire {
                helper.unpark();
            }
        }
        for _ in 0..noise % if phase == LATE { 20_000 } else { 64 } {
            std::hint::black_box(noise);
        }
        rig.clock.now.store(begun + LENGTH, Ordering::SeqCst);
        woke[match rig.queue(begun) {
            Woke::No => 0,
            Woke::Parked => 1,
            Woke::Late => 2,
        }] += 1;
    }
    until("the last frame was lost", || {
        if !timers_fire {
            helper.unpark();
        }
        rig.helper.idle()
    });
    rig.assert_ran_once_in_order();
    let ran = rig.shared().ran.lock().unwrap();
    assert_eq!(ran.len() as u64, frames);
    let count = |wanted: fn(&Start) -> bool| ran.iter().filter(|frame| wanted(&frame.1)).count();
    let (queued, slept, parked) = (
        count(|start| *start == Start::Queued),
        count(|start| matches!(start, Start::Slept(_))),
        count(|start| matches!(start, Start::Parked { .. })),
    );
    drop(ran);
    // Every way into a frame was raced, and the callback left many to the
    // helper without a wake.
    eprintln!("starts: queued {queued}, slept {slept}, parked {parked}; woke {woke:?}");
    assert!(
        queued > 100 && slept > 100 && parked > 100,
        "{queued} {slept} {parked}"
    );
    assert!(woke.iter().all(|count| *count > 100), "{woke:?}");
    rig.stop();
}

/// Handoffs racing the helper into its sleep, a hundred and fifty thousand
/// times, with the timing of each varied: none is lost, and each frame runs
/// once and in order. Some find the helper on its timer, near or too far
/// off, the rest find it parking, straight from a frame or after a timer
/// that found nothing.
#[test]
fn handoffs_racing_the_helper_into_its_sleep_are_never_lost() {
    race(true, 150_000);
}

/// The same race with timers that fire only when the callback was right to
/// leave the frame to them: no frame is ever left to a timer set later than
/// the helper may start by itself, which here would never fire.
#[test]
fn no_racing_handoff_is_left_to_a_timer_that_is_too_late() {
    race(false, 150_000);
}

/// The bench gauges tell the frames the helper started by itself from those
/// the callback woke it for, and stay untouched without the gauges.
#[test]
fn the_wake_gauges_tell_self_starts_from_wakes() {
    let starts = || {
        [
            STARTS_QUEUED,
            STARTS_TIMER,
            STARTS_WOKEN_EARLY,
            STARTS_PARKED_AFTER_TIMER,
            STARTS_PARKED_NO_FORECAST,
        ]
        .map(|kind| STARTS[kind].load(Ordering::Relaxed))
    };
    let calls = || {
        [
            &WAKE_LATENCY,
            &WOKEN_LATENCY,
            &SELF_LATENCY,
            &HANDOFF_LATE,
            &HANDOFF_EARLY,
        ]
        .map(|gauge| gauge.calls.load(Ordering::Relaxed))
    };
    let (starts_before, calls_before) = (starts(), calls());
    let mut rig = Rig::new(T0);
    rig.gauged = true;
    // 17 woken out of a park with no forecast, then 23 on the timer, each
    // the margin after its handoff: a self-start in the wide histogram, in
    // the bin that holds the margin.
    let margin_bin = Fine::WIDE_BOUNDS_US
        .iter()
        .position(|&bound| WAKE_MARGIN_NS / 1_000 < bound)
        .unwrap();
    let self_bin = |bin: usize| SELF_LATENCY.bins[bin].load(Ordering::Relaxed);
    let bin_before = self_bin(margin_bin);
    let timer_late_before = TIMER_LATE.bins[0].load(Ordering::Relaxed);
    rig.regular(T0, 40, |k| jitter(k, 1_000));
    // One woken out of a park after its timer, and one out of the timed sleep.
    let first = T0 + 40 * PERIOD_NS;
    rig.callback(first, first + LENGTH + WAKE_MARGIN_NS + 20_000);
    let first = first + PERIOD_NS;
    rig.regular(first, 4, |_| 0);
    let begun = first + 4 * PERIOD_NS - PERIOD_NS / 2;
    assert_eq!(rig.callback(begun, begun + LENGTH).0, Woke::Late);
    rig.assert_ran_once_in_order();
    rig.stop();
    let (starts_after, calls_after) = (starts(), calls());
    let grew = |after: u64, before: u64, least: u64| after >= before + least;
    assert!(grew(
        starts_after[STARTS_TIMER],
        starts_before[STARTS_TIMER],
        27
    ));
    assert!(grew(
        starts_after[STARTS_WOKEN_EARLY],
        starts_before[STARTS_WOKEN_EARLY],
        1
    ));
    assert!(grew(
        starts_after[STARTS_PARKED_AFTER_TIMER],
        starts_before[STARTS_PARKED_AFTER_TIMER],
        1
    ));
    assert!(grew(
        starts_after[STARTS_PARKED_NO_FORECAST],
        starts_before[STARTS_PARKED_NO_FORECAST],
        17
    ));
    assert!(grew(calls_after[0], calls_before[0], 46));
    assert!(grew(calls_after[1], calls_before[1], 19));
    assert!(grew(calls_after[2], calls_before[2], 27));
    assert!(grew(
        calls_after[3] + calls_after[4],
        calls_before[3] + calls_before[4],
        40
    ));
    // Self-starts land about the margin after the handoff.
    assert!(self_bin(margin_bin) >= bin_before + 23);
    // And on this clock at the timer's very time: within the first bin.
    assert!(TIMER_LATE.bins[0].load(Ordering::Relaxed) >= timer_late_before + 27);
    let report = wake_gauges();
    for key in [
        "wakeCall",
        "wakeSent",
        "wakeLatency",
        "wokenLatency",
        "selfLatency",
        "timerLate",
        "handoffLate",
        "handoffEarly",
        "helperSlack",
        "end",
        "place",
    ] {
        assert_eq!(
            report[key]["histogram"].as_array().unwrap().len(),
            8,
            "{key}"
        );
    }
    assert_eq!(report["marginNs"], WAKE_MARGIN_NS);
    assert_eq!(report["latestSelfStartNs"], LATEST_SELF_START_NS);
    assert!(report["starts"]["timer"].as_u64().unwrap() >= 27);
    assert!(report["wakes"]["parked"].is_u64() && report["wakes"]["pastLatestSelfStart"].is_u64());
    assert_eq!(report["wideBoundsUs"][6], 1024);

    // A wide gauge bins sixteen times coarser.
    let wide = Fine::wide();
    for (elapsed, bin) in [(16_000, 0), (16_001, 1), (200_000, 4), (1_024_001, 7)] {
        wide.record(elapsed);
        assert_eq!(wide.bins[bin].load(Ordering::Relaxed), 1, "{elapsed} ns");
    }
}

/// A whole pipeline on the real clock: one is given the callbacks' end stamps
/// a period apart, as the mixer gives them, so its helper comes to start from
/// its own timer; the other never is, so its helper parks and is woken every
/// frame as before. Every source sample they return is the same bit for bit,
/// and both process every frame.
#[test]
fn a_helper_that_starts_by_itself_returns_the_same_audio() {
    let plan = source_plan(1, true, PathMode::Processed);
    let callback = |pipeline: &Pipeline, buffers: &Buffers, index: usize, stamped: bool| {
        let value = ((index * 37 % 101) as f32 - 50.0) / 64.0;
        unsafe {
            (*buffers.input[0].get()).fill(value);
            pipeline.begin(FRAMES, test_buffer, 0);
        }
        let raw = buffers.input(0);
        let got = pipeline
            .buffer(raw as usize, FRAMES, raw.cast())
            .cast::<f32>();
        let seen: [f32; FRAMES] = unsafe { *got.cast() };
        unsafe { pipeline.end(FRAMES, true) };
        if stamped {
            pipeline.ended(crate::timing::monotonic());
        }
        seen
    };
    let frames = 120;
    let mut self_started = 0;
    // The host's scheduler decides how many frames find the timer; a few
    // tries make it likely that some do. The samples must match regardless.
    for _ in 0..5 {
        let (stamped_buffers, plain_buffers) = (Buffers::new(), Buffers::new());
        let (stamped, _stamped_controls) = pipeline_owned(&plan, &stamped_buffers);
        let (plain, _plain_controls) = pipeline_owned(&plan, &plain_buffers);
        let first = crate::timing::monotonic() + PERIOD_NS;
        for index in 0..frames {
            // A callback a period: spin to its start, as jackd would wake it.
            let begin = first + index as u64 * PERIOD_NS;
            while crate::timing::monotonic() < begin {
                std::hint::spin_loop();
            }
            let a = callback(&stamped, &stamped_buffers, index, true);
            let b = callback(&plain, &plain_buffers, index, false);
            // What each returned was processed by its helper a frame before.
            stamped.settle();
            plain.settle();
            assert_eq!(
                a.map(f32::to_bits),
                b.map(f32::to_bits),
                "frame {index}: the source as the mixer reads it"
            );
        }
        let ran = |pipeline: &Pipeline| pipeline.helper.shared.ran.lock().unwrap().clone();
        let (stamped_ran, plain_ran) = (ran(&stamped), ran(&plain));
        let order: Vec<u64> = (1..=frames as u64).collect();
        assert_eq!(stamped_ran.iter().map(|f| f.0).collect::<Vec<_>>(), order);
        assert_eq!(plain_ran.iter().map(|f| f.0).collect::<Vec<_>>(), order);
        // Never stamped: never on a timer.
        assert!(plain_ran.iter().all(|f| !matches!(f.1, Start::Slept(_))));
        let (a, b) = (stamped.counters(), plain.counters());
        assert_eq!(
            (a.input_blocks, a.faults, a.sources),
            (b.input_blocks, b.faults, b.sources)
        );
        assert_eq!(a.input_blocks, frames as u64);
        self_started += stamped_ran
            .iter()
            .filter(|f| matches!(f.1, Start::Slept(at) if f.2 >= at))
            .count();
        if self_started > 0 {
            break;
        }
    }
    eprintln!("frames the stamped helper started from its own timer: {self_started}");
}

// ---------------------------------------------------------------------------
// `await_frame` step by step, on the test's own thread: a script says what
// happens during each sleep the helper takes, and a sleep the script does not
// have fails the test, so a helper that spins cannot pass.
// ---------------------------------------------------------------------------

/// A sleep the helper took, with what it had published in `wake_at` by then.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Nap {
    /// Towards `until`.
    Timed {
        published: u64,
        until: u64,
    },
    Park {
        published: u64,
    },
}
/// What happens during a sleep, before it ends.
#[derive(Clone, Copy, Debug)]
enum Then {
    /// Nothing: the sleep ends for no reason, or on an older wake's token.
    Nothing,
    /// The callback hands a frame off.
    Queue,
    /// The pipeline is dropped.
    Stop,
    /// The clock reads this from now on.
    Clock(u64),
}
struct Script {
    shared: Shared,
    clock: AtomicU64,
    naps: std::sync::Mutex<Vec<Nap>>,
    then: std::sync::Mutex<std::collections::VecDeque<Then>>,
}
impl Script {
    fn happen(&self, event: Then) {
        match event {
            Then::Nothing => {}
            Then::Queue => self.shared.state.store(QUEUED, Ordering::SeqCst),
            Then::Stop => self.shared.stop.store(true, Ordering::SeqCst),
            Then::Clock(at) => self.clock.store(at, Ordering::SeqCst),
        }
    }
    fn nap(&self, nap: Nap) {
        self.naps.lock().unwrap().push(nap);
        let event = self.then.lock().unwrap().pop_front();
        self.happen(event.unwrap_or_else(|| panic!("a sleep too many, the helper spins: {nap:?}")));
    }
}
impl Pace for Script {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }
    fn sleep_until(&self, until: u64) {
        let published = self.shared.wake_at.load(Ordering::SeqCst);
        self.nap(Nap::Timed { published, until });
    }
    fn park(&self) {
        let published = self.shared.wake_at.load(Ordering::SeqCst);
        self.nap(Nap::Park { published });
    }
}
/// `await_frame` with this forecast at time T0, after `before` and with
/// `then` during its sleeps: how it ended and the sleeps it took.
fn play(wake_at: Option<u64>, before: &[Then], then: &[Then]) -> (Option<Start>, Vec<Nap>) {
    let script = Script {
        shared: Shared::new(),
        clock: AtomicU64::new(T0),
        naps: Default::default(),
        then: std::sync::Mutex::new(then.iter().copied().collect()),
    };
    for event in before {
        script.happen(*event);
    }
    let start = await_frame(&script.shared, &script, wake_at);
    assert!(
        script.then.lock().unwrap().is_empty(),
        "{wake_at:?} {then:?}: ended before the script did"
    );
    (start, script.naps.into_inner().unwrap())
}

#[test]
fn every_way_through_the_helpers_wait_takes_only_the_sleeps_it_must() {
    const T: u64 = T0 + WAKE_MARGIN_NS;
    let timed = Nap::Timed {
        published: T,
        until: T,
    };
    let park = Nap::Park { published: PARKED };
    // A frame that is already there is started without any sleep.
    for wake_at in [None, Some(T)] {
        assert_eq!(
            play(wake_at, &[Then::Queue], &[]),
            (Some(Start::Queued), vec![])
        );
    }
    // No forecast: one park, with PARKED published before it.
    assert_eq!(
        play(None, &[], &[Then::Queue]),
        (Some(Start::Parked { expired: false }), vec![park])
    );
    // A forecast: one timed sleep, with the timer published before it.
    assert_eq!(
        play(Some(T), &[], &[Then::Queue]),
        (Some(Start::Slept(T)), vec![timed])
    );
    // The timer finds nothing: one park after it, and PARKED published.
    assert_eq!(
        play(Some(T), &[], &[Then::Clock(T), Then::Queue]),
        (Some(Start::Parked { expired: true }), vec![timed, park])
    );
    // Sleeps that end for nothing: the same sleep again each time, published
    // again each time, and nothing started.
    let nothing = Then::Nothing;
    assert_eq!(
        play(
            Some(T),
            &[],
            &[
                nothing,
                nothing,
                Then::Clock(T - 1),
                Then::Clock(T),
                nothing,
                nothing,
                Then::Queue
            ]
        ),
        (
            Some(Start::Parked { expired: true }),
            vec![timed, timed, timed, timed, park, park, park]
        )
    );
    // The stop: out of either sleep, and before any; a frame that is queued
    // by then is not started.
    assert_eq!(play(Some(T), &[], &[Then::Stop]), (None, vec![timed]));
    assert_eq!(play(None, &[], &[Then::Stop]), (None, vec![park]));
    assert_eq!(
        play(Some(T), &[], &[Then::Clock(T), Then::Stop]),
        (None, vec![timed, park])
    );
    for wake_at in [None, Some(T)] {
        assert_eq!(play(wake_at, &[Then::Stop], &[]), (None, vec![]));
        assert_eq!(
            play(wake_at, &[Then::Queue, Then::Stop], &[]),
            (None, vec![])
        );
    }
}

/// A forecast the clock does not bear out is never slept on: one that is due
/// or past, one more than two periods ahead (the clock was set back, or the
/// stamps were not this clock's), and the two values `wake_at` keeps for
/// AWAKE and PARKED. The helper parks, PARKED is what the callback reads, and
/// the callback wakes it: as with no forecast at all.
#[test]
fn a_forecast_that_makes_no_sense_is_not_slept_on() {
    let park = Nap::Park { published: PARKED };
    for wake_at in [
        AWAKE,
        1,
        T0 - PERIOD_NS,
        T0 - 1,
        T0,
        T0 + 2 * PERIOD_NS + 1,
        T0 + 1_000 * PERIOD_NS,
        PARKED - 1,
        PARKED,
    ] {
        assert_eq!(
            play(Some(wake_at), &[], &[Then::Queue]),
            (Some(Start::Parked { expired: false }), vec![park]),
            "forecast {wake_at}"
        );
        // And such a value, read by a callback in the moment before PARKED
        // replaces it, is one it wakes for or one that is already due: never
        // a timer it would wait on.
        let begun = T0 - LENGTH;
        assert!(must_wake(wake_at, begun) || wake_at <= T0, "{wake_at}");
    }
    // Within two periods it is.
    for wake_at in [T0 + 1, T0 + 2 * PERIOD_NS] {
        assert_eq!(
            play(Some(wake_at), &[], &[Then::Queue]),
            (
                Some(Start::Slept(wake_at)),
                vec![Nap::Timed {
                    published: wake_at,
                    until: wake_at
                }]
            )
        );
    }
    // The clock set back, or far forward, during the timed sleep: parked.
    const T: u64 = T0 + WAKE_MARGIN_NS;
    for clock in [T0 - 10 * PERIOD_NS, T0 + 1_000_000 * PERIOD_NS] {
        let (start, naps) = play(Some(T), &[], &[Then::Clock(clock), Then::Queue]);
        assert_eq!(start, Some(Start::Parked { expired: true }), "{clock}");
        assert_eq!(naps.len(), 2);
        assert_eq!(naps[1], park);
    }
    // The forecast itself never gives one of the two kept values, nor a timer
    // the callback of the next period would not leave the helper to.
    let mut forecast = Forecast::default();
    for k in 0..2_000u64 {
        let begun = T0 + k * PERIOD_NS + (k * 7_919) % 150_000;
        let ended = begun + LENGTH + (k * 104_729) % 180_000;
        if let Some(wake_at) = forecast.observe(begun, ended) {
            assert!(wake_at != AWAKE && wake_at != PARKED);
            assert!(wake_at > ended && wake_at <= begun + PERIOD_NS + LATEST_SELF_START_NS);
        }
    }
}

// ---------------------------------------------------------------------------
// The mixer's own pace (`Kernel`): the real clock and the real futex wait.
// The host is no real-time system, so nothing here is held to a time of its
// own: only to the order of things, and to what happens in some round of many.
// ---------------------------------------------------------------------------

/// `Kernel`, with its sleeps counted.
#[derive(Default)]
struct CountedKernel {
    timed: AtomicU64,
    parks: AtomicU64,
}
impl Pace for CountedKernel {
    fn now(&self) -> u64 {
        Kernel.now()
    }
    fn sleep_until(&self, wake_at: u64) {
        self.timed.fetch_add(1, Ordering::SeqCst);
        Kernel.sleep_until(wake_at);
    }
    fn park(&self) {
        self.parks.fetch_add(1, Ordering::SeqCst);
        Kernel.park();
    }
}
/// One wait of a helper thread on the mixer's pace, towards a timer `ahead`
/// of when it starts. `during` gets the timer and the thread and plays the
/// callback. Returns the timer, how the wait ended and when.
fn kernel_wait(
    shared: &Arc<Shared>,
    pace: &Arc<CountedKernel>,
    ahead: u64,
    during: impl FnOnce(u64, &thread::Thread),
) -> (u64, Option<Start>, u64) {
    shared.state.store(IDLE, Ordering::SeqCst);
    shared.wake_at.store(AWAKE, Ordering::SeqCst);
    let (send, receive) = std::sync::mpsc::channel();
    let (worker, kernel) = (Arc::clone(shared), Arc::clone(pace));
    let helper = thread::spawn(move || {
        let wake_at = crate::timing::monotonic() + ahead;
        send.send((wake_at, None, 0)).unwrap();
        let start = await_frame(&worker, &*kernel, Some(wake_at));
        send.send((wake_at, start, crate::timing::monotonic()))
            .unwrap();
    });
    let (wake_at, _, _) = receive.recv_timeout(PATIENCE).unwrap();
    during(wake_at, helper.thread());
    let ended = receive
        .recv_timeout(PATIENCE)
        .expect("the helper never came out of its wait");
    helper.join().unwrap();
    ended
}

/// With nothing handed off the helper sleeps to its timer in one futex wait
/// (a few if the kernel ends one early), then parks: it does not spin towards
/// the timer, and does not give up on it before its time.
#[test]
fn on_the_mixers_pace_an_idle_helper_sleeps_to_its_timer_then_parks() {
    let shared = Arc::new(Shared::new());
    let pace = Arc::new(CountedKernel::default());
    let rounds = 6;
    let mut slept_rounds = 0;
    for _ in 0..rounds {
        let before = pace.timed.load(Ordering::SeqCst);
        let (wake_at, start, _) = kernel_wait(&shared, &pace, 3_000_000, |wake_at, helper| {
            until("the helper never parked after its timer", || {
                shared.wake_at.load(Ordering::SeqCst) == PARKED
            });
            // It published PARKED only once the timer's time had come.
            assert!(crate::timing::monotonic() >= wake_at);
            shared.state.store(QUEUED, Ordering::SeqCst);
            helper.unpark();
        });
        let timed = pace.timed.load(Ordering::SeqCst) - before;
        // No timed sleep at all only if the host held the thread up for the
        // whole three milliseconds before its first look.
        assert!(timed <= 3, "{timed} timed sleeps towards one timer");
        assert_eq!(
            start,
            Some(Start::Parked { expired: timed > 0 }),
            "timer {wake_at}"
        );
        slept_rounds += u32::from(timed > 0);
    }
    assert!(slept_rounds > 0, "no round reached the timed sleep");
    // One park a round, and one more for each wake the park did not need.
    assert!(pace.parks.load(Ordering::SeqCst) <= 3 * rounds);
}

/// A frame handed off with no wake is started by the helper's own timer, at
/// its time and not before; a wake ends the timed sleep before its time.
#[test]
fn on_the_mixers_pace_the_timer_starts_a_frame_and_a_wake_ends_the_sleep() {
    let shared = Arc::new(Shared::new());
    let pace = Arc::new(CountedKernel::default());
    let published = |wake_at: u64| {
        until("the helper never published its timer", || {
            let seen = shared.wake_at.load(Ordering::SeqCst);
            seen == wake_at || seen == PARKED
        });
    };
    // Left to the timer: the handoff as `Helper::queue` makes it, by a
    // callback that started early enough to leave the frame to this timer.
    // It wakes the helper only if the host held a thread up until the timer
    // had found nothing and the helper had parked.
    let (mut on_timer, mut early) = (0, 0);
    for _ in 0..8 {
        let woke = std::cell::Cell::new(false);
        let (wake_at, start, ended) = kernel_wait(&shared, &pace, 3_000_000, |wake_at, helper| {
            published(wake_at);
            // Let it get into the sleep.
            thread::sleep(Duration::from_micros(300));
            let begun = wake_at - LATEST_SELF_START_NS;
            shared.state.store(QUEUED, Ordering::SeqCst);
            if must_wake(shared.wake_at.load(Ordering::SeqCst), begun) {
                helper.unpark();
                woke.set(true);
            }
        });
        match start {
            Some(Start::Slept(timer)) if !woke.get() => {
                assert_eq!(timer, wake_at);
                on_timer += u32::from(ended >= wake_at);
                early += u32::from(ended < wake_at);
            }
            // The host held one of the two threads up past the timer.
            Some(_) => {}
            None => panic!("stopped"),
        }
    }
    // A futex wait may end early without a wake, but not as a rule.
    assert!(
        on_timer >= 1 && early <= 2,
        "{on_timer} on time, {early} early"
    );
    // Woken out of the timed sleep.
    let mut woken_early = 0;
    for _ in 0..12 {
        let (wake_at, start, ended) = kernel_wait(&shared, &pace, 4_000_000, |wake_at, helper| {
            published(wake_at);
            thread::sleep(Duration::from_micros(300));
            shared.state.store(QUEUED, Ordering::SeqCst);
            helper.unpark();
        });
        assert!(start.is_some());
        woken_early += u32::from(start == Some(Start::Slept(wake_at)) && ended < wake_at);
    }
    assert!(
        woken_early >= 1,
        "no wake ended a timed sleep before its time"
    );
}

/// The helper's timed sleeps are not rounded by the 50 us of timer slack a
/// thread has by default, which this kernel applies to a real-time thread's
/// futex wait too: `Kernel::prepare` sets it to the least there is.
#[test]
fn the_mixers_pace_takes_the_timer_slack_off_the_helper_thread() {
    let slack = thread::spawn(|| {
        Kernel.prepare();
        unsafe { libc::prctl(libc::PR_GET_TIMERSLACK) }
    })
    .join()
    .unwrap();
    assert_eq!(slack, 1);
}

// ---------------------------------------------------------------------------
// The real-time rule, on its positive side.
// ---------------------------------------------------------------------------

/// Where the timed sleep needs a real-time thread, the helper sets its first
/// timer in the very frame in which it takes a real-time placement, and not
/// before, however long the callbacks have been regular. The host cannot
/// make a thread real-time, so the pace here calls the placement the helper
/// takes real-time; the rule the mixer applies to it is checked by itself.
#[test]
fn a_helper_sets_its_timer_once_its_placement_is_real_time() {
    struct Placed(Stepped);
    impl Pace for Placed {
        fn now(&self) -> u64 {
            self.0.now()
        }
        fn sleep_until(&self, wake_at: u64) {
            self.0.sleep_until(wake_at);
        }
        fn park(&self) {
            self.0.park();
        }
        fn needs_realtime(&self) -> bool {
            true
        }
        fn realtime(&self, _: i32) -> bool {
            true
        }
    }
    let clock = Clock::at(T0);
    let mut rig = Rig::with(
        Placed(Stepped {
            clock: clock.clone(),
        }),
        clock,
    );
    // No placement yet: parked and woken, long past the warm-up.
    let woken = (Woke::Parked, Start::Parked { expired: false });
    for (k, frame) in rig.regular(T0, 40, |_| 0).into_iter().enumerate() {
        assert_eq!((frame.0, frame.1), woken, "frame {k}");
    }
    assert_eq!(rig.asleep(), PARKED);
    // The callback samples its placement; the helper takes it when it starts
    // the next frame, which it is still woken for.
    let mut sampler = Sampler::default();
    sampler.mark(0);
    rig.helper.place(0, &mut sampler);
    assert_eq!(rig.shared().generation.load(Ordering::SeqCst), 1);
    let first = T0 + 40 * PERIOD_NS;
    let frame = rig.callback(first, first + LENGTH);
    assert_eq!((frame.0, frame.1), woken);
    assert_eq!(rig.shared().applied.load(Ordering::SeqCst), 1);
    // From then on its own timer.
    assert_eq!(rig.asleep(), first + LENGTH + PERIOD_NS + WAKE_MARGIN_NS);
    for (k, frame) in rig
        .regular(first + PERIOD_NS, 20, |_| 0)
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            (frame.0, frame.1),
            (Woke::No, Start::Slept(frame.2)),
            "frame {k}"
        );
    }
    rig.assert_ran_once_in_order();
    rig.stop();

    // The mixer's rule: the two real-time policies, and nothing else, least
    // of all a policy that could not be read.
    for (policy, realtime) in [
        (libc::SCHED_FIFO, true),
        (libc::SCHED_RR, true),
        (libc::SCHED_OTHER, false),
        (libc::SCHED_BATCH, false),
        (libc::SCHED_IDLE, false),
        (-1, false),
    ] {
        assert_eq!(Kernel.realtime(policy), realtime, "policy {policy}");
    }
}

// ---------------------------------------------------------------------------
// The pipeline's end.
// ---------------------------------------------------------------------------

/// A pipeline dropped with a frame still in the helper's hands, or just
/// handed off: the helper has stopped before the frame's buffers are freed
/// (`Handoff`'s destructor checks that here, for every pipeline every test
/// drops), and the frame either ran whole or was never started.
#[test]
fn a_pipeline_dropped_with_a_frame_in_flight_stops_its_helper_first() {
    let plan = source_plan(1, true, PathMode::Processed);
    let mut unstarted = 0;
    for round in 0..60 {
        let buffers = Buffers::new();
        let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
        let shared = Arc::clone(&pipeline.helper.shared);
        let settled = round % 4;
        for _ in 0..settled {
            source_block(&pipeline, &buffers, &[0], true);
        }
        source_block_unsettled(&pipeline, &buffers, &[0], true);
        drop(pipeline);
        assert!(shared.gone.load(Ordering::SeqCst), "round {round}");
        let ran = shared.ran.lock().unwrap().len();
        let queued = shared.state.load(Ordering::SeqCst) == QUEUED;
        assert!(
            (ran == settled + 1 && !queued) || (ran == settled && queued),
            "round {round}: {ran} of {} frames ran, queued: {queued}",
            settled + 1
        );
        unstarted += u32::from(queued);
    }
    eprintln!("frames in flight that were never started: {unstarted} of 60");
}

// ---------------------------------------------------------------------------
// What no host test can see.
// ---------------------------------------------------------------------------

/// The orderings of the handshake, read from the source. On the host a
/// weaker load is the same instruction as a SeqCst one, so no test here can
/// fail for it; on the mixer's processor it is not, and the helper could then
/// read `state` before its own store of `wake_at` is visible: a lost wake.
/// This fails when someone touches them, so that they read `await_frame`.
#[test]
fn the_handshakes_four_accesses_are_sequentially_consistent_in_the_source() {
    let source = include_str!("../../pipeline.rs");
    let between = |from: &str, to: &str| {
        let start = source.find(from).unwrap_or_else(|| panic!("{from}"));
        let length = source[start..].find(to).unwrap_or_else(|| panic!("{to}"));
        &source[start..start + length]
    };
    let places = |text: &str, what: &str| -> Vec<usize> {
        text.match_indices(what).map(|found| found.0).collect()
    };
    // The helper: before each of its two sleeps, the store, then the look.
    let helper = between("\nfn await_frame<", "\nfn helper_loop<");
    let stores = places(helper, "shared.wake_at.store(");
    let looks = places(helper, "shared.state.load(");
    let sleeps = [
        places(helper, "pace.sleep_until("),
        places(helper, "pace.park("),
    ]
    .concat();
    assert_eq!((stores.len(), looks.len(), sleeps.len()), (2, 2, 2));
    for sleep in 0..2 {
        assert!(stores[sleep] < looks[sleep] && looks[sleep] < sleeps[sleep]);
        for access in [stores[sleep], looks[sleep]] {
            let line = helper[access..].lines().next().unwrap();
            assert!(line.contains("Ordering::SeqCst)"), "{line}");
        }
    }
    assert!(stores[1] > sleeps[0]);
    // The callback: the store, then the read, then the wake.
    let callback = between("\n    fn queue(", "\nimpl Helper {");
    let store = places(callback, "shared.state.store(QUEUED, Ordering::SeqCst);");
    let read = places(callback, "shared.wake_at.load(Ordering::SeqCst);");
    let wake = places(callback, ".unpark()");
    assert_eq!((store.len(), read.len(), wake.len()), (1, 1, 1));
    assert!(store[0] < read[0] && read[0] < wake[0]);
    assert_eq!(places(callback, "shared.state.").len(), 1);
    assert_eq!(places(callback, "shared.wake_at.").len(), 1);
}

// ---------------------------------------------------------------------------
// The bench gauges, off.
// ---------------------------------------------------------------------------

/// Without the bench gauges nothing writes a helper wake gauge: not the
/// callback and not the helper, whichever way its frames start. The gauges
/// are the process's, and other tests turn them on, so this runs once more
/// in a process of its own, where every one of them must still read zero.
#[test]
fn without_the_gauges_no_wake_gauge_is_written() {
    const CHILD: &str = "RC2FX_TEST_WAKE_GAUGES_OFF";
    if std::env::var_os(CHILD).is_none() {
        let name = concat!(
            module_path!(),
            "::without_the_gauges_no_wake_gauge_is_written"
        );
        let name = name.split_once("::").unwrap().1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1", "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && text.contains("1 passed"),
            "{text}"
        );
        return;
    }
    assert!(!crate::gauges::enabled());
    // The helper alone, every way into a frame: woken out of a park with no
    // forecast, on its timer, woken out of the park after a timer, and out
    // of the timed sleep.
    let mut rig = Rig::new(T0);
    let mut frames = rig.regular(T0, 40, |k| jitter(k, 30_000));
    let first = T0 + 40 * PERIOD_NS;
    frames.push(rig.callback(first, first + LENGTH + WAKE_MARGIN_NS + 30_000));
    frames.extend(rig.regular(first + PERIOD_NS, 4, |_| 0));
    let begun = first + 5 * PERIOD_NS - PERIOD_NS / 2;
    frames.push(rig.callback(begun, begun + LENGTH));
    for wanted in [
        (Woke::Parked, Start::Parked { expired: false }),
        (Woke::Parked, Start::Parked { expired: true }),
    ] {
        assert!(
            frames.iter().any(|frame| (frame.0, frame.1) == wanted),
            "{wanted:?}"
        );
    }
    let last = frames.last().unwrap();
    assert!(last.0 == Woke::Late && matches!(last.1, Start::Slept(timer) if timer > last.2));
    assert!(frames.iter().filter(|frame| frame.0 == Woke::No).count() >= 20);
    rig.assert_ran_once_in_order();
    rig.stop();
    // A whole pipeline, stamped as the mixer stamps it.
    let plan = source_plan(1, true, PathMode::Processed);
    let buffers = Buffers::new();
    let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
    pipeline.set_gauged(false);
    for _ in 0..40 {
        source_block(&pipeline, &buffers, &[0], true);
        pipeline.ended(crate::timing::monotonic());
    }
    assert_eq!(pipeline.helper.shared.queued_at.load(Ordering::Relaxed), 0);
    drop(pipeline);

    let report = wake_gauges();
    for key in [
        "wakeCall",
        "wakeSent",
        "wakeLatency",
        "wokenLatency",
        "selfLatency",
        "timerLate",
        "handoffLate",
        "handoffEarly",
        "helperSlack",
        "end",
        "place",
    ] {
        let gauge = &report[key];
        assert_eq!(gauge["calls"], 0, "{key}");
        assert_eq!(gauge["totalNs"], 0, "{key}");
        assert_eq!(gauge["maxNs"], 0, "{key}");
        assert!(
            gauge["histogram"]
                .as_array()
                .unwrap()
                .iter()
                .all(|bin| bin == 0),
            "{key}"
        );
    }
    for group in ["starts", "wakes"] {
        for (kind, count) in report[group].as_object().unwrap() {
            assert_eq!(count, 0, "{group}.{kind}");
        }
    }
}
