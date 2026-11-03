//! One immutable configuration publication shared by independent audio callbacks.
//!
//! Each JACK path retains its own DSP history. Control retains the previous
//! prepared set until every callback has finished reading it. There is at most
//! one outstanding publication, so delayed callbacks cause bounded backpressure
//! rather than allocation, reclamation or retry loops on an audio thread.
use crate::store::{PreparedSave, StoreError};
use rc2fx_core::{CoreError, MAX_TARGETS, Target};
use rc2fx_dsp::{Dsp, DspError, Meter, Prepared, RetirementError, Suppression};
use std::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
    time::{Duration, Instant},
};

struct PreparedSet {
    parameters: Vec<Prepared>,
    revision: u64,
}

/// Immutable allocation with explicit ownership. Using a raw owner avoids
/// manufacturing exclusive references to data still borrowed by callbacks when
/// control moves its ownership handle between current and retired slots.
struct Snapshot(NonNull<PreparedSet>);
impl Snapshot {
    fn new(parameters: Vec<Prepared>, revision: u64) -> Self {
        Self(NonNull::from(Box::leak(Box::new(PreparedSet {
            parameters,
            revision,
        }))))
    }
    fn get(&self) -> &PreparedSet {
        // Valid immutable allocation for this owner's entire lifetime.
        unsafe { self.0.as_ref() }
    }
    fn pointer(&self) -> *mut PreparedSet {
        self.0.as_ptr()
    }
}
// PreparedSet is immutable and contains Send+Sync values. Owner transfer/free
// is exclusively control-side; callback borrows are protected by group epochs.
unsafe impl Send for Snapshot {}
unsafe impl Sync for Snapshot {}
impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(self.0.as_ptr()));
        }
    }
}

#[derive(Default)]
// Each path has an independent writer. Keep adjacent paths off the same
// 64-byte cache line instead of bouncing their progress counters between cores.
#[repr(align(64))]
struct Progress {
    seen: AtomicU64,
    applied: AtomicU64,
    frames: AtomicU64,
    callbacks: AtomicU64,
}

/// A fully constructed processing path. Construct and destroy on control.
/// This does not register a JACK client or admit a physical route.
pub struct Chain {
    id: String,
    prepared: Prepared,
    processor: Dsp,
}

#[derive(Debug)]
pub enum PrepareError {
    Invalid(CoreError),
    Dsp(DspError),
    WrongTargets,
    StaleRevision,
    Busy,
    RequiresRestart,
    Retired,
}
impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => error.fmt(f),
            Self::Dsp(error) => error.fmt(f),
            Self::WrongTargets => {
                f.write_str("Audio paths must have unique, matching target identities")
            }
            Self::StaleRevision => f.write_str("Audio update revision must advance"),
            Self::Busy => {
                f.write_str("Some audio callbacks have not observed the last publication")
            }
            Self::RequiresRestart => f.write_str("This change requires a controlled audio restart"),
            Self::Retired => f.write_str("Audio storage has been permanently retired"),
        }
    }
}
impl std::error::Error for PrepareError {}

#[derive(Debug)]
pub enum SaveError {
    Audio(PrepareError),
    Store(StoreError),
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Audio(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for SaveError {}

/// Coordinate a previously validated settings or preset transaction with every
/// active path. Audio admission/preparation precedes the disk commit. Failure at
/// either stage preserves the previous published audio revision. After the
/// durable commit there is only an ownership move and one release publication.
pub fn save_and_publish(
    save: PreparedSave<'_>,
    control: &mut Control<'_>,
) -> Result<rc2fx_core::Config, SaveError> {
    let candidate = save.candidate_config();
    let reservation = control
        .reserve(&candidate.targets, candidate.revision)
        .map_err(SaveError::Audio)?;
    let acknowledged = save.commit().map_err(SaveError::Store)?;
    reservation.commit();
    Ok(acknowledged)
}

impl Chain {
    pub fn new(
        target: &Target,
        providers: [Option<Box<dyn Suppression>>; 2],
    ) -> Result<Self, PrepareError> {
        let prepared = Prepared::new(target, 48_000).map_err(PrepareError::Invalid)?;
        Self::prepared(&target.id, prepared, providers)
    }
    fn prepared(
        target_id: &str,
        prepared: Prepared,
        providers: [Option<Box<dyn Suppression>>; 2],
    ) -> Result<Self, PrepareError> {
        let processor = Dsp::new(&prepared, providers).map_err(PrepareError::Dsp)?;
        Ok(Self {
            id: target_id.to_owned(),
            prepared,
            processor,
        })
    }
}

/// Control-owned storage. The borrowed endpoints cannot outlive this owner;
/// native callback registration must likewise be deactivated before releasing
/// its endpoint. No Arc final-owner destructor can run inside `process`.
pub struct Group {
    ids: Vec<String>,
    current: Snapshot,
    retired: Option<Snapshot>,
    published: AtomicPtr<PreparedSet>,
    processors: Vec<Dsp>,
    progress: Vec<Progress>,
    processing_retired: bool,
}

impl Group {
    pub fn new(chains: Vec<Chain>, revision: u64) -> Result<Self, PrepareError> {
        if revision == 0 {
            return Err(PrepareError::StaleRevision);
        }
        if chains.is_empty()
            || chains.len() > MAX_TARGETS
            || chains
                .iter()
                .enumerate()
                .any(|(i, chain)| chains[..i].iter().any(|prior| prior.id == chain.id))
        {
            return Err(PrepareError::WrongTargets);
        }
        let mut ids = Vec::with_capacity(chains.len());
        let mut parameters = Vec::with_capacity(chains.len());
        let mut processors = Vec::with_capacity(chains.len());
        let progress = (0..chains.len()).map(|_| Progress::default()).collect();
        for chain in chains {
            ids.push(chain.id);
            parameters.push(chain.prepared);
            processors.push(chain.processor);
        }
        let current = Snapshot::new(parameters, revision);
        let published = AtomicPtr::new(current.pointer());
        Ok(Self {
            ids,
            current,
            retired: None,
            published,
            processors,
            progress,
            processing_retired: false,
        })
    }

    pub fn is_retired(&self) -> bool {
        self.processing_retired
    }

    pub fn selected_ids(&self) -> &[String] {
        &self.ids
    }

    /// Control only after ALL borrowed endpoints/native clients have returned.
    /// The exclusive borrow prevents a safe callback loan from surviving here.
    /// Retirement is permanent even when a worker does not join in time; retry
    /// may observe eventual joins but may never resume processing/publication.
    ///
    /// ```compile_fail
    /// use rc2fx_device::audio::Group;
    /// use std::time::Duration;
    /// fn retire_while_callback_exists(group: &mut Group) {
    ///     let (_control, callbacks) = group.split().unwrap();
    ///     group.drain_suppression(Duration::ZERO).unwrap();
    ///     let _ = callbacks[0].channels();
    /// }
    /// ```
    pub fn drain_suppression(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        self.processing_retired = true;
        let deadline = Instant::now().checked_add(timeout);
        let mut failure = deadline.is_none().then_some(RetirementError::TimedOut);
        for processor in &mut self.processors {
            let remaining = deadline
                .map(|end| end.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::ZERO);
            if let Err(error) = processor.drain_suppression(remaining) {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// One exclusive publisher and one exclusive consumer per DSP path. Endpoint
    /// creation allocates only here, before any real-time callbacks are active.
    pub fn split(&mut self) -> Result<(Control<'_>, Vec<Callback<'_>>), PrepareError> {
        if self.processing_retired {
            return Err(PrepareError::Retired);
        }
        let callbacks = self
            .processors
            .iter_mut()
            .zip(&self.progress)
            .enumerate()
            .map(|(index, (processor, progress))| Callback {
                channels: self.current.get().parameters[index].channels(),
                index,
                processor,
                progress,
                published: &self.published,
            })
            .collect();
        Ok((
            Control {
                ids: &self.ids,
                current: &mut self.current,
                retired: &mut self.retired,
                published: &self.published,
                progress: &self.progress,
            },
            callbacks,
        ))
    }
}

pub struct Control<'a> {
    ids: &'a [String],
    current: &'a mut Snapshot,
    retired: &'a mut Option<Snapshot>,
    published: &'a AtomicPtr<PreparedSet>,
    progress: &'a [Progress],
}

impl<'owner> Control<'owner> {
    /// Actual callback slot identities, in their stable allocation order.
    pub fn selected_ids(&self) -> &[String] {
        self.ids
    }

    /// Reserve before saving. All selected paths must prepare successfully;
    /// inactive targets may be present and are handled by the durable Store.
    /// Dropping the reservation cancels it without touching audio.
    pub fn reserve<'a>(
        &'a mut self,
        targets: &[Target],
        revision: u64,
    ) -> Result<Reservation<'a, 'owner>, PrepareError> {
        self.reserve_prepared(targets, revision, |target| {
            Prepared::new(target, 48_000).map_err(PrepareError::Invalid)
        })
    }

    fn reserve_prepared<'a>(
        &'a mut self,
        targets: &[Target],
        revision: u64,
        mut prepare: impl FnMut(&Target) -> Result<Prepared, PrepareError>,
    ) -> Result<Reservation<'a, 'owner>, PrepareError> {
        if revision <= self.current.get().revision {
            return Err(PrepareError::StaleRevision);
        }
        self.reclaim();
        if self.retired.is_some() {
            return Err(PrepareError::Busy);
        }
        if targets.len() > MAX_TARGETS
            || targets
                .iter()
                .enumerate()
                .any(|(i, target)| targets[..i].iter().any(|prior| prior.id == target.id))
        {
            return Err(PrepareError::WrongTargets);
        }
        let mut parameters = Vec::with_capacity(self.ids.len());
        for (index, id) in self.ids.iter().enumerate() {
            let target = targets
                .iter()
                .find(|target| target.id == *id)
                .ok_or(PrepareError::WrongTargets)?;
            let prepared = prepare(target)?;
            if self.current.get().parameters[index].requires_reset(&prepared) {
                return Err(PrepareError::RequiresRestart);
            }
            parameters.push(prepared);
        }
        let snapshot = Snapshot::new(parameters, revision);
        Ok(Reservation {
            control: self,
            snapshot,
        })
    }

    /// Only returns the published revision once EVERY selected path completed
    /// nonempty audio with it. Zero means no complete group acknowledgement yet.
    /// This is not a liveness check; the lifecycle monitor separately checks
    /// advancing frame counts and native client/server health.
    pub fn applied_revision(&self) -> u64 {
        let revision = self.current.get().revision;
        if self
            .progress
            .iter()
            .all(|slot| slot.applied.load(Ordering::Acquire) == revision)
        {
            revision
        } else {
            0
        }
    }
    pub fn published_revision(&self) -> u64 {
        self.current.get().revision
    }
    pub fn processed_frames(&self, index: usize) -> Option<u64> {
        self.progress
            .get(index)
            .map(|slot| slot.frames.load(Ordering::Relaxed))
    }
    pub fn callbacks(&self, index: usize) -> Option<u64> {
        self.progress
            .get(index)
            .map(|slot| slot.callbacks.load(Ordering::Relaxed))
    }

    /// Destruction happens only on control. Observing each callback's release
    /// means none can retain an older reference: each callback is exclusive,
    /// and its subsequent acquire loads cannot go backwards in atomic order.
    pub fn reclaim(&mut self) -> bool {
        if self.retired.is_some()
            && self
                .progress
                .iter()
                .all(|slot| slot.seen.load(Ordering::Acquire) == self.current.get().revision)
        {
            self.retired.take();
            true
        } else {
            false
        }
    }
}

pub struct Reservation<'a, 'owner> {
    control: &'a mut Control<'owner>,
    snapshot: Snapshot,
}

impl Reservation<'_, '_> {
    /// One release publication after durable save. Callbacks independently pick
    /// it up at their next block; this does not claim sample-atomic transitions
    /// across separately scheduled JACK clients. No per-target partial commit.
    pub fn commit(self) {
        let Self { control, snapshot } = self;
        let old = std::mem::replace(control.current, snapshot);
        // Exclusive reservation guarantees retired was empty. Retain old data
        // before exposing the new pointer to any reader.
        *control.retired = Some(old);
        control
            .published
            .store(control.current.pointer(), Ordering::Release);
    }
}

pub struct Callback<'a> {
    channels: usize,
    index: usize,
    processor: &'a mut Dsp,
    progress: &'a Progress,
    published: &'a AtomicPtr<PreparedSet>,
}

impl Callback<'_> {
    /// Revision used by this endpoint's last successful nonempty callback.
    /// This endpoint is its sole writer; a concurrent control publication does
    /// not change it. Read immediately after process to label that audio block.
    pub fn applied_revision(&self) -> u64 {
        self.progress.applied.load(Ordering::Relaxed)
    }
    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn latency_samples(&self) -> usize {
        self.processor.latency_samples()
    }
    /// Exactly one acquire, bounded scalar DSP and progress stores. No queues,
    /// locks, retries, model construction, allocation or destruction here.
    pub fn process(
        &mut self,
        input: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) -> Result<Meter, DspError> {
        let pointer = self.published.load(Ordering::Acquire);
        // Pointer is initialized before split. Control retains every published
        // set until THIS callback finishes a later set; at most one old set can
        // exist. The immutable set contains every stable callback index.
        let snapshot = unsafe { &*pointer };
        let revision = snapshot.revision;
        let frames = input[0].len() as u64;
        let result = self
            .processor
            .process(&snapshot.parameters[self.index], input, output);
        // No snapshot access after this release: it is the reclamation fence,
        // including for empty blocks or invalid buffer shapes.
        self.progress.seen.store(revision, Ordering::Release);
        if result.is_ok() && frames != 0 {
            // This endpoint is the sole writer; control only loads counters.
            // RMW would introduce exclusive atomic operations unnecessarily.
            // Keep the previous fetch_add wrapping semantics.
            self.progress.frames.store(
                self.progress
                    .frames
                    .load(Ordering::Relaxed)
                    .wrapping_add(frames),
                Ordering::Relaxed,
            );
            self.progress.callbacks.store(
                self.progress
                    .callbacks
                    .load(Ordering::Relaxed)
                    .wrapping_add(1),
                Ordering::Relaxed,
            );
            self.progress.applied.store(revision, Ordering::Release);
        }
        result
    }
}
