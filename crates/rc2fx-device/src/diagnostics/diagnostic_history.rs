//! Bounded control-thread diagnostics history of telemetry samples and events,
//! keyed by engine generation.
use rc2fx_core::{
    DeviceState, EngineStatus, Meter, Performance, RoutingStatus, SelectionStatus, TargetMeter,
};
use serde::Serialize;
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;
pub const MAX_HISTORY_BYTES: usize = 384 * 1024;
// Samples may borrow unused event capacity, but may never evict an event.
// Event-to-event FIFO eviction remains independently bounded by bytes/count.
pub const MAX_EVENT_HISTORY_BYTES: usize = 128 * 1024;
pub const MAX_SAMPLES: usize = 300;
pub const MAX_EVENTS: usize = 256;
const MAX_RECORD_BYTES: usize = 32 * 1024;
// Native windows cover the full path catalog, including per-path workers.
// Keep room for the observation wrapper even when event history is full.
const MAX_NATIVE_TELEMETRY_BYTES: usize = 128 * 1024;
const MAX_IDENTITY_BYTES: usize = 16 * 1024;
const NOTES: &str = "resources describes only the controller process; nativeMixerResources, when present, separately describes the vendor mixer process including RC2FX native processing. Shared memory/RSS must not be summed. Independent recovery watcher resources are not included. Native telemetry retains wire counters and optional timing in nativeDiagnostics. Native wall/owned/native clocks are distinct; CPU clocks are sampled at cpuSampleEvery, and their calls count identifies the sampled population. Absent timing is unavailable, never zero cost. History clears on daemon restart. Samples are at most once per second with no catch-up. telemetryStatus explains missing telemetry (no fresh window, epoch change or oversized record); null is never zero audio or zero cost. Fresh means previously unconsumed, not contemporaneous: telemetry.at is control receipt time and observation.at is later process-sample start time; neither is callback time. CPU ticks are cumulative process CPU at the reported ticksPerSecond. Counters are individually sampled. Peaks describe the latest consumed window, not the whole second. No recorded audio or saved effect parameters are included.";

#[derive(Debug)]
pub struct ExportError;
impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Diagnostics exceed the bounded export or could not serialize")
    }
}
impl std::error::Error for ExportError {}

struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Limited {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(1024)),
            limit,
        }
    }
    fn json(&mut self, value: &impl Serialize) -> Result<(), ExportError> {
        serde_json::to_writer(self, value).map_err(|_| ExportError)
    }
    fn raw(&mut self, value: &[u8]) -> Result<(), ExportError> {
        self.write_all(value).map_err(|_| ExportError)
    }
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("bounded JSON capacity"));
        }
        // Grow geometrically only within the explicit cap, rather than letting
        // Vec::extend choose a capacity beyond it or reallocating each JSON token.
        if bytes.len() > self.bytes.capacity() - self.bytes.len() {
            let desired = (self.bytes.capacity().saturating_mul(2))
                .max(self.bytes.len() + bytes.len())
                .min(self.limit);
            self.bytes.reserve_exact(desired - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded(value: &impl Serialize, limit: usize) -> Result<Arc<[u8]>, ExportError> {
    let mut out = Limited::new(limit);
    out.json(value)?;
    Ok(out.bytes.into())
}
fn short(text: &str) -> String {
    let mut end = text.len().min(1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Identity {
    revision: u64,
    selected_targets: Vec<String>,
    engine: EngineStatus,
    routing: RoutingStatus,
    selection: SelectionStatus,
}
impl Identity {
    fn new(state: &DeviceState) -> Self {
        let mut value = Self {
            revision: state.config.revision,
            selected_targets: state.config.selected_targets.clone(),
            engine: state.engine.clone(),
            routing: state.routing.clone(),
            selection: state.selection.clone(),
        };
        value.engine.message = short(&value.engine.message);
        value.routing.message = short(&value.routing.message);
        value.selection.message = short(&value.selection.message);
        value
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Stamp {
    elapsed_ms: u64,
    unix_time_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ProcessResources {
    Measured {
        pid: u32,
        #[serde(rename = "startTicks")]
        start_ticks: u64,
        #[serde(rename = "cpuTicks")]
        cpu_ticks: u64,
        #[serde(rename = "ticksPerSecond")]
        ticks_per_second: u64,
        #[serde(rename = "rssBytes")]
        rss_bytes: u64,
        #[serde(rename = "virtualBytes")]
        virtual_bytes: u64,
        threads: u64,
    },
    Unavailable {
        message: String,
    },
}
pub(crate) fn process_resources() -> ProcessResources {
    process_resources_for(std::process::id())
}
fn process_resources_for(pid: u32) -> ProcessResources {
    use std::io::Read;
    let result = (|| {
        let mut bytes = Vec::new();
        std::fs::File::open(format!("/proc/{pid}/stat"))?
            .take(8193)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 8192 {
            return Err(io::Error::other("process stat exceeds bound"));
        }
        // These queries do not modify process state and run only off-callback.
        let (hz, page) = unsafe {
            (
                libc::sysconf(libc::_SC_CLK_TCK),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        if hz <= 0 || page <= 0 {
            return Err(io::Error::other("process units unavailable"));
        }
        parse_process(&bytes, pid, hz as u64, page as u64)
    })();
    result.unwrap_or_else(|error| ProcessResources::Unavailable {
        message: short(&error.to_string()),
    })
}
/// Engine process start time on the diagnostics event clock (`CLOCK_MONOTONIC`).
///
/// `/proc/<pid>/stat` field 22 (`starttime`) counts `_SC_CLK_TCK` ticks since
/// boot. Because this device never suspends, `CLOCK_MONOTONIC` tracks boot time
/// directly and converts onto the same clock as `diagnostics_log::monotonic_ns`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessStart {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat`: the process identity, and what changes
    /// when the audio process restarts.
    pub start_ticks: u64,
    /// `start_ticks` on the event clock, in nanoseconds.
    pub at_ns: u64,
}
impl ProcessStart {
    /// The boot-relative tick count converted onto the event clock. `None`
    /// when the tick rate is unusable, which leaves the caller its own
    /// fallback anchor.
    pub fn from_ticks(pid: u32, start_ticks: u64, ticks_per_second: u64) -> Option<Self> {
        if ticks_per_second == 0 {
            return None;
        }
        // A 128-bit product keeps a long boot from overflowing before the
        // division; the result of a real boot always fits in 64 bits.
        let at_ns = (u128::from(start_ticks) * 1_000_000_000 / u128::from(ticks_per_second))
            .min(u128::from(u64::MAX)) as u64;
        Some(Self {
            pid,
            start_ticks,
            at_ns,
        })
    }
    /// How long the process had been running at `now_ns` on the same clock.
    /// Saturates, so a read that lands before the start reports zero instead
    /// of wrapping.
    pub fn running_for_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.at_ns)
    }
}

/// Bind accounting once to the native process's start identity. A reused PID
/// must never relabel some other process's CPU/RSS as mixer performance.
#[derive(Clone)]
pub(crate) struct NativeProcess {
    pid: u32,
    start_ticks: Option<u64>,
    /// The start identity is read from `/proc` once, when the session is
    /// bound, and converted with it: no later meter or event path reads
    /// `/proc` again, and nothing here runs on an audio thread.
    started: Option<ProcessStart>,
}
impl NativeProcess {
    pub(crate) fn capture(pid: u32) -> Self {
        match process_resources_for(pid) {
            ProcessResources::Measured {
                start_ticks,
                ticks_per_second,
                ..
            } => Self {
                pid,
                start_ticks: Some(start_ticks),
                started: ProcessStart::from_ticks(pid, start_ticks, ticks_per_second),
            },
            // An unreadable stat leaves the caller its first-observation
            // anchor rather than inventing a start time.
            ProcessResources::Unavailable { .. } => Self {
                pid,
                start_ticks: None,
                started: None,
            },
        }
    }
    /// When this process started, on the diagnostics clock. `None` when
    /// `/proc/<pid>/stat` could not be read at capture.
    pub(crate) fn start(&self) -> Option<ProcessStart> {
        self.started
    }
    pub(crate) fn resources(&self) -> ProcessResources {
        let Some(expected) = self.start_ticks else {
            return ProcessResources::Unavailable {
                message: "Native mixer process accounting could not be bound at startup".into(),
            };
        };
        let resources = process_resources_for(self.pid);
        match resources {
            ProcessResources::Measured { start_ticks, .. } if start_ticks != expected => {
                ProcessResources::Unavailable {
                    message:
                        "Native mixer process identity changed; refusing reused PID accounting"
                            .into(),
                }
            }
            resources => resources,
        }
    }
}
fn parse_process(bytes: &[u8], pid: u32, hz: u64, page: u64) -> io::Result<ProcessResources> {
    let invalid = || io::Error::other("invalid process stat");
    let (parsed_pid, fields) = crate::proc_stat::split_stat(bytes)?;
    if parsed_pid != pid || hz == 0 || page == 0 || fields.len() < 22 {
        return Err(invalid());
    }
    let number = |index: usize| fields[index].parse::<u64>().map_err(|_| invalid());
    let cpu_ticks = number(11)?.checked_add(number(12)?).ok_or_else(invalid)?;
    let threads = number(17)?;
    let start_ticks = number(19)?;
    let virtual_bytes = number(20)?;
    let rss = fields[21].parse::<i64>().map_err(|_| invalid())?;
    if threads == 0 || start_ticks == 0 || virtual_bytes == 0 || rss < 0 {
        return Err(invalid());
    }
    let rss_bytes = (rss as u64).checked_mul(page).ok_or_else(invalid)?;
    Ok(ProcessResources::Measured {
        pid,
        start_ticks,
        cpu_ticks,
        ticks_per_second: hz,
        rss_bytes,
        virtual_bytes,
        threads,
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Counters {
    id: String,
    callbacks: u64,
    processed_frames: u64,
    suppression_frames: u64,
    missed_deadlines: u64,
    stale_results: u64,
    input_drops: u64,
    output_drops: u64,
    worker_failed: bool,
    performance: Option<Performance>,
}
struct PendingTelemetry {
    at: Stamp,
    meter: Meter,
    contexts: Vec<rc2fx_native_control::ContextStatus>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Telemetry<'a> {
    at: Stamp,
    source: &'static str,
    meter: &'a Meter,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_diagnostics: Option<&'a [rc2fx_native_control::ContextStatus]>,
}
impl From<&TargetMeter> for Counters {
    fn from(t: &TargetMeter) -> Self {
        Self {
            id: t.id.clone(),
            callbacks: t.callbacks,
            processed_frames: t.processed_frames,
            suppression_frames: t.suppression_frames,
            missed_deadlines: t.missed_deadlines,
            stale_results: t.stale_results,
            input_drops: t.input_drops,
            output_drops: t.output_drops,
            worker_failed: t.worker_failed,
            performance: t.performance.clone(),
        }
    }
}
impl Counters {
    fn all(&self) -> [u64; 47] {
        let mut v = [0; 47];
        v[..7].copy_from_slice(&[
            self.callbacks,
            self.processed_frames,
            self.suppression_frames,
            self.missed_deadlines,
            self.stale_results,
            self.input_drops,
            self.output_drops,
        ]);
        if let Some(p) = &self.performance {
            v[7] = p.xruns;
            for (index, t) in [&p.callback, &p.worker_wall, &p.worker_cpu]
                .into_iter()
                .enumerate()
            {
                let start = 8 + index * 13;
                v[start..start + 5].copy_from_slice(&[
                    t.calls,
                    t.total_ns,
                    t.max_ns,
                    t.overruns,
                    t.clock_errors,
                ]);
                v[start + 5..start + 13].copy_from_slice(&t.histogram);
            }
        }
        v
    }
    /// Per-target fault counters. Native xruns are engine-wide and are kept
    /// with their timing by `capture_native_faults`; repeating them for every
    /// target would crowd that evidence out of the bounded event history
    /// during a burst.
    fn faults(&self) -> [u64; 10] {
        let mut v = [
            self.missed_deadlines,
            self.stale_results,
            self.input_drops,
            self.output_drops,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        if let Some(p) = &self.performance {
            v[4..].copy_from_slice(&[
                p.callback.overruns,
                p.callback.clock_errors,
                p.worker_wall.overruns,
                p.worker_wall.clock_errors,
                p.worker_cpu.overruns,
                p.worker_cpu.clock_errors,
            ]);
        }
        v
    }
}

struct Record {
    bytes: Arc<[u8]>,
}
#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Retention {
    evicted_samples: u64,
    evicted_events: u64,
    rejected_records: u64,
    duplicate_meters: u64,
    native_windows_serialized: u64,
    native_serialized_bytes: u64,
    replaced_pending_native_windows: u64,
    event_serializations: u64,
    skipped_unobserved_event_serializations: u64,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum TelemetryStatus {
    Fresh,
    NoFreshWindow,
    EpochChanged,
    OversizeRejected,
}
pub(crate) struct History {
    start: Instant,
    last_sample: Option<Instant>,
    identity: Option<Identity>,
    samples: VecDeque<Record>,
    events: VecDeque<Record>,
    bytes: usize,
    event_bytes: usize,
    pending: Option<PendingTelemetry>,
    telemetry_status: TelemetryStatus,
    previous: Vec<Counters>,
    epoch: Option<(u64, u64)>,
    retention: Retention,
    native_retirements: Vec<(String, u64)>,
    native_fault_counters: Vec<(u64, u64, rc2fx_native_control::EngineCounters)>,
}
impl Default for History {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}
impl History {
    fn new(start: Instant) -> Self {
        Self {
            start,
            last_sample: None,
            identity: None,
            samples: VecDeque::new(),
            events: VecDeque::new(),
            bytes: 0,
            event_bytes: 0,
            pending: None,
            telemetry_status: TelemetryStatus::NoFreshWindow,
            previous: vec![],
            epoch: None,
            retention: Retention::default(),
            native_retirements: Vec::new(),
            native_fault_counters: Vec::new(),
        }
    }
    fn stamp(&self, now: Instant) -> Stamp {
        Stamp {
            elapsed_ms: now
                .saturating_duration_since(self.start)
                .as_millis()
                .min(u64::MAX as u128) as u64,
            unix_time_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|v| u64::try_from(v.as_millis()).ok()),
        }
    }
    pub(crate) fn state(&mut self, state: &DeviceState) {
        let next = Identity::new(state);
        if self.identity.as_ref() == Some(&next) {
            return;
        }
        if self.identity.as_ref().is_some_and(|old| {
            old.revision != next.revision || old.engine.generation != next.engine.generation
        }) {
            self.pending = None;
            self.telemetry_status = TelemetryStatus::EpochChanged;
        }
        #[derive(Serialize)]
        struct Changed<'a> {
            at: Stamp,
            kind: &'static str,
            state: &'a Identity,
        }
        let encoded = encoded(
            &Changed {
                at: self.stamp(Instant::now()),
                kind: "state",
                state: &next,
            },
            MAX_IDENTITY_BYTES,
        );
        self.identity = Some(next);
        self.push(false, encoded);
    }
    fn push(&mut self, sample: bool, record: Result<Arc<[u8]>, ExportError>) {
        let Ok(bytes) = record else {
            self.retention.rejected_records = self.retention.rejected_records.saturating_add(1);
            return;
        };
        self.bytes += bytes.len();
        if !sample {
            self.event_bytes += bytes.len();
        }
        let queue = if sample {
            &mut self.samples
        } else {
            &mut self.events
        };
        queue.push_back(Record { bytes });
        if queue.len() > if sample { MAX_SAMPLES } else { MAX_EVENTS } {
            self.evict(sample);
        }
        while self.event_bytes > MAX_EVENT_HISTORY_BYTES {
            self.evict(false);
        }
        // New samples cannot erase faults, state transitions or rejections.
        // New events displace samples until their own independent cap is hit.
        while self.bytes > MAX_HISTORY_BYTES {
            self.evict(true);
        }
    }
    fn evict(&mut self, sample: bool) {
        let (queue, count) = if sample {
            (&mut self.samples, &mut self.retention.evicted_samples)
        } else {
            (&mut self.events, &mut self.retention.evicted_events)
        };
        if let Some(record) = queue.pop_front() {
            self.bytes -= record.bytes.len();
            if !sample {
                self.event_bytes -= record.bytes.len();
            }
            *count = count.saturating_add(1);
        }
    }
    pub(crate) fn event_serialization(&mut self, observed: bool) {
        let count = if observed {
            &mut self.retention.event_serializations
        } else {
            &mut self.retention.skipped_unobserved_event_serializations
        };
        *count = count.saturating_add(1);
    }
    pub(crate) fn native_meter(
        &mut self,
        meter: Meter,
        native: Vec<rc2fx_native_control::ContextStatus>,
    ) {
        if self.identity.as_ref().is_none_or(|current| {
            current.engine.generation != meter.generation || current.revision != meter.revision
        }) {
            return;
        }
        let targets: Vec<_> = native.iter().flat_map(|context| &context.targets).collect();
        if targets.len() != meter.targets.len()
            || targets.iter().enumerate().any(|(index, target)| {
                !meter.targets.iter().any(|value| value.id == target.id)
                    || targets[..index].iter().any(|prior| prior.id == target.id)
            })
        {
            return;
        }
        self.capture_native_faults(&meter, &native);
        for target in targets {
            let previous = self
                .native_retirements
                .iter()
                .find(|(id, _)| id == &target.id)
                .map_or(0, |(_, count)| *count);
            if target.retired_worker_count != previous {
                #[derive(Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Retirement<'a> {
                    at: Stamp,
                    kind: &'static str,
                    generation: u64,
                    revision: u64,
                    target_id: &'a str,
                    retired_worker_count: u64,
                    previous_observed_count: u64,
                    retired_workers: &'a [rc2fx_native_control::RetiredWorker],
                }
                self.push(
                    false,
                    encoded(
                        &Retirement {
                            at: self.stamp(Instant::now()),
                            kind: if target.retired_worker_count > previous {
                                "native-worker-retired"
                            } else {
                                "native-worker-retirements-reset"
                            },
                            generation: meter.generation,
                            revision: meter.revision,
                            target_id: &target.id,
                            retired_worker_count: target.retired_worker_count,
                            previous_observed_count: previous,
                            retired_workers: &target.retired_workers,
                        },
                        MAX_RECORD_BYTES,
                    ),
                );
                if let Some((_, count)) = self
                    .native_retirements
                    .iter_mut()
                    .find(|(id, _)| id == &target.id)
                {
                    *count = target.retired_worker_count;
                } else {
                    self.native_retirements
                        .push((target.id.clone(), target.retired_worker_count));
                }
            }
        }
        if self.accept_meter(&meter) {
            if self.pending.is_some() {
                self.retention.replaced_pending_native_windows = self
                    .retention
                    .replaced_pending_native_windows
                    .saturating_add(1);
            }
            self.pending = Some(PendingTelemetry {
                at: self.stamp(Instant::now()),
                meter,
                contexts: native,
            });
            self.telemetry_status = TelemetryStatus::Fresh;
        }
    }
    fn capture_native_faults(
        &mut self,
        meter: &Meter,
        contexts: &[rc2fx_native_control::ContextStatus],
    ) {
        use rc2fx_native_control::{CallbackTiming, EngineCounters, NativeEvent, WorkerMeter};
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Worker<'a> {
            id: &'a str,
            runtime_epoch: u64,
            worker: &'a WorkerMeter,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Fault<'a> {
            at: Stamp,
            kind: &'static str,
            scope: &'static str,
            generation: u64,
            revision: u64,
            context: u64,
            previous_counters: Option<EngineCounters>,
            counters: EngineCounters,
            timing: &'a Option<CallbackTiming>,
            events: &'a [NativeEvent],
            workers: Vec<Worker<'a>>,
        }
        for context in contexts {
            let previous = self
                .native_fault_counters
                .iter()
                .find(|(generation, id, _)| {
                    *generation == meter.generation && *id == context.context
                })
                .map(|(_, _, counters)| *counters);
            let old = previous.unwrap_or_default();
            let now = context.counters;
            if now.xruns > old.xruns
                || now.helper_late > old.helper_late
                || now.helper_stalls > old.helper_stalls
            {
                self.push(false, encoded(&Fault {
                    at: self.stamp(Instant::now()), kind: "native-engine-fault",
                    scope: "observation time; cumulative engine counters are shared across contexts, not additive; null previousCounters means no baseline; clocks independently sampled",
                    generation: meter.generation, revision: meter.revision, context: context.context,
                    previous_counters: previous, counters: now, timing: &context.timing, events: &context.events,
                    workers: context.targets.iter().filter_map(|target| target.worker.as_ref().map(|worker| Worker {
                        id: &target.id, runtime_epoch: target.runtime_epoch, worker,
                    })).collect(),
                }, MAX_RECORD_BYTES));
            }
        }
        // Keep only the currently observed contexts, not an unbounded lifetime list.
        self.native_fault_counters = contexts
            .iter()
            .map(|c| (meter.generation, c.context, c.counters))
            .collect();
    }
    fn accept_meter(&mut self, meter: &Meter) -> bool {
        let Some(current) = &self.identity else {
            return false;
        };
        if meter.generation != current.engine.generation
            || meter.revision != current.revision
            || meter.targets.is_empty()
            || meter.targets.len() > rc2fx_core::MAX_TARGETS
        {
            return false;
        }
        let epoch = (meter.generation, meter.revision);
        // Effects revisions do not reset cumulative callbacks or necessarily
        // replace workers. Keep same-generation baselines across such saves.
        let same = self
            .epoch
            .is_some_and(|(generation, _)| generation == epoch.0);
        let next: Vec<Counters> = meter.targets.iter().map(Into::into).collect();
        // A window is new when any path advanced. Bound native paths whose
        // stock source is idle never advance, so requiring every path would
        // reject every window; a fully repeated window is still a duplicate.
        let mut fresh = false;
        for counters in &next {
            let old = self
                .previous
                .iter()
                .find(|p| p.id == counters.id)
                .filter(|_| same);
            let reset = old.is_some_and(|old| {
                counters
                    .all()
                    .iter()
                    .zip(old.all())
                    .take(
                        if old.performance.is_some() && counters.performance.is_some() {
                            47
                        } else {
                            7
                        },
                    )
                    .any(|(n, o)| *n < o)
            });
            let fault = old.map_or_else(
                || counters.worker_failed || counters.faults().iter().any(|v| *v > 0),
                |old| {
                    counters.worker_failed != old.worker_failed
                        || counters
                            .faults()
                            .iter()
                            .zip(old.faults())
                            .any(|(n, o)| *n > o)
                },
            );
            fresh |= old.is_none_or(|old| {
                reset
                    || (counters.callbacks > old.callbacks
                        && counters.processed_frames > old.processed_frames)
            });
            if reset || fault {
                #[derive(Serialize)]
                struct Fault<'a> {
                    at: Stamp,
                    kind: &'static str,
                    generation: u64,
                    revision: u64,
                    counters: &'a Counters,
                }
                self.push(
                    false,
                    encoded(
                        &Fault {
                            at: self.stamp(Instant::now()),
                            kind: if reset {
                                "counter-reset"
                            } else if old.is_none() {
                                "initial-fault-counters"
                            } else {
                                "audio-fault"
                            },
                            generation: meter.generation,
                            revision: meter.revision,
                            counters,
                        },
                        MAX_RECORD_BYTES,
                    ),
                );
            }
        }
        self.previous = next;
        self.epoch = Some(epoch);
        if !fresh {
            self.retention.duplicate_meters = self.retention.duplicate_meters.saturating_add(1);
            return false;
        }
        true
    }
    fn encoded_telemetry(&mut self, encoded: Result<Arc<[u8]>, ExportError>) -> Option<Arc<[u8]>> {
        match encoded {
            Ok(bytes) => {
                self.telemetry_status = TelemetryStatus::Fresh;
                Some(bytes)
            }
            Err(_) => {
                self.telemetry_status = TelemetryStatus::OversizeRejected;
                self.retention.rejected_records = self.retention.rejected_records.saturating_add(1);
                None
            }
        }
    }
    pub(crate) fn rejection(
        &mut self,
        generation: u64,
        revision: u64,
        operation: &str,
        expected_revision: Option<u64>,
        targets: &[String],
        reason: &str,
    ) {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Rejected<'a> {
            at: Stamp,
            kind: &'static str,
            generation: u64,
            revision: u64,
            operation: String,
            expected_revision: Option<u64>,
            target_ids: &'a [String],
            reason: String,
        }
        self.push(
            false,
            encoded(
                &Rejected {
                    at: self.stamp(Instant::now()),
                    kind: "control-rejected",
                    generation,
                    revision,
                    operation: short(operation),
                    expected_revision,
                    target_ids: &targets[..targets.len().min(rc2fx_core::MAX_TARGETS)],
                    reason: short(reason),
                },
                MAX_RECORD_BYTES,
            ),
        );
    }
    fn materialize_pending(&mut self) -> Option<Arc<[u8]>> {
        match self.pending.take() {
            Some(PendingTelemetry {
                at,
                meter,
                contexts,
            }) => {
                self.retention.native_windows_serialized =
                    self.retention.native_windows_serialized.saturating_add(1);
                let bytes = encoded(
                    &Telemetry {
                        at,
                        source: "native-mixer",
                        meter: &meter,
                        native_diagnostics: Some(&contexts),
                    },
                    MAX_NATIVE_TELEMETRY_BYTES,
                );
                if let Ok(value) = &bytes {
                    self.retention.native_serialized_bytes = self
                        .retention
                        .native_serialized_bytes
                        .saturating_add(value.len() as u64);
                }
                self.encoded_telemetry(bytes)
            }
            None => None,
        }
    }
    pub(crate) fn begin_sample(&mut self, now: Instant) -> Option<SampleTicket> {
        if self
            .last_sample
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(1))
        {
            return None;
        }
        let identity = self.identity.as_ref()?;
        let (generation, revision) = (identity.engine.generation, identity.revision);
        let telemetry = self.materialize_pending();
        let ticket = SampleTicket {
            at: self.stamp(now),
            generation,
            revision,
            interval_ms: self.last_sample.map(|last| {
                now.saturating_duration_since(last)
                    .as_millis()
                    .min(u64::MAX as u128) as u64
            }),
            telemetry,
            telemetry_status: self.telemetry_status,
        };
        self.last_sample = Some(now);
        self.telemetry_status = TelemetryStatus::NoFreshWindow;
        Some(ticket)
    }
    #[cfg(test)]
    pub(crate) fn finish_sample(
        &mut self,
        ticket: SampleTicket,
        resources: ProcessResources,
        usb: rc2fx_core::UsbStatus,
    ) {
        self.finish_sample_with_native(ticket, resources, usb, None);
    }
    pub(crate) fn finish_sample_with_native(
        &mut self,
        ticket: SampleTicket,
        resources: ProcessResources,
        usb: rc2fx_core::UsbStatus,
        native_resources: Option<ProcessResources>,
    ) {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Sample {
            at: Stamp,
            generation: u64,
            revision: u64,
            interval_ms: Option<u64>,
            resources: ProcessResources,
            #[serde(skip_serializing_if = "Option::is_none")]
            native_mixer_resources: Option<ProcessResources>,
            usb: rc2fx_core::UsbStatus,
        }
        let record = (|| {
            let mut out = Limited::new(MAX_NATIVE_TELEMETRY_BYTES + 2048);
            out.raw(b"{\"observation\":")?;
            out.json(&Sample {
                at: ticket.at,
                generation: ticket.generation,
                revision: ticket.revision,
                interval_ms: ticket.interval_ms,
                resources,
                native_mixer_resources: native_resources,
                usb,
            })?;
            out.raw(b",\"telemetryStatus\":")?;
            out.json(&ticket.telemetry_status)?;
            out.raw(b",\"telemetry\":")?;
            out.raw(ticket.telemetry.as_deref().unwrap_or(b"null"))?;
            out.raw(b"}")?;
            Ok(out.bytes.into())
        })();
        self.push(true, record);
    }
    pub(crate) fn snapshot(&self) -> Report {
        Report {
            at: self.stamp(Instant::now()),
            identity: self.identity.clone(),
            samples: self.samples.iter().map(|r| r.bytes.clone()).collect(),
            events: self.events.iter().map(|r| r.bytes.clone()).collect(),
            bytes: self.bytes,
            event_bytes: self.event_bytes,
            retention: self.retention.clone(),
        }
    }
}
pub(crate) struct SampleTicket {
    at: Stamp,
    generation: u64,
    revision: u64,
    interval_ms: Option<u64>,
    telemetry: Option<Arc<[u8]>>,
    telemetry_status: TelemetryStatus,
}
pub(crate) struct Report {
    at: Stamp,
    identity: Option<Identity>,
    samples: Vec<Arc<[u8]>>,
    events: Vec<Arc<[u8]>>,
    bytes: usize,
    event_bytes: usize,
    retention: Retention,
}
impl Report {
    pub(crate) fn encode(self) -> Result<Vec<u8>, ExportError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Header<'a> {
            version: u32,
            scope: &'static str,
            captured: Stamp,
            state: &'a Option<Identity>,
            history_bytes: usize,
            sample_history_bytes: usize,
            event_history_bytes: usize,
            maximum_history_bytes: usize,
            maximum_event_history_bytes: usize,
            maximum_response_bytes: usize,
            retention: &'a Retention,
            notes: &'static str,
        }
        let mut out = Limited::new(MAX_RESPONSE_BYTES);
        out.raw(b"{\"report\":")?;
        out.json(&Header {
            version: 1,
            scope: "rc2fx-controller",
            captured: self.at,
            state: &self.identity,
            history_bytes: self.bytes,
            sample_history_bytes: self.bytes - self.event_bytes,
            event_history_bytes: self.event_bytes,
            maximum_history_bytes: MAX_HISTORY_BYTES,
            maximum_event_history_bytes: MAX_EVENT_HISTORY_BYTES,
            maximum_response_bytes: MAX_RESPONSE_BYTES,
            retention: &self.retention,
            notes: NOTES,
        })?;
        for (name, records) in [
            (b",\"samples\":".as_slice(), self.samples),
            (b",\"events\":".as_slice(), self.events),
        ] {
            out.raw(name)?;
            out.raw(b"[")?;
            for (index, record) in records.iter().enumerate() {
                if index != 0 {
                    out.raw(b",")?;
                }
                out.raw(record)?;
            }
            out.raw(b"]")?;
        }
        out.raw(b"}")?;
        Ok(out.bytes)
    }
}

#[cfg(test)]
mod tests;
