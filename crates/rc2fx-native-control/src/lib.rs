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
/// Frames in one meter window. The native library finishes a path's window at
/// the first cycle that reaches this count: 22 cycles of 112 frames, 51 ms.
pub const METER_WINDOW_FRAMES: u64 = 2400;
/// Finished meter windows the native library keeps per path until the
/// controller reads them. With a slower reader the ring fills, and each window
/// that finishes while it is full is lost, which `AudioMeter::dropped_windows`
/// counts. The controller derives its slowest inventory interval from this
/// depth.
///
/// The native telemetry ring takes its depth from this constant, so the two
/// cannot drift apart. It must stay a power of two.
pub const METER_RING_WINDOWS: usize = 8;

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

/// An inventory response as a plain struct, with the reply's tag as an
/// ordinary field. The same JSON as `Response` with `Reply::Inventory`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InventoryResponse {
    version: u32,
    request_id: u64,
    session: Session,
    reply: InventoryReply,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryReply {
    #[serde(rename = "result")]
    _result: InventoryTag,
    contexts: Vec<ContextStatus>,
}
/// The tag of an inventory reply: the string "inventory" and nothing else.
///
/// A derived enum would not do. serde also reads a unit variant from the map
/// `{"inventory": null}`, which the general decoder refuses as a tag, so the
/// direct decoder would accept a document the general one does not.
struct InventoryTag;
impl<'de> Deserialize<'de> for InventoryTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Tag;
        impl serde::de::Visitor<'_> for Tag {
            type Value = InventoryTag;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("the reply tag \"inventory\"")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<InventoryTag, E> {
                if value == "inventory" {
                    Ok(InventoryTag)
                } else {
                    Err(E::unknown_variant(value, &["inventory"]))
                }
            }
        }
        // As the general decoder reads a tag: an identifier, so a string.
        deserializer.deserialize_identifier(Tag)
    }
}

impl Response {
    /// Decodes the response to an inventory request. It gives the same result
    /// as decoding a `Response` for every input, in less time.
    ///
    /// `Reply` is internally tagged, and serde decodes such an enum by copying
    /// the whole object into a generic tree before it reads the typed value
    /// from that tree. An inventory is about 24 KB and is read nine times a
    /// second, and the copy is a large part of the controller's work per read.
    /// This reads a well-formed inventory straight into its types. Anything
    /// else, such as a Busy error or malformed JSON, is decoded the general
    /// way, which also produces the error.
    pub fn decode_inventory(bytes: &[u8]) -> serde_json::Result<Self> {
        match serde_json::from_slice::<InventoryResponse>(bytes) {
            Ok(response) => Ok(Self {
                version: response.version,
                request_id: response.request_id,
                session: response.session,
                reply: Reply::Inventory {
                    contexts: response.reply.contexts,
                },
            }),
            Err(_) => serde_json::from_slice(bytes),
        }
    }
}

/// `Response::decode_inventory` against the general decoder, which is the
/// reference: the same value or the same error for every input.
#[cfg(test)]
mod decode_tests {
    use super::*;
    use serde_json::{Value, json};

    /// One change to a JSON document.
    type Edit = Box<dyn Fn(&mut Value)>;

    /// Both decoders on the same bytes. Returns whether they accepted them.
    fn same(bytes: &[u8]) -> bool {
        let reference = serde_json::from_slice::<Response>(bytes);
        let direct = Response::decode_inventory(bytes);
        // Debug prints every field, and tells 0.0 from -0.0.
        assert_eq!(
            format!("{direct:?}"),
            format!("{reference:?}"),
            "{}",
            String::from_utf8_lossy(bytes)
        );
        reference.is_ok()
    }

    /// A small deterministic generator, so a failure can be reproduced.
    struct Random(u64);
    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
        fn count(&mut self) -> u64 {
            match self.below(6) {
                0 => 0,
                1 => u64::MAX,
                2 => self.next(),
                _ => self.next() % 100_000,
            }
        }
        fn level(&mut self) -> f64 {
            match self.below(8) {
                0 => 0.0,
                1 => -0.0,
                2 => f64::MAX,
                3 => f64::MIN_POSITIVE,
                4 => 5e-324,
                5 => -1.0,
                _ => (self.next() % 1_000_000) as f64 / 999_983.0,
            }
        }
        fn timing(&mut self) -> Timing {
            Timing {
                calls: self.count(),
                total_ns: self.count(),
                max_ns: self.count(),
                overruns: self.count(),
                clock_errors: self.count(),
                histogram: std::array::from_fn(|_| self.count()),
            }
        }
        fn worker(&mut self) -> WorkerMeter {
            WorkerMeter {
                completed_frames: self.count(),
                missed_deadlines: self.count(),
                stale_results: self.count(),
                input_drops: self.count(),
                output_drops: self.count(),
                failed: self.below(2) == 0,
                frame_budget_ns: self.count(),
                worker_allowance_ns: self.count(),
                allowance_overruns: self.count(),
                wall: self.timing(),
                cpu: self.timing(),
                queue_wait: self.timing(),
                turnaround: self.timing(),
            }
        }
        fn history(&mut self) -> NativeHistory {
            NativeHistory {
                total: self.count(),
                lost: self.count(),
                events: (0..self.below(5))
                    .map(|_| NativeEventRecord {
                        sequence: self.count(),
                        at_ns: self.count(),
                    })
                    .collect(),
            }
        }
        fn target(&mut self, index: usize) -> TargetStatus {
            TargetStatus {
                id: format!("path-{index}-\u{e9}\"\\\n"),
                mode: if self.below(2) == 0 {
                    PathMode::Processed
                } else {
                    PathMode::Passthrough
                },
                runtime_epoch: self.count(),
                retired_workers: (0..self.below(3))
                    .map(|_| RetiredWorker {
                        runtime_epoch: self.count(),
                        worker: self.worker(),
                        joined: self.below(2) == 0,
                        drained: self.below(2) == 0,
                    })
                    .collect(),
                retired_worker_count: self.count(),
                published: self.count(),
                seen: self.count(),
                applied: self.count(),
                fingerprint: std::array::from_fn(|_| self.next() as u8),
                meter: (self.below(4) != 0).then(|| AudioMeter {
                    revision: self.count(),
                    callbacks: self.count(),
                    processed_frames: self.count(),
                    input_peak: self.level(),
                    output_peak: self.level(),
                    rms: self.level(),
                    gain_reduction: self.level(),
                    voice: self.level(),
                    gate_open: self.level(),
                    clipped: self.count(),
                    latency_samples: self.next() as u32,
                    dropped_windows: self.count(),
                }),
                worker: (self.below(3) == 0).then(|| self.worker()),
                misses: (self.below(3) == 0).then(|| self.history()),
                lates: (self.below(3) == 0).then(|| self.history()),
            }
        }
        fn context(&mut self) -> ContextStatus {
            const KINDS: [NativeEventKind; 12] = [
                NativeEventKind::Xrun,
                NativeEventKind::HelperBusy,
                NativeEventKind::HelperStall,
                NativeEventKind::HelperLateFinish,
                NativeEventKind::LateEnd,
                NativeEventKind::Overrun,
                NativeEventKind::LateStart,
                NativeEventKind::Gap,
                NativeEventKind::CycleSkip,
                NativeEventKind::VendorJobLate,
                NativeEventKind::UsbGadget,
                NativeEventKind::Other,
            ];
            ContextStatus {
                context: self.count(),
                targets: (0..self.below(6)).map(|index| self.target(index)).collect(),
                timing: (self.below(3) != 0).then(|| CallbackTiming {
                    callback_budget_ns: self.count(),
                    cpu_sample_every: self.next() as u32,
                    attempts: self.count(),
                    unexpected_frame_sizes: self.count(),
                    callback_wall: self.timing(),
                    callback_cpu: self.timing(),
                    owned_wall: self.timing(),
                    owned_cpu: self.timing(),
                    native_wall: self.timing(),
                }),
                counters: EngineCounters {
                    xruns: self.count(),
                    helper_late: self.count(),
                    helper_stalls: self.count(),
                },
                events: (0..self.below(8))
                    .map(|_| NativeEvent {
                        seq: self.count(),
                        at_ns: self.count(),
                        kind: KINDS[self.below(KINDS.len())],
                        value_ns: (self.below(2) == 0).then(|| self.count()),
                        delayed_us: (self.below(2) == 0).then(|| self.count()),
                    })
                    .collect(),
            }
        }
        fn response(&mut self, reply: Reply) -> Response {
            Response {
                version: self.next() as u32,
                request_id: self.count(),
                session: Session {
                    pid: self.next() as u32,
                    nonce: format!("{:032x}", self.next()),
                },
                reply,
            }
        }
        fn inventory(&mut self) -> Response {
            let contexts = (0..self.below(4)).map(|_| self.context()).collect();
            self.response(Reply::Inventory { contexts })
        }
    }

    fn transaction() -> Transaction {
        Transaction {
            session: Session {
                pid: 42,
                nonce: "a".repeat(32),
            },
            id: "b".repeat(32),
            context: 7,
            target_id: "source".into(),
            revision: 2,
        }
    }

    /// Every reply the library can send, including each one an inventory
    /// request can be answered with instead of an inventory.
    fn replies(random: &mut Random) -> Vec<Response> {
        let target = random.target(0);
        let mut all = vec![
            random.response(Reply::Hello),
            random.response(Reply::Inventory { contexts: vec![] }),
            random.response(Reply::Status { context: 7, target }),
            random.response(Reply::Prepared {
                transaction: transaction(),
            }),
            random.response(Reply::Published {
                transaction: transaction(),
            }),
            random.response(Reply::Cancelled {
                transaction: transaction(),
            }),
        ];
        for code in [
            ErrorCode::UnsupportedVersion,
            ErrorCode::WrongSession,
            ErrorCode::Unavailable,
            ErrorCode::Busy,
            ErrorCode::SuppressionInUse,
            ErrorCode::StaleRevision,
            ErrorCode::RequiresReset,
            ErrorCode::InvalidParameters,
            ErrorCode::Internal,
        ] {
            all.push(random.response(Reply::Error { code }));
        }
        all
    }

    #[test]
    fn every_reply_kind_decodes_as_the_general_decoder_does() {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for response in replies(&mut random) {
            let bytes = serde_json::to_vec(&response).unwrap();
            assert!(same(&bytes));
            // The same document with its keys in another order (sorted, so
            // the reply's tag is no longer first) and with whitespace.
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(same(&serde_json::to_vec(&value).unwrap()));
            assert!(same(&serde_json::to_vec_pretty(&value).unwrap()));
        }
        // An inventory takes the direct path; the comparisons above are not
        // all the general decoder against itself.
        let bytes = serde_json::to_vec(&random.inventory()).unwrap();
        assert!(serde_json::from_slice::<InventoryResponse>(&bytes).is_ok());
        assert!(same(&bytes));
    }

    #[test]
    fn random_inventories_decode_as_the_general_decoder_does() {
        let mut random = Random(0x0123_4567_89ab_cdef);
        for _ in 0..300 {
            let bytes = serde_json::to_vec(&random.inventory()).unwrap();
            assert!(same(&bytes));
        }
    }

    /// Documents that are almost an inventory. The direct decoder must not
    /// accept one the general decoder refuses, or read one differently.
    #[test]
    fn malformed_inventories_are_refused_exactly_as_before() {
        let mut random = Random(0xfeed_face_cafe_beef);
        let mut context = random.context();
        context.targets = vec![random.target(0), random.target(1)];
        while context.targets[0].meter.is_none() {
            context.targets[0] = random.target(0);
        }
        context.targets[0].misses = Some(random.history());
        context.targets[0].lates = Some(random.history());
        context.targets[0].worker = Some(random.worker());
        context.timing = Some(CallbackTiming::default());
        let good = serde_json::to_value(random.response(Reply::Inventory {
            contexts: vec![context],
        }))
        .unwrap();
        assert!(same(&serde_json::to_vec(&good).unwrap()));
        let edits: Vec<Edit> = vec![
            // Unknown fields at each level.
            Box::new(|v| v["extra"] = json!(1)),
            Box::new(|v| v["reply"]["extra"] = json!(1)),
            Box::new(|v| v["reply"]["contexts"][0]["extra"] = json!(1)),
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["extra"] = json!(1)),
            Box::new(|v| v["session"]["extra"] = json!(1)),
            // A wrong, missing or oddly typed tag.
            Box::new(|v| v["reply"]["result"] = json!("Inventory")),
            Box::new(|v| v["reply"]["result"] = json!("status")),
            Box::new(|v| v["reply"]["result"] = json!("error")),
            Box::new(|v| v["reply"]["result"] = json!("hello")),
            Box::new(|v| v["reply"]["result"] = json!(null)),
            Box::new(|v| v["reply"]["result"] = json!(7)),
            Box::new(|v| v["reply"]["result"] = json!(["inventory"])),
            // An enum's other JSON form. A tag is only ever a string.
            Box::new(|v| v["reply"]["result"] = json!({"inventory": null})),
            Box::new(|v| v["reply"]["result"] = json!({"inventory": []})),
            Box::new(|v| v["reply"]["result"] = json!(0)),
            Box::new(|v| drop(v["reply"].as_object_mut().unwrap().remove("result"))),
            // Missing and mistyped fields.
            Box::new(|v| drop(v["reply"].as_object_mut().unwrap().remove("contexts"))),
            Box::new(|v| drop(v.as_object_mut().unwrap().remove("session"))),
            Box::new(|v| drop(v.as_object_mut().unwrap().remove("version"))),
            Box::new(|v| v["reply"]["contexts"] = json!(null)),
            Box::new(|v| v["reply"]["contexts"] = json!({})),
            Box::new(|v| v["reply"] = json!(null)),
            Box::new(|v| v["reply"] = json!([])),
            Box::new(|v| v["reply"] = json!("inventory")),
            Box::new(|v| v["version"] = json!(4_294_967_296_u64)),
            Box::new(|v| v["version"] = json!(-1)),
            Box::new(|v| v["version"] = json!(6.0)),
            Box::new(|v| v["requestId"] = json!("1")),
            Box::new(|v| v["reply"]["contexts"][0]["context"] = json!(1.5)),
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["fingerprint"] = json!([1, 2])),
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["fingerprint"][3] = json!(256)),
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["mode"] = json!("bypass")),
        ];
        // Well-formed all the same, and the two decoders must read them
        // alike: optional fields left out or null, defaulted fields left out,
        // numbers of other kinds.
        let alike: Vec<Edit> = vec![
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["meter"] = json!(null)),
            Box::new(|v| v["reply"]["contexts"][0]["targets"][0]["misses"] = json!(null)),
            Box::new(|v| {
                let target = v["reply"]["contexts"][0]["targets"][0]
                    .as_object_mut()
                    .unwrap();
                target.remove("misses");
                target.remove("lates");
            }),
            Box::new(|v| {
                let context = v["reply"]["contexts"][0].as_object_mut().unwrap();
                context.remove("counters");
                context.remove("events");
            }),
            Box::new(|v| v["reply"]["contexts"][0]["timing"] = json!(null)),
            Box::new(|v| {
                v["reply"]["contexts"][0]["targets"][0]["meter"] = json!({
                    "revision": 1, "callbacks": 2, "processedFrames": 3,
                    "inputPeak": 1, "outputPeak": -2, "rms": 18_446_744_073_709_551_615_u64,
                    "gainReduction": 1e-320, "voice": -0.0, "gateOpen": 1e308,
                    "clipped": 0, "latencySamples": 0, "droppedWindows": 0
                })
            }),
        ];
        for (index, edit) in edits.iter().enumerate() {
            let mut value = good.clone();
            edit(&mut value);
            assert_ne!(value, good, "edit {index} changed nothing");
            same(&serde_json::to_vec(&value).unwrap());
        }
        for (index, edit) in alike.iter().enumerate() {
            let mut value = good.clone();
            edit(&mut value);
            assert_ne!(value, good, "edit {index} changed nothing");
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(same(&bytes), "edit {index} was refused");
            assert!(serde_json::from_slice::<InventoryResponse>(&bytes).is_ok());
        }
        // Duplicate keys and other text a value tree cannot hold.
        let text = serde_json::to_string(&good).unwrap();
        let tag = r#""result":"inventory""#;
        assert!(text.contains(tag));
        for edited in [
            text.replacen(tag, r#""result":"inventory","result":"inventory""#, 1),
            text.replacen(tag, r#""result":"error","result":"inventory""#, 1),
            text.replacen(tag, r#""result":"inventory","contexts":[]"#, 1),
            text.replacen(r#""version":"#, r#""version":1,"version":"#, 1),
            text.replacen(r#""context":"#, r#""context":1,"context":"#, 1),
            text.replacen(r#""published":"#, r#""published":1,"published":"#, 1),
            text.replacen(tag, r#""result":"inventory","code":"busy""#, 1),
            text.replacen(tag, r#""result":"error","code":"busy""#, 1),
            format!("{text} "),
            format!("{text}x"),
            format!("{text}{text}"),
            format!("[{text}]"),
            text.replace("1e", "1E"),
            String::new(),
            "null".into(),
            "{}".into(),
        ] {
            same(edited.as_bytes());
        }
    }

    #[test]
    fn truncated_and_corrupted_bytes_are_refused_exactly_as_before() {
        const SYNTAX: &[u8] = b"{}[],:\"0-.e tnul";
        let mut random = Random(0x5bd1_e995_c0ff_ee11);
        let mut accepted = 0;
        for _ in 0..40 {
            let bytes = serde_json::to_vec(&random.inventory()).unwrap();
            // Cut anywhere.
            for _ in 0..40 {
                let cut = random.below(bytes.len());
                assert!(!same(&bytes[..cut]) || cut == 0);
            }
            // Change, drop or repeat bytes anywhere.
            for _ in 0..150 {
                let mut changed = bytes.clone();
                let at = random.below(changed.len());
                match random.below(4) {
                    0 => changed[at] = random.next() as u8,
                    1 => changed[at] = SYNTAX[random.below(SYNTAX.len())],
                    2 => drop(changed.remove(at)),
                    _ => changed.insert(at, bytes[at]),
                }
                accepted += usize::from(same(&changed));
            }
        }
        // Some corruptions only change a digit: both decoders took those.
        assert!(accepted > 0);
    }

    /// Every place in a document, as a JSON pointer.
    fn places(value: &Value, here: &str, out: &mut Vec<String>) {
        out.push(here.to_owned());
        match value {
            Value::Object(fields) => {
                for (key, field) in fields {
                    places(field, &format!("{here}/{key}"), out);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    places(item, &format!("{here}/{index}"), out);
                }
            }
            _ => {}
        }
    }

    /// Changes of shape rather than of bytes: a value anywhere in the document
    /// replaced by one of another JSON type, wrapped, or turned from an object
    /// into an array. A typed decoder and one that goes through a value tree
    /// are most likely to disagree on these, because serde reads structs,
    /// enums, options and numbers from more than one JSON form.
    #[test]
    fn documents_of_another_shape_decode_as_the_general_decoder_does() {
        let mut random = Random(0x1357_9bdf_0246_8ace);
        let (mut accepted, mut refused, mut direct) = (0, 0, 0);
        for _ in 0..12 {
            let good = serde_json::to_value(random.inventory()).unwrap();
            let mut all = Vec::new();
            places(&good, "", &mut all);
            for _ in 0..250 {
                let mut value = good.clone();
                let place = &all[random.below(all.len())];
                let slot = value.pointer_mut(place).unwrap();
                let old = slot.take();
                *slot = match random.below(16) {
                    0 => Value::Null,
                    1 => json!(true),
                    2 => json!(0),
                    3 => json!(-1),
                    4 => json!(1.5),
                    5 => json!("inventory"),
                    6 => json!([]),
                    7 => json!({}),
                    8 => json!([old]),
                    // The map form of an enum, with and without a null value.
                    9 => json!({"inventory": old}),
                    10 => json!({"inventory": null}),
                    11 => json!({"processed": null}),
                    12 => json!({"xrun": {}}),
                    // An object as the array of its values, which is how serde
                    // reads a struct from a sequence.
                    13 => match old {
                        Value::Object(fields) => fields.into_iter().map(|(_, v)| v).collect(),
                        other => json!([other, other]),
                    },
                    // A number as a string and a string as a number.
                    14 => match old {
                        Value::Number(number) => json!(number.to_string()),
                        Value::String(_) => json!(7),
                        other => json!({ "0": other }),
                    },
                    _ => json!(18_446_744_073_709_551_615_u64),
                };
                let bytes = serde_json::to_vec(&value).unwrap();
                if same(&bytes) {
                    accepted += 1;
                } else {
                    refused += 1;
                }
                direct += usize::from(serde_json::from_slice::<InventoryResponse>(&bytes).is_ok());
            }
        }
        // Both outcomes occur, and the direct path decoded some of them itself.
        assert!(accepted > 100 && refused > 100 && direct > 100);

        // Structs written as sequences, in field order, at each level. The
        // general decoder takes a tagged enum from a sequence too.
        for text in [
            r#"{"version":6,"requestId":1,"session":[42,"n"],"reply":{"result":"inventory","contexts":[]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":["inventory",[]]}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":["inventory"]}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":["inventory",[],[]]}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":["hello"]}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":["error","busy"]}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":[{"inventory":null},[]]}"#,
            r#"[6,1,{"pid":42,"nonce":"n"},{"result":"inventory","contexts":[]}]"#,
            r#"[6,1,[42,"n"],["inventory",[]]]"#,
            r#"[6,1,[42,"n"],["inventory",[]],0]"#,
            r#"[6,1,[42,"n"]]"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[[7,[],null,[1,2,3],[]]]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[[7,[],null,[1,2,3],[[1,2,"xrun",null,null]]]]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[{"context":7,"targets":[],"events":[{"seq":1,"atNs":2,"kind":{"xrun":null}}]}]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[{"context":7,"targets":[],"events":[{"seq":1,"atNs":2,"kind":{"xrun":{}}}]}]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[]}}"#,
            r#"{"version":6,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"contexts":[],"result":"inventory"}}"#,
            r#"{"version":6.0e0,"requestId":1,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[]}}"#,
            r#"{"version":6,"requestId":-0,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[]}}"#,
            r#"{"version":6,"requestId":18446744073709551616,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[]}}"#,
            r#"{"version":6,"requestId":1e999,"session":{"pid":42,"nonce":"n"},"reply":{"result":"inventory","contexts":[]}}"#,
        ] {
            same(text.as_bytes());
        }
    }

    /// Float text that a JSON value tree cannot hold as written: the two
    /// decoders must read every spelling of a number to the same bits.
    #[test]
    fn every_spelling_of_a_level_reads_to_the_same_bits() {
        // A real inventory whose six levels are a number that occurs nowhere
        // else in the text; each spelling is put in its place.
        const MARK: &str = "0.123456789";
        let mut random = Random(0x0f1e_2d3c_4b5a_6978);
        let mut target = random.target(0);
        target.meter = Some(AudioMeter {
            revision: 1,
            callbacks: 2,
            processed_frames: 3,
            input_peak: 0.123456789,
            output_peak: 0.123456789,
            rms: 0.123456789,
            gain_reduction: 0.123456789,
            voice: 0.123456789,
            gate_open: 0.123456789,
            clipped: 0,
            latency_samples: 0,
            dropped_windows: 0,
        });
        let mut context = random.context();
        context.targets = vec![target];
        let text = serde_json::to_string(&random.response(Reply::Inventory {
            contexts: vec![context],
        }))
        .unwrap();
        assert_eq!(text.matches(MARK).count(), 6);
        let meter = |level: &str| text.replace(MARK, level);
        let mut both = 0;
        for level in [
            "0",
            "-0",
            "0.0",
            "-0.0",
            "0e0",
            "-0e-999",
            "1",
            "-1",
            "1.0",
            "1e0",
            "1E+0",
            "0.1",
            "0.30000000000000004",
            "1e-320",
            "5e-324",
            "2.4703282292062327e-324",
            "2.4703282292062328e-324",
            "4.9406564584124654e-324",
            "2.2250738585072011e-308",
            "2.2250738585072014e-308",
            "1.7976931348623157e308",
            "1.7976931348623159e308",
            "1e308",
            "1e309",
            "-1e309",
            "9007199254740993",
            "-9007199254740993",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775808",
            "-9223372036854775809",
            "18446744073709551615",
            "18446744073709551616",
            "123456789012345678901234567890",
            "0.000000000000000000000000000000000000000000001",
            "1.00000000000000011102230246251565404236316680908203125",
            "1.00000000000000011102230246251565404236316680908203124",
            "1.00000000000000011102230246251565404236316680908203126",
            "8.5e-1",
            "NaN",
            "Infinity",
            "-Infinity",
            "null",
            "\"1\"",
            "01",
            "1.",
            ".5",
            "+1",
            "1e",
            "0x10",
        ] {
            both += usize::from(same(meter(level).as_bytes()));
        }
        assert!(both > 30, "{both}");
    }
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
