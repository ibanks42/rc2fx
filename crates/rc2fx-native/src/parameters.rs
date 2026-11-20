//! Per-path immutable descriptors; all runtime construction/retirement is control work.
use super::suppression::{Admission, Claim, Runtime, Start};
use rc2fx_core::Target;
use rc2fx_dsp::Prepared;
use rc2fx_native_control::{PathMode, ResetPolicy, RetiredWorker};
use std::{
    ptr::NonNull,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicPtr, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long publishing waits for a runtime that is being replaced to fade
/// out. The fade itself is `rc2fx_dsp::STAGE_FADE_SAMPLES` (10 ms) of
/// callbacks; without callbacks (the engine stopped, the path not running)
/// the wait ends here and the new snapshot is published anyway.
pub const HANDOFF_TIMEOUT: Duration = Duration::from_millis(100);

/// What the callback does with a path's unprocessed audio while it plays it
/// (`PassFade::target`). The pipeline fades that audio over
/// `rc2fx_dsp::STAGE_FADE_SAMPLES`, so that switching processing on or off
/// never steps between unprocessed audio and a processor's output.
pub const RAW_PLAY: u8 = 0;
/// Fade the unprocessed audio out, before a processor takes over: the
/// callback answers `faded` once it is silent. While the processor has not
/// yet delivered a frame, the unprocessed fallback stays silent too.
pub const RAW_MUTE: u8 = 1;
/// Before the processor hands back to unprocessed audio: start that audio
/// from silence and fade it in.
pub const RAW_RISE: u8 = 2;
/// A path's unprocessed-audio fade, shared by control and the callback.
#[derive(Default)]
pub struct PassFade {
    target: std::sync::atomic::AtomicU8,
    faded: std::sync::atomic::AtomicBool,
    /// Set once by the pipeline: the path is processed in the callback, in
    /// place, so its processor's dry signal is the unprocessed audio at the
    /// same moment (no helper frame between them).
    aligned: std::sync::atomic::AtomicBool,
}
impl PassFade {
    /// Callback: the requested `RAW_*`.
    pub fn target(&self) -> u8 {
        self.target.load(Ordering::Acquire)
    }
    /// Callback: whether the unprocessed audio is now silent after
    /// `RAW_MUTE`.
    pub fn answer(&self, faded: bool) {
        self.faded.store(faded, Ordering::Release);
    }
    /// Pipeline construction: see `aligned`.
    pub fn set_aligned(&self, aligned: bool) {
        self.aligned.store(aligned, Ordering::Relaxed);
    }
    fn aligned(&self) -> bool {
        self.aligned.load(Ordering::Relaxed)
    }
    fn faded(&self) -> bool {
        self.faded.load(Ordering::Acquire)
    }
    fn request(&self, target: u8) {
        if target == RAW_MUTE {
            self.faded.store(false, Ordering::Release);
        }
        self.target.store(target, Ordering::Release);
    }
}
/// A hand-off under way for one staged publication (`Control::hand_off`):
/// what it asked of the callback and what it waits for. It holds no lock,
/// only shared references, so it can be waited on with every lock released.
/// Dropped before its publication commits, it asks the callback to undo it.
pub struct HandingOff {
    ticket: u64,
    /// The runtime asked to fade out.
    runtime: Option<Arc<Runtime>>,
    /// The path's unprocessed-audio fade, and whether this waits for it.
    pass: Arc<PassFade>,
    pass_wait: bool,
    /// The target to restore when undone.
    pass_before: u8,
    publications: Arc<AtomicU64>,
    timeout: Duration,
    committed: bool,
}
impl HandingOff {
    /// Whether every fade this waits for has finished.
    pub fn done(&self) -> bool {
        self.runtime
            .as_ref()
            .is_none_or(|runtime| runtime.handed_off())
            && (!self.pass_wait || self.pass.faded())
    }
    /// Polls `done` for up to the control's hand-off timeout; returns it.
    /// Holds no lock. Without callbacks (the engine stopped) the timeout
    /// ends the wait and the publication goes ahead.
    pub fn wait(&self) -> bool {
        let end = Instant::now() + self.timeout;
        while !self.done() {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }
}
impl Drop for HandingOff {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(runtime) = &self.runtime {
            runtime.request_hand_off(None);
        }
        if self.pass.target() != self.pass_before {
            self.pass.request(self.pass_before);
            self.publications.fetch_add(1, Ordering::Release);
        }
    }
}

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
    pass: Arc<PassFade>,
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
    handoff_timeout: Duration,
    shared: Arc<Shared>,
    next_ticket: u64,
    next_epoch: u64,
    telemetry: super::telemetry::Consumer,
    options: rc2fx_suppression::WorkerOptions,
    admission: Admission,
    publications: Arc<AtomicU64>,
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
    /// The hand-off this publication waited for: undone if the reservation
    /// is dropped uncommitted.
    handing: HandingOff,
    published: &'a AtomicPtr<Snapshot>,
    publications: &'a AtomicU64,
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
        // What was handed off stays so for whatever frames it still serves.
        self.handing.committed = true;
        self.published
            .store(self.versions.current.pointer(), Ordering::Release);
        // After the store: a callback that sees the count sees this snapshot.
        self.publications.fetch_add(1, Ordering::Release);
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
        Some(Runtime::new(
            target,
            &prepared,
            options,
            1,
            &admission,
            Start::Playing,
        )?)
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
        pass: Arc::default(),
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
            handoff_timeout: HANDOFF_TIMEOUT,
            shared: Arc::clone(&shared),
            next_ticket: 1,
            next_epoch: 2,
            telemetry: consumer,
            options,
            admission,
            publications: Arc::default(),
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
    /// Counts this path's publications in `publications`, so that a callback
    /// can leave paths alone until any of them changes.
    pub fn count_publications(&mut self, publications: Arc<AtomicU64>) {
        self.publications = publications;
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    /// How long `reserve` waits for a replaced runtime to fade out.
    #[cfg(test)]
    pub fn set_handoff_timeout(&mut self, timeout: Duration) {
        self.handoff_timeout = timeout;
    }
    /// `prepare_with` allowing a reset, for tests that predate the policy.
    #[cfg(test)]
    pub fn prepare_mode(
        &mut self,
        target: &Target,
        revision: u64,
        mode: PathMode,
    ) -> Result<Staged, Error> {
        self.prepare_with(target, revision, mode, ResetPolicy::Allow)
    }
    /// `prepare_mode` that, with `ResetPolicy::Refuse`, answers
    /// `RequiresReset` instead of building a new runtime: when the path mode
    /// changes, or a processed path's settings need one (`requires_reset`).
    /// Nothing is staged or claimed then.
    pub fn prepare_with(
        &mut self,
        target: &Target,
        revision: u64,
        mode: PathMode,
        reset: ResetPolicy,
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
        if reset == ResetPolicy::Refuse
            && (versions.current.get().mode != mode
                || (mode == PathMode::Processed
                    && versions.current.get().prepared.requires_reset(&prepared)))
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
            // Published only after the current runtime, if any, has faded
            // out (`reserve`), so it starts silent or from the dry signal.
            let start = if versions.current.get().runtime.is_none() && self.shared.pass.aligned() {
                Start::AfterPassthrough
            } else {
                Start::Silent
            };
            let runtime = Runtime::new(
                target,
                &prepared,
                self.options,
                self.next_epoch,
                &self.admission,
                start,
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
    /// Starts the hand-off publishing `staged` needs, without waiting for it:
    ///
    /// - A new runtime in place of the running one (a reset): the running
    ///   one fades to silence; the new one starts silent for its delay, then
    ///   fades in (`Start::Silent`).
    /// - Processing off, on an aligned path (`PassFade::aligned`) whose
    ///   processor has no delay: it fades to its dry signal, which is the
    ///   unprocessed audio. Otherwise it fades to silence and the unprocessed
    ///   audio then fades in from silence (`RAW_RISE`).
    /// - Processing on: on an aligned path with a processor without a delay,
    ///   nothing to wait for (the processor starts from its dry signal).
    ///   Otherwise the unprocessed audio fades to silence first
    ///   (`RAW_MUTE`), and the processor starts silent.
    ///
    /// The version lock is held only to read the current snapshot. Wait on
    /// the result with every lock released, then pass it to `reserve_after`.
    pub fn hand_off(&self, staged: &Staged) -> Result<HandingOff, Error> {
        if !Arc::ptr_eq(&self.shared, &staged.shared) {
            return Err(Error::WrongTarget);
        }
        let (runtime, to, raw) = {
            let versions = self.shared.versions.lock().map_err(|_| Error::Poisoned)?;
            check(&versions, staged)?;
            let current = versions.current.get();
            let next = staged.snapshot.as_ref().expect("staged snapshot").get();
            let aligned = self.shared.pass.aligned();
            match (&current.runtime, &next.runtime) {
                _ if same_runtime(current, next) => (None, None, None),
                // A reset: silence between the two.
                (Some(old), Some(_)) => (
                    Some(Arc::clone(old)),
                    Some(rc2fx_dsp::HandOff::Silence),
                    None,
                ),
                // Processing off.
                (Some(old), None) if aligned && old.latency_samples() == 0 => (
                    Some(Arc::clone(old)),
                    Some(rc2fx_dsp::HandOff::Dry),
                    Some(RAW_PLAY),
                ),
                (Some(old), None) => (
                    Some(Arc::clone(old)),
                    Some(rc2fx_dsp::HandOff::Silence),
                    Some(RAW_RISE),
                ),
                // Processing on.
                (None, Some(new)) if aligned && new.latency_samples() == 0 => {
                    (None, None, Some(RAW_PLAY))
                }
                (None, Some(_)) => (None, None, Some(RAW_MUTE)),
                (None, None) => (None, None, None),
            }
        };
        let pass_before = self.shared.pass.target();
        if let Some(raw) = raw
            && raw != pass_before
        {
            self.shared.pass.request(raw);
            // Every path is visited the next callbacks, this one included.
            self.publications.fetch_add(1, Ordering::Release);
        }
        if let (Some(runtime), Some(to)) = (&runtime, to) {
            runtime.request_hand_off(Some(to));
        }
        Ok(HandingOff {
            ticket: staged.ticket,
            runtime,
            pass: Arc::clone(&self.shared.pass),
            pass_wait: raw == Some(RAW_MUTE),
            pass_before,
            publications: Arc::clone(&self.publications),
            timeout: self.handoff_timeout,
            committed: false,
        })
    }
    /// Takes the version lock for publishing `staged` once `handing` (its
    /// `hand_off`) has been waited for. On an error, `handing` is undone.
    pub fn reserve_after(
        &mut self,
        mut staged: Staged,
        handing: HandingOff,
    ) -> Result<Reservation<'_>, Error> {
        if !Arc::ptr_eq(&self.shared, &staged.shared) || handing.ticket != staged.ticket {
            return Err(Error::WrongTarget);
        }
        // Only this control changes the versions; the reader never locks.
        let versions = self.shared.versions.lock().map_err(|_| Error::Poisoned)?;
        if let Err(error) = check(&versions, &staged) {
            drop(versions);
            return Err(error);
        }
        Ok(Reservation {
            versions,
            handing,
            published: &self.shared.published,
            publications: &self.publications,
            ticket: staged.ticket,
            next: staged.snapshot.take(),
        })
    }
    /// `hand_off`, its wait (no lock held, though `self` is) and
    /// `reserve_after`: for tests, which own the control outright. The IPC
    /// commit runs the three steps itself so it can drop its locks for the
    /// wait.
    #[cfg(test)]
    pub fn reserve(&mut self, staged: Staged) -> Result<Reservation<'_>, Error> {
        let handing = self.hand_off(&staged)?;
        handing.wait();
        self.reserve_after(staged, handing)
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
    /// The worst live-worker turnaround since the previous status read. Read
    /// only by `target_status`: the audit report's `worker_status` must not
    /// consume the peak.
    pub fn worker_recent_turnaround_ns(&self) -> Option<u64> {
        self.shared
            .versions
            .lock()
            .ok()?
            .current
            .get()
            .runtime
            .as_ref()?
            .worker()
            .and_then(|worker| worker.telemetry().take_turnaround_recent())
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
    /// The current worker's runtime epoch and (missed-deadline, late-result)
    /// histories, typed for the control API. All three are read under one
    /// version lock, so the histories always belong to the same worker and
    /// the epoch names it. None without a worker; the histories retain their
    /// existing per-ring read semantics.
    pub fn worker_histories(
        &self,
    ) -> Option<(
        u64,
        rc2fx_native_control::NativeHistory,
        rc2fx_native_control::NativeHistory,
    )> {
        let versions = self.shared.versions.lock().ok()?;
        let runtime = versions.current.get().runtime.as_ref()?;
        let worker = runtime.worker()?;
        let (misses, lates) = native_histories(worker.telemetry().histories());
        Some((runtime.epoch(), misses, lates))
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
/// Whether `staged` may still be published over `versions`.
fn check(versions: &Versions, staged: &Staged) -> Result<(), Error> {
    if versions.pending != Some(staged.ticket)
        || versions.current.get().revision != staged.expected_revision
    {
        Err(Error::StaleRevision)
    } else if versions.retired.is_some() {
        Err(Error::Busy)
    } else {
        Ok(())
    }
}
fn native_histories(
    (misses, lates): (
        rc2fx_suppression::EventHistory,
        rc2fx_suppression::EventHistory,
    ),
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
    (convert(misses), convert(lates))
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
    /// The path's unprocessed-audio fade, for the pipeline to keep beside
    /// the reader (construction only).
    pub fn pass_fade(&self) -> Arc<PassFade> {
        Arc::clone(&self.shared.pass)
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
    /// As `record` once per cycle, for `cycles` unmetered frames left alone.
    pub fn record_skipped(&mut self, cycles: u64, frames: usize, latency: u32) {
        self.telemetry.record_skipped(cycles, frames, latency);
    }
    pub fn begin(&mut self) -> bool {
        if !self.current.is_null() {
            return false;
        }
        self.current = self.shared.published.load(Ordering::Acquire);
        true
    }
    /// Asks for the publication `begin` loads (see prefetch.rs).
    pub fn warm_begin(&self) {
        super::prefetch::read(&raw const self.shared.published, 1);
    }
    /// Asks for what the end of this frame uses, between `begin` and `finish`
    /// (see prefetch.rs): the acknowledgement `finish` writes and the
    /// descriptor's own fields; with `processed`, also both ends of the
    /// settings the DSP runs on and of its processor.
    pub fn warm_end(&self, processed: bool) {
        use super::prefetch;
        prefetch::write(&raw const self.shared.seen, 1);
        prefetch::write(&raw const self.shared.applied, 1);
        let Some(snapshot) = self.descriptor() else {
            return;
        };
        prefetch::read(&raw const snapshot.revision, 1);
        prefetch::read(&raw const snapshot.mode, 1);
        prefetch::read(&raw const snapshot.runtime, 1);
        if processed {
            prefetch::ends(&raw const snapshot.prepared, false);
            // The processor is two loads from here, through lines asked for
            // just above: these loads wait for them.
            if let Some(runtime) = &snapshot.runtime {
                unsafe { runtime.warm() };
            }
        }
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
mod tests;
