//! Callback-owned signal state; construction and destruction are control work.
mod helper;
mod lookup;
mod placement;
mod plan;
mod stats;

use super::{parameters, prefetch};
#[cfg(test)]
use helper::Shared;
use helper::{HELPER_WAIT_NS, Helper, Woke};
use lookup::PinLookup;
use placement::Sampler;
pub use placement::placement;
pub use plan::{Direction, Pin, Plan, Registry};
use rc2fx_dsp::Prepared;
use rc2fx_native_control::PathMode;
pub use stats::{
    Counters, HELPER_LATE, HELPER_MIN_MARGIN_NS, HELPER_STALLS, HELPER_TIME, PATH_COST_NS,
    RESOLVE_NS, processing_counters, sources, wake_gauges,
};
use stats::{
    END, HELPER_SLACK, LIVE_FAULTS, LIVE_INPUT_BLOCKS, LIVE_OUTPUT_BLOCKS, LIVE_SKIPPED_OUTPUTS,
    Outcome, PLACE, SOURCES, WAKE_CALL, WAKE_SENT, WAKES, charge, tally,
};
#[cfg(test)]
use std::thread;
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub const FRAMES: usize = 112;
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
/// Which side processes a path. Ownership follows the direction and is fixed
/// at construction, not chosen per mode or history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// In the callback, in place, after the mixer's own processing returns and
    /// before the callback does: every output, USB sends and hardware outputs
    /// alike, meter-only ones included.
    Callback,
    /// By the helper, from a copy the callback makes: sources, which reach the
    /// mixer one frame later. The helper never touches a jackd buffer.
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
    /// The path's unprocessed-audio fade (`parameters::RAW_*`), and where it
    /// stands: 0 (silent) to `STAGE_FADE_SAMPLES` (as the mixer made it).
    pass: Arc<parameters::PassFade>,
    raw_position: u32,
    /// This frame's unprocessed-audio gain: from `raw_from`, a step of
    /// `raw_step` (-1, 0 or 1) a sample; `raw_scale` when it is not 1.
    raw_from: u32,
    raw_step: i32,
    raw_scale: bool,
    /// The last `faded` answered.
    raw_answered: bool,
    /// A processor's frame has reached the mixer since the path last played
    /// unprocessed audio.
    processed_flowing: bool,
    /// A helper-owned source's faded unprocessed audio, handed to the mixer
    /// in place of the jackd buffer.
    raw_scratch: Box<[[f32; FRAMES]; 2]>,
}
/// `Port::raw` for a callback-owned output, in place: the frame's gain,
/// fading from `from` by `step` a sample.
fn scale_raw(from: u32, step: i32, buffers: [&mut [f32]; 2]) {
    for buffer in buffers {
        for (t, sample) in buffer.iter_mut().enumerate() {
            *sample *= raw_gain(from, step, t);
        }
    }
}
/// Sample `t`'s gain of a frame of unprocessed audio fading from `from` by
/// `step` a sample.
#[inline(always)]
fn raw_gain(from: u32, step: i32, t: usize) -> f32 {
    let at = (from as i64 + i64::from(step) * (t as i64 + 1))
        .clamp(0, i64::from(rc2fx_dsp::STAGE_FADE_SAMPLES));
    at as f32 / rc2fx_dsp::STAGE_FADE_SAMPLES as f32
}
impl Port {
    /// Decides this frame's unprocessed-audio gain (see `parameters::RAW_*`)
    /// from the mode the path is in (`processed_intent`) and the requested
    /// fade, and answers control. Returns whether the path's unprocessed
    /// audio needs the gain every frame, so the path must be resolved and
    /// visited every cycle until it does not. One atomic load.
    fn step_raw(&mut self) -> bool {
        use parameters::{RAW_MUTE, RAW_PLAY, RAW_RISE};
        let full = rc2fx_dsp::STAGE_FADE_SAMPLES;
        let target = self.pass.target();
        let processed = self.processed_intent;
        self.raw_from = self.raw_position;
        if processed {
            // Only a fallback frame plays unprocessed audio now: as the mixer
            // made it, unless a fade to or from the processor is under way.
            self.raw_position = match target {
                RAW_PLAY => full,
                RAW_RISE => 0,
                // Silent until the processor's first frame arrives.
                _ if self.processed_flowing => full,
                _ => 0,
            };
            self.raw_from = self.raw_position;
            self.raw_step = 0;
        } else {
            self.processed_flowing = false;
            self.raw_step = if target == RAW_MUTE { -1 } else { 1 };
            self.raw_position = (self.raw_position as i64
                + i64::from(self.raw_step) * FRAMES as i64)
                .clamp(0, i64::from(full)) as u32;
        }
        self.raw_scale = !(self.raw_from == full && self.raw_step >= 0);
        let faded = !processed && target == RAW_MUTE && self.raw_position == 0;
        if faded != self.raw_answered {
            self.raw_answered = faded;
            self.pass.answer(faded);
        }
        !processed && self.raw_scale
    }
    /// The unprocessed audio of a source's `channel` for the mixer: the jackd
    /// buffer itself, or this frame's faded copy of it.
    ///
    /// # Safety
    /// `native` is this channel's live jackd buffer of `FRAMES` samples.
    unsafe fn raw(&mut self, channel: usize, native: *mut c_void) -> *mut c_void {
        if !self.raw_scale {
            return native;
        }
        let input = unsafe { std::slice::from_raw_parts(native as *const f32, FRAMES) };
        let output = &mut self.raw_scratch[channel];
        for (t, (out, sample)) in output.iter_mut().zip(input).enumerate() {
            *out = sample * raw_gain(self.raw_from, self.raw_step, t);
        }
        output.as_mut_ptr().cast()
    }

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
    fn note(&mut self, outcome: Outcome) {
        if self.direction != Direction::Input || !self.processed_intent {
            return;
        }
        if outcome != Outcome::Processed {
            SOURCES.raw_returns.fetch_add(1, Ordering::Relaxed);
            #[cfg(test)]
            stats::here::tally(None);
        }
        if matches!(self.outcome, None | Some(Outcome::Processed)) {
            self.outcome = Some(outcome);
        }
    }
    /// Counts this frame's outcome for a source meant to be processed;
    /// `usable`: the frame completed (native success, every channel).
    fn tally_source(&mut self, usable: bool) {
        if self.direction == Direction::Input && self.processed_intent {
            let outcome = match self.outcome {
                None => Outcome::Unobserved,
                Some(Outcome::Processed) if !usable => Outcome::ProcessedUnacknowledged,
                Some(outcome) => outcome,
            };
            tally(outcome);
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
            Job::Input if !processed => {
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
) -> bool {
    port.active = false;
    port.counted = false;
    port.observed = 0;
    port.outcome = None;
    if let Some(reader) = &mut port.reader
        && !reader.begin()
    {
        port.fail(counters);
        return false;
    }
    if port.failed {
        return false;
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
    // Unprocessed audio being faded is handled every frame, like processed.
    let fading = port.step_raw();
    if unprocessed && !fading && !meters(cycle, index) {
        port.counted = true;
        return false;
    }
    if frames != FRAMES || !port.alive() {
        port.fail(counters);
        return fading;
    }
    let mut valid = true;
    for (channel, pin) in port.pins.iter().enumerate() {
        // Same lookup rules as the vendor's own calls: the cache answers only
        // where the pointer cannot change within this graph generation.
        let pointer =
            unsafe { super::port_buffer_in_frame(get, pin.pointer as *mut c_void, frames as u32) }
                as usize;
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
        return fading;
    }
    if port.direction == Direction::Output && port.pins.len() == 2 {
        let [a, b] = port.native;
        if a < b + frames * 4 && b < a + frames * 4 {
            port.fail(counters);
            return fading;
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
    fading
}
/// What every path's end of the callback shares.
struct Ended {
    frames: usize,
    native_success: bool,
    slot: usize,
    seq: u64,
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
    port.tally_source(usable);
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
                    if port.raw_scale {
                        scale_raw(port.raw_from, port.raw_step, [&mut *left, &mut *right]);
                    }
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
                    // Only sources are helper-owned (see Pipeline::new).
                    debug_assert_eq!(port.direction, Direction::Input);
                    copy([&*left, &*right], &mut work.raw[cycle.slot], cycle.frames);
                    work.captured[cycle.slot] = cycle.seq;
                    job = Job::Input;
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
                Direction::Output => Owner::Callback,
            };
            let pass = reader.pass_fade();
            // A callback-owned path's processor reads the very samples the
            // unprocessed path would play, at the same time.
            pass.set_aligned(owner == Owner::Callback);
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
                pass,
                raw_position: rc2fx_dsp::STAGE_FADE_SAMPLES,
                raw_from: rc2fx_dsp::STAGE_FADE_SAMPLES,
                raw_step: 0,
                raw_scale: false,
                raw_answered: false,
                processed_flowing: false,
                raw_scratch: Box::new([[0.0; FRAMES]; 2]),
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
    /// After callbacks have stopped. Waits for the helper, so never from the
    /// callback or while reporting.
    #[cfg(test)]
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
            let mut fading = false;
            for (index, port) in front.ports.iter_mut().enumerate() {
                fading |= unsafe {
                    begin_port(index, port, &mut front.counters, seen, cycle, frames, get)
                };
            }
            // Every path is visited until no unprocessed audio is fading.
            if fading {
                front.full_left = METER_STRIDE;
            }
        } else {
            // A sparse cycle has the handoff: the helper is idle.
            if let Some(handoff) = seen {
                unsafe { front.warm(handoff, None) };
            }
            for &index in &front.visits[front.phase] {
                let port = &mut front.ports[index];
                let planned = port.processed_intent;
                let fading = unsafe {
                    begin_port(index, port, &mut front.counters, seen, cycle, frames, get)
                };
                // A new mode can be seen before its publication is counted.
                // Every following frame must then be visited, or a processed
                // frame would sit alone between unprocessed ones.
                if port.processed_intent != planned || fading {
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
            item.note(Outcome::Invalid);
            return native;
        }
        if frames != FRAMES
            || !item.pins[channel].alive.load(Ordering::Acquire)
            || item.native[channel] != native as usize
        {
            item.fail(&mut front.counters);
            item.note(Outcome::Invalid);
            return native;
        }
        item.observed |= 1 << channel;
        if item.direction != Direction::Input {
            return native;
        }
        if !front.idle || !front.previous_ready {
            item.note(Outcome::HelperUnready);
            // SAFETY: checked above to be this channel's jackd buffer.
            return unsafe { item.raw(channel, native) };
        }
        // The mixer reads the source processed over the last frame.
        // SAFETY: the helper is idle (see `begin`).
        let handoff = unsafe { &mut *self.handoff.get() };
        let Some(work) = &mut handoff.work[index] else {
            return native;
        };
        let previous = ((front.seq - 1) & 1) as usize;
        if work.done[previous] {
            item.note(Outcome::Processed);
            item.processed_flowing = true;
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
            item.note(outcome);
            // SAFETY: as above.
            unsafe { item.raw(channel, native) }
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
            seq: front.seq,
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
#[path = "pipeline/tests/core.rs"]
mod tests;
