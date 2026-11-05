//! Per-path immutable descriptors; all runtime construction/retirement is control work.
use super::suppression::{Admission, Claim, Runtime};
use rc2fx_core::Target;
use rc2fx_dsp::Prepared;
use rc2fx_native_control::{PathMode, RetiredWorker};
use std::{
    ptr::NonNull,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicPtr, AtomicU64, Ordering},
    },
};

struct Snapshot {
    revision: u64,
    prepared: Prepared,
    fingerprint: [u8; 32],
    mode: PathMode,
    runtime: Option<Arc<Runtime>>,
    // Desired suppression on a passthrough slot still owns its fixed mic claim.
    _claim: Option<Claim>,
}
struct OwnedSnapshot(NonNull<Snapshot>);
impl OwnedSnapshot {
    fn new(
        revision: u64,
        prepared: Prepared,
        fingerprint: [u8; 32],
        mode: PathMode,
        runtime: Option<Arc<Runtime>>,
        claim: Option<Claim>,
    ) -> Self {
        Self(NonNull::from(Box::leak(Box::new(Snapshot {
            revision,
            prepared,
            fingerprint,
            mode,
            runtime,
            _claim: claim,
        }))))
    }
    fn get(&self) -> &Snapshot {
        unsafe { self.0.as_ref() }
    }
    fn pointer(&self) -> *mut Snapshot {
        self.0.as_ptr()
    }
}
// Control alone owns/retires immutable descriptors after the sole reader fence.
unsafe impl Send for OwnedSnapshot where Snapshot: Send + Sync {}
impl Drop for OwnedSnapshot {
    fn drop(&mut self) {
        unsafe { drop(Box::from_raw(self.pointer())) };
    }
}
struct Retired {
    snapshot: OwnedSnapshot,
    release_after: Option<u64>,
}
struct Versions {
    current: OwnedSnapshot,
    retired: Option<Retired>,
    pending: Option<u64>,
    latest_worker: Option<RetiredWorker>,
    retired_worker_count: u64,
}
struct Shared {
    versions: Mutex<Versions>,
    published: AtomicPtr<Snapshot>,
    seen: AtomicU64,
    applied: AtomicU64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    WrongTarget,
    Busy,
    SuppressionInUse,
    StaleRevision,
    RequiresReset,
    Invalid,
    Poisoned,
    Runtime,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Status {
    pub published: u64,
    pub seen: u64,
    pub applied: u64,
    pub fingerprint: [u8; 32],
    pub meter: Option<rc2fx_native_control::AudioMeter>,
    pub mode: PathMode,
    pub runtime_epoch: u64,
}
pub struct Control {
    id: String,
    shared: Arc<Shared>,
    next_ticket: u64,
    next_epoch: u64,
    telemetry: super::telemetry::Consumer,
    options: rc2fx_suppression::WorkerOptions,
    admission: Admission,
}
pub struct Staged {
    shared: Arc<Shared>,
    expected_revision: u64,
    ticket: u64,
    snapshot: Option<OwnedSnapshot>,
}
fn same_runtime(a: &Snapshot, b: &Snapshot) -> bool {
    match (&a.runtime, &b.runtime) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}
fn worker_meter(worker: &super::suppression::Record) -> rc2fx_native_control::WorkerMeter {
    let mut meter = worker_only_meter(worker);
    let archived = archived_timings();
    meter.wall = merge_timing(meter.wall, archived[0]);
    meter.cpu = merge_timing(meter.cpu, archived[1]);
    meter.turnaround = merge_timing(meter.turnaround, archived[2]);
    meter
}
/// A retirement receipt and its archive contribution must exclude all prior
/// workers. Feeding the cumulative display meter back into its archive doubles
/// the prior history at every runtime replacement.
fn worker_only_meter(worker: &super::suppression::Record) -> rc2fx_native_control::WorkerMeter {
    let value = worker.telemetry().snapshot();
    let geometry = worker.geometry();
    rc2fx_native_control::WorkerMeter {
        completed_frames: value.completed_frames,
        missed_deadlines: value.missed_deadlines,
        stale_results: value.stale_results,
        input_drops: value.input_drops,
        output_drops: value.output_drops,
        failed: value.failed,
        frame_budget_ns: geometry.frame_budget_ns(),
        worker_allowance_ns: geometry.worker_allowance_ns(),
        allowance_overruns: value.allowance_overruns,
        wall: worker_timing(value.wall),
        cpu: worker_timing(value.cpu),
        queue_wait: worker_timing(value.queue_wait),
        turnaround: worker_timing(value.turnaround),
    }
}
fn outliers_json(value: rc2fx_suppression::Outliers) -> serde_json::Value {
    use rc2fx_suppression::Recorded;
    let frame = |record: Recorded<rc2fx_suppression::FrameRecord>| match record {
        Recorded::Empty => serde_json::json!({"state": "empty"}),
        Recorded::Busy => serde_json::json!({"state": "busy"}),
        Recorded::Stable(f) => serde_json::json!({
            "state": "stable",
            "sequence": f.sequence,
            "submittedNs": f.submitted_ns,
            "startedNs": f.started_ns,
            "finishedNs": f.finished_ns,
            "publishedNs": f.published_ns,
            "cpuNs": f.cpu_ns,
            "wallNs": f.wall_ns,
            "queueWaitNs": f.queue_wait_ns,
            "turnaroundNs": f.turnaround_ns,
        }),
    };
    let event = |record: Recorded<rc2fx_suppression::EventRecord>| match record {
        Recorded::Empty => serde_json::json!({"state": "empty"}),
        Recorded::Busy => serde_json::json!({"state": "busy"}),
        Recorded::Stable(e) => serde_json::json!({
            "state": "stable",
            "sequence": e.sequence,
            "atNs": e.at_ns,
        }),
    };
    // An atNs of 0 is a failed clock read: null, never a time.
    let history = |history: &rc2fx_suppression::EventHistory| {
        serde_json::json!({
            "capacity": history.capacity,
            "total": history.total,
            "recorded": history.events.len(),
            "lost": history.lost,
            "unpublished": history.unpublished,
            "complete": history.complete(),
            "events": history.events.iter().map(|e| serde_json::json!({
                "sequence": e.sequence,
                "atNs": (e.at_ns != 0).then_some(e.at_ns),
            })).collect::<Vec<_>>(),
        })
    };
    serde_json::json!({
        "worst": frame(value.worst),
        "latestSlow": frame(value.latest_slow),
        "slowFrames": value.slow_frames,
        // Cumulative per worker: frames whose submission-to-publication
        // turnaround exceeded 5.5, 6.0, 6.5 and 7.0 ms.
        "turnaroundsAboveMs": serde_json::json!({
            "5.5": value.above[0],
            "6.0": value.above[1],
            "6.5": value.above[2],
            "7.0": value.above[3],
        }),
        "lastMiss": event(value.last_miss),
        "lastLate": event(value.last_late),
        "missHistory": history(&value.miss_history),
        "lateHistory": history(&value.late_history),
    })
}
const ZERO_TIMING: rc2fx_native_control::Timing = rc2fx_native_control::Timing {
    calls: 0,
    total_ns: 0,
    max_ns: 0,
    overruns: 0,
    clock_errors: 0,
    histogram: [0; 8],
};

/// Every retired worker's wall, CPU and turnaround, added up. A worker's own
/// counters start at zero, so without this the app's view would jump back on
/// every pipeline rebuild (and its fault counters would treat that as a new
/// baseline rather than a fault, but the totals would be useless). Each worker
/// is archived exactly once, so nothing is counted twice; the live worker is
/// added on top when a meter is built.
static RETIRED_TIMINGS: Mutex<[rc2fx_native_control::Timing; 3]> = Mutex::new([ZERO_TIMING; 3]);

fn merge_into(slot: &mut rc2fx_native_control::Timing, add: rc2fx_native_control::Timing) {
    *slot = merge_timing(*slot, add);
}

/// Two control timings added: calls, totals, overruns and clock errors sum,
/// maxima take the larger, histograms add bin by bin.
fn merge_timing(
    a: rc2fx_native_control::Timing,
    b: rc2fx_native_control::Timing,
) -> rc2fx_native_control::Timing {
    let mut histogram = a.histogram;
    for (slot, add) in histogram.iter_mut().zip(b.histogram) {
        *slot = slot.saturating_add(add);
    }
    rc2fx_native_control::Timing {
        calls: a.calls.saturating_add(b.calls),
        total_ns: a.total_ns.saturating_add(b.total_ns),
        max_ns: a.max_ns.max(b.max_ns),
        overruns: a.overruns.saturating_add(b.overruns),
        clock_errors: a.clock_errors.saturating_add(b.clock_errors),
        histogram,
    }
}

fn archived_timings() -> [rc2fx_native_control::Timing; 3] {
    RETIRED_TIMINGS
        .lock()
        .map(|value| *value)
        .unwrap_or([ZERO_TIMING; 3])
}

fn archive(versions: &mut Versions, snapshot: &Snapshot) {
    if let Some(runtime) = &snapshot.runtime
        && let Some(worker) = runtime.worker()
    {
        let terminal = worker
            .telemetry()
            .terminal()
            .expect("joined runtime terminal receipt");
        let meter = worker_only_meter(worker);
        versions.retired_worker_count = versions.retired_worker_count.saturating_add(1);
        versions.latest_worker = Some(RetiredWorker {
            runtime_epoch: runtime.epoch(),
            worker: meter,
            joined: terminal.joined,
            drained: terminal.drained,
        });
        if let Ok(mut totals) = RETIRED_TIMINGS.lock() {
            merge_into(&mut totals[0], meter.wall);
            merge_into(&mut totals[1], meter.cpu);
            merge_into(&mut totals[2], meter.turnaround);
        }
    }
}
fn reap(versions: &mut Versions, seen: u64) -> bool {
    let Some(retired) = &versions.retired else {
        return true;
    };
    if retired
        .release_after
        .is_some_and(|revision| revision != seen)
    {
        return false;
    }
    let distinct = !same_runtime(retired.snapshot.get(), versions.current.get());
    if distinct
        && retired
            .snapshot
            .get()
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.retire())
    {
        return false;
    }
    let retired = versions.retired.take().unwrap();
    if distinct {
        archive(versions, retired.snapshot.get());
    }
    // Final Arc/DSP/provider destruction occurs here, on control after the fence.
    drop(retired);
    true
}
fn cancel(versions: &mut Versions, snapshot: OwnedSnapshot) {
    if !same_runtime(snapshot.get(), versions.current.get()) {
        if snapshot
            .get()
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.retire())
        {
            assert!(
                versions.retired.is_none(),
                "one bounded candidate/retirement slot"
            );
            versions.retired = Some(Retired {
                snapshot,
                release_after: None,
            });
            return;
        }
        archive(versions, snapshot.get());
    }
    drop(snapshot);
}
impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            let Ok(mut versions) = self.shared.versions.lock() else {
                std::process::abort()
            };
            if versions.pending == Some(self.ticket) {
                versions.pending = None;
            }
            cancel(&mut versions, snapshot);
        }
    }
}
pub struct Reservation<'a> {
    versions: MutexGuard<'a, Versions>,
    published: &'a AtomicPtr<Snapshot>,
    ticket: u64,
    next: Option<OwnedSnapshot>,
}
impl Reservation<'_> {
    pub fn commit(mut self) {
        let next = self.next.take().expect("prepared native reservation");
        let revision = next.get().revision;
        let previous = std::mem::replace(&mut self.versions.current, next);
        self.versions.retired = Some(Retired {
            snapshot: previous,
            release_after: Some(revision),
        });
        self.versions.pending = None;
        self.published
            .store(self.versions.current.pointer(), Ordering::Release);
    }
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.versions.pending == Some(self.ticket) {
            self.versions.pending = None;
        }
        if let Some(snapshot) = self.next.take() {
            cancel(&mut self.versions, snapshot);
        }
    }
}
pub struct Reader {
    shared: Arc<Shared>,
    current: *mut Snapshot,
    telemetry: super::telemetry::Producer,
}
// Exactly one Reader per bound path; callback state never enters control.
unsafe impl Send for Reader {}
pub fn channel_with_options(
    target: &Target,
    prepared: Prepared,
    options: rc2fx_suppression::WorkerOptions,
    admission: Admission,
    mode: PathMode,
) -> Result<(Control, Reader), Error> {
    validate_shape(target)?;
    let claim = if target.suppression.enabled {
        Some(admission.claim(&target.id)?)
    } else {
        None
    };
    let runtime = if mode == PathMode::Processed {
        Some(Runtime::new(target, &prepared, options, 1, &admission)?)
    } else {
        None
    };
    let current = OwnedSnapshot::new(
        1,
        prepared,
        rc2fx_native_control::fingerprint(target).map_err(|_| Error::Invalid)?,
        mode,
        runtime,
        claim,
    );
    let (producer, consumer) = super::telemetry::channel();
    let shared = Arc::new(Shared {
        published: AtomicPtr::new(current.pointer()),
        versions: Mutex::new(Versions {
            current,
            retired: None,
            pending: None,
            latest_worker: None,
            retired_worker_count: 0,
        }),
        seen: AtomicU64::new(0),
        applied: AtomicU64::new(0),
    });
    Ok((
        Control {
            id: target.id.clone(),
            shared: Arc::clone(&shared),
            next_ticket: 1,
            next_epoch: 2,
            telemetry: consumer,
            options,
            admission,
        },
        Reader {
            shared,
            current: std::ptr::null_mut(),
            telemetry: producer,
        },
    ))
}
#[cfg(test)]
pub fn channel(target: &Target, prepared: Prepared) -> (Control, Reader) {
    channel_with_options(
        target,
        prepared,
        rc2fx_suppression::WorkerOptions::default(),
        Admission::default(),
        PathMode::Processed,
    )
    .unwrap()
}
fn validate_shape(target: &Target) -> Result<(), Error> {
    target.validate().map_err(|_| Error::Invalid)?;
    if target.suppression.enabled && !target.supports_suppression() {
        return Err(Error::Invalid);
    }
    if target.suppression.enabled
        && target.is_chat_output()
        && !cfg!(test)
        && !super::chat_mix::installed()
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
impl Control {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn prepare_mode(
        &mut self,
        target: &Target,
        revision: u64,
        mode: PathMode,
    ) -> Result<Staged, Error> {
        if target.id != self.id {
            return Err(Error::WrongTarget);
        }
        validate_shape(target)?;
        let mut versions = self.shared.versions.lock().map_err(|_| Error::Poisoned)?;
        if revision <= versions.current.get().revision {
            return Err(Error::StaleRevision);
        }
        if versions.pending.is_some() || self.next_ticket == u64::MAX || self.next_epoch == u64::MAX
        {
            return Err(Error::Busy);
        }
        if !reap(&mut versions, self.shared.seen.load(Ordering::Acquire)) {
            return Err(Error::Busy);
        }
        let prepared = Prepared::new(target, 48_000).map_err(|_| Error::Invalid)?;
        if prepared.channels() != versions.current.get().prepared.channels()
            || prepared.kind() != versions.current.get().prepared.kind()
        {
            return Err(Error::RequiresReset);
        }
        let claim = if target.suppression.enabled {
            Some(self.admission.claim(&target.id)?)
        } else {
            None
        };
        let runtime = if mode == PathMode::Passthrough {
            None
        } else if versions.current.get().mode == mode
            && !versions.current.get().prepared.requires_reset(&prepared)
        {
            versions.current.get().runtime.clone()
        } else {
            let runtime = Runtime::new(
                target,
                &prepared,
                self.options,
                self.next_epoch,
                &self.admission,
            )?;
            self.next_epoch += 1;
            Some(runtime)
        };
        let next = OwnedSnapshot::new(
            revision,
            prepared,
            rc2fx_native_control::fingerprint(target).map_err(|_| Error::Invalid)?,
            mode,
            runtime,
            claim,
        );
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        versions.pending = Some(ticket);
        Ok(Staged {
            shared: Arc::clone(&self.shared),
            expected_revision: versions.current.get().revision,
            ticket,
            snapshot: Some(next),
        })
    }
    #[cfg(test)]
    pub fn prepare(&mut self, target: &Target, revision: u64) -> Result<Staged, Error> {
        self.prepare_mode(target, revision, PathMode::Processed)
    }
    pub fn reserve(&mut self, mut staged: Staged) -> Result<Reservation<'_>, Error> {
        if !Arc::ptr_eq(&self.shared, &staged.shared) {
            return Err(Error::WrongTarget);
        }
        let versions = self.shared.versions.lock().map_err(|_| Error::Poisoned)?;
        let error = if versions.pending != Some(staged.ticket)
            || versions.current.get().revision != staged.expected_revision
        {
            Some(Error::StaleRevision)
        } else if versions.retired.is_some() {
            Some(Error::Busy)
        } else {
            None
        };
        if let Some(error) = error {
            drop(versions);
            return Err(error);
        }
        Ok(Reservation {
            versions,
            published: &self.shared.published,
            ticket: staged.ticket,
            next: staged.snapshot.take(),
        })
    }
    #[cfg(test)]
    pub fn publish(&mut self, target: &Target, revision: u64) -> Result<(), Error> {
        let staged = self.prepare(target, revision)?;
        self.reserve(staged)?.commit();
        Ok(())
    }
    pub fn worker_status(&self) -> Option<rc2fx_native_control::WorkerMeter> {
        self.shared
            .versions
            .lock()
            .ok()?
            .current
            .get()
            .runtime
            .as_ref()?
            .worker()
            .map(worker_meter)
    }
    /// Clone the current worker handle while holding the version lock briefly;
    /// capture snapshots and all file I/O happen after the lock is released.
    pub fn worker_telemetry(&self) -> Result<Option<(u64, rc2fx_suppression::Telemetry)>, Error> {
        let versions = self.shared.versions.try_lock().map_err(|_| Error::Busy)?;
        Ok(versions.current.get().runtime.as_ref().and_then(|runtime| {
            runtime
                .worker()
                .map(|worker| (runtime.epoch(), worker.telemetry()))
        }))
    }
    /// The current worker's outlier records (`rc2fx_suppression::Outliers`),
    /// for the native report; times are CLOCK_MONOTONIC ns, as `monotonicNs`.
    pub fn worker_outliers(&self) -> Option<serde_json::Value> {
        let versions = self.shared.versions.lock().ok()?;
        let worker = versions.current.get().runtime.as_ref()?.worker()?;
        Some(outliers_json(worker.telemetry().outliers()))
    }
    /// The current worker's (missed-deadline, late-result) histories, typed
    /// for the control API. Both come from one outliers snapshot under one
    /// version lock, so they always belong to the same worker. None without
    /// a worker; the histories retain their existing per-ring read semantics.
    pub fn worker_histories(
        &self,
    ) -> Option<(
        rc2fx_native_control::NativeHistory,
        rc2fx_native_control::NativeHistory,
    )> {
        let versions = self.shared.versions.lock().ok()?;
        let worker = versions.current.get().runtime.as_ref()?.worker()?;
        Some(native_histories(worker.telemetry().outliers()))
    }
    pub fn retired_workers(&self) -> Vec<RetiredWorker> {
        self.shared
            .versions
            .lock()
            .ok()
            .and_then(|v| v.latest_worker.clone())
            .into_iter()
            .collect()
    }
    pub fn retired_worker_count(&self) -> u64 {
        self.shared
            .versions
            .lock()
            .map_or(0, |v| v.retired_worker_count)
    }
    pub fn status(&mut self) -> Result<Status, Error> {
        let mut versions = self.shared.versions.lock().map_err(|_| Error::Poisoned)?;
        reap(&mut versions, self.shared.seen.load(Ordering::Acquire));
        let current = versions.current.get();
        Ok(Status {
            published: current.revision,
            seen: self.shared.seen.load(Ordering::Acquire),
            applied: self.shared.applied.load(Ordering::Acquire),
            fingerprint: current.fingerprint,
            meter: self.telemetry.latest(),
            mode: current.mode,
            runtime_epoch: current
                .runtime
                .as_ref()
                .map_or(0, |runtime| runtime.epoch()),
        })
    }
}
fn native_histories(
    outliers: rc2fx_suppression::Outliers,
) -> (
    rc2fx_native_control::NativeHistory,
    rc2fx_native_control::NativeHistory,
) {
    let convert = |history: rc2fx_suppression::EventHistory| rc2fx_native_control::NativeHistory {
        total: history.total,
        lost: history.lost,
        events: history
            .events
            .into_iter()
            .map(|event| rc2fx_native_control::NativeEventRecord {
                sequence: event.sequence,
                at_ns: event.at_ns,
            })
            .collect(),
    };
    // Keep the wire mapping unchanged: unpublished records remain accounted
    // for by total, not added to lost; zero timestamps remain zero.
    (
        convert(outliers.miss_history),
        convert(outliers.late_history),
    )
}

pub(crate) fn worker_timing(value: rc2fx_suppression::Timing) -> rc2fx_native_control::Timing {
    rc2fx_native_control::Timing {
        calls: value.calls,
        total_ns: value.total_ns,
        max_ns: value.max_ns,
        overruns: value.overruns,
        clock_errors: value.clock_errors,
        histogram: value.bins,
    }
}
impl Reader {
    fn descriptor(&self) -> Option<&Snapshot> {
        unsafe { self.current.as_ref() }
    }
    pub fn mode(&self) -> Option<PathMode> {
        self.descriptor().map(|s| s.mode)
    }
    pub fn input(
        &self,
        input: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        let s = self
            .descriptor()
            .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?;
        unsafe {
            s.runtime
                .as_ref()
                .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?
                .input(&s.prepared, input, output)
        }
    }
    pub fn output(
        &self,
        buffers: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        let s = self
            .descriptor()
            .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?;
        unsafe {
            s.runtime
                .as_ref()
                .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?
                .output(&s.prepared, buffers)
        }
    }
    pub fn needs_chat_stem(&self) -> bool {
        self.descriptor()
            .is_some_and(|s| s.mode == PathMode::Processed && s.prepared.is_chat_voice())
    }
    pub fn chat_output(
        &self,
        voice: [&[f32]; 2],
        buffers: [&mut [f32]; 2],
    ) -> Result<rc2fx_dsp::Meter, rc2fx_dsp::DspError> {
        let s = self
            .descriptor()
            .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?;
        unsafe {
            s.runtime
                .as_ref()
                .ok_or(rc2fx_dsp::DspError::IncompatibleSnapshot)?
                .chat_output(&s.prepared, voice, buffers)
        }
    }
    pub fn latency_samples(&self) -> usize {
        self.descriptor()
            .and_then(|s| s.runtime.as_ref())
            .map_or(0, |r| unsafe { r.latency() })
    }
    pub fn record(&mut self, frames: usize, meter: rc2fx_dsp::Meter, latency: u32) {
        if let Some(snapshot) = unsafe { self.current.as_ref() } {
            self.telemetry
                .record(snapshot.revision, frames, meter, latency);
        }
    }
    pub fn begin(&mut self) -> bool {
        if !self.current.is_null() {
            return false;
        }
        self.current = self.shared.published.load(Ordering::Acquire);
        true
    }
    #[cfg(test)]
    pub fn prepared(&self) -> Option<&Prepared> {
        self.descriptor().map(|s| &s.prepared)
    }
    pub fn finish(&mut self, applied: bool) {
        let Some(snapshot) = (unsafe { self.current.as_ref() }) else {
            return;
        };
        let revision = snapshot.revision;
        self.current = std::ptr::null_mut();
        if applied {
            self.shared.applied.store(revision, Ordering::Release);
        }
        self.shared.seen.store(revision, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::TargetKind;

    fn target() -> Target {
        Target::new("headphones", "Headphones", TargetKind::Output, 2)
    }
    /// Arithmetic for disjoint worker totals. The live lookahead-switch test
    /// covers retirement at the real call site, including archive feedback.
    #[test]
    fn timing_merge_sums_disjoint_workers() {
        let first = rc2fx_native_control::Timing {
            calls: 10,
            total_ns: 1_000,
            max_ns: 200,
            overruns: 2,
            clock_errors: 1,
            histogram: [1, 0, 0, 0, 0, 0, 0, 0],
        };
        let second = rc2fx_native_control::Timing {
            calls: 5,
            total_ns: 400,
            max_ns: 300,
            overruns: 1,
            clock_errors: 0,
            histogram: [0, 2, 0, 0, 0, 0, 0, 0],
        };
        let once = merge_timing(first, second);
        assert_eq!(
            (
                once.calls,
                once.total_ns,
                once.max_ns,
                once.overruns,
                once.clock_errors,
                once.histogram,
            ),
            (15, 1_400, 300, 3, 1, [1, 2, 0, 0, 0, 0, 0, 0])
        );
        // A worker that has not run contributes nothing.
        assert_eq!(merge_timing(once, ZERO_TIMING), once);
    }

    #[test]
    fn paired_worker_histories_preserve_empty_and_absent_wire_fields() {
        let mut target = Target::new("source", "Source", TargetKind::Mic, 1);
        let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        assert_eq!(control.worker_histories(), None);
        let status = crate::target_status(&mut control).unwrap();
        assert_eq!((status.misses, status.lates), (None, None));

        target.suppression.enabled = true;
        let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        let empty = rc2fx_native_control::NativeHistory::default();
        assert_eq!(
            control.worker_histories(),
            Some((empty.clone(), empty.clone()))
        );
        let status = crate::target_status(&mut control).unwrap();
        assert_eq!(
            (status.misses, status.lates),
            (Some(empty.clone()), Some(empty))
        );
        assert_eq!(status.runtime_epoch, 1);
    }

    #[test]
    fn paired_histories_preserve_both_rings_loss_unpublished_and_clock_failures() {
        use rc2fx_native_control::{NativeEventRecord, NativeHistory};
        use rc2fx_suppression::{EventHistory, EventRecord, Outliers, Recorded};
        let outliers = Outliers {
            worst: Recorded::Empty,
            latest_slow: Recorded::Empty,
            slow_frames: 0,
            above: [0; 4],
            last_miss: Recorded::Empty,
            last_late: Recorded::Empty,
            miss_history: EventHistory {
                total: 8,
                capacity: 2,
                events: vec![
                    EventRecord {
                        sequence: 1,
                        at_ns: 123,
                    },
                    EventRecord {
                        sequence: 2,
                        at_ns: 0,
                    },
                ],
                lost: 5,
                unpublished: 1,
            },
            late_history: EventHistory {
                total: 4,
                capacity: 2,
                events: vec![
                    EventRecord {
                        sequence: 7,
                        at_ns: 456,
                    },
                    EventRecord {
                        sequence: 11,
                        at_ns: 789,
                    },
                ],
                lost: 0,
                unpublished: 2,
            },
        };
        assert_eq!(
            native_histories(outliers),
            (
                NativeHistory {
                    total: 8,
                    lost: 5,
                    events: vec![
                        NativeEventRecord {
                            sequence: 1,
                            at_ns: 123
                        },
                        NativeEventRecord {
                            sequence: 2,
                            at_ns: 0
                        },
                    ],
                },
                NativeHistory {
                    total: 4,
                    lost: 0,
                    events: vec![
                        NativeEventRecord {
                            sequence: 7,
                            at_ns: 456
                        },
                        NativeEventRecord {
                            sequence: 11,
                            at_ns: 789
                        },
                    ],
                },
            )
        );
    }

    #[test]
    fn deadline_histories_report_their_accounting_and_no_invented_times() {
        use rc2fx_suppression::{EventHistory, EventRecord, Outliers, Recorded};
        let events: Vec<_> = (0..64)
            .map(|sequence| EventRecord {
                sequence,
                at_ns: if sequence == 2 { 0 } else { 100 + sequence },
            })
            .collect();
        let outliers = Outliers {
            worst: Recorded::Empty,
            latest_slow: Recorded::Empty,
            slow_frames: 0,
            above: [3, 2, 1, 0],
            last_miss: Recorded::Stable(EventRecord {
                sequence: 69,
                at_ns: 500,
            }),
            last_late: Recorded::Empty,
            miss_history: EventHistory {
                total: 70,
                capacity: 64,
                events,
                lost: 6,
                unpublished: 0,
            },
            late_history: EventHistory {
                capacity: 64,
                ..Default::default()
            },
        };
        let json = outliers_json(outliers);
        let misses = &json["missHistory"];
        for key in [
            "capacity",
            "total",
            "recorded",
            "lost",
            "unpublished",
            "complete",
            "events",
        ] {
            assert!(misses.get(key).is_some(), "missing {key}");
        }
        assert_eq!(
            (misses["total"].as_u64(), misses["recorded"].as_u64()),
            (Some(70), Some(64))
        );
        assert_eq!(misses["lost"], 6);
        assert_eq!(misses["complete"], false);
        assert_eq!(
            misses["events"][0],
            serde_json::json!({"sequence": 0, "atNs": 100})
        );
        // A failed clock read is null, not epoch 0.
        assert_eq!(
            misses["events"][2],
            serde_json::json!({"sequence": 2, "atNs": null})
        );
        let lates = &json["lateHistory"];
        assert_eq!(
            (lates["total"].as_u64(), lates["recorded"].as_u64()),
            (Some(0), Some(0))
        );
        assert_eq!(lates["complete"], true);
        assert_eq!(lates["events"], serde_json::json!([]));
        // The latest records are unchanged alongside.
        assert_eq!(json["lastMiss"]["sequence"], 69);
        assert_eq!(json["lastLate"]["state"], "empty");
    }
    #[test]
    fn compatible_updates_keep_an_enabled_classic_provider_signature() {
        let mut target = Target::new("source", "Source", TargetKind::Mic, 1);
        target.suppression.enabled = true;
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        target.eq.input_gain_db = -6.0;
        let staged = control.prepare(&target, 2).unwrap();
        control.reserve(staged).unwrap().commit();
        assert!(reader.begin());
        reader.finish(true);
        assert_eq!(control.status().unwrap().applied, 2);
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        target.suppression.enabled = false;
        let staged = control.prepare(&target, 3).unwrap();
        control.reserve(staged).unwrap().commit();
        assert_eq!(control.status().unwrap().runtime_epoch, 2);
        assert!(control.worker_status().is_none());
        assert!(reader.begin());
        reader.finish(true);
        control.status().unwrap();
        assert_eq!(control.retired_worker_count(), 1);
        let retired = control.retired_workers();
        assert_eq!(retired[0].runtime_epoch, 1);
        assert!(retired[0].joined && retired[0].drained);
    }
    #[test]
    fn old_frame_remains_valid_and_updates_backpressure_until_new_frame_finishes() {
        let target = target();
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        assert!(reader.begin());
        control.publish(&target, 2).unwrap();
        assert_eq!(reader.prepared().unwrap().channels(), 2);
        assert_eq!(control.publish(&target, 3), Err(Error::Busy));
        reader.finish(true);
        assert!(reader.prepared().is_none());
        assert_eq!(control.status().unwrap().applied, 1);
        assert_eq!(control.publish(&target, 3), Err(Error::Busy));
        assert!(reader.begin());
        reader.finish(true);
        control.publish(&target, 3).unwrap();
        assert_eq!(control.status().unwrap().published, 3);
        assert_eq!(control.status().unwrap().applied, 2);
    }
    #[test]
    fn reader_retains_storage_after_control_is_dropped() {
        let target = target();
        let (control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        assert!(reader.begin());
        drop(control);
        assert_eq!(reader.prepared().unwrap().channels(), 2);
        reader.finish(true);
        assert!(reader.begin());
        reader.finish(true);
    }
    #[test]
    fn rejected_updates_leave_current_revision_unchanged() {
        let mut target = target();
        let (mut control, _reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        assert_eq!(control.publish(&target, 1), Err(Error::StaleRevision));
        target.channels = 1;
        assert_eq!(control.publish(&target, 2), Err(Error::RequiresReset));
        target.id = "another-path".into();
        assert_eq!(control.publish(&target, 2), Err(Error::WrongTarget));
        assert_eq!(control.status().unwrap().published, 1);
    }
    #[test]
    fn cancelling_staged_settings_keeps_audio_live_and_bounds_candidate_storage() {
        let target = target();
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        let staged = control.prepare(&target, 2).unwrap();
        assert_eq!(control.status().unwrap().published, 1);
        assert!(matches!(control.prepare(&target, 3), Err(Error::Busy)));
        assert!(reader.begin());
        reader.finish(true);
        assert_eq!(control.status().unwrap().applied, 1);
        drop(staged);
        let staged = control.prepare(&target, 3).unwrap();
        drop(staged);
        assert_eq!(control.status().unwrap().published, 1);
    }
    #[test]
    fn commit_loan_does_not_block_reader_and_drop_aborts_publication() {
        let target = target();
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        let staged = control.prepare(&target, 2).unwrap();
        let reservation = control.reserve(staged).unwrap();
        assert!(reader.begin());
        assert_eq!(reader.prepared().unwrap().channels(), 2);
        reader.finish(true);
        drop(reservation);
        assert_eq!(control.status().unwrap().published, 1);
        assert_eq!(control.status().unwrap().applied, 1);
        let staged = control.prepare(&target, 3).unwrap();
        control.reserve(staged).unwrap().commit();
        assert_eq!(control.status().unwrap().published, 3);
        assert!(reader.begin());
        reader.finish(true);
        assert_eq!(control.status().unwrap().applied, 3);
    }
    #[test]
    fn replacement_endpoint_refuses_old_staging_and_releases_its_original_slot() {
        let target = target();
        let prepared = Prepared::new(&target, 48_000).unwrap();
        let (mut original, _original_reader) = channel(&target, prepared.clone());
        let (mut replacement, _replacement_reader) = channel(&target, prepared);
        let staged = original.prepare(&target, 2).unwrap();
        assert!(matches!(
            replacement.reserve(staged),
            Err(Error::WrongTarget)
        ));
        assert_eq!(replacement.status().unwrap().published, 1);
        assert!(original.prepare(&target, 3).is_ok());
    }

    #[test]
    fn mode_descriptor_is_fixed_for_the_frame_and_retirement_waits_for_newer_release() {
        let target = target();
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        let old = Arc::downgrade(
            control
                .shared
                .versions
                .lock()
                .unwrap()
                .current
                .get()
                .runtime
                .as_ref()
                .unwrap(),
        );
        assert!(reader.begin());
        let staged = control
            .prepare_mode(&target, 2, PathMode::Passthrough)
            .unwrap();
        control.reserve(staged).unwrap().commit();
        assert_eq!(reader.mode(), Some(PathMode::Processed));
        assert_eq!(control.status().unwrap().runtime_epoch, 0);
        assert!(old.upgrade().is_some());
        reader.finish(true);
        control.status().unwrap();
        assert!(
            old.upgrade().is_some(),
            "old frame release is not the newer descriptor fence"
        );
        assert!(reader.begin());
        assert_eq!(reader.mode(), Some(PathMode::Passthrough));
        assert_eq!(reader.latency_samples(), 0);
        reader.finish(true);
        control.status().unwrap();
        assert!(old.upgrade().is_none());
        let staged = control
            .prepare_mode(&target, 3, PathMode::Processed)
            .unwrap();
        control.reserve(staged).unwrap().commit();
        assert_eq!(control.status().unwrap().runtime_epoch, 2);
    }

    #[test]
    fn cancelled_provider_is_joined_and_does_not_replace_current_runtime() {
        let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        target.suppression.enabled = true;
        let staged = control
            .prepare_mode(&target, 2, PathMode::Processed)
            .unwrap();
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        assert!(reader.begin());
        reader.finish(true);
        drop(staged);
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        assert_eq!(control.retired_worker_count(), 1);
        let retired = control.retired_workers();
        assert_eq!(retired[0].runtime_epoch, 2);
        assert!(retired[0].joined && retired[0].drained);
        let staged = control
            .prepare_mode(&target, 3, PathMode::Processed)
            .unwrap();
        control.reserve(staged).unwrap().commit();
        assert_eq!(control.status().unwrap().runtime_epoch, 3);
    }

    #[test]
    fn active_and_staged_descriptors_admit_only_one_mono_microphone_target() {
        let admission = Admission::default();
        let first = Target::new("first", "First", TargetKind::Mic, 1);
        let mut second = Target::new("second", "Second", TargetKind::Mic, 1);
        let (mut a, mut ar) = channel_with_options(
            &first,
            Prepared::new(&first, 48_000).unwrap(),
            rc2fx_suppression::WorkerOptions::default(),
            admission.clone(),
            PathMode::Processed,
        )
        .unwrap();
        let (mut b, _br) = channel_with_options(
            &second,
            Prepared::new(&second, 48_000).unwrap(),
            rc2fx_suppression::WorkerOptions::default(),
            admission,
            PathMode::Processed,
        )
        .unwrap();
        let mut enabled_first = first.clone();
        enabled_first.suppression.enabled = true;
        let staged = a
            .prepare_mode(&enabled_first, 2, PathMode::Processed)
            .unwrap();
        second.suppression.enabled = true;
        assert!(matches!(
            b.prepare_mode(&second, 2, PathMode::Processed),
            Err(Error::SuppressionInUse)
        ));
        drop(staged);
        let staged = b.prepare_mode(&second, 3, PathMode::Passthrough).unwrap();
        b.reserve(staged).unwrap().commit();
        assert!(
            matches!(
                a.prepare_mode(&enabled_first, 3, PathMode::Processed),
                Err(Error::SuppressionInUse)
            ),
            "a saved enable keeps its claim even while its path is unprocessed"
        );
        assert!(ar.begin());
        ar.finish(true);
    }

    #[test]
    fn chat_owns_one_worker_and_excludes_an_enabled_microphone() {
        let admission = Admission::default();
        let mut chat = Target::new("send-chat", "Chat", TargetKind::Output, 2);
        chat.suppression.enabled = true;
        let (control, mut reader) = channel_with_options(
            &chat,
            Prepared::new(&chat, 48_000).unwrap(),
            rc2fx_suppression::WorkerOptions::default(),
            admission.clone(),
            PathMode::Processed,
        )
        .unwrap();
        assert!(control.worker_status().is_some());
        let mut mic = Target::new("mic-1", "Mic", rc2fx_core::TargetKind::Mic, 1);
        mic.suppression.enabled = true;
        assert!(matches!(
            channel_with_options(
                &mic,
                Prepared::new(&mic, 48_000).unwrap(),
                rc2fx_suppression::WorkerOptions::default(),
                admission,
                PathMode::Processed
            ),
            Err(Error::SuppressionInUse)
        ));
        assert!(reader.begin());
        let mut left = [0.25; 112];
        let mut right = [-0.25; 112];
        assert_eq!(
            reader.output([&mut left, &mut right]).unwrap_err(),
            rc2fx_dsp::DspError::MissingChatStem
        );
        assert_eq!(left, [0.25; 112]);
        assert_eq!(right, [-0.25; 112]);
        reader
            .chat_output([&[0.; 112], &[0.; 112]], [&mut left, &mut right])
            .unwrap();
        reader.finish(true);
    }

    #[test]
    fn unresolved_cancel_retains_one_runtime_and_blocks_only_its_path() {
        let mut target = Target::new("mic", "Mic", TargetKind::Mic, 1);
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        target.suppression.enabled = true;
        let staged = control
            .prepare_mode(&target, 2, PathMode::Processed)
            .unwrap();
        let runtime = staged
            .snapshot
            .as_ref()
            .unwrap()
            .get()
            .runtime
            .as_ref()
            .unwrap()
            .clone();
        runtime.block_retirement(true);
        drop(staged);
        let blocked = matches!(
            control.prepare_mode(&target, 3, PathMode::Processed),
            Err(Error::Busy)
        );
        let retained = control.shared.versions.lock().unwrap().retired.is_some();
        let archived = control.retired_worker_count();
        assert!(reader.begin());
        reader.finish(true);
        // Release the test fence before any assertion can unwind into teardown.
        runtime.block_retirement(false);
        control.status().unwrap();
        assert!(blocked && retained);
        assert_eq!(archived, 0, "unresolved work is not a completed retirement");
        assert_eq!(control.retired_worker_count(), 1);
        assert!(control.shared.versions.lock().unwrap().retired.is_none());
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        assert!(
            control
                .prepare_mode(&target, 4, PathMode::Processed)
                .is_ok()
        );
    }

    #[test]
    fn repeated_eq_enable_changes_on_512k_callback_thread_keep_runtime_alive() {
        use std::{
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };
        let mut target = target();
        target.limiter.enabled = false;
        target.eq.enabled = true;
        target.eq.bands = (0..20)
            .map(|index| {
                let mut band = rc2fx_core::Band::new(format!("toggle-{index}"));
                band.frequency = 40.0 * 1.3_f64.powi(index);
                band.gain = if index % 2 == 0 { 3.0 } else { -3.0 };
                band
            })
            .collect();
        let (mut control, mut reader) = channel(&target, Prepared::new(&target, 48_000).unwrap());
        let initial = Arc::downgrade(
            control
                .shared
                .versions
                .lock()
                .unwrap()
                .current
                .get()
                .runtime
                .as_ref()
                .unwrap(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(AtomicU64::new(0));
        let audio_stop = Arc::clone(&stop);
        let audio_frames = Arc::clone(&frames);
        let stack = std::env::var("RC2FX_EQ_CALLBACK_STACK_BYTES")
            .map_or(524288, |value| value.parse::<usize>().unwrap());
        assert!((32768..=2097152).contains(&stack));
        // Runtime and prepared storage are constructed on control first. Only
        // callback work runs on the explicitly bounded audio thread stack.
        let audio = std::thread::Builder::new()
            .name("eq-toggle-audio".into())
            .stack_size(stack)
            .spawn(move || {
                let mut left = [0.01_f32; 112];
                let mut right = [-0.02_f32; 112];
                while !audio_stop.load(Ordering::Acquire) {
                    left.fill(0.01);
                    right.fill(-0.02);
                    assert!(reader.begin());
                    let meter = reader.output([&mut left, &mut right]).unwrap();
                    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
                    reader.record(112, meter, reader.latency_samples() as u32);
                    reader.finish(true);
                    audio_frames.fetch_add(1, Ordering::Release);
                }
                reader
            })
            .unwrap();
        let result = (|| -> Result<(), String> {
            for revision in 2..=257 {
                target.eq.enabled = revision % 2 != 0;
                let deadline = Instant::now() + Duration::from_secs(10);
                let staged = loop {
                    match control.prepare_mode(&target, revision, PathMode::Processed) {
                        Ok(staged) => break staged,
                        Err(Error::Busy) if Instant::now() < deadline => std::thread::yield_now(),
                        Err(error) => {
                            return Err(format!("prepare revision {revision}: {error:?}"));
                        }
                    }
                };
                control
                    .reserve(staged)
                    .map_err(|error| format!("{error:?}"))?
                    .commit();
                let mut applied_at = None;
                loop {
                    let status = control.status().map_err(|error| format!("{error:?}"))?;
                    if status.runtime_epoch != 1 || initial.upgrade().is_none() {
                        return Err("compatible runtime was replaced or freed".into());
                    }
                    let at = frames.load(Ordering::Acquire);
                    if status.applied == revision {
                        applied_at.get_or_insert(at);
                    }
                    if applied_at.is_some_and(|start| at >= start + 8) {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(format!("audio revision {revision} did not advance"));
                    }
                    std::thread::yield_now();
                }
            }
            Ok(())
        })();
        stop.store(true, Ordering::Release);
        let reader = audio.join().expect("EQ callback thread failed");
        drop(reader);
        result.unwrap();
        assert_eq!(control.status().unwrap().runtime_epoch, 1);
        assert_eq!(control.retired_worker_count(), 0);
        assert!(frames.load(Ordering::Acquire) >= 256 * 8);
    }
}
