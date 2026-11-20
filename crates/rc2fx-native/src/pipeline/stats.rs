//! Pipeline counters and bench-only timing gauges for the native report.
use super::super::Line;
use super::helper::{LATEST_SELF_START_NS, WAKE_MARGIN_NS};
use rc2fx_native_control::MAX_PATHS;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-path callback time (plan order) and buffer-resolution time (bench gauges only).
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
/// Per-frame source outcome totals across all pipelines (callback-written,
/// isolated on its own cache line).
pub(super) struct SourceTotals {
    /// Every request received processed audio and the frame completed cleanly.
    processed: AtomicU64,
    /// Requests got processed audio, but the frame didn't complete (vendor
    /// callback failed, unrequested channel, or path invalidated at end).
    processed_unacknowledged: AtomicU64,
    /// Helper was still busy or hadn't finished the previous frame.
    raw_helper_unready: AtomicU64,
    /// No handoff occurred last cycle (first frame, post-stall, inactive, or mode switch).
    raw_no_handoff: AtomicU64,
    /// Helper received the previous frame but produced no output (mode change or DSP error).
    raw_not_done: AtomicU64,
    /// Path was unusable this frame (settings fence, wrong frame count, dead port, buffer mismatch).
    raw_invalid: AtomicU64,
    /// Enabled source whose buffer was never requested by the mixer.
    unobserved: AtomicU64,
    /// Individual buffer lookups answered with raw input for any reason.
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
/// Helper execution time stats (written only by the helper thread, own cache line).
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
/// Microsecond histogram for bench gauges (`new`: 1..64 us, `wide`: 16..1024 us).
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
/// Bench-only helper wake/handoff gauges (stay zero when gauges are off).
///
/// Callback-written: `WAKE_CALL`, `WAKE_SENT`, `WAKES` (`[parked, past_latest]`),
/// `END`, `PLACE`, `HELPER_SLACK`.
/// Helper-written: `WAKE_LATENCY`, `WOKEN_LATENCY`, `SELF_LATENCY`, `TIMER_LATE`,
/// `HANDOFF_LATE`, `HANDOFF_EARLY`, `STARTS`.
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
pub(super) const STARTS_QUEUED: usize = 0;
pub(super) const STARTS_TIMER: usize = 1;
pub(super) const STARTS_WOKEN_EARLY: usize = 2;
pub(super) const STARTS_PARKED_AFTER_TIMER: usize = 3;
pub(super) const STARTS_PARKED_NO_FORECAST: usize = 4;
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
/// Callbacks where the helper was still busy after `HELPER_WAIT_NS` (frame skipped).
pub static HELPER_STALLS: AtomicU64 = AtomicU64::new(0);
/// Callbacks that observed the helper still busy on entry.
pub static HELPER_LATE: AtomicU64 = AtomicU64::new(0);
/// Minimum observed slack between helper completion and the next callback start
/// (0 means the helper finished after the next callback started).
pub static HELPER_MIN_MARGIN_NS: Line<AtomicU64> = Line(AtomicU64::new(u64::MAX));

/// Process-wide cumulative DSP block counters (monotonically increasing, never
/// reset). `LIVE_INPUT_BLOCKS` (helper-written) and `LIVE_OUTPUT_BLOCKS`
/// (callback-written) sit on separate cache lines.
pub static LIVE_INPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_OUTPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_FAULTS: AtomicU64 = AtomicU64::new(0);
pub static LIVE_SKIPPED_OUTPUTS: AtomicU64 = AtomicU64::new(0);

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
/// Thread-local mirror of `SOURCES` so parallel tests can isolate their own
/// callback counts from the process-wide statics.
#[cfg(test)]
pub(super) mod here {
    use super::Outcome;
    use std::cell::Cell;

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
    pub fn sources() -> SourceCounts {
        COUNTS.with(Cell::get)
    }
}
