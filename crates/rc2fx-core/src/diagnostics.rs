//! The mixer's diagnostics event list: a bounded, sequenced log of what went
//! wrong and when, and the paged reply a client reads it with.
//!
//! Entries are made off the audio path by rc2fxd (see the device crate): it
//! turns the engine's own event ring, each worker's miss and late histories,
//! and the counter increases it can only see as totals, into these. Timestamps
//! are the mixer's CLOCK_MONOTONIC nanoseconds; a client maps them to host time
//! through `nowNs` at receipt (the mixer's wall clock is not reliable).

use serde::{Deserialize, Serialize};

/// The longest page a client can ask for. Chosen so a full reply stays well
/// inside the USB control transport's frame limit.
pub const EVENTS_PAGE: usize = 256;

/// One entry. `seq` increases by one per entry, never repeats, and survives a
/// diagnostics reset (which adds a `reset` entry rather than restarting).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsEvent {
    pub seq: u64,
    pub kind: DiagnosticsKind,
    /// The path it concerns; absent for engine-wide events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The mixer's CLOCK_MONOTONIC nanoseconds.
    pub at_ns: u64,
    /// A short sentence for the window, already phrased for a person.
    pub message: String,
    /// What the kind measured, when it measured something.
    #[serde(default, skip_serializing_if = "EventDetails::is_empty")]
    pub details: EventDetails,
}

/// The longest event sentence a client keeps.
pub const MAX_EVENT_MESSAGE: usize = 512;

impl DiagnosticsEvent {
    /// This event as it is safe to show: the sentence cut to
    /// [`MAX_EVENT_MESSAGE`] bytes with control characters removed. `None` if
    /// `path` is not an id, which no mixer sends.
    pub fn for_display(mut self) -> Option<Self> {
        if self.path.as_deref().is_some_and(|id| !crate::valid_id(id)) {
            return None;
        }
        let mut message = String::with_capacity(self.message.len().min(MAX_EVENT_MESSAGE));
        for c in self
            .message
            .chars()
            .filter(|&c| !c.is_control() && !crate::is_bidi_control(c))
        {
            if message.len() + c.len_utf8() > MAX_EVENT_MESSAGE {
                break;
            }
            message.push(c);
        }
        self.message = message;
        Some(self)
    }
}

/// What happened, in a closed set: the window can label each kind without
/// guessing, and a newer mixer's addition arrives as `other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticsKind {
    /// A JACK xrun notification (the engine was late somewhere).
    Xrun,
    /// A callback found the helper still busy with the previous frame.
    HelperBusy,
    /// A callback gave up waiting for the helper and passed its frame through.
    HelperStall,
    /// The helper finished after its frame's cycle had ended.
    HelperLateFinish,
    /// The callback ended this late into its cycle.
    LateEnd,
    /// The callback took longer than a period.
    Overrun,
    /// The callback started this late into its cycle.
    LateStart,
    /// Consecutive callbacks were further apart than a period and a half.
    Gap,
    /// The estimated cycle start jumped by more than a period and a half.
    CycleSkip,
    /// A vendor job finished after the next callback had started.
    VendorJobLate,
    /// The mixer rebuilt its USB gadget (a USB mode change).
    UsbGadget,
    /// The suppression worker missed a frame's deadline: the listener heard
    /// the delayed original for that frame.
    MissedDeadline,
    /// A worker result arrived after its deadline (still used, but late).
    LateFrame,
    /// Stale suppression results, counted between two readings.
    StaleResults,
    /// Dropped input frames, counted between two readings.
    InputDrops,
    /// Dropped output frames, counted between two readings.
    OutputDrops,
    /// Clipped samples, counted between two readings.
    Clipped,
    /// Events that fell out of the engine's ring before they could be read.
    Lost,
    /// The user reset diagnostics; the counters the window shows restart here.
    Reset,
    /// A kind this build does not know (a newer mixer).
    Other,
}

/// The numbers a kind carries. Every field is optional: an xrun has no delta,
/// a counter increase has no duration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventDetails {
    /// What the kind measured: a cycle position, a gap, a turnaround.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ns: Option<u64>,
    /// jackd's xrun delay in microseconds, when it reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delayed_us: Option<u64>,
    /// How much a counted figure rose (drops, clipped samples, lost events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<u64>,
    /// The interval the delta accumulated over, from the previous reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ns: Option<u64>,
}

impl EventDetails {
    pub fn is_empty(&self) -> bool {
        self.value_ns.is_none()
            && self.delayed_us.is_none()
            && self.delta.is_none()
            && self.interval_ns.is_none()
    }
}

/// One page of the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsEvents {
    /// The mixer's CLOCK_MONOTONIC at the moment of the reply: a client adds
    /// (local receive time − this) to each `atNs`.
    pub now_ns: u64,
    /// The sequence number the next entry will take, i.e. what to pass as
    /// `after` to continue reading.
    pub next_seq: u64,
    /// Entries that fell out of the bounded log after `after` and can no
    /// longer be read. Zero when the client kept up.
    pub lost: u64,
    /// More entries are waiting: ask again with `after = nextSeq`.
    pub more: bool,
    pub events: Vec<DiagnosticsEvent>,
}
