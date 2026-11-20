//! Bounded, sequenced diagnostics event log and its paged wire response.
//!
//! `rc2fxd` builds these off the audio thread from the engine event ring,
//! worker miss/late histories, and monotonic counter deltas. Timestamps use
//! the mixer's `CLOCK_MONOTONIC` in nanoseconds; clients map them to local
//! wall time via `nowNs` because the mixer's wall clock is unreliable.

use serde::{Deserialize, Serialize};

/// Max entries per page, sized to fit inside the USB control frame limit.
pub const EVENTS_PAGE: usize = 256;

/// Single diagnostics log entry. `seq` increments by 1, never repeats, and
/// continues across a diagnostics reset (which appends a `reset` entry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsEvent {
    pub seq: u64,
    pub kind: DiagnosticsKind,
    /// Target path, or `None` for engine-wide events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Mixer `CLOCK_MONOTONIC` timestamp in nanoseconds.
    pub at_ns: u64,
    /// Human-readable summary for the UI.
    pub message: String,
    /// Measured values for this event, if any.
    #[serde(default, skip_serializing_if = "EventDetails::is_empty")]
    pub details: EventDetails,
}

/// Closed set of event kinds; unknown kinds from newer firmware deserialize as `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticsKind {
    /// JACK xrun notification.
    Xrun,
    /// Audio callback found the helper still busy with the previous frame.
    HelperBusy,
    /// Audio callback gave up waiting for the helper and passed the frame through.
    HelperStall,
    /// Helper finished after its frame's cycle had already ended.
    HelperLateFinish,
    /// Audio callback finished late in its cycle.
    LateEnd,
    /// Audio callback took longer than one period.
    Overrun,
    /// Audio callback started late in its cycle.
    LateStart,
    /// Gap between consecutive callbacks exceeded 1.5 periods.
    Gap,
    /// Estimated cycle start jumped by more than 1.5 periods.
    CycleSkip,
    /// Vendor job finished after the next callback had started.
    VendorJobLate,
    /// Mixer rebuilt its USB gadget (USB mode change).
    UsbGadget,
    /// Suppression worker missed a frame deadline (listener heard delayed dry audio).
    /// With `delta`, reports additional misses beyond the worker's kept history.
    MissedDeadline,
    /// Worker result arrived past its deadline (still used, but late).
    /// With `delta`, reports additional late frames beyond the kept history.
    LateFrame,
    /// Stale suppression results observed between readings.
    StaleResults,
    /// Dropped input frames observed between readings.
    InputDrops,
    /// Dropped output frames observed between readings.
    OutputDrops,
    /// Clipped samples observed between readings.
    Clipped,
    /// Events dropped from the engine ring before they were read.
    Lost,
    /// Diagnostics reset marker; UI counters restart from here.
    Reset,
    /// Unknown event kind from a newer mixer build.
    #[serde(other)]
    Other,
}

/// Optional numeric payload for a diagnostics event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventDetails {
    /// Measured duration or timestamp offset (cycle position, gap, turnaround).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ns: Option<u64>,
    /// Xrun delay reported by jackd, in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delayed_us: Option<u64>,
    /// Counter increase since the previous reading (drops, clips, lost events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<u64>,
    /// Elapsed time over which `delta` accumulated.
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

/// Paged diagnostics log response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsEvents {
    /// Mixer `CLOCK_MONOTONIC` at reply time; clients add `(local_recv - now_ns)` to `atNs`.
    pub now_ns: u64,
    /// Next sequence number; pass as `after` to fetch subsequent entries.
    pub next_seq: u64,
    /// Entries evicted after `after` before the client read them (0 if caught up).
    pub lost: u64,
    /// True if more entries are available starting at `nextSeq`.
    pub more: bool,
    pub events: Vec<DiagnosticsEvent>,
}
