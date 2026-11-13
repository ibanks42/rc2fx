//! Callback-owned signal state; construction and destruction are control work.
use super::{Line, parameters, prefetch};
use rc2fx_core::{Target, TargetKind};
#[cfg(test)]
use rc2fx_dsp::Dsp;
use rc2fx_dsp::Prepared;
use rc2fx_native_control::{MAX_PATHS, PathMode};
use rc2fx_suppression::{PRODUCTION_WORKER, WorkerOptions};
use serde::Deserialize;
use std::{
    cell::UnsafeCell,
    collections::HashSet,
    ffi::c_void,
    sync::{
        Arc,
        atomic::{
            AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering,
        },
    },
    thread,
};

pub const FRAMES: usize = 112;
/// Hardware outputs (heard only on the mixer's own outputs) are processed in
/// the callback, in place, after the mixer's own processing returns, rather
/// than by the helper one frame later. Every hardware output does, meter-only
/// ones included: ownership is fixed at construction, not chosen per mode or
/// history. Sources stay with the helper.
pub const DIRECT_OUTPUT: bool = true;
/// Unprocessed paths only feed level meters; fetch and meter their buffers on
/// one callback in this many. Every callback still counts their frames: the
/// helper's paths each frame, callback-owned ones at their next visit.
const METER_STRIDE: u32 = 8;
/// Whether path `path` (plan order) meters on callback `cycle`: each path once
/// in METER_STRIDE callbacks, the paths staggered so that they do not all
/// meter on the same one.
fn meters(cycle: u32, path: usize) -> bool {
    cycle.wrapping_add(path as u32).is_multiple_of(METER_STRIDE)
}
/// Callback time spent per bound path (plan order) and resolving buffers.
/// Bench gauges only (`Front::gauged`): each charge costs two clock reads, on
/// every visited path of every callback.
pub static PATH_COST_NS: Line<[AtomicU64; MAX_PATHS]> =
    Line([const { AtomicU64::new(0) }; MAX_PATHS]);
pub static RESOLVE_NS: Line<AtomicU64> = Line(AtomicU64::new(0));
fn charge(counter: &AtomicU64, since: u64) {
    counter.fetch_add(
        super::timing::monotonic().saturating_sub(since),
        Ordering::Relaxed,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Plan {
    version: u32,
    sample_rate: u32,
    frames: usize,
    /// Omitted: the production worker policy. An explicit object sets each
    /// field; an absent field inherits (unprivileged fixtures use `{}`).
    #[serde(default)]
    worker: Option<WorkerPlan>,
    pub paths: Vec<PathPlan>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorkerPlan {
    cpu_mask: Option<u64>,
    realtime_priority: Option<u8>,
    #[serde(default)]
    nice: Option<i8>,
}
impl Plan {
    fn worker_options(&self) -> WorkerOptions {
        self.worker
            .as_ref()
            .map_or(PRODUCTION_WORKER, |worker| WorkerOptions {
                cpu_mask: worker.cpu_mask,
                realtime_priority: worker.realtime_priority,
                nice: worker.nice,
            })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathPlan {
    pub direction: Direction,
    pub ports: Vec<String>,
    pub target: Target,
    pub mode: PathMode,
    /// An output heard only on the mixer's own hardware outputs.
    pub hardware: bool,
}
impl Plan {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let plan: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if plan.version != 1
            || plan.sample_rate != 48_000
            || plan.frames != FRAMES
            || plan.paths.len() > MAX_PATHS
        {
            return Err("unsupported native plan geometry/count".into());
        }
        let mut ids = HashSet::new();
        let mut ports = HashSet::new();
        let worker = plan.worker_options();
        if worker
            .realtime_priority
            .is_some_and(|priority| !(1..=10).contains(&priority))
        {
            return Err("native worker priority exceeds the admitted bound".into());
        }
        if worker.realtime_priority.is_some() && worker.nice.is_some() {
            return Err("native worker asks for both a real-time priority and a nice".into());
        }
        let mut suppressors = 0;
        for path in &plan.paths {
            path.target.validate().map_err(|error| error.to_string())?;
            if path.target.suppression.enabled {
                suppressors += 1;
                let mic = path.direction == Direction::Input
                    && path.target.kind == TargetKind::Mic
                    && path.target.channels == 1;
                let chat = path.direction == Direction::Output
                    && path.target.is_chat_output()
                    && !path.hardware;
                if suppressors > 1 || !(mic || chat) {
                    return Err("native plan admits one mono microphone or the Chat voice mix with suppression".into());
                }
            }
            if path.ports.len() != path.target.channels as usize
                || (path.hardware && path.direction != Direction::Output)
                || !ids.insert(&path.target.id)
                || (path.direction == Direction::Output) != (path.target.kind == TargetKind::Output)
            {
                return Err("native plan target identity/direction mismatch".into());
            }
            for port in &path.ports {
                if port.is_empty()
                    || port.len() > 64
                    || port.contains([':', '\0'])
                    || !ports.insert((path.direction as u8, port))
                {
                    return Err("native plan duplicate/invalid port".into());
                }
            }
        }
        Ok(plan)
    }
}

pub struct Pin {
    pub client: usize,
    pub pointer: usize,
    pub name: String,
    pub direction: Direction,
    pub alive: AtomicBool,
}
#[derive(Default)]
pub struct Registry {
    pub ports: Vec<Arc<Pin>>,
}
impl Registry {
    pub fn register(&mut self, client: usize, pointer: usize, name: &[u8], flags: usize) -> bool {
        // Invalidate the old identity even if the new registration cannot be
        // tracked. A full registry or invalid replacement must not leave an old
        // pipeline attached to a pointer now owned by a different port.
        self.retire(client, pointer);
        let direction = match flags & 3 {
            1 => Direction::Input,
            2 => Direction::Output,
            _ => return false,
        };
        let Ok(name) = std::str::from_utf8(name) else {
            return false;
        };
        if name.is_empty() || name.len() > 64 || self.ports.len() >= 512 {
            return false;
        }
        self.ports.push(Arc::new(Pin {
            client,
            pointer,
            name: name.into(),
            direction,
            alive: AtomicBool::new(true),
        }));
        true
    }
    pub fn retire(&mut self, client: usize, pointer: usize) {
        self.ports.retain(|pin| {
            if pin.client == client && pin.pointer == pointer {
                pin.alive.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }
    pub fn close(&mut self, client: usize) {
        self.ports.retain(|pin| {
            if pin.client == client {
                pin.alive.store(false, Ordering::Release);
                false
            } else {
                true
            }
        });
    }
}

/// Which side processes a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// In the callback, before it returns: outputs, which jackd hands on as
    /// soon as the graph completes.
    Callback,
    /// By the helper, from a copy the callback makes: sources, and outputs
    /// heard only on the mixer's own hardware. Both reach the mixer one
    /// frame later, and the helper never touches a jackd buffer.
    Helper,
}
/// What the helper does with the frame the callback copied for a path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    /// The callback left this path alone: `Count` if the mixer's callback
    /// succeeded, else `None`. The helper resolves it and restores it.
    Skipped,
    /// Nothing to apply: the path was inactive, incomplete or failed.
    None,
    /// Count the frames of an unprocessed path left alone (METER_STRIDE).
    Count,
    /// Meter an unprocessed path's copied buffers.
    Meter,
    /// Process a copied source; the mixer reads the result next frame.
    Input,
    /// Process a copied hardware output; the callback writes it next frame.
    Output,
}

/// The callback's view of a path.
struct Port {
    direction: Direction,
    pins: Vec<Arc<Pin>>,
    owner: Owner,
    /// Callback-processed paths keep their settings reader here.
    reader: Option<parameters::Reader>,
    /// The source's mode was Processed at the last look (for a helper-owned
    /// source, the helper's latest snapshot, while the helper was idle).
    processed_intent: bool,
    /// This frame's outcome so far: processed only while every request was.
    outcome: Option<Outcome>,
    chat: Option<Box<super::chat_mix::Capture>>,
    native: [usize; 2],
    observed: u8,
    active: bool,
    /// An unprocessed path left alone this frame, its buffers not resolved.
    counted: bool,
    failed: bool,
    /// `Front::skipped_frames` when this path was last visited.
    credited: u64,
}
impl Port {
    fn fail(&mut self, counters: &mut Counters) {
        self.active = false;
        if !self.failed {
            counters.faults += 1;
            LIVE_FAULTS.fetch_add(1, Ordering::Relaxed);
        }
        self.failed = true;
    }
    /// Records what one buffer request of a source meant to be processed
    /// returned. The frame's outcome keeps its first raw cause.
    fn note(&mut self, outcome: Outcome, counters: &mut Counters) {
        if self.direction != Direction::Input || !self.processed_intent {
            return;
        }
        if outcome != Outcome::Processed {
            counters.sources.raw_returns += 1;
            SOURCES.raw_returns.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(self.outcome, None | Some(Outcome::Processed)) {
            self.outcome = Some(outcome);
        }
    }
    /// Counts this frame's outcome for a source meant to be processed;
    /// `usable`: the frame completed (native success, every channel).
    fn tally_source(&mut self, usable: bool, counters: &mut Counters) {
        if self.direction == Direction::Input && self.processed_intent {
            let outcome = match self.outcome {
                None => Outcome::Unobserved,
                Some(Outcome::Processed) if !usable => Outcome::ProcessedUnacknowledged,
                Some(outcome) => outcome,
            };
            tally(&mut counters.sources, outcome);
        }
    }
    /// A path a cycle leaves alone must look like one that was never touched:
    /// the mixer can still ask for its buffer.
    fn reset(&mut self) {
        self.active = false;
        self.counted = false;
        self.observed = 0;
        self.outcome = None;
    }
    fn complete(&self) -> bool {
        self.observed == (1 << self.pins.len()) - 1
    }
    fn alive(&self) -> bool {
        self.pins
            .iter()
            .all(|pin| pin.alive.load(Ordering::Acquire))
    }
    /// Asks for what `begin_port` reads beyond the `Port` (see prefetch.rs):
    /// the publication a callback-owned path loads or the helper's mode of
    /// another, then the pins it resolves. `work`: this path's `Handoff::work`.
    fn warm_begin(&self, work: *const Option<Work>) {
        match &self.reader {
            Some(reader) => reader.warm_begin(),
            None => prefetch::read(work, 1),
        }
        // The list of pins is loaded here, so asking for it would gain nothing.
        for pin in &self.pins {
            prefetch::read(Arc::as_ptr(pin), 1);
        }
    }
    /// Asks for what `end_port` uses beyond the `Port` (see prefetch.rs): the
    /// buffers `begin` resolved for this cycle, then a callback-owned path's
    /// settings and processor, or the copy a helper-owned one makes into
    /// `raw[slot]` of its `work`.
    ///
    /// # Safety
    /// As `Pipeline::end`, with the helper idle: the callback owns `work`.
    unsafe fn warm_end(&self, work: *const Option<Work>, slot: usize) {
        // Only this cycle's addresses: an inactive path resolved none.
        let channels = if self.active { self.pins.len() } else { 0 };
        // An output is processed in place.
        let written = self.direction == Direction::Output && self.processed_intent;
        for &buffer in self.native.iter().take(channels) {
            if written {
                prefetch::write(buffer as *const f32, FRAMES);
            } else {
                prefetch::read(buffer as *const f32, FRAMES);
            }
        }
        match &self.reader {
            Some(reader) => reader.warm_end(self.active && self.processed_intent),
            None => {
                prefetch::write(work, 1);
                if let Some(work) = unsafe { &*work } {
                    for copy in work.raw[slot].iter().take(channels) {
                        prefetch::write(copy.as_ptr(), FRAMES);
                    }
                }
            }
        }
    }
    /// # Safety
    /// The resolved native buffers stay valid for this callback.
    unsafe fn slices<'a>(&self, frames: usize) -> [&'a mut [f32]; 2] {
        let left = unsafe { std::slice::from_raw_parts_mut(self.native[0] as *mut f32, frames) };
        let right = if self.pins.len() == 2 {
            unsafe { std::slice::from_raw_parts_mut(self.native[1] as *mut f32, frames) }
        } else {
            &mut []
        };
        [left, right]
    }
}
/// A helper-owned path's audio, by frame parity, and its settings reader.
/// The callback writes `raw[slot]` and reads `processed[slot ^ 1]`; the
/// helper reads `raw[slot]` and writes `processed[slot]`; `slot` is the
/// parity of the frame being handed off. Each `processed` slot is its own
/// cell: the mixer holds raw pointers into one while the helper writes the
/// other, and the callback hands them out without a mutable borrow.
struct Work {
    /// The path's settings reader.
    reader: parameters::Reader,
    stereo: bool,
    raw: Box<[[[f32; FRAMES]; 2]; 2]>,
    processed: Box<[UnsafeCell<[[f32; FRAMES]; 2]>; 2]>,
    /// The frame whose complete audio the callback copied into `raw[slot]`.
    captured: [u64; 2],
    /// `processed[slot]` holds the DSP output for `raw[slot]`.
    done: [bool; 2],
    /// The mode of the helper's latest snapshot, for the callback.
    processed_mode: bool,
}
/// Everything the callback hands to the helper. The callback owns it while
/// the helper is idle; the helper from `queue` until it stores IDLE.
struct Handoff {
    seq: u64,
    frames: usize,
    cycle_start: u64,
    /// CLOCK_MONOTONIC when this frame's callback started (its `started`).
    started: u64,
    native_success: bool,
    jobs: Vec<Job>,
    /// By path; callback-owned paths have none.
    work: Vec<Option<Work>>,
    counters: Counters,
    /// The frame the helper finished last.
    done_seq: u64,
    /// Tests: the helper this frame is handed to, which must have stopped
    /// before the frame is freed (see `Drop for Pipeline`).
    #[cfg(test)]
    helper: Option<Arc<Shared>>,
}
#[cfg(test)]
impl Drop for Handoff {
    fn drop(&mut self) {
        if let Some(helper) = &self.helper {
            assert!(
                helper.gone.load(Ordering::SeqCst),
                "the frame was freed while its helper thread was still alive"
            );
        }
    }
}
#[derive(Clone, Copy)]
struct PinEntry {
    /// 0: an empty entry.
    pointer: usize,
    path: u32,
    channel: u32,
}
/// How a table turns a port handle into its first entry: drop `drop` low
/// bits, multiply, and take the index from bit `shift` of the product up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PinHash {
    drop: u32,
    shift: u32,
}
impl PinHash {
    /// What construction tries, keeping the first of those that cost least.
    ///
    /// JACK2 port handles are small consecutive port indices, not addresses.
    /// The first two hash the whole word, which spreads consecutive indices;
    /// they differ in which product bits they use, and so in which strides of
    /// aligned addresses they spread well. The last drops the low four bits
    /// first: the hash this table always had. It suits aligned allocations,
    /// and starts sixteen consecutive indices on one entry.
    const CANDIDATES: [Self; 3] = [
        Self { drop: 0, shift: 32 },
        Self { drop: 0, shift: 48 },
        Self { drop: 4, shift: 20 },
    ];
}
/// Which path and channel a native port belongs to: an open-addressed table,
/// built at construction and at most half full, so a search always ends. The
/// mixer asks this for every buffer it fetches, most of them unbound.
struct PinLookup {
    entries: Box<[PinEntry]>,
    /// Chosen at construction for the handles this client really has. Any
    /// choice finds the same pins; they differ only in entries read.
    hash: PinHash,
}
impl PinLookup {
    fn new(ports: &[Port]) -> Self {
        let pins: Vec<PinEntry> = ports
            .iter()
            .enumerate()
            .flat_map(|(path, port)| {
                port.pins
                    .iter()
                    .enumerate()
                    .map(move |(channel, pin)| PinEntry {
                        pointer: pin.pointer,
                        path: path as u32,
                        channel: channel as u32,
                    })
            })
            .collect();
        Self::with_pins(&pins)
    }
    /// Control work: builds the table once per candidate hash and keeps the
    /// cheapest to search.
    fn with_pins(pins: &[PinEntry]) -> Self {
        PinHash::CANDIDATES
            .into_iter()
            .map(|hash| Self::with_hash(pins, hash))
            .min_by_key(Self::cost)
            .expect("there are candidates")
    }
    fn with_hash(pins: &[PinEntry], hash: PinHash) -> Self {
        let empty = PinEntry {
            pointer: 0,
            path: 0,
            channel: 0,
        };
        let size = (pins.len() * 2 + 1).next_power_of_two();
        let mut table = Self {
            entries: vec![empty; size].into_boxed_slice(),
            hash,
        };
        for pin in pins {
            table.insert(pin.pointer, pin.path, pin.channel);
        }
        table
    }
    /// The entries a search reads before it reaches an empty one, summed over
    /// every entry it could start at: what the table costs to search, for
    /// bound and unbound handles alike.
    fn cost(&self) -> usize {
        (0..self.entries.len())
            .map(|start| {
                let mut index = start;
                let mut read = 1;
                while self.entries[index].pointer != 0 {
                    index = (index + 1) & self.mask();
                    read += 1;
                }
                read
            })
            .sum()
    }
    fn start(&self, pointer: usize) -> usize {
        let product = ((pointer as u64) >> self.hash.drop).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (product >> self.hash.shift) as usize & self.mask()
    }
    fn mask(&self) -> usize {
        self.entries.len() - 1
    }
    /// The first path to name a port keeps it.
    fn insert(&mut self, pointer: usize, path: u32, channel: u32) {
        let mut index = self.start(pointer);
        while self.entries[index].pointer != 0 {
            if self.entries[index].pointer == pointer {
                return;
            }
            index = (index + 1) & self.mask();
        }
        self.entries[index] = PinEntry {
            pointer,
            path,
            channel,
        };
    }
    fn find(&self, pointer: usize) -> Option<(usize, usize)> {
        let mut index = self.start(pointer);
        loop {
            let entry = self.entries[index];
            if entry.pointer == pointer && pointer != 0 {
                return Some((entry.path as usize, entry.channel as usize));
            }
            if entry.pointer == 0 {
                return None;
            }
            index = (index + 1) & self.mask();
        }
    }
}
/// Callback-only state.
struct Front {
    ports: Vec<Port>,
    lookup: PinLookup,
    /// By `cycle % METER_STRIDE`: in plan order, the paths a sparse cycle
    /// visits. That is every path that is processed or failed, and each other
    /// path once in the stride. A failed path must be visited, as it must not
    /// count frames. The lists keep their capacity, so rebuilding them does
    /// not allocate.
    visits: [Vec<usize>; METER_STRIDE as usize],
    /// Successful full-stride cycles still needed before going sparse. Set by
    /// anything that could change what a path needs. A whole stride, so that
    /// every path gets the cycles it would have had to acknowledge a setting.
    full_left: u32,
    /// This cycle visits every path.
    full: bool,
    /// The index into `visits` of this cycle.
    phase: usize,
    /// The publication count last acted on.
    publications: u64,
    /// `counters.faults` last acted on.
    faults: u64,
    /// Sparse cycles the mixer completed: what a path left alone is owed.
    skipped_frames: u64,
    counters: Counters,
    cycle: u32,
    /// This frame's sequence number; `seq - 1` is the frame the helper
    /// should have finished.
    seq: u64,
    /// The helper was idle when this callback began: the handoff may be
    /// used and this frame handed off.
    idle: bool,
    /// The helper finished the previous frame in time.
    previous_ready: bool,
    sampler: Sampler,
    /// The bench gauges are on (fixed at load): time each path, the buffer
    /// resolve and the helper wake. Kept here so that the callback tests a
    /// field of its own state rather than loading a shared flag per path.
    gauged: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub input_blocks: u64,
    pub output_blocks: u64,
    pub faults: u64,
    pub skipped_outputs: u64,
    pub sources: SourceCounts,
}
impl Counters {
    fn add(self, other: Self) -> Self {
        Self {
            input_blocks: self.input_blocks + other.input_blocks,
            output_blocks: self.output_blocks + other.output_blocks,
            faults: self.faults + other.faults,
            skipped_outputs: self.skipped_outputs + other.skipped_outputs,
            sources: self.sources.add(other.sources),
        }
    }
}
/// Per frame, for each source meant to be processed: what the mixer read.
/// Every raw outcome is a fallback to unprocessed audio, whatever its cause.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct SourceCounts {
    /// Every request got processed audio and the frame was acknowledged.
    pub processed: u64,
    /// Every request got processed audio, but the frame did not complete:
    /// the mixer's callback failed, a channel went unrequested, or the path
    /// was unusable at the end. Not a raw fallback, and not counted processed.
    pub processed_unacknowledged: u64,
    /// Helper still busy, or it had not finished the previous frame.
    pub raw_helper_unready: u64,
    /// No frame was handed to the helper last time: the first frame, the
    /// frame after a stall, inactive frame or switch to processing.
    pub raw_no_handoff: u64,
    /// The helper was handed the previous frame but produced no result for
    /// it (a mode change or DSP error under that frame).
    pub raw_not_done: u64,
    /// The path failed or was unusable this frame (settings fence, frame
    /// size, dead port, buffer mismatch).
    pub raw_invalid: u64,
    /// Meant to be processed, but the mixer never requested the buffer.
    pub unobserved: u64,
    /// Individual buffer requests answered with raw input, for any reason.
    /// A frame counts once above, under its first raw cause, even when an
    /// earlier request in it got processed audio.
    pub raw_returns: u64,
}
impl SourceCounts {
    fn add(self, other: Self) -> Self {
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
/// Live totals of `SourceCounts` across pipelines, for the native report. The
/// callback thread writes them, one per processed source each cycle, so they
/// share a line of their own.
struct SourceTotals {
    processed: AtomicU64,
    processed_unacknowledged: AtomicU64,
    raw_helper_unready: AtomicU64,
    raw_no_handoff: AtomicU64,
    raw_not_done: AtomicU64,
    raw_invalid: AtomicU64,
    unobserved: AtomicU64,
    raw_returns: AtomicU64,
}
static SOURCES: Line<SourceTotals> = Line(SourceTotals {
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
enum Outcome {
    Processed,
    ProcessedUnacknowledged,
    HelperUnready,
    NoHandoff,
    NotDone,
    Invalid,
    Unobserved,
}
fn tally(counts: &mut SourceCounts, outcome: Outcome) {
    let (count, total) = match outcome {
        Outcome::Processed => (&mut counts.processed, &SOURCES.processed),
        Outcome::ProcessedUnacknowledged => (
            &mut counts.processed_unacknowledged,
            &SOURCES.processed_unacknowledged,
        ),
        Outcome::HelperUnready => (&mut counts.raw_helper_unready, &SOURCES.raw_helper_unready),
        Outcome::NoHandoff => (&mut counts.raw_no_handoff, &SOURCES.raw_no_handoff),
        Outcome::NotDone => (&mut counts.raw_not_done, &SOURCES.raw_not_done),
        Outcome::Invalid => (&mut counts.raw_invalid, &SOURCES.raw_invalid),
        Outcome::Unobserved => (&mut counts.unobserved, &SOURCES.unobserved),
    };
    *count += 1;
    total.fetch_add(1, Ordering::Relaxed);
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
pub struct Pipeline {
    front: UnsafeCell<Front>,
    handoff: UnsafeCell<Handoff>,
    helper: Helper,
    /// Every path's settings publications, counted by its Control.
    publications: Arc<AtomicU64>,
}
// `front` belongs to the callback; the callback and the helper alternate
// ownership of `handoff` (see Handoff).
unsafe impl Sync for Pipeline {}
unsafe impl Send for Pipeline {}
impl Drop for Pipeline {
    /// The helper first. The fields drop in the order they are declared,
    /// `handoff` before `helper`: without this a helper that still had a
    /// frame (queued and left to its timer, or in the middle of one) would
    /// work on buffers already freed. Once it has stopped it touches nothing.
    fn drop(&mut self) {
        self.helper.stop();
    }
}
/// The meter of an unprocessed path: left channel then right, sample by
/// sample. Two plain loops rather than one flattened iterator, whose state
/// machine the compiler keeps in the loop; the order of every operation, and
/// so every bit of the result, is unchanged.
fn native_meter(buffers: [&[f32]; 2]) -> rc2fx_dsp::Meter {
    let mut meter = rc2fx_dsp::Meter::default();
    for buffer in buffers {
        for &sample in buffer {
            let value = f64::from(sample);
            if value.is_finite() {
                meter.input_peak = meter.input_peak.max(value.abs());
                meter.sum_squares += value * value;
                meter.clipped += u64::from(value.abs() >= 1.0);
            }
            meter.samples += 1;
        }
    }
    meter.output_peak = meter.input_peak;
    meter
}
fn copy(from: [&[f32]; 2], into: &mut [[f32; FRAMES]; 2], frames: usize) {
    for (channel, source) in from.into_iter().enumerate() {
        if !source.is_empty() {
            into[channel][..frames].copy_from_slice(source);
        }
    }
}
fn split(buffers: &mut [[f32; FRAMES]; 2], frames: usize, stereo: bool) -> [&mut [f32]; 2] {
    let [left, right] = buffers;
    [
        &mut left[..frames],
        if stereo {
            &mut right[..frames]
        } else {
            &mut []
        },
    ]
}
fn view(buffers: &[[f32; FRAMES]; 2], frames: usize, stereo: bool) -> [&[f32]; 2] {
    [
        &buffers[0][..frames],
        if stereo { &buffers[1][..frames] } else { &[] },
    ]
}
const IDLE: u8 = 0;
const QUEUED: u8 = 1;

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
    calls: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    bins: [AtomicU64; 8],
}
impl Fine {
    const BOUNDS_US: [u64; 7] = [1, 2, 4, 8, 16, 32, 64];
    const WIDE_BOUNDS_US: [u64; 7] = [16, 32, 64, 128, 256, 512, 1024];
    const fn new() -> Self {
        Self::with(Self::BOUNDS_US)
    }
    const fn wide() -> Self {
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
    fn record(&self, elapsed_ns: u64) {
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
    fn snapshot(&self) -> serde_json::Value {
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
static WAKE_CALL: Line<Fine> = Line(Fine::new());
static WAKE_SENT: Line<Fine> = Line(Fine::new());
static WAKES: Line<[AtomicU64; 2]> = Line([const { AtomicU64::new(0) }; 2]);
static END: Line<Fine> = Line(Fine::new());
static PLACE: Line<Fine> = Line(Fine::new());
static HELPER_SLACK: Line<Fine> = Line(Fine::wide());
static WAKE_LATENCY: Line<Fine> = Line(Fine::new());
static WOKEN_LATENCY: Line<Fine> = Line(Fine::new());
static SELF_LATENCY: Line<Fine> = Line(Fine::wide());
static TIMER_LATE: Line<Fine> = Line(Fine::new());
static HANDOFF_LATE: Line<Fine> = Line(Fine::wide());
static HANDOFF_EARLY: Line<Fine> = Line(Fine::wide());
static STARTS: Line<[AtomicU64; 5]> = Line([const { AtomicU64::new(0) }; 5]);
/// `STARTS` indices. Started by itself: the frame was already queued when the
/// helper first looked, or its timer ended the sleep. Started by the
/// callback's wake: out of the timed sleep, out of the park that followed a
/// timer with nothing queued, or out of a park without a forecast.
const STARTS_QUEUED: usize = 0;
const STARTS_TIMER: usize = 1;
const STARTS_WOKEN_EARLY: usize = 2;
const STARTS_PARKED_AFTER_TIMER: usize = 3;
const STARTS_PARKED_NO_FORECAST: usize = 4;
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
/// The longest a callback waits for a busy helper before passing its frame
/// through.
const HELPER_WAIT_NS: u64 = 200_000;
/// One callback period.
const PERIOD_NS: u64 = super::timing::BUDGET_NS;
/// `Shared::wake_at` while the helper runs, or is about to look at `state`.
const AWAKE: u64 = 0;
/// `Shared::wake_at` while the helper is parked with no timer: only the
/// callback's wake starts it.
const PARKED: u64 = u64::MAX;
/// How long after the forecast end of the next callback the helper's timer
/// wakes it, so that the frame is normally queued by then and the callback
/// made no system call for it (on the mixer the wake cost the callback 12 us
/// at its very end, every cycle).
///
/// Measured on the mixer (2026-11-12, 10-minute blocks with the denoiser
/// hold at 2.5 ms): 50, 100 and 200 us all saved the callback the same 10 to
/// 11 us against the callback's wake, and 50 us had the fewest callbacks that
/// started late; at 400 us the timer is set past LATEST_SELF_START_NS, so
/// the callback wakes the helper every time and nothing is saved. A timer
/// that fires before the handoff finds nothing and leaves the helper parked
/// for the callback to wake, as before. The helper starts about 50 us after
/// the handoff instead of the 27 us a wake took.
pub(crate) const WAKE_MARGIN_NS: u64 = 50_000;
/// The latest the helper may start a frame by itself, from the start of the
/// callback that hands the frame off. The callback wakes a helper whose timer
/// is set later than this, as it wakes a parked one.
///
/// Callbacks start a period apart, and never less than half a period apart
/// in the 67 million of one 43-hour run (13,366 between 0.5 and 0.9 of a
/// period, against 14,174 between 1.1 and 1.5: mostly a late callback and
/// the short gap after it, and a late callback starts the forecast over).
/// A helper that starts by this bound and takes its usual 240 us (213 us for
/// the frame, 27 us to get its core) is done 990 us before a callback one
/// period later, and 173 us after the start of one half a period later: less
/// than that callback waits (HELPER_WAIT_NS), so its frame is not passed
/// through unprocessed. It is also no later than the wake from a callback
/// that ends 1100 us after its start, which is an ordinary callback today.
///
/// That holds for the usual frame only. A frame longer than 240 us, met by a
/// callback half a period early, is passed through where a woken helper would
/// have had it done (it started some 200 us sooner): the price of the timer,
/// to be judged on the mixer by `helper.stalls` and `helperSlack`.
const LATEST_SELF_START_NS: u64 = PERIOD_NS / 2 + HELPER_WAIT_NS - 266_666;
/// A callback that ends further than this from one period after the last
/// counts as irregular (a late start, a skipped or doubled cycle, a restart):
/// the forecast starts over from it. A tenth of a period, the band the
/// callback gap counters use.
const IRREGULAR_NS: u64 = PERIOD_NS / 10;
/// Regular callbacks in a row before the helper sleeps on a forecast (37 ms).
/// Until then it parks, and every callback wakes it.
const FORECAST_WARMUP: u32 = 16;

/// Where the next callback will end, from where the last ones did.
///
/// The callbacks' ends lie one period apart, give or take the jitter of the
/// cycle's start and of the callback's length; the anchor follows them with
/// a weight of an eighth for each new one. Anything irregular starts it over,
/// and the helper then parks as it always did.
#[derive(Default, Debug)]
struct Forecast {
    /// The smoothed end of the latest callback, 0 with no history.
    anchor: u64,
    /// Regular callbacks in a row.
    streak: u32,
    /// When the callback of the frame before started.
    previous: u64,
    /// The latest callback's end after (positive) or before the forecast, for
    /// the bench gauges; None for a frame that had no forecast to compare.
    deviation: Option<i64>,
}
impl Forecast {
    /// After a frame: `begun` is when its callback started, `ended` the
    /// latest end stamp published (`Shared::ended`). Returns when the helper
    /// should wake by itself for the next frame, or None to park.
    fn observe(&mut self, begun: u64, ended: u64) -> Option<u64> {
        // A callback publishes its end after it has handed its frame off, so
        // a short frame can be over first, and the stamp still that of the
        // callback before: this one is then taken to be as long as that one.
        // A stamp is a callback's own between its start and one period on;
        // anything else (no stamp, a callback that found the helper busy in
        // between) is no sample.
        let own = |start: u64| start != 0 && ended > start && ended - start < PERIOD_NS;
        let length = if own(begun) {
            Some(ended - begun)
        } else if begun != 0 && own(self.previous) {
            Some(ended - self.previous)
        } else {
            None
        };
        self.previous = begun;
        self.deviation = None;
        let expected = self.anchor.saturating_add(PERIOD_NS);
        match length.map(|length| begun + length) {
            Some(end) if self.anchor != 0 && end.abs_diff(expected) <= IRREGULAR_NS => {
                self.deviation = Some(if end >= expected {
                    (end - expected) as i64
                } else {
                    -((expected - end) as i64)
                });
                self.anchor = if end >= expected {
                    expected + (end - expected) / 8
                } else {
                    expected - (expected - end) / 8
                };
                self.streak = self.streak.saturating_add(1);
            }
            // The first callback, or an irregular one.
            Some(end) => {
                self.anchor = end;
                self.streak = 0;
            }
            None => {
                self.anchor = 0;
                self.streak = 0;
            }
        }
        if self.streak < FORECAST_WARMUP {
            return None;
        }
        let wake_at = self.anchor + PERIOD_NS + WAKE_MARGIN_NS;
        // Not when the next callback would have to wake the helper anyway.
        (wake_at <= begun + PERIOD_NS + LATEST_SELF_START_NS).then_some(wake_at)
    }
}

/// Whether a callback that started at `begun` must wake a helper that
/// published `wake_at`: when it is parked, or sleeps towards a wake-up later
/// than it may start by itself. A helper that is awake finds the frame.
fn must_wake(wake_at: u64, begun: u64) -> bool {
    wake_at == PARKED || (wake_at != AWAKE && wake_at > begun.saturating_add(LATEST_SELF_START_NS))
}
/// What `Helper::queue` did about the helper's sleep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Woke {
    /// Nothing: no system call.
    No,
    /// Woke it out of its park.
    Parked,
    /// Woke it out of a timed sleep that would have ended too late.
    Late,
}
/// How the helper came by a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Start {
    /// It was queued when the helper looked, before any sleep.
    Queued,
    /// During the timed sleep towards this wake-up: at its end, or earlier if
    /// the callback woke the helper.
    Slept(u64),
    /// While parked; `expired`: after a timed sleep that ended with nothing
    /// queued.
    Parked { expired: bool },
}
/// The helper's clock and sleeps, which the tests replace.
trait Pace {
    /// Once, on the helper thread, before its first sleep.
    fn prepare(&self) {}
    /// CLOCK_MONOTONIC in nanoseconds.
    fn now(&self) -> u64;
    /// Sleeps until `wake_at` at the latest. `Thread::unpark` ends it, and it
    /// may end early for no reason.
    fn sleep_until(&self, wake_at: u64);
    /// Sleeps until `Thread::unpark`; it may end for no reason.
    fn park(&self);
    /// After each frame, once it is handed back and before the helper turns
    /// to its sleep. Nothing here; a test takes its time, so that the next
    /// handoff meets every step of the way to sleep.
    fn rest(&self) {}
    /// Whether the timed sleep is on time only for a real-time thread: the
    /// helper then parks, as it always did, until it has been made one.
    fn needs_realtime(&self) -> bool {
        false
    }
    /// Whether a thread that has `policy` after taking its placement is a
    /// real-time one. The kernel's rule; a test, which cannot make its
    /// threads real-time, says which of its placements count.
    fn realtime(&self, policy: i32) -> bool {
        matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR)
    }
}
/// The mixer's: a futex wait, with the kernel's timer for the timed one. For
/// a real-time thread on PREEMPT_RT that timer is a hard interrupt on the
/// core the helper went to sleep on.
struct Kernel;
impl Pace for Kernel {
    fn prepare(&self) {
        // A futex wait's timeout is rounded by the thread's timer slack,
        // 50 us by default on this kernel even for a real-time thread.
        unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, 1 as libc::c_ulong) };
    }
    fn now(&self) -> u64 {
        super::timing::monotonic()
    }
    fn sleep_until(&self, wake_at: u64) {
        let left = wake_at.saturating_sub(super::timing::monotonic());
        if left != 0 {
            thread::park_timeout(std::time::Duration::from_nanos(left));
        }
    }
    fn park(&self) {
        thread::park();
    }
    fn needs_realtime(&self) -> bool {
        // An ordinary thread's timer waits for the kernel's timer thread,
        // which every real-time thread on that core holds up: so not before
        // the helper has taken its placement (the first 64 callbacks or
        // more). The hosts the tests run on have no such kernel, and give
        // the helper no real-time placement.
        !cfg!(test)
    }
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
/// The per-pipeline `Counters` are only summed into the audit's
/// `inputBlocks`/`outputBlocks`/`dspFaults`/`skippedOutputs` when a client
/// closes (`Pipeline::counters` waits for the helper, so it must never be called
/// from the callback or while reporting). These atomics mirror the same four
/// events at their existing increment sites, so an audit can read what the DSP
/// is doing while callbacks run. One relaxed increment per actual block or
/// event; no clock, allocation, lock or I/O on the audio thread. They are never
/// summed at retirement and never reset, so they already include retired
/// pipelines: do not add the retirement-only fields to them.
///
/// The helper writes `LIVE_INPUT_BLOCKS` and the callback writes
/// `LIVE_OUTPUT_BLOCKS`, once per processed block: each has its own line.
pub static LIVE_INPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_OUTPUT_BLOCKS: Line<AtomicU64> = Line(AtomicU64::new(0));
pub static LIVE_FAULTS: AtomicU64 = AtomicU64::new(0);
pub static LIVE_SKIPPED_OUTPUTS: AtomicU64 = AtomicU64::new(0);
/// Hardware outputs that got no result from their own DSP for this frame, so an
/// untreated (unprocessed) block was handed on instead: `noFrame` when no copied
/// slot was available (first frame after construction, an inactive frame, a
/// gap), `rawFrame` when the copied slot was written back instead of processed
/// audio (the helper was late, or the frame before failed).
///
/// These are fallback deliveries, not the DSP's error counter: a frame can be
/// counted here with `dspFaults` unchanged, and a DSP error can happen in a frame
/// whose delivery is counted here too. Whether such a frame is audible is NOT
/// established. Outside a planned warm-up an increment is a qualification
/// failure of that frame, not something to dismiss. `rawFrame` does not prove
/// the slot held the actual captured mixer block: the first frame's slot is
/// still zero-initialised (`captured` starts at 0), so startup can select an
/// unwritten slot. Counted once per hardware-output frame without a result.
pub static LIVE_OUTPUT_NO_FRAME: AtomicU64 = AtomicU64::new(0);
pub static LIVE_OUTPUT_RAW_FRAME: AtomicU64 = AtomicU64::new(0);

/// The audit's live view (see `LIVE_*`). Read-only, lock-free, no clocks.
pub fn processing_counters() -> serde_json::Value {
    let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
    serde_json::json!({
        "scope": "live cumulative since process load, all contexts, read with relaxed atomics; includes retired pipelines, so do not add the retirement-only inputBlocks/outputBlocks/dspFaults/skippedOutputs to these",
        "inputBlocks": read(&LIVE_INPUT_BLOCKS),
        "outputBlocks": read(&LIVE_OUTPUT_BLOCKS),
        "dspFaults": read(&LIVE_FAULTS),
        "skippedOutputs": read(&LIVE_SKIPPED_OUTPUTS),
        "hardwareOutputsWithoutResult": {
            "noCopiedFrame": read(&LIVE_OUTPUT_NO_FRAME),
            "copiedFrameRaw": read(&LIVE_OUTPUT_RAW_FRAME),
            "note": "frames whose own DSP result was missing: an untreated block was handed on. NOT the dspFaults error counter; audibility not established; outside planned warm-up an increment is a qualification failure; copiedFrameRaw may be a zero-initialised startup slot, not necessarily the captured mixer frame",
        },
    })
}

/// The mixer's audio cores (docs/reference/hardware.md). The helper runs on those the
/// callback is not pinned to, one real-time priority below it: RØDE's own
/// real-time threads there are short, so the helper is rarely kept waiting,
/// and it never competes with the callback for its core.
const AUDIO_CPUS: u64 = 0b1110;
/// Callbacks between re-reads of the callback thread's placement. RØDE pins
/// and reprioritises that thread after its first callbacks have run.
const PLACE_EVERY: u32 = 2048;
/// Until the callback is real-time on its own cores and the helper has taken
/// the placement derived from it, re-read every this many callbacks instead
/// (150 ms), for at most PLACE_FAST_UNTIL callbacks (19 s).
const PLACE_FAST_EVERY: u32 = 64;
const PLACE_FAST_UNTIL: u32 = 8192;
/// When the callback samples its placement (see Helper::place).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Sampler {
    settled: bool,
    /// The first boundary not yet sampled: its callback had no handoff.
    pending: Option<u32>,
    /// Samples taken after their boundary's callback.
    deferred: u64,
}
impl Sampler {
    /// Every callback: note a boundary, with or without a handoff. Later
    /// boundaries before the sample do not replace the first.
    fn mark(&mut self, cycle: u32) {
        if self.pending.is_none() && self.due(cycle) {
            self.pending = Some(cycle);
        }
    }
    /// With a handoff: whether to sample now (the boundary's own callback
    /// or the first one after it that hands off), and if so whether late.
    fn take(&mut self, cycle: u32) -> Option<bool> {
        let deferred = self.pending.take()? != cycle;
        if deferred {
            self.deferred += 1;
            PLACEMENT_DEFERRED.fetch_add(1, Ordering::Relaxed);
        }
        Some(deferred)
    }
    fn due(&self, cycle: u32) -> bool {
        let every = if !self.settled && cycle < PLACE_FAST_UNTIL {
            PLACE_FAST_EVERY
        } else {
            PLACE_EVERY
        };
        cycle.is_multiple_of(every)
    }
    /// After a sample: settled once the callback is real-time, leaves the
    /// helper some audio core of its own, and the helper has applied the
    /// placement of the latest generation.
    fn observe(&mut self, policy: i32, cpus: u64, generation: u64, applied: u64) {
        let realtime = matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR);
        let separate = AUDIO_CPUS & !cpus != 0;
        if realtime && separate && generation != 0 && applied == generation {
            self.settled = true;
        }
    }
}
/// The first placement changes since load, across pipelines, for the report:
/// what the callback sampled, and what the helper then requested and got.
/// Each record is claimed once and never reused.
const PLACEMENT_HISTORY: usize = 16;
static PLACEMENT_NEXT: AtomicUsize = AtomicUsize::new(0);
/// Pipelines created, to tell their records apart.
static PIPELINE_IDS: AtomicU64 = AtomicU64::new(0);
struct PlacementRecord {
    /// Written last by the callback: the generation, 0 for an empty slot.
    generation: AtomicU64,
    pipeline: AtomicU64,
    cycle: AtomicU64,
    /// Sampled after its boundary, whose callback had no handoff.
    deferred: AtomicBool,
    at_ns: AtomicU64,
    cpus: AtomicU64,
    policy: AtomicI64,
    priority: AtomicI64,
    /// Written by the helper when it applies this generation: what it
    /// requested, then its actual mask, policy and priority read back.
    applied_at_ns: AtomicU64,
    requested_cpus: AtomicU64,
    applied_cpus: AtomicU64,
    applied_policy: AtomicI64,
    applied_priority: AtomicI64,
    /// sched_setaffinity and pthread_setschedparam results (0 is success).
    affinity_result: AtomicI64,
    sched_result: AtomicI64,
}
static PLACEMENTS: [PlacementRecord; PLACEMENT_HISTORY] = [const {
    PlacementRecord {
        generation: AtomicU64::new(0),
        pipeline: AtomicU64::new(0),
        cycle: AtomicU64::new(0),
        deferred: AtomicBool::new(false),
        at_ns: AtomicU64::new(0),
        cpus: AtomicU64::new(0),
        policy: AtomicI64::new(-1),
        priority: AtomicI64::new(-1),
        applied_at_ns: AtomicU64::new(0),
        requested_cpus: AtomicU64::new(0),
        applied_cpus: AtomicU64::new(0),
        applied_policy: AtomicI64::new(-1),
        applied_priority: AtomicI64::new(-1),
        affinity_result: AtomicI64::new(i64::MIN),
        sched_result: AtomicI64::new(i64::MIN),
    }
}; PLACEMENT_HISTORY];
/// Placement samples taken, and the cycle at which sampling settled (0: not).
static PLACEMENT_SAMPLES: AtomicU64 = AtomicU64::new(0);
static PLACEMENT_DEFERRED: AtomicU64 = AtomicU64::new(0);
static PLACEMENT_SETTLED_CYCLE: AtomicU64 = AtomicU64::new(0);
struct Shared {
    state: AtomicU8,
    handoff: AtomicPtr<Handoff>,
    stop: AtomicBool,
    /// CLOCK_MONOTONIC when the helper began and finished its latest frame.
    started: AtomicU64,
    finished: AtomicU64,
    /// Bench gauges only: CLOCK_MONOTONIC just before the callback handed
    /// off the frame now queued. 0 while the gauges are off.
    queued_at: AtomicU64,
    /// What the helper is doing about its next frame: AWAKE, PARKED, or the
    /// CLOCK_MONOTONIC time its timer ends its sleep (see `await_frame`).
    wake_at: AtomicU64,
    /// CLOCK_MONOTONIC at the end of the latest callback (`Pipeline::ended`),
    /// 0 before the first: what the helper forecasts the next handoff from.
    ended: AtomicU64,
    /// Every frame the helper ran, in order: its sequence number, how it
    /// came by it and when it started.
    #[cfg(test)]
    ran: std::sync::Mutex<Vec<(u64, Start, u64)>>,
    /// Set when the helper's thread has returned from its loop.
    #[cfg(test)]
    gone: AtomicBool,
    /// The callback thread's CPUs, policy and priority; `generation` counts
    /// changes, and the helper derives its placement from each one.
    generation: AtomicU64,
    /// The generation whose placement the helper last applied.
    applied: AtomicU64,
    /// PLACEMENTS index of the latest generation's record, or usize::MAX.
    record: AtomicUsize,
    id: u64,
    cpus: AtomicU64,
    policy: AtomicI32,
    priority: AtomicI32,
}
/// Callback and helper thread placement, for the report: [tid, cpu, policy,
/// priority] each, from the pipeline that last sampled them.
static CALLBACK_PLACEMENT: [AtomicI64; 4] = [const { AtomicI64::new(-1) }; 4];
static HELPER_PLACEMENT: [AtomicI64; 4] = [const { AtomicI64::new(-1) }; 4];
pub fn placement() -> serde_json::Value {
    let read = |values: &[AtomicI64; 4]| {
        super::probe::placement_json(std::array::from_fn(|i| values[i].load(Ordering::Relaxed)))
    };
    let history: Vec<_> = PLACEMENTS
        .iter()
        .filter(|record| record.generation.load(Ordering::Acquire) != 0)
        .map(|record| {
            let get = |value: &AtomicU64| value.load(Ordering::Relaxed);
            let geti = |value: &AtomicI64| value.load(Ordering::Relaxed);
            serde_json::json!({
                "pipeline": get(&record.pipeline),
                "generation": get(&record.generation),
                "cycle": get(&record.cycle),
                "deferred": record.deferred.load(Ordering::Relaxed),
                "atNs": get(&record.at_ns),
                "callbackCpus": get(&record.cpus),
                "callbackPolicy": geti(&record.policy),
                "callbackPriority": geti(&record.priority),
                "appliedAtNs": get(&record.applied_at_ns),
                "helperRequestedCpus": get(&record.requested_cpus),
                "helperCpus": get(&record.applied_cpus),
                "helperPolicy": geti(&record.applied_policy),
                "helperPriority": geti(&record.applied_priority),
                "affinityResult": geti(&record.affinity_result),
                "schedResult": geti(&record.sched_result),
            })
        })
        .collect();
    serde_json::json!({
        "callback": read(&CALLBACK_PLACEMENT),
        "helper": read(&HELPER_PLACEMENT),
        "samples": PLACEMENT_SAMPLES.load(Ordering::Relaxed),
        "deferred": PLACEMENT_DEFERRED.load(Ordering::Relaxed),
        "settledCycle": PLACEMENT_SETTLED_CYCLE.load(Ordering::Relaxed),
        "history": history,
    })
}
fn store_placement(target: &[AtomicI64; 4]) {
    for (slot, value) in target.iter().zip(super::probe::placement()) {
        slot.store(value, Ordering::Relaxed);
    }
}
impl Shared {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            handoff: AtomicPtr::new(std::ptr::null_mut()),
            stop: AtomicBool::new(false),
            started: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            queued_at: AtomicU64::new(0),
            wake_at: AtomicU64::new(AWAKE),
            ended: AtomicU64::new(0),
            #[cfg(test)]
            ran: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            gone: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            record: AtomicUsize::new(usize::MAX),
            id: PIPELINE_IDS.fetch_add(1, Ordering::Relaxed) + 1,
            cpus: AtomicU64::new(0),
            policy: AtomicI32::new(0),
            priority: AtomicI32::new(0),
        }
    }
}
struct Helper {
    shared: Arc<Shared>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Helper {
    fn start() -> Result<Self, String> {
        Self::start_with(Kernel)
    }
    fn start_with<P: Pace + Send + 'static>(pace: P) -> Result<Self, String> {
        let shared = Arc::new(Shared::new());
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("rc2fx-dsp".into())
            .spawn(move || {
                pace.prepare();
                helper_loop(&worker, &pace);
                #[cfg(test)]
                worker.gone.store(true, Ordering::SeqCst);
            })
            .map_err(|error| format!("native helper thread: {error}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    fn idle(&self) -> bool {
        self.shared.state.load(Ordering::Acquire) == IDLE
    }
    /// Spin until the helper finishes, at most `limit` ns. The helper runs on
    /// another core, so spinning here does not hold it up.
    fn wait(&self, limit: u64) -> bool {
        let deadline = super::timing::monotonic().saturating_add(limit);
        while !self.idle() {
            if super::timing::monotonic() >= deadline {
                return false;
            }
            std::hint::spin_loop();
        }
        true
    }
    /// From the callback thread, when `sampler` says so (every
    /// PLACE_FAST_EVERY callbacks until settled, then every PLACE_EVERY; a
    /// boundary without a handoff at the next callback with one, since the
    /// helper reads the placement only when it starts a queued frame):
    /// record its placement for the helper, which follows any change.
    fn place(&self, cycle: u32, sampler: &mut Sampler) {
        let Some(deferred) = sampler.take(cycle) else {
            return;
        };
        PLACEMENT_SAMPLES.fetch_add(1, Ordering::Relaxed);
        store_placement(&CALLBACK_PLACEMENT);
        {
            let (mask, policy, priority) = current_placement();
            // As before: an unreadable policy counts as ordinary.
            let (policy, priority) = if policy < 0 {
                (libc::SCHED_OTHER, 0)
            } else {
                (policy, priority)
            };
            let param = libc::sched_param {
                sched_priority: priority,
            };
            let shared = &self.shared;
            if shared.cpus.load(Ordering::Relaxed) != mask
                || shared.policy.load(Ordering::Relaxed) != policy
                || shared.priority.load(Ordering::Relaxed) != param.sched_priority
                || shared.generation.load(Ordering::Relaxed) == 0
            {
                shared.cpus.store(mask, Ordering::Relaxed);
                shared.policy.store(policy, Ordering::Relaxed);
                shared
                    .priority
                    .store(param.sched_priority, Ordering::Relaxed);
                let generation = shared.generation.fetch_add(1, Ordering::Release) + 1;
                // Claim a record only while any are left.
                let index = PLACEMENT_NEXT.load(Ordering::Relaxed);
                let claimed = index < PLACEMENT_HISTORY
                    && PLACEMENT_NEXT
                        .compare_exchange(index, index + 1, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok();
                shared
                    .record
                    .store(if claimed { index } else { usize::MAX }, Ordering::Release);
                if let Some(record) = claimed.then(|| &PLACEMENTS[index]) {
                    record.pipeline.store(shared.id, Ordering::Relaxed);
                    record.cycle.store(u64::from(cycle), Ordering::Relaxed);
                    record.deferred.store(deferred, Ordering::Relaxed);
                    record
                        .at_ns
                        .store(super::timing::monotonic(), Ordering::Relaxed);
                    record.cpus.store(mask, Ordering::Relaxed);
                    record.policy.store(i64::from(policy), Ordering::Relaxed);
                    record
                        .priority
                        .store(i64::from(param.sched_priority), Ordering::Relaxed);
                    record.generation.store(generation, Ordering::Release);
                }
            }
            let settled = sampler.settled;
            sampler.observe(
                policy,
                mask,
                shared.generation.load(Ordering::Relaxed),
                shared.applied.load(Ordering::Acquire),
            );
            if sampler.settled && !settled {
                PLACEMENT_SETTLED_CYCLE.store(u64::from(cycle), Ordering::Relaxed);
            }
        }
    }
    /// Hands the frame to the helper, and wakes it only if it would not start
    /// in time by itself (`must_wake`): normally it sleeps towards a timer set
    /// just after this call, and the callback makes no system call. `begun`:
    /// when this callback started. `queued_at`: with the bench gauges on, the
    /// time of this call, for the helper's wake latency; 0 otherwise, and then
    /// nothing is stored.
    ///
    /// QUEUED is stored before `wake_at` is read, both SeqCst: `await_frame`
    /// has the other half and the reason.
    fn queue(&self, handoff: *mut Handoff, begun: u64, queued_at: u64) -> Woke {
        let shared = &*self.shared;
        shared.handoff.store(handoff, Ordering::Relaxed);
        if queued_at != 0 {
            // Before QUEUED, so that the helper never reads an older stamp.
            shared.queued_at.store(queued_at, Ordering::Relaxed);
        }
        shared.state.store(QUEUED, Ordering::SeqCst);
        let wake_at = shared.wake_at.load(Ordering::SeqCst);
        if !must_wake(wake_at, begun) {
            return Woke::No;
        }
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
        if wake_at == PARKED {
            Woke::Parked
        } else {
            Woke::Late
        }
    }
}
impl Helper {
    /// Tells the helper to stop and waits until its thread has returned: from
    /// then on it touches no frame. Control only, after callbacks have stopped.
    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Waits for the next frame: asleep until `wake_at` if there is a forecast,
/// then parked. None when the helper is to stop: it then starts no frame,
/// queued or not, since the pipeline that owns the frames is going.
///
/// No wake is lost. Before every sleep the helper stores what it is about to
/// do in `wake_at` and then reads `state`; the callback stores QUEUED in
/// `state` and then reads `wake_at` (`Helper::queue`). All four are SeqCst,
/// so they have one order, and in it either the helper's read comes after the
/// callback's store, and the helper sees QUEUED and does not sleep; or it
/// comes before, and then the helper's store, which is earlier still, comes
/// before the callback's read. The callback then sees exactly the sleep the
/// helper is entering: PARKED, and it unparks; or a timer, and it unparks
/// unless that timer is soon enough (`must_wake`), in which case the timer
/// itself ends the sleep. An unpark that comes before the sleep begins is
/// kept by the thread's token and ends it at once. With a weaker ordering
/// both sides could read the other's old value: the helper parks, the
/// callback does not wake it, and the frame waits for a wake that never comes.
///
/// No frame runs twice: the helper runs one only on reading QUEUED, stores
/// IDLE when it is done, and only the callback stores QUEUED, after reading
/// IDLE. A wake with nothing queued sends the helper back to sleep.
fn await_frame<P: Pace>(shared: &Shared, pace: &P, wake_at: Option<u64>) -> Option<Start> {
    let mut slept = false;
    if let Some(wake_at) = wake_at {
        loop {
            shared.wake_at.store(wake_at, Ordering::SeqCst);
            if shared.stop.load(Ordering::Acquire) {
                return None;
            }
            if shared.state.load(Ordering::SeqCst) == QUEUED {
                return Some(if slept {
                    Start::Slept(wake_at)
                } else {
                    Start::Queued
                });
            }
            // The timer has ended the sleep with nothing queued (the callback
            // is late, or there is none): park. Also if the clock makes no
            // sense: a forecast is never two periods ahead.
            let now = pace.now();
            if now >= wake_at || wake_at - now > 2 * PERIOD_NS {
                break;
            }
            // Also ended by the callback's wake, by an older wake's token, or
            // for no reason: each time round looks again.
            pace.sleep_until(wake_at);
            slept = true;
        }
    }
    let expired = slept;
    let mut parked = false;
    loop {
        shared.wake_at.store(PARKED, Ordering::SeqCst);
        if shared.stop.load(Ordering::Acquire) {
            return None;
        }
        if shared.state.load(Ordering::SeqCst) == QUEUED {
            return Some(if parked || expired {
                Start::Parked { expired }
            } else {
                Start::Queued
            });
        }
        pace.park();
        parked = true;
    }
}
fn helper_loop<P: Pace>(shared: &Shared, pace: &P) {
    let mut placed = 0;
    // Whether the placement the helper last took left it real-time.
    let mut realtime = false;
    let mut forecast = Forecast::default();
    let mut wake_at = None;
    loop {
        let Some(start) = await_frame(shared, pace, wake_at) else {
            return;
        };
        shared.wake_at.store(AWAKE, Ordering::SeqCst);
        let generation = shared.generation.load(Ordering::Acquire);
        if generation != placed {
            let taken = take_placement(shared);
            realtime = pace.realtime(taken.policy);
            if let Some(record) = PLACEMENTS.get(shared.record.load(Ordering::Acquire))
                && record.generation.load(Ordering::Acquire) == generation
                && record.pipeline.load(Ordering::Relaxed) == shared.id
            {
                record
                    .applied_at_ns
                    .store(super::timing::monotonic(), Ordering::Relaxed);
                record
                    .requested_cpus
                    .store(taken.requested_cpus, Ordering::Relaxed);
                record.applied_cpus.store(taken.cpus, Ordering::Relaxed);
                record
                    .applied_policy
                    .store(i64::from(taken.policy), Ordering::Relaxed);
                record
                    .applied_priority
                    .store(i64::from(taken.priority), Ordering::Relaxed);
                record
                    .affinity_result
                    .store(i64::from(taken.affinity), Ordering::Relaxed);
                record
                    .sched_result
                    .store(i64::from(taken.sched), Ordering::Relaxed);
            }
            shared.applied.store(generation, Ordering::Release);
            store_placement(&HELPER_PLACEMENT);
            placed = generation;
        }
        let started = pace.now();
        shared.started.store(started, Ordering::Relaxed);
        let queued_at = shared.queued_at.load(Ordering::Relaxed);
        if queued_at != 0 {
            let latency = started.saturating_sub(queued_at);
            WAKE_LATENCY.record(latency);
            let (kind, woken) = match start {
                Start::Queued => (STARTS_QUEUED, false),
                Start::Slept(wake_at) if started >= wake_at => {
                    TIMER_LATE.record(started - wake_at);
                    (STARTS_TIMER, false)
                }
                Start::Slept(_) => (STARTS_WOKEN_EARLY, true),
                Start::Parked { expired: true } => (STARTS_PARKED_AFTER_TIMER, true),
                Start::Parked { expired: false } => (STARTS_PARKED_NO_FORECAST, true),
            };
            STARTS[kind].fetch_add(1, Ordering::Relaxed);
            if woken {
                WOKEN_LATENCY.record(latency);
            } else {
                SELF_LATENCY.record(latency);
            }
        }
        // SAFETY: the callback handed the frame over with QUEUED and does not
        // touch it again until it observes IDLE.
        let handoff = unsafe { &mut *shared.handoff.load(Ordering::Relaxed) };
        #[cfg(test)]
        shared
            .ran
            .lock()
            .unwrap()
            .push((handoff.seq, start, started));
        let slot = (handoff.seq & 1) as usize;
        let left_alone = if handoff.native_success {
            Job::Count
        } else {
            Job::None
        };
        for job in handoff.jobs.iter_mut().filter(|job| **job == Job::Skipped) {
            *job = left_alone;
        }
        // Sources first: the next callback reads them before it writes the
        // hardware outputs at its end.
        for wanted in [Job::Input, Job::Output, Job::Meter, Job::Count, Job::None] {
            for (work, job) in handoff.work.iter_mut().zip(&handoff.jobs) {
                if let Some(work) = work
                    && *job == wanted
                {
                    work.run(
                        *job,
                        slot,
                        handoff.frames,
                        handoff.native_success,
                        &mut handoff.counters,
                    );
                }
            }
        }
        handoff.jobs.fill(Job::Skipped);
        handoff.done_seq = handoff.seq;
        let finished = pace.now();
        let spent = finished.saturating_sub(started);
        shared.finished.store(finished, Ordering::Relaxed);
        HELPER_TIME.frames.fetch_add(1, Ordering::Relaxed);
        HELPER_TIME.total_ns.fetch_add(spent, Ordering::Relaxed);
        HELPER_TIME.max_ns.fetch_max(spent, Ordering::Relaxed);
        if handoff.cycle_start != 0 {
            let position = finished.saturating_sub(handoff.cycle_start);
            super::probe::HELPER_POS.record(position);
            if position >= super::probe::HELPER_LATE_NS {
                super::probe::HELPER_LATE_FINISHES.fetch_add(1, Ordering::Relaxed);
                super::probe::record(super::probe::Kind::HelperLateFinish, position);
            }
        }
        // While the frame is still the helper's: when to wake for the next.
        wake_at = forecast.observe(handoff.started, shared.ended.load(Ordering::Relaxed));
        if !realtime && pace.needs_realtime() {
            wake_at = None;
        }
        if queued_at != 0
            && let Some(deviation) = forecast.deviation
        {
            let gauge = if deviation >= 0 {
                &HANDOFF_LATE
            } else {
                &HANDOFF_EARLY
            };
            gauge.record(deviation.unsigned_abs());
        }
        shared.state.store(IDLE, Ordering::Release);
        pace.rest();
    }
}
/// Best effort: unprivileged hosts (tests) keep ordinary scheduling.
/// The helper's cores for a callback on `callback`: the audio cores the
/// callback is not on, else (callback not pinned yet, or on every audio core,
/// or its mask unknown) all of them. Never CPU 0, the suppression worker's.
fn helper_mask(callback: u64) -> u64 {
    let mask = AUDIO_CPUS & !callback;
    if mask == 0 { AUDIO_CPUS } else { mask }
}
/// What the helper requested and then actually had, and the two calls'
/// results (0 is success).
struct Taken {
    requested_cpus: u64,
    cpus: u64,
    policy: i32,
    priority: i32,
    affinity: i32,
    sched: i32,
}
/// This thread's affinity mask, policy and priority (0, -1, -1 if unreadable).
fn current_placement() -> (u64, i32, i32) {
    unsafe {
        let mut cpus: libc::cpu_set_t = std::mem::zeroed();
        let mut mask = 0u64;
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpus) == 0 {
            for cpu in 0..64 {
                if libc::CPU_ISSET(cpu, &cpus) {
                    mask |= 1 << cpu;
                }
            }
        }
        let mut policy = -1;
        let mut param: libc::sched_param = std::mem::zeroed();
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) != 0 {
            return (mask, -1, -1);
        }
        (mask, policy, param.sched_priority)
    }
}
fn take_placement(shared: &Shared) -> Taken {
    unsafe {
        let mask = helper_mask(shared.cpus.load(Ordering::Relaxed));
        let mut cpus: libc::cpu_set_t = std::mem::zeroed();
        for cpu in (0..64).filter(|cpu| mask & (1 << cpu) != 0) {
            libc::CPU_SET(cpu, &mut cpus);
        }
        let affinity = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpus);
        let policy = shared.policy.load(Ordering::Relaxed);
        let mut priority = shared.priority.load(Ordering::Relaxed);
        if matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR) && priority > 1 {
            priority -= 1;
        }
        let param = libc::sched_param {
            sched_priority: priority,
        };
        let sched = libc::pthread_setschedparam(libc::pthread_self(), policy, &param);
        let (actual, policy, priority) = current_placement();
        Taken {
            requested_cpus: mask,
            cpus: actual,
            policy,
            priority,
            affinity,
            sched,
        }
    }
}
impl Work {
    fn new(reader: parameters::Reader, stereo: bool, processed_mode: bool) -> Self {
        Self {
            reader,
            stereo,
            raw: Box::new([[[0.0; FRAMES]; 2]; 2]),
            processed: Box::new([const { UnsafeCell::new([[0.0; FRAMES]; 2]) }; 2]),
            captured: [0; 2],
            done: [false; 2],
            processed_mode,
        }
    }
    /// The helper's share of the frame: one snapshot read, the job, and the
    /// acknowledgement. Only a processed, complete frame applies settings.
    fn run(
        &mut self,
        job: Job,
        slot: usize,
        frames: usize,
        native_success: bool,
        counters: &mut Counters,
    ) {
        let reader = &mut self.reader;
        if !reader.begin() {
            return;
        }
        let processed = reader.mode() == Some(PathMode::Processed);
        self.processed_mode = processed;
        let latency = reader.latency_samples() as u32;
        let stereo = self.stereo;
        let mut applied = native_success && job != Job::None;
        self.done[slot] = false;
        match job {
            Job::None | Job::Skipped => {}
            Job::Count => reader.record(frames, rc2fx_dsp::Meter::default(), latency),
            Job::Meter => {
                let meter = native_meter(view(&self.raw[slot], frames, stereo));
                reader.record(frames, meter, latency);
            }
            // A mode change under a copied frame leaves it unprocessed.
            Job::Input | Job::Output if !processed => {
                let meter = native_meter(view(&self.raw[slot], frames, stereo));
                reader.record(frames, meter, latency);
            }
            Job::Input => {
                let input = view(&self.raw[slot], frames, stereo);
                // SAFETY: the helper owns `processed[slot]` while handed off.
                let output = split(unsafe { &mut *self.processed[slot].get() }, frames, stereo);
                match reader.input(input, output) {
                    Ok(meter) => {
                        self.done[slot] = true;
                        counters.input_blocks += 1;
                        LIVE_INPUT_BLOCKS.fetch_add(1, Ordering::Relaxed);
                        // Processed sources reach the mixer one frame later.
                        reader.record(frames, meter, latency + FRAMES as u32);
                    }
                    Err(_) => {
                        applied = false;
                        counters.faults += 1;
                        LIVE_FAULTS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Job::Output => {
                // SAFETY: as above.
                let (raw, processed) =
                    (&self.raw[slot], unsafe { &mut *self.processed[slot].get() });
                copy(view(raw, frames, stereo), processed, frames);
                match reader.output(split(processed, frames, stereo)) {
                    Ok(meter) => {
                        self.done[slot] = true;
                        counters.output_blocks += 1;
                        LIVE_OUTPUT_BLOCKS.fetch_add(1, Ordering::Relaxed);
                        // Hardware outputs are written one frame later.
                        reader.record(frames, meter, latency + FRAMES as u32);
                    }
                    Err(_) => {
                        applied = false;
                        counters.faults += 1;
                        LIVE_FAULTS.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        reader.finish(applied);
    }
}
/// Prepares one path for this callback.
///
/// # Safety
/// As `Pipeline::begin`.
unsafe fn begin_port(
    index: usize,
    port: &mut Port,
    counters: &mut Counters,
    handoff: Option<&Handoff>,
    cycle: u32,
    frames: usize,
    get: super::GetBuffer,
) {
    port.active = false;
    port.counted = false;
    port.observed = 0;
    port.outcome = None;
    if let Some(reader) = &mut port.reader
        && !reader.begin()
    {
        port.fail(counters);
        return;
    }
    if port.failed {
        return;
    }
    let unprocessed = match port.owner {
        Owner::Callback => port
            .reader
            .as_ref()
            .is_some_and(|reader| reader.mode() != Some(PathMode::Processed)),
        Owner::Helper => handoff
            .and_then(|handoff| handoff.work[index].as_ref())
            .is_some_and(|work| !work.processed_mode),
    };
    // A busy helper's mode is unknown; keep the last one seen.
    if port.owner == Owner::Callback || handoff.is_some() {
        port.processed_intent = !unprocessed;
    }
    if unprocessed && !meters(cycle, index) {
        port.counted = true;
        return;
    }
    if frames != FRAMES || !port.alive() {
        port.fail(counters);
        return;
    }
    let mut valid = true;
    for (channel, pin) in port.pins.iter().enumerate() {
        let pointer = unsafe { get(pin.pointer as *mut c_void, frames as u32) } as usize;
        if pointer == 0
            || !pointer.is_multiple_of(std::mem::align_of::<f32>())
            || pointer.checked_add(frames * 4).is_none()
        {
            valid = false;
            break;
        }
        port.native[channel] = pointer;
    }
    if !valid {
        port.fail(counters);
        return;
    }
    if port.direction == Direction::Output && port.pins.len() == 2 {
        let [a, b] = port.native;
        if a < b + frames * 4 && b < a + frames * 4 {
            port.fail(counters);
            return;
        }
    }
    port.active = true;
    if port
        .reader
        .as_ref()
        .is_some_and(parameters::Reader::needs_chat_stem)
        && let Some(capture) = &mut port.chat
    {
        // Capture is boxed in this Port and remains live until end().
        unsafe {
            capture.arm(port.native);
        }
    }
}
/// What every path's end of the callback shares.
struct Ended {
    frames: usize,
    native_success: bool,
    slot: usize,
    previous: usize,
    seq: u64,
    previous_ready: bool,
    /// `Front::skipped_frames` before this cycle, and after it.
    owed: u64,
    credited: u64,
    /// `Front::gauged`.
    gauged: bool,
}
/// Finishes one path's callback.
///
/// # Safety
/// As `Pipeline::end`.
unsafe fn end_port(
    index: usize,
    port: &mut Port,
    counters: &mut Counters,
    handoff: &mut Option<&mut Handoff>,
    cycle: &Ended,
) {
    let owed = cycle.owed - port.credited;
    port.credited = cycle.credited;
    if owed > 0
        && let Some(reader) = &mut port.reader
    {
        let latency = reader.latency_samples() as u32;
        reader.record_skipped(owed, FRAMES, latency);
    }
    let started = if cycle.gauged {
        super::timing::monotonic()
    } else {
        0
    };
    let complete = port.complete();
    let usable = cycle.native_success && port.active && complete;
    if port.direction == Direction::Output && port.active && !complete {
        counters.skipped_outputs += 1;
        LIVE_SKIPPED_OUTPUTS.fetch_add(1, Ordering::Relaxed);
    }
    if usable && !port.alive() {
        port.fail(counters);
    }
    let usable = usable && !port.failed;
    // An unprocessed path left alone applies trivially.
    let counted = cycle.native_success && port.counted;
    port.tally_source(usable, counters);
    match port.owner {
        Owner::Callback => {
            let mut applied = counted;
            if usable {
                let buffers = unsafe { port.slices(cycle.frames) };
                let reader = port.reader.as_mut().expect("callback-owned output");
                let latency = reader.latency_samples() as u32;
                if reader.mode() == Some(PathMode::Processed) {
                    let result = if reader.needs_chat_stem() {
                        match port.chat.as_ref().and_then(|capture| capture.voice()) {
                            Some(voice) => reader.chat_output(voice, buffers),
                            None => Err(rc2fx_dsp::DspError::MissingChatStem),
                        }
                    } else {
                        reader.output(buffers)
                    };
                    match result {
                        Ok(meter) => {
                            reader.record(cycle.frames, meter, latency);
                            counters.output_blocks += 1;
                            LIVE_OUTPUT_BLOCKS.fetch_add(1, Ordering::Relaxed);
                            applied = true;
                        }
                        Err(_) => port.fail(counters),
                    }
                } else {
                    let [left, right] = buffers;
                    reader.record(cycle.frames, native_meter([left, right]), latency);
                    applied = true;
                }
            } else if counted {
                let reader = port.reader.as_mut().expect("callback-owned output");
                let latency = reader.latency_samples() as u32;
                reader.record(cycle.frames, rc2fx_dsp::Meter::default(), latency);
            }
            if let Some(reader) = &mut port.reader {
                reader.finish(applied);
            }
        }
        Owner::Helper => {
            let Some(handoff) = handoff.as_mut() else {
                return;
            };
            let Some(work) = &mut handoff.work[index] else {
                return;
            };
            let mut job = if counted { Job::Count } else { Job::None };
            if usable {
                let [left, right] = unsafe { port.slices(cycle.frames) };
                if work.processed_mode {
                    copy([&*left, &*right], &mut work.raw[cycle.slot], cycle.frames);
                    work.captured[cycle.slot] = cycle.seq;
                    if port.direction == Direction::Input {
                        job = Job::Input;
                    } else {
                        job = Job::Output;
                        // Write the previous frame: processed if the
                        // helper finished it, else as the mixer made it.
                        let source = if cycle.previous_ready && work.done[cycle.previous] {
                            // SAFETY: the helper is idle.
                            Some(unsafe { &*work.processed[cycle.previous].get() })
                        } else if work.captured[cycle.previous] == cycle.seq - 1 {
                            // An unprocessed slot taken from the copy:
                            // counted as a fallback, not as a DSP error
                            // (see LIVE_OUTPUT_RAW_FRAME).
                            LIVE_OUTPUT_RAW_FRAME.fetch_add(1, Ordering::Relaxed);
                            Some(&work.raw[cycle.previous])
                        } else {
                            LIVE_OUTPUT_NO_FRAME.fetch_add(1, Ordering::Relaxed);
                            None
                        };
                        if let Some(source) = source {
                            left.copy_from_slice(&source[0][..cycle.frames]);
                            if !right.is_empty() {
                                right.copy_from_slice(&source[1][..cycle.frames]);
                            }
                        }
                    }
                } else {
                    copy([&*left, &*right], &mut work.raw[cycle.slot], cycle.frames);
                    job = Job::Meter;
                }
            }
            handoff.jobs[index] = job;
        }
    }
    if cycle.gauged {
        charge(&PATH_COST_NS[index], started);
    }
}
impl Front {
    /// Asks for what `begin` (`slot`: None) or `end` is about to touch for the
    /// paths this sparse cycle visits, before either touches any of it (see
    /// prefetch.rs): first the paths themselves, whose addresses need nothing
    /// else, then what each points to. It changes nothing, and reads nothing
    /// that the phase does not read itself.
    ///
    /// Out of line, so that a profile shows its cost under its own name.
    ///
    /// # Safety
    /// As `Pipeline::begin` and `end`, with the helper idle.
    #[inline(never)]
    unsafe fn warm(&self, handoff: &Handoff, slot: Option<usize>) {
        let visits = &self.visits[self.phase];
        for &index in visits {
            prefetch::write(&raw const self.ports[index], 1);
        }
        for &index in visits {
            let (port, work) = (&self.ports[index], &raw const handoff.work[index]);
            match slot {
                None => port.warm_begin(work),
                Some(slot) => unsafe { port.warm_end(work, slot) },
            }
        }
    }
    /// Decides what each sparse cycle visits, from what the full cycles found.
    fn plan_visits(&mut self) {
        for (phase, visits) in self.visits.iter_mut().enumerate() {
            visits.clear();
            let due = self.ports.iter().enumerate().filter(|(index, port)| {
                port.processed_intent || port.failed || meters(phase as u32, *index)
            });
            visits.extend(due.map(|(index, _)| index));
        }
    }
}
impl Pipeline {
    /// Control-only readiness: native hosts may install their callback before
    /// registering USB/other ports. Do not construct any worker until the whole
    /// selected plan has exact live identities on this client.
    pub fn ports_ready(plan: &Plan, registry: &Registry, client: usize) -> bool {
        plan.paths.iter().all(|path| {
            path.ports.iter().all(|name| {
                registry
                    .ports
                    .iter()
                    .filter(|pin| {
                        pin.client == client
                            && pin.name == *name
                            && pin.direction == path.direction
                            && pin.alive.load(Ordering::Acquire)
                    })
                    .count()
                    == 1
            })
        })
    }
    pub fn new(
        plan: &Plan,
        registry: &Registry,
        client: usize,
    ) -> Result<(Self, Vec<parameters::Control>), String> {
        Self::with_ownership(plan, registry, client, DIRECT_OUTPUT)
    }
    /// `direct_output`: every hardware output is callback-owned (see
    /// DIRECT_OUTPUT), fixed here rather than per mode. Tests also build the
    /// helper-owned arrangement.
    fn with_ownership(
        plan: &Plan,
        registry: &Registry,
        client: usize,
        direct_output: bool,
    ) -> Result<(Self, Vec<parameters::Control>), String> {
        let mut ports = Vec::with_capacity(plan.paths.len());
        let mut work = Vec::with_capacity(plan.paths.len());
        let mut controls = Vec::with_capacity(plan.paths.len());
        let admission = super::suppression::Admission::default();
        let publications = Arc::<AtomicU64>::default();
        for target in &plan.paths {
            let pins = target
                .ports
                .iter()
                .map(|name| {
                    let matches: Vec<_> = registry
                        .ports
                        .iter()
                        .filter(|pin| {
                            pin.client == client
                                && pin.name == *name
                                && pin.direction == target.direction
                                && pin.alive.load(Ordering::Acquire)
                        })
                        .collect();
                    if matches.len() != 1 {
                        return Err(format!(
                            "native port {name} {:?}: {} matches; registered for client: {:?}",
                            target.direction,
                            matches.len(),
                            registry
                                .ports
                                .iter()
                                .filter(|pin| pin.client == client)
                                .map(|pin| &pin.name)
                                .collect::<Vec<_>>()
                        ));
                    }
                    Ok(Arc::clone(matches[0]))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let prepared =
                Prepared::new(&target.target, 48_000).map_err(|error| error.to_string())?;
            let (mut control, reader) = parameters::channel_with_options(
                &target.target,
                prepared,
                plan.worker_options(),
                admission.clone(),
                target.mode,
            )
            .map_err(|error| format!("native path construction: {error:?}"))?;
            control.count_publications(Arc::clone(&publications));
            controls.push(control);
            let owner = match target.direction {
                Direction::Input => Owner::Helper,
                Direction::Output if target.hardware && !direct_output => Owner::Helper,
                Direction::Output => Owner::Callback,
            };
            let reader = if owner == Owner::Helper {
                work.push(Some(Work::new(
                    reader,
                    pins.len() == 2,
                    target.mode == PathMode::Processed,
                )));
                None
            } else {
                work.push(None);
                Some(reader)
            };
            ports.push(Port {
                direction: target.direction,
                pins,
                owner,
                reader,
                processed_intent: target.mode == PathMode::Processed,
                outcome: None,
                chat: target
                    .target
                    .is_chat_output()
                    .then(|| Box::new(super::chat_mix::Capture::new())),
                native: [0; 2],
                observed: 0,
                active: false,
                counted: false,
                failed: false,
                credited: 0,
            });
        }
        let jobs = vec![Job::Skipped; ports.len()];
        let lookup = PinLookup::new(&ports);
        let visits = std::array::from_fn(|_| Vec::with_capacity(ports.len()));
        let helper = Helper::start()?;
        Ok((
            Self {
                front: UnsafeCell::new(Front {
                    ports,
                    lookup,
                    visits,
                    full_left: METER_STRIDE,
                    full: true,
                    phase: 0,
                    publications: 0,
                    faults: 0,
                    skipped_frames: 0,
                    counters: Counters::default(),
                    cycle: 0,
                    seq: 0,
                    idle: true,
                    previous_ready: false,
                    sampler: Sampler::default(),
                    gauged: super::gauges::enabled(),
                }),
                handoff: UnsafeCell::new(Handoff {
                    seq: 0,
                    frames: FRAMES,
                    cycle_start: 0,
                    started: 0,
                    native_success: false,
                    jobs,
                    work,
                    counters: Counters::default(),
                    done_seq: 0,
                    #[cfg(test)]
                    helper: Some(Arc::clone(&helper.shared)),
                }),
                helper,
                publications,
            },
            controls,
        ))
    }
    /// Control only, after callbacks have stopped.
    pub fn counters(&self) -> Counters {
        while !self.helper.idle() {
            thread::yield_now();
        }
        let front = unsafe { &*self.front.get() };
        let handoff = unsafe { &*self.handoff.get() };
        front.counters.add(handoff.counters)
    }
    /// As `begin_at`, for a callback that starts now.
    ///
    /// # Safety
    /// As `begin_at`.
    #[cfg(test)]
    pub unsafe fn begin(&self, frames: usize, get: super::GetBuffer, cycle_start: u64) {
        let started = super::timing::monotonic();
        unsafe { self.begin_at(frames, get, cycle_start, started) }
    }
    /// Turns this pipeline's bench gauges on or off, as the marker would at
    /// load. Between callbacks only.
    #[cfg(test)]
    fn set_gauged(&self, gauged: bool) {
        unsafe { &mut *self.front.get() }.gauged = gauged;
    }
    /// `started`: CLOCK_MONOTONIC when the wrapped callback started.
    ///
    /// # Safety
    /// The original JACK getter is valid for these live ports during this
    /// callback. Native client/port mutation must obey JACK's lifetime rules.
    pub unsafe fn begin_at(
        &self,
        frames: usize,
        get: super::GetBuffer,
        cycle_start: u64,
        started: u64,
    ) {
        super::chat_mix::disarm();
        // SAFETY: the callback owns `front`.
        let front = unsafe { &mut *self.front.get() };
        front.seq += 1;
        front.idle = self.helper.idle();
        if !front.idle {
            // Rare: the helper is almost done. The callback has the time to
            // spare, and waiting beats passing a frame through.
            HELPER_LATE.fetch_add(1, Ordering::Relaxed);
            let running = super::timing::monotonic()
                .saturating_sub(self.helper.shared.started.load(Ordering::Relaxed));
            super::probe::record(super::probe::Kind::HelperBusy, running);
            front.idle = self.helper.wait(HELPER_WAIT_NS);
        }
        front.previous_ready = false;
        // SAFETY: the helper is idle; the callback owns the handoff until
        // `end` queues it again.
        let handoff = front.idle.then(|| unsafe { &mut *self.handoff.get() });
        if let Some(handoff) = &handoff {
            front.previous_ready = handoff.done_seq == front.seq - 1;
            let finished = self.helper.shared.finished.load(Ordering::Relaxed);
            if finished != 0 && front.previous_ready {
                // From the callback's own start stamp: no clock read here.
                let margin = started.saturating_sub(finished);
                if margin < HELPER_MIN_MARGIN_NS.load(Ordering::Relaxed) {
                    HELPER_MIN_MARGIN_NS.fetch_min(margin, Ordering::Relaxed);
                }
                if front.gauged {
                    HELPER_SLACK.record(margin);
                }
            }
        } else {
            HELPER_STALLS.fetch_add(1, Ordering::Relaxed);
            super::probe::record(super::probe::Kind::HelperStall, HELPER_WAIT_NS);
        }
        let resolving = if front.gauged {
            super::timing::monotonic()
        } else {
            0
        };
        let cycle = front.cycle;
        front.cycle = front.cycle.wrapping_add(1);
        // Resolve all buffers before touching any signal or DSP history.
        // Unprocessed paths are left alone between meter readings, their
        // buffers not even resolved, and usually not even visited.
        let publications = self.publications.load(Ordering::Acquire);
        if publications != front.publications {
            front.publications = publications;
            front.full_left = METER_STRIDE;
        }
        // A busy helper's paths and an odd frame size take the full route.
        front.full = !front.idle || frames != FRAMES || front.full_left > 0;
        front.phase = cycle as usize % METER_STRIDE as usize;
        let seen = handoff.as_deref();
        if front.full {
            for (index, port) in front.ports.iter_mut().enumerate() {
                unsafe {
                    begin_port(index, port, &mut front.counters, seen, cycle, frames, get);
                }
            }
        } else {
            // A sparse cycle has the handoff: the helper is idle.
            if let Some(handoff) = seen {
                unsafe { front.warm(handoff, None) };
            }
            for &index in &front.visits[front.phase] {
                let port = &mut front.ports[index];
                let planned = port.processed_intent;
                unsafe {
                    begin_port(index, port, &mut front.counters, seen, cycle, frames, get);
                }
                // A new mode can be seen before its publication is counted.
                // Every following frame must then be visited, or a processed
                // frame would sit alone between unprocessed ones.
                if port.processed_intent != planned {
                    front.full_left = METER_STRIDE;
                }
            }
        }
        if front.gauged {
            charge(&RESOLVE_NS, resolving);
        }
        if let Some(handoff) = handoff {
            handoff.frames = frames;
            handoff.cycle_start = cycle_start;
            handoff.started = started;
            handoff.seq = front.seq;
        }
    }
    pub fn buffer(&self, port: usize, frames: usize, native: *mut c_void) -> *mut c_void {
        // SAFETY: called by the mixer inside the callback, between `begin`
        // and `end`.
        let front = unsafe { &mut *self.front.get() };
        let Some((index, channel)) = front.lookup.find(port) else {
            return native;
        };
        let item = &mut front.ports[index];
        if !item.active {
            item.note(Outcome::Invalid, &mut front.counters);
            return native;
        }
        if frames != FRAMES
            || !item.pins[channel].alive.load(Ordering::Acquire)
            || item.native[channel] != native as usize
        {
            item.fail(&mut front.counters);
            item.note(Outcome::Invalid, &mut front.counters);
            return native;
        }
        item.observed |= 1 << channel;
        if item.direction != Direction::Input {
            return native;
        }
        if !front.idle || !front.previous_ready {
            item.note(Outcome::HelperUnready, &mut front.counters);
            return native;
        }
        // The mixer reads the source processed over the last frame.
        // SAFETY: the helper is idle (see `begin`).
        let handoff = unsafe { &mut *self.handoff.get() };
        let Some(work) = &mut handoff.work[index] else {
            return native;
        };
        let previous = ((front.seq - 1) & 1) as usize;
        if work.done[previous] {
            item.note(Outcome::Processed, &mut front.counters);
            let cell = &work.processed[previous];
            // In bounds, and without a mutable borrow of the slot.
            unsafe { cell.get().cast::<[f32; FRAMES]>().add(channel).cast() }
        } else {
            // No frame was handed off last time (the first frame, or
            // the path inactive), or the helper produced no result.
            // (`captured` starts at zero, as if frame 0 had been.)
            let outcome = if front.seq > 1 && work.captured[previous] == front.seq - 1 {
                Outcome::NotDone
            } else {
                Outcome::NoHandoff
            };
            item.note(outcome, &mut front.counters);
            native
        }
    }
    /// # Safety
    /// Native output buffers remain live and uniquely writable until this
    /// callback returns. No caller may unregister them concurrently.
    pub unsafe fn end(&self, frames: usize, native_success: bool) {
        // No subsequent vendor call may retain this callback's capture pointer.
        super::chat_mix::disarm();
        // SAFETY: the callback owns `front`.
        let front = unsafe { &mut *self.front.get() };
        let entered = if front.gauged {
            super::timing::monotonic()
        } else {
            0
        };
        let slot = (front.seq & 1) as usize;
        let previous = slot ^ 1;
        // SAFETY: the helper is idle (see `begin`), so the callback owns the
        // handoff until it is queued below.
        let mut handoff = front.idle.then(|| unsafe { &mut *self.handoff.get() });
        if !front.full
            && let Some(handoff) = handoff.as_deref()
        {
            unsafe { front.warm(handoff, Some(slot)) };
        }
        let owed = front.skipped_frames;
        // This cycle's success is owed to the paths it left alone, not to the
        // ones it visits, which count it themselves.
        let credited = owed + u64::from(!front.full && native_success);
        let cycle = Ended {
            frames,
            native_success,
            slot,
            previous,
            seq: front.seq,
            previous_ready: front.previous_ready,
            owed,
            credited,
            gauged: front.gauged,
        };
        if front.full {
            for (index, port) in front.ports.iter_mut().enumerate() {
                unsafe { end_port(index, port, &mut front.counters, &mut handoff, &cycle) };
                port.reset();
            }
        } else {
            for &index in &front.visits[front.phase] {
                let port = &mut front.ports[index];
                unsafe { end_port(index, port, &mut front.counters, &mut handoff, &cycle) };
                port.reset();
            }
        }
        front.skipped_frames = credited;
        if front.counters.faults != front.faults {
            front.faults = front.counters.faults;
            front.full_left = METER_STRIDE;
        }
        if front.full && front.idle && native_success && front.full_left > 0 {
            front.full_left -= 1;
            if front.full_left == 0 {
                front.plan_visits();
            }
        }
        front.sampler.mark(front.cycle);
        if let Some(handoff) = handoff {
            handoff.native_success = native_success;
            let begun = handoff.started;
            if front.gauged {
                let placing = super::timing::monotonic();
                self.helper.place(front.cycle, &mut front.sampler);
                let waking = super::timing::monotonic();
                let woke = self.helper.queue(handoff, begun, waking);
                let woken = super::timing::monotonic();
                PLACE.record(waking.saturating_sub(placing));
                END.record(waking.saturating_sub(entered));
                WAKE_CALL.record(woken.saturating_sub(waking));
                if woke != Woke::No {
                    WAKE_SENT.record(woken.saturating_sub(waking));
                    WAKES[usize::from(woke == Woke::Late)].fetch_add(1, Ordering::Relaxed);
                }
            } else {
                self.helper.place(front.cycle, &mut front.sampler);
                self.helper.queue(handoff, begun, 0);
            }
        }
    }
    /// The callback's end stamp, once it is taken: the helper forecasts the
    /// next handoff from these. One store; without it the helper parks after
    /// every frame and each handoff wakes it.
    pub fn ended(&self, at: u64) {
        self.helper.shared.ended.store(at, Ordering::Relaxed);
    }
    /// Wait for the helper to finish the frame just handed off, as the next
    /// callback would find it.
    #[cfg(test)]
    fn settle(&self) {
        while !self.helper.idle() {
            thread::yield_now();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::UnsafeCell;
    #[path = "direct_output.rs"]
    mod direct_output;
    #[path = "helper_wake.rs"]
    mod helper_wake;
    #[path = "passthrough_invariance.rs"]
    mod passthrough_invariance;
    #[path = "processing_counters.rs"]
    mod processing_counters;
    #[path = "skipped_paths.rs"]
    mod skipped_paths;

    /// The unprocessed-path meter as it was: one flattened iterator.
    fn flattened_meter(buffers: [&[f32]; 2]) -> rc2fx_dsp::Meter {
        let mut meter = rc2fx_dsp::Meter::default();
        for sample in buffers.into_iter().flatten().copied() {
            let value = f64::from(sample);
            if value.is_finite() {
                meter.input_peak = meter.input_peak.max(value.abs());
                meter.sum_squares += value * value;
                meter.clipped += u64::from(value.abs() >= 1.0);
            }
            meter.samples += 1;
        }
        meter.output_peak = meter.input_peak;
        meter
    }
    fn meter_bits(meter: rc2fx_dsp::Meter) -> [u64; 8] {
        [
            meter.input_peak.to_bits(),
            meter.output_peak.to_bits(),
            meter.sum_squares.to_bits(),
            meter.gain_reduction.to_bits(),
            meter.voice.to_bits(),
            meter.gate_open.to_bits(),
            meter.samples,
            meter.clipped,
        ]
    }
    /// The meter shown for unprocessed paths is the same value bit for bit:
    /// over random samples of every scale, NaNs, infinities, denormals,
    /// signed zeros, full scale and just beside it, huge values and silence,
    /// mono and stereo, and every length up to a block.
    #[test]
    fn the_unprocessed_meter_is_bit_identical_to_the_flattened_loop() {
        let special = [
            0.0f32,
            -0.0,
            f32::NAN,
            -f32::NAN,
            f32::from_bits(0x7fa0_0001),
            f32::from_bits(0xffc0_1234),
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::from_bits(1),
            f32::from_bits(0x8000_0001),
            f32::from_bits(0x007f_ffff),
            1.0,
            -1.0,
            f32::from_bits(1.0f32.to_bits() - 1),
            f32::from_bits(1.0f32.to_bits() + 1),
            -f32::from_bits(1.0f32.to_bits() - 1),
            f32::MAX,
            f32::MIN,
            1.0e30,
            -3.0e38,
            0.5,
            -0.25,
        ];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut random = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut compared = 0;
        for round in 0..6000 {
            let frames = match round % 5 {
                0 => FRAMES,
                1 => (random() % (FRAMES as u64 + 1)) as usize,
                2 => round % 4,
                _ => FRAMES,
            };
            let mut block = [[0.0f32; FRAMES]; 2];
            for channel in &mut block {
                for sample in channel.iter_mut() {
                    let bits = random();
                    *sample = match (round / 5) % 6 {
                        // Every bit pattern: NaNs, infinities and denormals.
                        0 => f32::from_bits(bits as u32),
                        // Programme-level audio.
                        1 => (bits >> 40) as f32 / (1u64 << 23) as f32 - 1.0,
                        // Mostly special values.
                        2 => special[(bits % special.len() as u64) as usize],
                        // Audio with the odd special value in it.
                        3 if bits % 9 == 0 => special[(bits >> 8) as usize % special.len()],
                        3 => (bits >> 40) as f32 / (1u64 << 24) as f32 - 0.5,
                        // Silence, and denormal noise.
                        4 => 0.0,
                        _ => f32::from_bits((bits as u32) & 0x807f_ffff),
                    };
                }
            }
            for stereo in [false, true] {
                let buffers = view(&block, frames, stereo);
                assert_eq!(
                    meter_bits(native_meter(buffers)),
                    meter_bits(flattened_meter(buffers)),
                    "round {round}, {frames} frames, stereo {stereo}"
                );
                compared += 1;
            }
        }
        assert_eq!(compared, 12_000);
        // Every special value alone, in each channel position.
        for value in special {
            let block = [[value; FRAMES], [-value; FRAMES]];
            for stereo in [false, true] {
                let buffers = view(&block, FRAMES, stereo);
                assert_eq!(
                    meter_bits(native_meter(buffers)),
                    meter_bits(flattened_meter(buffers)),
                    "{value:e} ({:#x}), stereo {stereo}",
                    value.to_bits()
                );
            }
        }
        // The fixture is not vacuous: the meters see clips, sums and peaks.
        let loud = native_meter(view(&[[1.5; FRAMES], [-0.5; FRAMES]], FRAMES, true));
        assert_eq!(loud.samples, 2 * FRAMES as u64);
        assert_eq!(loud.clipped, FRAMES as u64);
        assert_eq!(loud.input_peak, 1.5);
        assert!(loud.sum_squares > 0.0);
    }

    /// The fine histogram's bins are microsecond powers of two, inclusive at
    /// the top of each, with the count, total and longest alongside.
    #[test]
    fn the_fine_gauge_bins_by_microseconds() {
        let gauge = Fine::new();
        for (elapsed, bin) in [
            (0, 0),
            (1_000, 0),
            (1_001, 1),
            (2_000, 1),
            (4_000, 2),
            (4_001, 3),
            (8_000, 3),
            (16_000, 4),
            (32_000, 5),
            (64_000, 6),
            (64_001, 7),
            (5_000_000, 7),
        ] {
            let before = gauge.bins[bin].load(Ordering::Relaxed);
            gauge.record(elapsed);
            assert_eq!(
                gauge.bins[bin].load(Ordering::Relaxed),
                before + 1,
                "{elapsed} ns"
            );
        }
        let report = gauge.snapshot();
        assert_eq!(report["calls"], 12);
        assert_eq!(report["maxNs"], 5_000_000);
        assert_eq!(
            report["totalNs"],
            5_000_000u64
                + 64_001
                + 64_000
                + 32_000
                + 16_000
                + 8_000
                + 4_001
                + 4_000
                + 2_000
                + 1_001
                + 1_000
        );
        assert_eq!(report["histogram"].as_array().unwrap().len(), 8);
        assert_eq!(std::mem::align_of_val(&WAKE_CALL), 64);
    }

    #[test]
    fn expanded_route_plan_uses_the_shared_bounded_path_capacity() {
        let paths: Vec<_> = (0..MAX_PATHS).map(|index| {
            serde_json::json!({
                "direction": "input", "ports": [format!("in_{}", index + 1)],
                "target": Target::new(format!("path-{index}"), "Playback", TargetKind::Playback, 1),
                "mode": "passthrough", "hardware": false,
            })
        }).collect();
        let mut value = serde_json::json!({
            "version": 1, "sampleRate": 48000, "frames": FRAMES, "paths": paths,
        });
        let plan = Plan::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(plan.paths.len(), MAX_PATHS);
        assert!(
            plan.paths.len() >= 25,
            "the expanded Pro II catalog must fit"
        );
        let extra = value["paths"][0].clone();
        value["paths"].as_array_mut().unwrap().push(extra);
        assert!(
            Plan::parse(&serde_json::to_vec(&value).unwrap())
                .err()
                .unwrap()
                .contains("geometry/count")
        );
    }

    struct Buffers {
        input: [UnsafeCell<[f32; FRAMES]>; 2],
        output: [UnsafeCell<[f32; FRAMES]>; 2],
    }
    impl Buffers {
        fn new() -> Self {
            Self {
                input: [
                    UnsafeCell::new([0.25; FRAMES]),
                    UnsafeCell::new([-0.125; FRAMES]),
                ],
                output: [const { UnsafeCell::new([0.0; FRAMES]) }; 2],
            }
        }
        fn input(&self, channel: usize) -> *mut f32 {
            self.input[channel].get().cast()
        }
        fn output(&self, channel: usize) -> *mut f32 {
            self.output[channel].get().cast()
        }
    }
    unsafe extern "C" fn test_buffer(port: *mut c_void, _: u32) -> *mut c_void {
        port
    }
    fn plan_with_source_and_output(highpass: bool) -> Plan {
        let mut paths = Vec::new();
        for (id, kind, channels, direction, ports) in [
            (
                "source",
                TargetKind::Mic,
                1,
                Direction::Input,
                vec!["in_1".into()],
            ),
            (
                "headphones",
                TargetKind::Output,
                2,
                Direction::Output,
                vec!["out_1".into(), "out_2".into()],
            ),
        ] {
            let mut target = Target::new(id, id, kind, channels);
            target.limiter.enabled = false;
            target.gain.enabled = false;
            target.eq.enabled = true;
            target.eq.input_gain_db = 20.0 * 0.5_f64.log10();
            target.highpass.enabled = direction == Direction::Input && highpass;
            target.highpass.frequency = 120.0;
            paths.push(PathPlan {
                direction,
                ports,
                target,
                mode: PathMode::Processed,
                hardware: direction == Direction::Output,
            });
        }
        Plan {
            version: 1,
            sample_rate: 48_000,
            frames: FRAMES,
            worker: None,
            paths,
        }
    }
    fn pipeline(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
        let mut registry = Registry::default();
        for channel in 0..2 {
            assert!(registry.register(
                1,
                buffers.input(channel) as usize,
                format!("in_{}", channel + 1).as_bytes(),
                1
            ));
            assert!(registry.register(
                1,
                buffers.output(channel) as usize,
                format!("out_{}", channel + 1).as_bytes(),
                2
            ));
        }
        // These tests describe helper-owned sources; the direct ones say so.
        Pipeline::with_ownership(plan, &registry, 1, false).unwrap()
    }
    fn block(pipeline: &mut Pipeline, buffers: &Buffers, native_success: bool) {
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        for channel in 0..2 {
            let raw = buffers.input(channel);
            let input = pipeline
                .buffer(raw as usize, FRAMES, raw.cast())
                .cast::<f32>();
            let raw = buffers.output(channel);
            let output = pipeline
                .buffer(raw as usize, FRAMES, raw.cast())
                .cast::<f32>();
            for index in 0..FRAMES {
                unsafe { output.add(index).write(input.add(index).read() * 2.0) };
            }
        }
        unsafe { pipeline.end(FRAMES, native_success) };
        pipeline.settle();
    }

    #[test]
    fn chat_without_a_verified_stem_leaves_the_completed_stereo_mix_untouched() {
        let mut target = Target::new("send-chat", "Chat", TargetKind::Output, 2);
        target.suppression.enabled = true;
        let plan = Plan {
            version: 1,
            sample_rate: 48_000,
            frames: FRAMES,
            worker: Some(WorkerPlan {
                cpu_mask: None,
                realtime_priority: None,
                nice: None,
            }),
            paths: vec![PathPlan {
                direction: Direction::Output,
                ports: vec!["out_1".into(), "out_2".into()],
                target,
                mode: PathMode::Processed,
                hardware: false,
            }],
        };
        let buffers = Buffers::new();
        let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
        block(&mut pipeline, &buffers, true);
        assert!(
            unsafe { &*buffers.output[0].get() }
                .iter()
                .all(|&v| v == 0.5)
        );
        assert!(
            unsafe { &*buffers.output[1].get() }
                .iter()
                .all(|&v| v == -0.25)
        );
        assert_eq!(pipeline.counters().faults, 1);
        assert_eq!(controls[0].status().unwrap().applied, 0);
        assert_eq!(
            controls[0].worker_status().unwrap().completed_frames,
            0,
            "the mixed output never becomes model input"
        );
    }

    #[test]
    fn live_source_and_output_updates_remain_independent_and_keep_raw_fanout() {
        let plan = plan_with_source_and_output(false);
        let buffers = Buffers::new();
        let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
        block(&mut pipeline, &buffers, true);
        assert_eq!(controls[0].status().unwrap().applied, 1);
        assert_eq!(controls[1].status().unwrap().applied, 1);
        let mut source = plan.paths[0].target.clone();
        source.eq.input_gain_db = 20.0 * 0.25_f64.log10();
        controls[0].publish(&source, 2).unwrap();
        for _ in 0..100 {
            block(&mut pipeline, &buffers, true);
        }
        assert_eq!(controls[0].status().unwrap().applied, 2);
        assert_eq!(controls[1].status().unwrap().published, 1);
        for channel in 0..2 {
            let expected = [0.0625, -0.125][channel];
            assert!(
                unsafe { &*buffers.output[channel].get() }
                    .iter()
                    .all(|value| (value - expected).abs() < 2e-6)
            );
            assert!(
                unsafe { &*buffers.input[channel].get() }
                    .iter()
                    .all(|value| *value == [0.25, -0.125][channel])
            );
        }
        let mut output = plan.paths[1].target.clone();
        output.eq.input_gain_db = 0.0;
        controls[1].publish(&output, 2).unwrap();
        for _ in 0..100 {
            block(&mut pipeline, &buffers, true);
        }
        assert_eq!(controls[1].status().unwrap().applied, 2);
        assert_eq!(controls[0].status().unwrap().published, 2);
        for channel in 0..2 {
            let expected = [0.125, -0.25][channel];
            assert!(
                unsafe { &*buffers.output[channel].get() }
                    .iter()
                    .all(|value| (value - expected).abs() < 2e-6)
            );
        }
        assert_eq!(pipeline.counters().faults, 0);
    }

    #[test]
    fn native_pass_and_replacement_change_only_one_bound_path_without_callback_memory_work() {
        use super::super::{
            CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
        };
        let plan = plan_with_source_and_output(false);
        let buffers = Buffers::new();
        let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
        for _ in 0..8 {
            block(&mut pipeline, &buffers, true);
        }
        let output_epoch = controls[1].status().unwrap().runtime_epoch;
        let staged = controls[0]
            .prepare_mode(&plan.paths[0].target, 2, PathMode::Passthrough)
            .unwrap();
        controls[0].reserve(staged).unwrap().commit();
        let memory = [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
        ];
        let previous = INSIDE.with(|inside| {
            inside.replace(CallbackFrame {
                owner: 1,
                processing: true,
                pipeline: 0,
                started: 0,
            })
        });
        for _ in 0..100 {
            block(&mut pipeline, &buffers, true);
        }
        INSIDE.with(|inside| inside.set(previous));
        assert_eq!(
            [
                CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
                CALLBACK_FREES.load(Ordering::Relaxed),
                CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
            ],
            memory
        );
        assert_eq!(controls[0].status().unwrap().runtime_epoch, 0);
        assert_eq!(controls[1].status().unwrap().runtime_epoch, output_epoch);
        assert_eq!(
            controls[0].status().unwrap().meter.unwrap().latency_samples,
            0
        );
        for (channel, expected) in [0.25_f32, -0.125].into_iter().enumerate() {
            assert!(
                unsafe { &*buffers.output[channel].get() }
                    .iter()
                    .all(|value| (*value - expected).abs() < 1e-7)
            );
            assert!(
                unsafe { &*buffers.input[channel].get() }
                    .iter()
                    .all(|value| value.to_bits() == [0.25_f32, -0.125][channel].to_bits())
            );
        }
        let staged = controls[0]
            .prepare_mode(&plan.paths[0].target, 3, PathMode::Processed)
            .unwrap();
        controls[0].reserve(staged).unwrap().commit();
        let previous = INSIDE.with(|inside| {
            inside.replace(CallbackFrame {
                owner: 1,
                processing: true,
                pipeline: 0,
                started: 0,
            })
        });
        for _ in 0..100 {
            block(&mut pipeline, &buffers, true);
        }
        INSIDE.with(|inside| inside.set(previous));
        assert_eq!(
            [
                CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
                CALLBACK_FREES.load(Ordering::Relaxed),
                CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
            ],
            memory
        );
        assert_eq!(controls[0].status().unwrap().runtime_epoch, 2);
        assert_eq!(controls[1].status().unwrap().runtime_epoch, output_epoch);
        for (channel, expected) in [0.125_f32, -0.125].into_iter().enumerate() {
            assert!(
                unsafe { &*buffers.output[channel].get() }
                    .iter()
                    .all(|value| (*value - expected).abs() < 1e-7)
            );
        }
        assert_eq!(pipeline.counters().faults, 0);
    }

    #[test]
    fn live_parameters_preserve_filter_tail_and_native_failure_does_not_apply() {
        let plan = plan_with_source_and_output(true);
        let buffers = Buffers::new();
        unsafe {
            (*buffers.input[0].get()).fill(0.0);
            buffers.input(0).write(0.25);
        }
        let (mut pipeline, mut controls) = pipeline(&plan, &buffers);
        let prepared = Prepared::new(&plan.paths[0].target, 48_000).unwrap();
        let mut reference = Dsp::without_suppression(&prepared).unwrap();
        let mut expected = [0.0; FRAMES];
        reference
            .process(
                &prepared,
                [unsafe { &*buffers.input[0].get() }, &[]],
                [&mut expected, &mut []],
            )
            .unwrap();
        block(&mut pipeline, &buffers, true);
        let mut source = plan.paths[0].target.clone();
        source.eq.input_gain_db = 20.0 * 0.25_f64.log10();
        controls[0].publish(&source, 2).unwrap();
        let prepared = Prepared::new(&source, 48_000).unwrap();
        unsafe {
            (*buffers.input[0].get()).fill(0.0);
        }
        reference
            .process(
                &prepared,
                [unsafe { &*buffers.input[0].get() }, &[]],
                [&mut expected, &mut []],
            )
            .unwrap();
        // The mixer hears a processed source one frame later, and its
        // hardware output is written one frame after that.
        block(&mut pipeline, &buffers, true);
        block(&mut pipeline, &buffers, true);
        block(&mut pipeline, &buffers, true);
        assert!(expected.iter().any(|value| value.abs() > 1e-6));
        for (actual, expected) in unsafe { &*buffers.output[0].get() }.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-7, "filter history changed");
        }
        controls[0].publish(&source, 3).unwrap();
        block(&mut pipeline, &buffers, false);
        let status = controls[0].status().unwrap();
        assert_eq!(status.seen, 3);
        assert_eq!(status.applied, 2);
        controls[0].publish(&source, 4).unwrap();
        block(&mut pipeline, &buffers, true);
        assert_eq!(controls[0].status().unwrap().applied, 4);
    }

    #[test]
    fn output_settings_are_not_acknowledged_when_native_callback_skips_output() {
        let plan = plan_with_source_and_output(false);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline(&plan, &buffers);
        controls[1].publish(&plan.paths[1].target, 2).unwrap();
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        let input = buffers.input(0);
        pipeline.buffer(input as usize, FRAMES, input.cast());
        unsafe { pipeline.end(FRAMES, true) };
        pipeline.settle();
        let source = controls[0].status().unwrap();
        let output = controls[1].status().unwrap();
        assert_eq!(source.applied, 1);
        assert_eq!(output.published, 2);
        assert_eq!(output.seen, 2);
        assert_eq!(output.applied, 0);
        assert_eq!(pipeline.counters().skipped_outputs, 1);
        controls[1].publish(&plan.paths[1].target, 3).unwrap();
    }

    #[test]
    fn a_stereo_output_never_processes_only_one_observed_channel() {
        for hardware in [false, true] {
            let mut plan = plan_with_source_and_output(false);
            plan.paths[1].hardware = hardware;
            let buffers = Buffers::new();
            let (pipeline, mut controls) = pipeline(&plan, &buffers);
            controls[1].publish(&plan.paths[1].target, 2).unwrap();
            unsafe {
                (*buffers.output[0].get()).fill(0.7);
                (*buffers.output[1].get()).fill(-0.4);
                pipeline.begin(FRAMES, test_buffer, 0);
            }
            let left = buffers.output(0);
            pipeline.buffer(left as usize, FRAMES, left.cast());
            unsafe { pipeline.end(FRAMES, true) };
            pipeline.settle();
            assert_eq!(controls[1].status().unwrap().applied, 0);
            assert_eq!(pipeline.counters().skipped_outputs, 1);
            assert_eq!(unsafe { &*buffers.output[0].get() }, &[0.7; FRAMES]);
            assert_eq!(unsafe { &*buffers.output[1].get() }, &[-0.4; FRAMES]);
        }
    }

    #[test]
    fn a_missing_monitor_port_is_not_replaced_by_the_next_registered_output() {
        let mut plan = plan_with_source_and_output(false);
        plan.paths[1].ports = vec!["out_5".into(), "out_6".into()];
        let buffers = Buffers::new();
        let mut registry = Registry::default();
        assert!(registry.register(1, buffers.input(0) as usize, b"in_1", 1));
        assert!(registry.register(1, buffers.output(0) as usize, b"out_5", 2));
        assert!(registry.register(1, buffers.output(1) as usize, b"out_7", 2));
        assert!(Pipeline::new(&plan, &registry, 1).is_err());
    }

    /// One source path (mono mic or stereo playback) on in_1/in_2, EQ input
    /// gain 0.5, optionally a 120 Hz high-pass (stateful).
    fn source_plan(channels: u8, highpass: bool, mode: PathMode) -> Plan {
        let kind = if channels == 1 {
            TargetKind::Mic
        } else {
            TargetKind::Playback
        };
        let mut target = Target::new("source", "source", kind, channels);
        target.limiter.enabled = false;
        target.gain.enabled = false;
        target.eq.enabled = true;
        target.eq.input_gain_db = 20.0 * 0.5_f64.log10();
        target.highpass.enabled = highpass;
        target.highpass.frequency = 120.0;
        Plan {
            version: 1,
            sample_rate: 48_000,
            frames: FRAMES,
            worker: Some(WorkerPlan {
                cpu_mask: None,
                realtime_priority: None,
                nice: None,
            }),
            paths: vec![PathPlan {
                direction: Direction::Input,
                ports: (1..=channels).map(|c| format!("in_{c}")).collect(),
                target,
                mode,
                hardware: false,
            }],
        }
    }
    fn pipeline_owned(plan: &Plan, buffers: &Buffers) -> (Pipeline, Vec<parameters::Control>) {
        let mut registry = Registry::default();
        for channel in 0..2 {
            assert!(registry.register(
                1,
                buffers.input(channel) as usize,
                format!("in_{}", channel + 1).as_bytes(),
                1
            ));
        }
        Pipeline::with_ownership(plan, &registry, 1, false).unwrap()
    }
    /// One callback in which the mixer requests the source channels in
    /// `order` (repeats allowed) and copies what each request returned.
    fn source_block(
        pipeline: &Pipeline,
        buffers: &Buffers,
        order: &[usize],
        native_success: bool,
    ) -> [[f32; FRAMES]; 2] {
        let mut seen = [[f32::NAN; FRAMES]; 2];
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        for &channel in order {
            let raw = buffers.input(channel);
            let got = pipeline
                .buffer(raw as usize, FRAMES, raw.cast())
                .cast::<f32>();
            let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
            seen[channel].copy_from_slice(values);
        }
        unsafe { pipeline.end(FRAMES, native_success) };
        pipeline.settle();
        seen
    }
    /// One callback as `source_block`, without waiting for the helper.
    fn source_block_unsettled(
        pipeline: &Pipeline,
        buffers: &Buffers,
        order: &[usize],
        native_success: bool,
    ) {
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        for &channel in order {
            let raw = buffers.input(channel);
            pipeline.buffer(raw as usize, FRAMES, raw.cast());
        }
        unsafe { pipeline.end(FRAMES, native_success) };
    }

    /// Helper-owned stages each add one frame: the source's, then the hardware
    /// output's, written by the helper one frame after the mixer made it.
    #[test]
    fn helper_owned_source_and_output_each_add_a_frame() {
        let plan = plan_with_source_and_output(false);
        let buffers = Buffers::new();
        let mut registry = Registry::default();
        for channel in 0..2 {
            assert!(registry.register(
                1,
                buffers.input(channel) as usize,
                format!("in_{}", channel + 1).as_bytes(),
                1
            ));
            assert!(registry.register(
                1,
                buffers.output(channel) as usize,
                format!("out_{}", channel + 1).as_bytes(),
                2
            ));
        }
        let (mut pipeline, _controls) =
            Pipeline::with_ownership(&plan, &registry, 1, false).unwrap();
        // Source 0.25 x 0.5, mixer x 2, output x 0.5: 0.125 once both
        // stages have been applied.
        let mut first = None;
        for frame in 0..4 {
            block(&mut pipeline, &buffers, true);
            let left = unsafe { &*buffers.output[0].get() };
            if first.is_none() && left.iter().all(|v| (v - 0.125).abs() < 1e-6) {
                first = Some(frame);
            }
        }
        assert_eq!(first, Some(2));
        assert_eq!(pipeline.counters().faults, 0);
    }

    /// The mixer may keep every pointer a callback's requests returned and
    /// read them all afterwards, until the callback returns: later and
    /// repeated requests never move or rewrite what an earlier one got.
    #[test]
    fn every_returned_source_pointer_stays_valid_through_the_callback() {
        let plan = source_plan(2, true, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
        let prepared = Prepared::new(&plan.paths[0].target, 48_000).unwrap();
        let mut reference = Dsp::without_suppression(&prepared).unwrap();
        let mut expected = [[0.0; FRAMES]; 2];
        for frame in 0..4 {
            unsafe {
                (*buffers.input[0].get()).fill(0.0);
                (*buffers.input[1].get()).fill(0.0);
                buffers.input(0).add(5 * frame).write(0.5);
                buffers.input(1).add(7 * frame + 2).write(-0.25);
            }
            // The helper hands the mixer the previous frame's result.
            let current = expected;
            let [left, right] = &mut expected;
            reference
                .process(
                    &prepared,
                    unsafe { [&*buffers.input[0].get(), &*buffers.input[1].get()] },
                    [left, right],
                )
                .unwrap();
            let wanted = current;
            unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
            let mut held = Vec::new();
            for channel in [1, 0, 1, 0, 0, 1] {
                let raw = buffers.input(channel);
                let got = pipeline
                    .buffer(raw as usize, FRAMES, raw.cast())
                    .cast::<f32>();
                held.push((channel, got));
            }
            // Read every held pointer only after all requests.
            for &(channel, got) in &held {
                let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
                if frame > 0 {
                    assert_eq!(
                        values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                        wanted[channel].map(f32::to_bits).to_vec(),
                        "frame {frame} channel {channel}"
                    );
                }
            }
            let first = held.iter().find(|(c, _)| *c == 0).unwrap().1;
            assert!(
                held.iter().all(|&(c, p)| c != 0 || p == first),
                "one pointer per channel"
            );
            unsafe { pipeline.end(FRAMES, true) };
            // Still readable after end, until the next callback begins.
            for &(channel, got) in &held {
                let values = unsafe { std::slice::from_raw_parts(got, FRAMES) };
                if frame > 0 {
                    assert_eq!(
                        values[..].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                        wanted[channel].map(f32::to_bits).to_vec()
                    );
                }
            }
            pipeline.settle();
        }
        assert_eq!(pipeline.counters().faults, 0);
    }

    #[test]
    fn a_processed_frame_the_mixer_failed_is_counted_apart() {
        let plan = source_plan(2, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..2 {
            source_block(&pipeline, &buffers, &[0, 1], true);
        }
        let before = pipeline.counters().sources;
        controls[0].publish(&plan.paths[0].target, 2).unwrap();
        // Processed audio delivered, but the mixer's callback failed.
        source_block(&pipeline, &buffers, &[0, 1], false);
        // Processed audio delivered for one channel; the other never asked.
        source_block(&pipeline, &buffers, &[0], true);
        let after = pipeline.counters().sources;
        assert_eq!(after.processed, before.processed);
        assert_eq!(after.raw_invalid, before.raw_invalid);
        // The helper is handed only completed frames: the failed one
        // still delivered last frame's result, but the next has none.
        assert_eq!(
            after.processed_unacknowledged,
            before.processed_unacknowledged + 1
        );
        assert_eq!(after.raw_no_handoff, before.raw_no_handoff + 1);
        assert_eq!(after.raw_returns, before.raw_returns + 1);
        let status = controls[0].status().unwrap();
        assert_eq!((status.seen, status.applied), (2, 1));
        source_block(&pipeline, &buffers, &[0, 1], true);
        assert_eq!(controls[0].status().unwrap().applied, 2);
        let last = pipeline.counters().sources;
        // Its previous frame was incomplete, so not handed off either.
        assert_eq!(last.processed, before.processed);
        assert_eq!(last.raw_no_handoff, after.raw_no_handoff + 1);
    }

    #[test]
    fn the_helper_never_shares_the_suppression_workers_core() {
        // Pinned callback: the other audio cores.
        assert_eq!(helper_mask(0b0100), 0b1010);
        // Before RØDE pins the callback (every core), or with no mask known,
        // the helper still keeps to the audio cores, never CPU 0, where the
        // suppression worker runs (rc2fx_suppression::PRODUCTION_WORKER).
        for callback in [0b1111, 0b1110, 0] {
            let mask = helper_mask(callback);
            assert_ne!(mask, 0, "callback {callback:#b}");
            assert_eq!(mask & !AUDIO_CPUS, 0, "callback {callback:#b}: {mask:#b}");
        }
        assert_eq!(
            rc2fx_suppression::PRODUCTION_WORKER.cpu_mask.unwrap() & AUDIO_CPUS,
            0
        );
    }

    #[test]
    fn placement_is_sampled_fast_until_settled_then_at_the_steady_cadence() {
        let rr = libc::SCHED_RR;
        let mut sampler = Sampler::default();
        let samples = |sampler: &Sampler, cycles: std::ops::Range<u32>| {
            cycles
                .filter(|&cycle| sampler.due(cycle))
                .collect::<Vec<_>>()
        };
        // Startup: every 64 callbacks.
        assert_eq!(&samples(&sampler, 1..300), &[64, 128, 192, 256]);
        // Real-time but not yet pinned (every core): not settled.
        sampler.observe(rr, 0b1111, 1, 1);
        assert!(!sampler.settled);
        // Pinned and real-time, but the helper has not applied it yet.
        sampler.observe(rr, 0b0100, 2, 1);
        assert!(!sampler.settled);
        // Ordinary policy: not settled.
        sampler.observe(libc::SCHED_OTHER, 0b0100, 2, 2);
        assert!(!sampler.settled);
        // A promotion at callback 100 is seen at 128, not 2048.
        let promoted = 100;
        let seen = (promoted..).find(|&cycle| sampler.due(cycle)).unwrap();
        assert!(seen - promoted <= PLACE_FAST_EVERY, "seen at {seen}");
        sampler.observe(rr, 0b0100, 2, 2);
        assert!(sampler.settled);
        // Settled: the steady cadence, unchanged.
        assert_eq!(&samples(&sampler, 1..8193), &[2048, 4096, 6144, 8192]);
        // Never settled: fast only until PLACE_FAST_UNTIL, then steady.
        let unsettled = Sampler::default();
        assert!(unsettled.due(PLACE_FAST_UNTIL - PLACE_FAST_EVERY));
        assert!(!unsettled.due(PLACE_FAST_UNTIL + PLACE_FAST_EVERY));
        assert!(unsettled.due(4 * PLACE_EVERY));
    }

    /// A placement boundary whose callback finds the helper busy has no
    /// handoff to sample with; the sample is taken at the next handoff, not
    /// a whole interval later.
    #[test]
    fn a_placement_boundary_without_a_handoff_is_sampled_at_the_next_one() {
        let _events = super::super::probe::EVENT_TESTS
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let plan = source_plan(1, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
        let shared = &pipeline.helper.shared;
        for _ in 1..PLACE_FAST_EVERY {
            source_block(&pipeline, &buffers, &[0], true);
        }
        assert_eq!(shared.generation.load(Ordering::Acquire), 0);
        // Callback 64: neither idle nor queued, so the helper stays parked
        // and the callback gives up waiting for it.
        shared.state.store(QUEUED + 1, Ordering::Release);
        source_block_unsettled(&pipeline, &buffers, &[0], true);
        assert_eq!(unsafe { &*pipeline.front.get() }.cycle, PLACE_FAST_EVERY);
        assert_eq!(shared.generation.load(Ordering::Acquire), 0);
        shared.state.store(IDLE, Ordering::Release);
        // Callback 65 hands off and takes the deferred sample.
        source_block(&pipeline, &buffers, &[0], true);
        let generation = shared.generation.load(Ordering::Acquire);
        assert!(
            generation >= 1,
            "sample not taken after the missed boundary"
        );
        source_block(&pipeline, &buffers, &[0], true);
        assert_eq!(shared.applied.load(Ordering::Acquire), generation);
        let sampler = unsafe { &*pipeline.front.get() }.sampler;
        assert_eq!(sampler.pending, None);
        assert_eq!(sampler.deferred, 1, "one deferral, counted once");
    }

    #[test]
    fn a_missed_boundary_stays_pending_until_sampled_and_counts_once() {
        let mut sampler = Sampler::default();
        for cycle in 60..64 {
            sampler.mark(cycle);
            assert_eq!(sampler.pending, None);
        }
        // Boundary 64 and the callbacks after it without a handoff.
        for cycle in 64..70 {
            sampler.mark(cycle);
            assert_eq!(sampler.pending, Some(64));
        }
        assert_eq!(sampler.take(70), Some(true));
        assert_eq!(sampler.pending, None);
        assert_eq!(sampler.take(71), None);
        assert_eq!(sampler.deferred, 1);
        // On time: taken at the boundary, not a deferral.
        sampler.mark(128);
        assert_eq!(sampler.take(128), Some(false));
        assert_eq!(sampler.deferred, 1);
        // Missed through the next boundary: still the first boundary's
        // sample, deferred, counted once.
        sampler.mark(192);
        for cycle in 193..256 {
            sampler.mark(cycle);
        }
        sampler.mark(256);
        assert_eq!(sampler.take(256), Some(true));
        assert_eq!(sampler.deferred, 2);
        assert_eq!(sampler.take(257), None);
        // The callback counter wraps onto a boundary.
        sampler.mark(u32::MAX);
        sampler.mark(0);
        assert_eq!(sampler.take(1), Some(true));
        assert_eq!(sampler.deferred, 3);
    }

    /// Runs a pipeline past its first fast placement sample; the report's
    /// history shows what the callback had and what the helper then got.
    #[test]
    fn placement_changes_are_recorded_with_actual_masks() {
        let plan = source_plan(1, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..PLACE_FAST_EVERY + 2 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        let report = placement();
        assert!(report["samples"].as_u64().unwrap() >= 1);
        let history = report["history"].as_array().unwrap();
        assert!(!history.is_empty());
        for record in history {
            assert!(record["generation"].as_u64().unwrap() >= 1);
            // On a boundary, unless its callback had no handoff.
            let boundary = record["cycle"]
                .as_u64()
                .unwrap()
                .is_multiple_of(u64::from(PLACE_FAST_EVERY));
            assert!(boundary || record["deferred"] == true, "{record}");
            assert!(record["atNs"].as_u64().unwrap() > 0);
            if record["appliedAtNs"].as_u64().unwrap() != 0 {
                let requested = record["helperRequestedCpus"].as_u64().unwrap();
                assert_eq!(requested & !AUDIO_CPUS, 0, "never CPU 0: {record}");
                assert!(record["appliedAtNs"].as_u64() >= record["atNs"].as_u64());
            }
        }
    }

    #[test]
    fn a_helper_source_reaches_the_mixer_a_frame_later() {
        let plan = source_plan(1, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        let first = source_block(&pipeline, &buffers, &[0], true);
        let second = source_block(&pipeline, &buffers, &[0], true);
        // Enough frames for one telemetry window.
        for _ in 0..48 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        // The first frame passes raw; processing arrives a frame (112
        // samples) later.
        assert!(first[0].iter().all(|v| (v - 0.25).abs() < 1e-6));
        assert!(second[0].iter().all(|v| (v - 0.125).abs() < 1e-6));
        let status = controls[0].status().unwrap();
        assert_eq!(status.applied, 1);
        let latency = status.meter.unwrap().latency_samples;
        assert_eq!(latency, FRAMES as u32);
        let counts = pipeline.counters().sources;
        assert_eq!(counts.processed, 49);
        assert_eq!(counts.raw_no_handoff, 1);
        assert_eq!(counts.raw_returns, 1);
        assert_eq!(pipeline.counters().input_blocks, 50);
        assert_eq!(pipeline.counters().faults, 0);
    }

    #[test]
    fn a_later_raw_request_counts_the_whole_frame_raw() {
        let plan = source_plan(2, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, _controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..2 {
            source_block(&pipeline, &buffers, &[0, 1], true);
        }
        let before = pipeline.counters().sources;
        assert_eq!(before.processed, 1);
        // Left is served processed, then right arrives with a buffer that
        // is not the one resolved for this frame.
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        let left = buffers.input(0);
        let got = pipeline.buffer(left as usize, FRAMES, left.cast());
        assert_ne!(got, left.cast(), "left processed");
        let right = buffers.input(1);
        let mut elsewhere = [0.0f32; FRAMES];
        let got = pipeline.buffer(right as usize, FRAMES, elsewhere.as_mut_ptr().cast());
        assert_eq!(got, elsewhere.as_mut_ptr().cast(), "right raw");
        unsafe { pipeline.end(FRAMES, true) };
        pipeline.settle();
        let after = pipeline.counters().sources;
        assert_eq!(after.processed, before.processed);
        assert_eq!(after.raw_invalid, before.raw_invalid + 1);
        assert_eq!(after.raw_returns, before.raw_returns + 1);
        // The failed path stays raw, and each frame says so.
        source_block(&pipeline, &buffers, &[0, 1], true);
        let failed = pipeline.counters().sources;
        assert_eq!(failed.raw_invalid, after.raw_invalid + 1);
        assert_eq!(failed.raw_returns, after.raw_returns + 2);
        assert_eq!(pipeline.counters().faults, 1);
    }

    #[test]
    fn an_unrequested_source_frame_is_counted_and_not_acknowledged() {
        let plan = source_plan(1, false, PathMode::Processed);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        source_block(&pipeline, &buffers, &[0], true);
        controls[0].publish(&plan.paths[0].target, 2).unwrap();
        source_block(&pipeline, &buffers, &[], true);
        let status = controls[0].status().unwrap();
        assert_eq!((status.seen, status.applied), (2, 1));
        let counts = pipeline.counters().sources;
        assert_eq!(counts.unobserved, 1);
        source_block(&pipeline, &buffers, &[0], true);
        assert_eq!(controls[0].status().unwrap().applied, 2);
    }

    /// The mic to headphones path of the Pro II profile: DeepFilterNet3's
    /// 240-sample frame, 120 intrinsic, the 320 lead and the helper's period,
    /// 792 samples (16.5 ms).
    #[test]
    fn a_deepfilternet_mic_reports_the_profile_latency() {
        let mut plan = source_plan(1, false, PathMode::Processed);
        plan.paths[0].target.suppression.enabled = true;
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..40 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        assert_eq!(
            controls[0].status().unwrap().meter.unwrap().latency_samples,
            240 + 120 + 320 + FRAMES as u32
        );
        assert_eq!(pipeline.counters().faults, 0);
    }

    /// DeepFilterNet3's lookahead is a live setting: a transaction that
    /// changes it replaces only that path's suppression worker (the old
    /// runtime is admitted out and a new one published), the callback does no
    /// memory work meanwhile, and the reported latency follows by one hop.
    #[test]
    fn a_live_lookahead_switch_rebuilds_the_worker_and_moves_the_latency_by_one_hop() {
        use super::super::{
            CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
        };
        let mut plan = source_plan(1, false, PathMode::Processed);
        plan.paths[0].target.suppression.enabled = true;
        plan.paths[0].target.suppression.lookahead = 0;
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..40 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        let hop = rc2fx_core::DFN_HOP_SAMPLES;
        let base = rc2fx_suppression::Geometry::for_lookahead(0)
            .unwrap()
            .latency_samples() as u32;
        let helper = FRAMES as u32;
        let latency = |controls: &mut [parameters::Control]| {
            controls[0].status().unwrap().meter.unwrap().latency_samples
        };
        assert_eq!(latency(&mut controls), base + helper);
        // Every switch is a live transaction on this path alone, and the
        // callbacks either side of it do no memory work (the transaction
        // itself is control work and may allocate).
        let memory = [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
        ];
        // Revisions must climb past the one the plan was built with.
        let first = controls[0].status().unwrap().applied + 1;
        let mut previous_epoch = controls[0].status().unwrap().runtime_epoch;
        for (revision, lookahead, expected) in [
            (first, 1, base + hop + helper),
            (first + 1, 0, base + helper),
            (first + 2, 1, base + hop + helper),
            (first + 3, 0, base + helper),
        ] {
            let retiring = controls[0].worker_telemetry().unwrap().unwrap().1;
            let mut target = plan.paths[0].target.clone();
            target.suppression.lookahead = lookahead;
            let staged = controls[0]
                .prepare_mode(&target, revision, PathMode::Processed)
                .unwrap();
            controls[0].reserve(staged).unwrap().commit();
            let previous = INSIDE.with(|inside| {
                inside.replace(CallbackFrame {
                    owner: 1,
                    processing: true,
                    pipeline: 0,
                    started: 0,
                })
            });
            for _ in 0..40 {
                source_block(&pipeline, &buffers, &[0], true);
            }
            INSIDE.with(|inside| inside.set(previous));
            assert_eq!(latency(&mut controls), expected, "lookahead {lookahead}");
            let status = controls[0].status().unwrap();
            assert_eq!(status.applied, revision);
            assert!(
                status.runtime_epoch > previous_epoch,
                "a fresh runtime was published"
            );
            previous_epoch = status.runtime_epoch;
            let retired = controls[0].retired_workers();
            assert_eq!(retired.len(), 1);
            let worker = &retired[0].worker;
            assert!(worker.completed_frames > 0);
            let terminal = retiring.snapshot();
            for (reported, raw) in [
                (worker.wall, terminal.wall),
                (worker.cpu, terminal.cpu),
                (worker.turnaround, terminal.turnaround),
            ] {
                assert_eq!(
                    reported.calls, raw.calls,
                    "retirement must contain only this worker's timing"
                );
                assert_eq!(reported.total_ns, raw.total_ns);
            }
        }
        assert_eq!(
            [
                CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
                CALLBACK_FREES.load(Ordering::Relaxed),
                CALLBACK_REALLOCATIONS.load(Ordering::Relaxed)
            ],
            memory,
            "no callback allocation across the switches"
        );
        let counters = pipeline.counters();
        assert_eq!(counters.faults, 0);
        assert!(counters.sources.processed >= 100, "{:?}", counters.sources);
    }

    #[test]
    fn a_suppressed_mic_reports_its_dsp_delay_and_the_helper_frame() {
        let mut plan = source_plan(1, false, PathMode::Processed);
        plan.paths[0].target.suppression.enabled = true;
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..40 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        let worker = rc2fx_suppression::Geometry::for_lookahead(0)
            .unwrap()
            .latency_samples() as u32;
        let latency = controls[0].status().unwrap().meter.unwrap().latency_samples;
        assert_eq!(latency, worker + FRAMES as u32);
        let counters = pipeline.counters();
        assert_eq!(counters.faults, 0);
        assert_eq!(counters.sources.processed, 39);
        // The worker runs asynchronously; give it time to catch up.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while controls[0].worker_status().unwrap().completed_frames == 0 {
            assert!(std::time::Instant::now() < deadline, "worker never ran");
            thread::sleep(std::time::Duration::from_millis(1));
        }
        // The report's outlier records come from the same worker.
        let outliers = controls[0].worker_outliers().unwrap();
        assert_eq!(outliers["worst"]["state"], "stable");
        assert!(outliers["worst"]["turnaroundNs"].as_u64().unwrap() > 0);
        // Cumulative turnaround counts above 5.5, 6.0, 6.5 and 7.0 ms,
        // alongside the worst; each threshold counts at most its
        // predecessor, and the worst must exceed the top one it counts.
        let above = &outliers["turnaroundsAboveMs"];
        let counts: Vec<u64> = ["5.5", "6.0", "6.5", "7.0"]
            .iter()
            .map(|key| above[key].as_u64().unwrap())
            .collect();
        assert!(
            counts.windows(2).all(|pair| pair[1] <= pair[0]),
            "{counts:?}"
        );
        assert!(["empty", "stable"].contains(&outliers["lastMiss"]["state"].as_str().unwrap()));
        for kind in ["missHistory", "lateHistory"] {
            let history = &outliers[kind];
            let count = |key: &str| history[key].as_u64().unwrap();
            assert_eq!(
                count("total"),
                count("recorded") + count("lost") + count("unpublished")
            );
            assert_eq!(
                count("recorded"),
                history["events"].as_array().unwrap().len() as u64
            );
        }
        // The history holds the latest miss whenever it has room.
        if outliers["lastMiss"]["state"] == "stable" && outliers["missHistory"]["lost"] == 0 {
            let events = outliers["missHistory"]["events"].as_array().unwrap();
            assert_eq!(
                events.last().unwrap()["sequence"],
                outliers["lastMiss"]["sequence"]
            );
        }
    }

    fn full_registry() -> Registry {
        let mut registry = Registry::default();
        for pointer in 1..=512 {
            assert!(registry.register(1, pointer, format!("in_{pointer}").as_bytes(), 1));
        }
        registry
    }

    #[test]
    fn replacement_at_capacity_retires_old_identity_before_admitting_new_one() {
        let mut registry = full_registry();
        let retired = Arc::clone(&registry.ports[0]);
        assert!(registry.register(1, retired.pointer, b"replacement", 1));
        assert_eq!(registry.ports.len(), 512);
        assert!(!retired.alive.load(Ordering::Acquire));
        let replacement = registry.ports.last().unwrap();
        assert!(replacement.alive.load(Ordering::Acquire));
        assert_eq!(replacement.pointer, retired.pointer);
        assert_eq!(replacement.name, "replacement");
        assert!(!Arc::ptr_eq(replacement, &retired));
    }

    #[test]
    fn registry_capacity_refuses_new_identity_without_retiring_other_ports() {
        let mut registry = full_registry();
        assert!(!registry.register(1, 513, b"extra", 1));
        assert_eq!(registry.ports.len(), 512);
        assert!(
            registry
                .ports
                .iter()
                .all(|pin| pin.alive.load(Ordering::Acquire))
        );
    }

    #[test]
    fn untrackable_replacement_does_not_leave_old_pointer_identity_alive() {
        let mut registry = Registry::default();
        assert!(registry.register(1, 1, b"source", 1));
        let retired = Arc::clone(&registry.ports[0]);
        assert!(!registry.register(1, 1, &[0xff], 1));
        assert!(!retired.alive.load(Ordering::Acquire));
        assert!(registry.ports.is_empty());
    }

    thread_local! {
        /// The pins `begin` resolved, in order (`recording_buffer`).
        static RESOLVED: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    unsafe extern "C" fn recording_buffer(port: *mut c_void, _: u32) -> *mut c_void {
        RESOLVED.with(|resolved| resolved.borrow_mut().push(port as usize));
        port
    }

    #[test]
    fn unprocessed_paths_meter_once_in_the_stride_on_staggered_cycles() {
        // Three meter-only paths: two mics and a hardware output.
        let mut plan = plan_with_source_and_output(false);
        let second = PathPlan {
            direction: Direction::Input,
            ports: vec!["in_2".into()],
            target: Target::new("second", "second", TargetKind::Mic, 1),
            mode: PathMode::Passthrough,
            hardware: false,
        };
        plan.paths.insert(1, second);
        for path in &mut plan.paths {
            path.mode = PathMode::Passthrough;
        }
        let buffers = Buffers::new();
        let mut registry = Registry::default();
        for channel in 0..2 {
            assert!(registry.register(
                1,
                buffers.input(channel) as usize,
                format!("in_{}", channel + 1).as_bytes(),
                1
            ));
            assert!(registry.register(
                1,
                buffers.output(channel) as usize,
                format!("out_{}", channel + 1).as_bytes(),
                2
            ));
        }
        let (pipeline, _controls) = Pipeline::with_ownership(&plan, &registry, 1, false).unwrap();
        let first_pin = [buffers.input(0), buffers.input(1), buffers.output(0)];
        let mut metered = [const { Vec::new() }; 3];
        for cycle in 0..2 * METER_STRIDE {
            RESOLVED.with(|resolved| resolved.borrow_mut().clear());
            unsafe { pipeline.begin(FRAMES, recording_buffer, 0) };
            // The mixer asks for every buffer every cycle and always gets
            // its own: metering never changes the audio.
            for pin in [
                buffers.input(0),
                buffers.input(1),
                buffers.output(0),
                buffers.output(1),
            ] {
                assert_eq!(
                    pipeline.buffer(pin as usize, FRAMES, pin.cast()),
                    pin.cast()
                );
            }
            unsafe { pipeline.end(FRAMES, true) };
            pipeline.settle();
            let resolved = RESOLVED.with(|resolved| resolved.borrow().clone());
            for (path, pin) in first_pin.iter().enumerate() {
                if resolved.contains(&(*pin as usize)) {
                    metered[path].push(cycle);
                }
            }
        }
        for (path, cycles) in metered.iter().enumerate() {
            assert_eq!(cycles.len(), 2, "path {path}: {cycles:?}");
            assert_eq!(cycles[1] - cycles[0], METER_STRIDE);
            let expected: Vec<u32> = (0..2 * METER_STRIDE)
                .filter(|&cycle| meters(cycle, path))
                .collect();
            assert_eq!(*cycles, expected);
        }
        for a in 0..3 {
            for b in a + 1..3 {
                assert!(
                    metered[a].iter().all(|cycle| !metered[b].contains(cycle)),
                    "paths {a} and {b} meter on the same cycle: {metered:?}"
                );
            }
        }
        let counters = pipeline.counters();
        assert_eq!(counters.faults, 0);
        assert_eq!(counters.sources, SourceCounts::default());
    }

    #[test]
    fn the_meter_stagger_survives_the_cycle_counter_wrapping() {
        for path in 0..MAX_PATHS {
            let hits = (0..2 * METER_STRIDE)
                .filter(|&k| meters((u32::MAX - METER_STRIDE + 1).wrapping_add(k), path))
                .count();
            assert_eq!(hits, 2, "path {path}");
        }
    }

    /// Fills the mono source with `x`; one settled callback; what the mixer
    /// read (every sample equal).
    fn source_value(pipeline: &Pipeline, buffers: &Buffers, x: f32) -> f32 {
        unsafe { (*buffers.input[0].get()).fill(x) };
        let seen = source_block(pipeline, buffers, &[0], true);
        assert!(seen[0].iter().all(|v| v.to_bits() == seen[0][0].to_bits()));
        seen[0][0]
    }
    fn source_owner(pipeline: &Pipeline) -> Owner {
        unsafe { &*pipeline.front.get() }.ports[0].owner
    }
    fn commit_mode(control: &mut parameters::Control, plan: &Plan, revision: u64, mode: PathMode) {
        let staged = control
            .prepare_mode(&plan.paths[0].target, revision, mode)
            .unwrap();
        control.reserve(staged).unwrap().commit();
    }
    /// The source plan's EQ halves the input.
    fn processed(x: f32) -> f32 {
        0.5 * x
    }
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    /// Sources stay with the helper: it processes the source one frame later,
    /// before and after a live change.
    #[test]
    fn a_helper_source_keeps_its_owner_across_live_mode_changes() {
        let plan = source_plan(1, false, PathMode::Passthrough);
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        let mut out = Vec::new();
        for k in 0..30 {
            if k == 5 {
                commit_mode(&mut controls[0], &plan, 2, PathMode::Processed);
            }
            if k == 20 {
                commit_mode(&mut controls[0], &plan, 3, PathMode::Passthrough);
            }
            let x = 0.1 + 0.001 * k as f32;
            out.push((x, source_value(&pipeline, &buffers, x)));
            assert!(source_owner(&pipeline) == Owner::Helper);
            let port = &unsafe { &*pipeline.front.get() }.ports[0];
            assert!(port.reader.is_none());
        }
        // Adopted under frame 5; frame 6 is raw (nothing handed off yet),
        // then each frame gets the previous one processed.
        for k in 7..20 {
            assert!(
                close(out[k].1, processed(out[k - 1].0)),
                "frame {k}: {:?}",
                out[k]
            );
        }
        assert_eq!(pipeline.counters().faults, 0);
    }

    /// A suppressed mic switched live to Processed reports its DSP delay plus
    /// the helper frame, as a mic planned Processed does.
    #[test]
    fn a_suppressed_mic_switched_live_reports_the_helper_frame() {
        let mut plan = source_plan(1, false, PathMode::Passthrough);
        plan.paths[0].target.suppression.enabled = true;
        let worker = rc2fx_suppression::Geometry::for_lookahead(0)
            .unwrap()
            .latency_samples() as u32;
        let buffers = Buffers::new();
        let (pipeline, mut controls) = pipeline_owned(&plan, &buffers);
        for _ in 0..5 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        commit_mode(&mut controls[0], &plan, 2, PathMode::Processed);
        for _ in 0..40 {
            source_block(&pipeline, &buffers, &[0], true);
        }
        let latency = controls[0].status().unwrap().meter.unwrap().latency_samples;
        assert_eq!(latency, worker + FRAMES as u32);
        assert!(source_owner(&pipeline) == Owner::Helper);
        assert_eq!(pipeline.counters().faults, 0);
    }
}
