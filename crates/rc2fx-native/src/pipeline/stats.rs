//! Counters and bench gauges, read by the native report.
use super::super::Line;
use super::helper::{LATEST_SELF_START_NS, WAKE_MARGIN_NS};
use rc2fx_native_control::MAX_PATHS;
use std::sync::atomic::{AtomicU64, Ordering};

/// Callback time spent per bound path (plan order) and resolving buffers.
/// Bench gauges only (`Front::gauged`): each charge costs two clock reads, on
/// every visited path of every callback.
pub static PATH_COST_NS: Line<[AtomicU64; MAX_PATHS]> =
    Line([const { AtomicU64::new(0) }; MAX_PATHS]);
pub static RESOLVE_NS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub(super) fn charge(counter: &AtomicU64, since: u64) {
    counter.fetch_add(
        super::super::timing::monotonic().saturating_sub(since),
        Ordering::Relaxed,
    );
}
#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub input_blocks: u64,
    pub output_blocks: u64,
    pub faults: u64,
    pub skipped_outputs: u64,
}
impl Counters {
    #[cfg(test)]
    pub(super) fn add(self, other: Self) -> Self {
        Self {
            input_blocks: self.input_blocks + other.input_blocks,
            output_blocks: self.output_blocks + other.output_blocks,
            faults: self.faults + other.faults,
            skipped_outputs: self.skipped_outputs + other.skipped_outputs,
        }
    }
}
/// Live totals across pipelines, for the native report: per frame, for each
/// source meant to be processed, what the mixer read. Every raw outcome is a
/// fallback to unprocessed audio, whatever its cause. The callback thread
/// writes them, one per processed source each cycle, so they share a line of
/// their own.
pub(super) struct SourceTotals {
    /// Every request got processed audio and the frame was acknowledged.
    processed: AtomicU64,
    /// Every request got processed audio, but the frame did not complete:
    /// the mixer's callback failed, a channel went unrequested, or the path
    /// was unusable at the end. Not a raw fallback, and not counted processed.
    processed_unacknowledged: AtomicU64,
    /// Helper still busy, or it had not finished the previous frame.
    raw_helper_unready: AtomicU64,
    /// No frame was handed to the helper last time: the first frame, the
    /// frame after a stall, inactive frame or switch to processing.
    raw_no_handoff: AtomicU64,
    /// The helper was handed the previous frame but produced no result for
    /// it (a mode change or DSP error under that frame).
    raw_not_done: AtomicU64,
    /// The path failed or was unusable this frame (settings fence, frame
    /// size, dead port, buffer mismatch).
    raw_invalid: AtomicU64,
    /// Meant to be processed, but the mixer never requested the buffer.
    unobserved: AtomicU64,
    /// Individual buffer requests answered with raw input, for any reason.
    /// A frame counts once above, under its first raw cause, even when an
    /// earlier request in it got processed audio.
    pub(super) raw_returns: AtomicU64,
}
pub(super) static SOURCES: Line<SourceTotals> = Line(SourceTotals {
    processed: AtomicU64::new(0),
    processed_unacknowledged: AtomicU64::new(0),
    raw_helper_unready: AtomicU64::new(0),
    raw_no_handoff: AtomicU64::new(0),
    raw_not_done: AtomicU64::new(0),
    raw_invalid: AtomicU64::new(0),
    unobserved: AtomicU64::new(0),
    raw_returns: AtomicU64::new(0),
});
const _: () = assert!(std::mem::size_of::<Line<SourceTotals>>() == 64);
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Processed,
    ProcessedUnacknowledged,
    HelperUnready,
    NoHandoff,
    NotDone,
    Invalid,
    Unobserved,
}
pub(super) fn tally(outcome: Outcome) {
    let total = match outcome {
        Outcome::Processed => &SOURCES.processed,
        Outcome::ProcessedUnacknowledged => &SOURCES.processed_unacknowledged,
        Outcome::HelperUnready => &SOURCES.raw_helper_unready,
        Outcome::NoHandoff => &SOURCES.raw_no_handoff,
        Outcome::NotDone => &SOURCES.raw_not_done,
        Outcome::Invalid => &SOURCES.raw_invalid,
        Outcome::Unobserved => &SOURCES.unobserved,
    };
    total.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    here::tally(Some(outcome));
}
pub fn sources() -> serde_json::Value {
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    serde_json::json!({
        "processed": read(&SOURCES.processed),
        "processedUnacknowledged": read(&SOURCES.processed_unacknowledged),
        "rawHelperUnready": read(&SOURCES.raw_helper_unready),
        "rawNoHandoff": read(&SOURCES.raw_no_handoff),
        "rawNotDone": read(&SOURCES.raw_not_done),
        "rawInvalid": read(&SOURCES.raw_invalid),
        "unobserved": read(&SOURCES.unobserved),
        "rawReturns": read(&SOURCES.raw_returns),
    })
}
/// Helper timing, for the native report. The helper thread writes all three
/// every frame, so they share a line of their own.
pub struct HelperTime {
    pub frames: AtomicU64,
    pub total_ns: AtomicU64,
    pub max_ns: AtomicU64,
}
pub static HELPER_TIME: Line<HelperTime> = Line(HelperTime {
    frames: AtomicU64::new(0),
    total_ns: AtomicU64::new(0),
    max_ns: AtomicU64::new(0),
});
/// Bench gauge: a histogram of short times, in microseconds
/// [<=1, <=2, <=4, <=8, <=16, <=32, <=64, >64], with the count, total and
/// longest. Each instance has one writer thread and its own lines. A `wide`
/// one has bounds sixteen times larger, for times of the order of the
/// helper's margins.
pub struct Fine {
    bounds_us: [u64; 7],
    pub(super) calls: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    pub(super) bins: [AtomicU64; 8],
}
impl Fine {
    const BOUNDS_US: [u64; 7] = [1, 2, 4, 8, 16, 32, 64];
    pub(super) const WIDE_BOUNDS_US: [u64; 7] = [16, 32, 64, 128, 256, 512, 1024];
    pub(super) const fn new() -> Self {
        Self::with(Self::BOUNDS_US)
    }
    pub(super) const fn wide() -> Self {
        Self::with(Self::WIDE_BOUNDS_US)
    }
    const fn with(bounds_us: [u64; 7]) -> Self {
        Self {
            bounds_us,
            calls: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            bins: [const { AtomicU64::new(0) }; 8],
        }
    }
    pub(super) fn record(&self, elapsed_ns: u64) {
        let bin = self
            .bounds_us
            .iter()
            .position(|bound| elapsed_ns <= bound * 1_000)
            .unwrap_or(7);
        self.bins[bin].fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        self.max_ns.fetch_max(elapsed_ns, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
    pub(super) fn snapshot(&self) -> serde_json::Value {
        let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
        serde_json::json!({
            "calls": read(&self.calls),
            "totalNs": read(&self.total_ns),
            "maxNs": read(&self.max_ns),
            "histogram": self.bins.iter().map(read).collect::<Vec<_>>(),
        })
    }
}
/// Bench gauges around the helper's start. Recorded only while the gauges
/// are on; without them no clock is read for these and they stay zero.
///
/// Written by the callback. `WAKE_CALL`: the time every `Helper::queue` takes
/// on the callback thread, which delays the callback's return by that much;
/// `WAKE_SENT`: the same for the calls that woke the helper, a futex system
/// call, with `WAKES` counting them by reason: [the helper was parked, it
/// slept towards a wake-up later than LATEST_SELF_START_NS]. Every other call
/// left the helper to start by itself. `END`: `Pipeline::end` from its entry to
/// just before that call, so that the rest of the callback's tail is not
/// mistaken for the wake; `PLACE` is the part of it spent sampling the
/// thread's placement. `HELPER_SLACK`: the time from the helper finishing a
/// frame to the start of the callback that consumes it.
///
/// Written by the helper. `WAKE_LATENCY`: from just before `Helper::queue` to
/// the helper starting its frame, for every frame; `WOKEN_LATENCY` for the
/// frames the callback woke it for and `SELF_LATENCY` for those it started by
/// itself. `STARTS` counts frames by how the helper came by them (`Start`,
/// `STARTS_*`). `TIMER_LATE`: for the frames its timer started, from the
/// timer's time to the helper starting the frame: what the kernel's timer and
/// the helper's way back to a core take, which no host test can tell.
/// `HANDOFF_LATE` and `HANDOFF_EARLY`: by how much a callback ended after or
/// before the forecast, which the margin has to cover.
pub(super) static WAKE_CALL: Line<Fine> = Line(Fine::new());
pub(super) static WAKE_SENT: Line<Fine> = Line(Fine::new());
pub(super) static WAKES: Line<[AtomicU64; 2]> = Line([const { AtomicU64::new(0) }; 2]);
pub(super) static END: Line<Fine> = Line(Fine::new());
pub(super) static PLACE: Line<Fine> = Line(Fine::new());
pub(super) static HELPER_SLACK: Line<Fine> = Line(Fine::wide());
pub(super) static WAKE_LATENCY: Line<Fine> = Line(Fine::new());
pub(super) static WOKEN_LATENCY: Line<Fine> = Line(Fine::new());
pub(super) static SELF_LATENCY: Line<Fine> = Line(Fine::wide());
pub(super) static TIMER_LATE: Line<Fine> = Line(Fine::new());
pub(super) static HANDOFF_LATE: Line<Fine> = Line(Fine::wide());
pub(super) static HANDOFF_EARLY: Line<Fine> = Line(Fine::wide());
pub(super) static STARTS: Line<[AtomicU64; 5]> = Line([const { AtomicU64::new(0) }; 5]);
/// `STARTS` indices. Started by itself: the frame was already queued when the
/// helper first looked, or its timer ended the sleep. Started by the
/// callback's wake: out of the timed sleep, out of the park that followed a
/// timer with nothing queued, or out of a park without a forecast.
pub(super) const STARTS_QUEUED: usize = 0;
pub(super) const STARTS_TIMER: usize = 1;
pub(super) const STARTS_WOKEN_EARLY: usize = 2;
pub(super) const STARTS_PARKED_AFTER_TIMER: usize = 3;
pub(super) const STARTS_PARKED_NO_FORECAST: usize = 4;
/// The helper wake gauges, for the report's `gauges` section.
pub fn wake_gauges() -> serde_json::Value {
    let read = |value: &AtomicU64| value.load(Ordering::Relaxed);
    serde_json::json!({
        "boundsUs": Fine::BOUNDS_US,
        "wideBoundsUs": Fine::WIDE_BOUNDS_US,
        "meaning": "wakeCall: time inside every helper handoff (Helper::queue) on the callback thread; wakeSent: the same for the handoffs that woke the helper with a futex call, counted by reason in wakes, every other handoff made no system call; wakeLatency: from just before the handoff to the helper starting its frame, wokenLatency for the frames the callback woke it for, selfLatency (wide) for those it started by itself; timerLate: from the helper's timer to its start of the frame, for the frames the timer started; starts: frames by how the helper came by them; handoffLate, handoffEarly (wide): the callback's end after or before the helper's forecast; helperSlack (wide): from the helper finishing a frame to the start of the callback that consumes it; end: Pipeline::end from entry to just before the handoff, including place and the end part of pathCostNs; place: placement sampling within end",
        "marginNs": WAKE_MARGIN_NS,
        "latestSelfStartNs": LATEST_SELF_START_NS,
        "wakeCall": WAKE_CALL.snapshot(),
        "wakeSent": WAKE_SENT.snapshot(),
        "wakes": {
            "parked": read(&WAKES[0]),
            "pastLatestSelfStart": read(&WAKES[1]),
        },
        "wakeLatency": WAKE_LATENCY.snapshot(),
        "wokenLatency": WOKEN_LATENCY.snapshot(),
        "selfLatency": SELF_LATENCY.snapshot(),
        "timerLate": TIMER_LATE.snapshot(),
        "starts": {
            "alreadyQueued": read(&STARTS[STARTS_QUEUED]),
            "timer": read(&STARTS[STARTS_TIMER]),
            "wokenEarly": read(&STARTS[STARTS_WOKEN_EARLY]),
            "parkedAfterTimer": read(&STARTS[STARTS_PARKED_AFTER_TIMER]),
            "parkedNoForecast": read(&STARTS[STARTS_PARKED_NO_FORECAST]),
        },
        "handoffLate": HANDOFF_LATE.snapshot(),
        "handoffEarly": HANDOFF_EARLY.snapshot(),
        "helperSlack": HELPER_SLACK.snapshot(),
        "end": END.snapshot(),
        "place": PLACE.snapshot(),
    })
}
/// Callbacks that found the helper still busy after the wait: their frame
/// was not handed off, and the previous frame's processed audio not ready.
pub static HELPER_STALLS: AtomicU64 = AtomicU64::new(0);
/// Callbacks that found the helper still busy at all.
pub static HELPER_LATE: AtomicU64 = AtomicU64::new(0);
/// The least time between the helper finishing a frame and the next callback
/// starting, which consumes it. 0: the helper finished after that callback
/// had started. The callback reads it every cycle and writes it only for a new
/// minimum.
pub static HELPER_MIN_MARGIN_NS: Line<AtomicU64> = Line(AtomicU64::new(u64::MAX));

/// Live cumulative processing counters, since process load, across contexts.
///
/// These atomics count the same four events as the per-pipeline `Counters`, at
/// the same increment sites, so an audit can read what the DSP is doing while
/// callbacks run. One relaxed increment per actual block or event; no clock,
/// allocation, lock or I/O on the audio thread. They are never reset, so they
/// include retired pipelines.
///
/// The helper writes `LIVE_INPUT_BLOCKS` and the callback writes
/// `LIVE_OUTPUT_BLOCKS`, once per processed block: each has its own line.
pub static LIVE_INPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_OUTPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_FAULTS: AtomicU64 = AtomicU64::new(0);
pub static LIVE_SKIPPED_OUTPUTS: AtomicU64 = AtomicU64::new(0);

/// The audit's live view (see `LIVE_*`). Read-only, lock-free, no clocks.
pub fn processing_counters() -> serde_json::Value {
    let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    serde_json::json!({
        "scope": "live cumulative since process load, all contexts, read with relaxed atomics; includes retired pipelines",
        "inputBlocks": read(&LIVE_INPUT_BLOCKS),
        "outputBlocks": read(&LIVE_OUTPUT_BLOCKS),
        "dspFaults": read(&LIVE_FAULTS),
        "skippedOutputs": read(&LIVE_SKIPPED_OUTPUTS),
    })
}
/// Tests: what this thread's callbacks added to `SOURCES`. The statics are
/// shared by every test running at once, and a test's callbacks run on its
/// own thread.
#[cfg(test)]
pub(super) mod here {
    use super::Outcome;
    use std::cell::Cell;

    /// As `SourceTotals`, field for field.
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct SourceCounts {
        pub processed: u64,
        pub processed_unacknowledged: u64,
        pub raw_helper_unready: u64,
        pub raw_no_handoff: u64,
        pub raw_not_done: u64,
        pub raw_invalid: u64,
        pub unobserved: u64,
        pub raw_returns: u64,
    }
    impl SourceCounts {
        /// What was added between `before` and `self`.
        pub fn since(self, before: Self) -> Self {
            Self {
                processed: self.processed - before.processed,
                processed_unacknowledged: self.processed_unacknowledged
                    - before.processed_unacknowledged,
                raw_helper_unready: self.raw_helper_unready - before.raw_helper_unready,
                raw_no_handoff: self.raw_no_handoff - before.raw_no_handoff,
                raw_not_done: self.raw_not_done - before.raw_not_done,
                raw_invalid: self.raw_invalid - before.raw_invalid,
                unobserved: self.unobserved - before.unobserved,
                raw_returns: self.raw_returns - before.raw_returns,
            }
        }
        pub fn add(self, other: Self) -> Self {
            Self {
                processed: self.processed + other.processed,
                processed_unacknowledged: self.processed_unacknowledged
                    + other.processed_unacknowledged,
                raw_helper_unready: self.raw_helper_unready + other.raw_helper_unready,
                raw_no_handoff: self.raw_no_handoff + other.raw_no_handoff,
                raw_not_done: self.raw_not_done + other.raw_not_done,
                raw_invalid: self.raw_invalid + other.raw_invalid,
                unobserved: self.unobserved + other.unobserved,
                raw_returns: self.raw_returns + other.raw_returns,
            }
        }
    }
    thread_local! {
        static COUNTS: Cell<SourceCounts> = Cell::new(SourceCounts::default());
    }
    /// One outcome, or with `None` one raw return.
    pub(in super::super) fn tally(outcome: Option<Outcome>) {
        COUNTS.with(|counts| {
            let mut value = counts.get();
            *match outcome {
                Some(Outcome::Processed) => &mut value.processed,
                Some(Outcome::ProcessedUnacknowledged) => &mut value.processed_unacknowledged,
                Some(Outcome::HelperUnready) => &mut value.raw_helper_unready,
                Some(Outcome::NoHandoff) => &mut value.raw_no_handoff,
                Some(Outcome::NotDone) => &mut value.raw_not_done,
                Some(Outcome::Invalid) => &mut value.raw_invalid,
                Some(Outcome::Unobserved) => &mut value.unobserved,
                None => &mut value.raw_returns,
            } += 1;
            counts.set(value);
        });
    }
    /// The totals this thread has added so far.
    pub fn sources() -> SourceCounts {
        COUNTS.with(Cell::get)
    }
}
