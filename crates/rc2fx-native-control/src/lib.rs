//! Internal native-audio control protocol and client. No ELF audit entry points,
//! callback implementation, global allocator or audio processing dependencies.
use rc2fx_core::Target;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Client;
#[cfg(target_os = "linux")]
mod save;
#[cfg(target_os = "linux")]
pub use save::{ControlFailure, Publication, SaveFailure, Saved};
pub const VERSION: u32 = 6;
/// Shared bound for the native launch plan and controller's path registry.
/// Includes the physical channels and the virtual USB playback/send paths.
pub const MAX_PATHS: usize = 32;
pub const MAX_REQUEST: usize = 65_536;
pub const MAX_RESPONSE: usize = 131_072;

/// Exact desired-target identity. Hashing and serialization are control work.
pub fn fingerprint(target: &Target) -> Result<[u8; 32], serde_json::Error> {
    use sha2::{Digest, Sha256};
    target.validate().map_err(serde::ser::Error::custom)?;
    serde_json::to_vec(target).map(|bytes| Sha256::digest(bytes).into())
}

/// A completed audio window, never synthesized from publication or wall time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AudioMeter {
    pub revision: u64,
    pub callbacks: u64,
    pub processed_frames: u64,
    pub input_peak: f64,
    pub output_peak: f64,
    pub rms: f64,
    pub gain_reduction: f64,
    /// Highest suppression voice score in the window, 0 to 1.
    pub voice: f64,
    /// Most open voice gate position in the window, 0 to 1.
    pub gate_open: f64,
    pub clipped: u64,
    pub latency_samples: u32,
    pub dropped_windows: u64,
}
/// Live cumulative timings. Counter fields are individually sampled; histogram
/// quantiles are bucket bounds, not exact percentiles. Bounds use the containing
/// budget: <=1/16, <=1/8, <=1/4, <=1/2, <=3/4, <=1, <=2, >2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Timing {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub overruns: u64,
    pub clock_errors: u64,
    pub histogram: [u64; 8],
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CallbackTiming {
    pub callback_budget_ns: u64,
    /// CPU clocks are queried only every Nth callback. Their calls count is the
    /// sampled population; unsampled callbacks are not clock failures or zero CPU.
    pub cpu_sample_every: u32,
    pub attempts: u64,
    pub unexpected_frame_sizes: u64,
    pub callback_wall: Timing,
    pub callback_cpu: Timing,
    /// Pre/post DSP and wrapper work, excluding the original vendor callback.
    pub owned_wall: Timing,
    pub owned_cpu: Timing,
    /// Original native callback including its intercepted buffer lookups.
    pub native_wall: Timing,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkerMeter {
    pub completed_frames: u64,
    pub missed_deadlines: u64,
    pub stale_results: u64,
    pub input_drops: u64,
    pub output_drops: u64,
    pub failed: bool,
    pub frame_budget_ns: u64,
    pub worker_allowance_ns: u64,
    pub allowance_overruns: u64,
    /// Model execution; wall and CPU histogram budgets are frame_budget_ns.
    pub wall: Timing,
    pub cpu: Timing,
    /// Submission to dequeue/publication, using worker_allowance_ns as budget.
    pub queue_wait: Timing,
    pub turnaround: Timing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Session {
    pub pid: u32,
    pub nonce: String,
}
/// Identifies one prepared change in an exact native process/context lifetime.
/// Reuse this receipt when a reply is lost; never manufacture a new save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Transaction {
    pub session: Session,
    pub id: String,
    pub context: u64,
    pub target_id: String,
    pub revision: u64,
}
impl Transaction {
    pub fn valid(&self) -> bool {
        let hex = |value: &str| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        self.context != 0
            && self.revision != 0
            && rc2fx_core::valid_id(&self.target_id)
            && self.session.pid != 0
            && hex(&self.session.nonce)
            && hex(&self.id)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnsupportedVersion,
    WrongSession,
    Unavailable,
    Busy,
    SuppressionInUse,
    StaleRevision,
    RequiresReset,
    InvalidParameters,
    Internal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RetiredWorker {
    pub runtime_epoch: u64,
    pub worker: WorkerMeter,
    pub joined: bool,
    pub drained: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PathMode {
    Processed,
    Passthrough,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TargetStatus {
    pub id: String,
    pub mode: PathMode,
    pub runtime_epoch: u64,
    /// Latest completed retirement only; the count exposes skipped receipts.
    pub retired_workers: Vec<RetiredWorker>,
    pub retired_worker_count: u64,
    pub published: u64,
    pub seen: u64,
    pub applied: u64,
    pub fingerprint: [u8; 32],
    pub meter: Option<AudioMeter>,
    pub worker: Option<WorkerMeter>,
    /// The endpoint's missed-deadline and late-result histories. Each restarts
    /// with a new worker, so a reader resets its sequence when one decreases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub misses: Option<NativeHistory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lates: Option<NativeHistory>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextStatus {
    pub context: u64,
    pub targets: Vec<TargetStatus>,
    pub timing: Option<CallbackTiming>,
    /// Engine-wide counters, always sent: cheap atomics the audio path already
    /// writes, independent of whether callback timing is sampled.
    #[serde(default)]
    pub counters: EngineCounters,
    /// The engine event ring's tail (the newest entries). The ring is
    /// process-wide, so every context returns the same tail; a reader keeps
    /// the highest `seq` it has seen and ignores repeats.
    #[serde(default)]
    pub events: Vec<NativeEvent>,
}
/// One engine event, as the mixer's own ring holds it (see audit.json's
/// `events`). `valueNs` is whatever the kind measures: a gap, a late end, a
/// turnaround. `delayedUs` is jackd's xrun delay for an xrun, when known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeEvent {
    pub seq: u64,
    pub at_ns: u64,
    pub kind: NativeEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delayed_us: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeEventKind {
    Xrun,
    HelperBusy,
    HelperStall,
    HelperLateFinish,
    LateEnd,
    Overrun,
    LateStart,
    Gap,
    CycleSkip,
    VendorJobLate,
    UsbGadget,
    /// A kind this reader does not know (a newer mixer).
    Other,
}
/// A worker's miss or late history, typed for the control API: the first
/// `capacity` events with their sequence numbers and times, and how many came
/// after them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeHistory {
    pub total: u64,
    pub lost: u64,
    pub events: Vec<NativeEventRecord>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NativeEventRecord {
    pub sequence: u64,
    pub at_ns: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EngineCounters {
    /// JACK xrun notifications since this process started. The same figure on
    /// every path; it is not per-notification evidence (audit's xrunDelayed).
    pub xruns: u64,
    /// Helper routines that finished after their frame's cycle had ended.
    pub helper_late: u64,
    /// Callbacks that found the helper still busy with the previous frame.
    pub helper_stalls: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "op", rename_all = "camelCase")]
pub enum Operation {
    Hello,
    Inventory,
    Status {
        context: u64,
        id: String,
    },
    Prepare {
        transaction: Transaction,
        target: Box<Target>,
        mode: PathMode,
    },
    Commit {
        transaction: Transaction,
    },
    Cancel {
        transaction: Transaction,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Request {
    pub version: u32,
    pub request_id: u64,
    pub session: Option<Session>,
    pub operation: Operation,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "result", rename_all = "camelCase")]
// Control-thread wire messages only; keeping Status inline avoids a separate
// allocation per status query. No Reply is constructed by the audio callback.
#[allow(clippy::large_enum_variant)]
pub enum Reply {
    Hello,
    Inventory { contexts: Vec<ContextStatus> },
    Status { context: u64, target: TargetStatus },
    Prepared { transaction: Transaction },
    Published { transaction: Transaction },
    Cancelled { transaction: Transaction },
    Error { code: ErrorCode },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Response {
    pub version: u32,
    pub request_id: u64,
    pub session: Session,
    pub reply: Reply,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_bounds_refuse_ambiguous_lifetime_identifiers() {
        let transaction = Transaction {
            session: Session {
                pid: 42,
                nonce: "a".repeat(32),
            },
            id: "b".repeat(32),
            context: 7,
            target_id: "source".into(),
            revision: 2,
        };
        assert!(transaction.valid());
        let mut changed = transaction.clone();
        changed.id.push('b');
        assert!(!changed.valid());
        let mut changed = transaction.clone();
        changed.session.nonce = "G".repeat(32);
        assert!(!changed.valid());
        let mut changed = transaction.clone();
        changed.context = 0;
        assert!(!changed.valid());
        let mut changed = transaction;
        changed.target_id.clear();
        assert!(!changed.valid());
    }
    #[test]
    fn old_immediate_publication_and_duplicate_transaction_fields_are_rejected() {
        assert!(
            serde_json::from_str::<Operation>(
                r#"{"op":"publish","context":7,"revision":2,"target":{}}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Transaction>(r#"{"session":{"pid":42,"nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","context":7,"context":8,"targetId":"source","revision":2}"#).is_err());
    }
    #[test]
    fn path_mode_is_required_and_distinct_from_effect_bypass() {
        let target = Target::new("source", "Source", rc2fx_core::TargetKind::Mic, 1);
        let transaction = Transaction {
            session: Session {
                pid: 42,
                nonce: "a".repeat(32),
            },
            id: "b".repeat(32),
            context: 7,
            target_id: target.id.clone(),
            revision: 2,
        };
        let operation = Operation::Prepare {
            transaction,
            target: Box::new(target),
            mode: PathMode::Passthrough,
        };
        let mut json = serde_json::to_value(operation).unwrap();
        assert_eq!(json["mode"], "passthrough");
        assert!(matches!(
            serde_json::from_value::<Operation>(json.clone()).unwrap(),
            Operation::Prepare {
                mode: PathMode::Passthrough,
                ..
            }
        ));
        json.as_object_mut().unwrap().remove("mode");
        assert!(serde_json::from_value::<Operation>(json).is_err());
    }
}
