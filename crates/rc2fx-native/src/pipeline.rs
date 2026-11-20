//! Audio-thread DSP pipeline and helper-thread handoff.
//! Construction and teardown happen on the control thread.
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
/// Unprocessed paths are metered once every `METER_STRIDE` callbacks (staggered
/// by path index); frames in between are still counted.
const METER_STRIDE: u32 = 8;
fn meters(cycle: u32, path: usize) -> bool {
    cycle.wrapping_add(path as u32).is_multiple_of(METER_STRIDE)
}
/// Which thread processes a path (fixed by direction at construction).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Owner {
    /// Outputs run in-place on the callback thread after the vendor callback.
    Callback,
    /// Sources run on the helper thread from a copied buffer (1-frame latency);
    /// the helper never touches JACK buffers directly.
    Helper,
}
/// Per-path work item handed to the helper thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    /// Untouched by the callback; helper resolves to `Count` or `None` based on `native_success`.
    Skipped,
    None,
    Count,
    Meter,
    Input,
}

/// Callback-side state for one bound audio path.
struct Port {
    direction: Direction,
    pins: Vec<Arc<Pin>>,
    owner: Owner,
    /// Present for callback-owned outputs; helper-owned sources keep their reader in `Work`.
    reader: Option<parameters::Reader>,
    processed_intent: bool,
    outcome: Option<Outcome>,
    chat: Option<Box<super::chat_mix::Capture>>,
    native: [usize; 2],
    observed: u8,
    active: bool,
    /// True when an unprocessed path was skipped this cycle (buffers not resolved).
    counted: bool,
    failed: bool,
    /// `Front::skipped_frames` snapshot from the last visit.
    credited: u64,
    /// Passthrough crossfade state (`0` = muted, `STAGE_FADE_SAMPLES` = unity).
    pass: Arc<parameters::PassFade>,
    raw_position: u32,
    raw_from: u32,
    raw_step: i32,
    raw_scale: bool,
    raw_answered: bool,
    /// True once at least one processed frame has been delivered since the last passthrough.
    processed_flowing: bool,
    /// Scratch buffer for fading a helper-owned source's raw audio.
    raw_scratch: Box<[[f32; FRAMES]; 2]>,
}
fn scale_raw(from: u32, step: i32, buffers: [&mut [f32]; 2]) {
    for buffer in buffers {
        for (t, sample) in buffer.iter_mut().enumerate() {
            *sample *= raw_gain(from, step, t);
        }
    }
}
#[inline(always)]
fn raw_gain(from: u32, step: i32, t: usize) -> f32 {
    let at = (from as i64 + i64::from(step) * (t as i64 + 1))
        .clamp(0, i64::from(rc2fx_dsp::STAGE_FADE_SAMPLES));
    at as f32 / rc2fx_dsp::STAGE_FADE_SAMPLES as f32
}
impl Port {
    /// Advances the passthrough fade ramp for this frame and updates the
    /// control-visible `faded` flag. Returns true if raw audio is actively
    /// fading and must be visited every cycle.
    fn step_raw(&mut self) -> bool {
        use parameters::{RAW_MUTE, RAW_PLAY, RAW_RISE};
        let full = rc2fx_dsp::STAGE_FADE_SAMPLES;
        let target = self.pass.target();
        let processed = self.processed_intent;
        self.raw_from = self.raw_position;
        if processed {
            // While processed, raw audio is only used on fallback frames.
            self.raw_position = match target {
                RAW_PLAY => full,
                RAW_RISE => 0,
                // Hold silent until the first processed frame arrives.
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
    /// Returns either `native` directly or a faded copy in `raw_scratch`.
    ///
    /// # Safety
    /// `native` must point to `FRAMES` valid `f32` samples for this callback.
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
    /// Records a single buffer lookup outcome for a processed source, keeping
    /// the first raw fallback cause for the frame.
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
    /// Clears per-cycle flags so skipped paths look unvisited to `buffer()`.
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
    fn warm_begin(&self, work: *const Option<Work>) {
        match &self.reader {
            Some(reader) => reader.warm_begin(),
            None => prefetch::read(work, 1),
        }
        for pin in &self.pins {
            prefetch::read(Arc::as_ptr(pin), 1);
        }
    }
    /// # Safety
    /// Caller must own `work` (helper idle), as in `Pipeline::end`.
    unsafe fn warm_end(&self, work: *const Option<Work>, slot: usize) {
        let channels = if self.active { self.pins.len() } else { 0 };
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
    /// Resolved native buffers must remain valid for this callback.
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
/// Double-buffered audio state for a helper-owned source path.
///
/// Indexed by frame parity (`slot = seq & 1`): the callback copies raw input
/// into `raw[slot]` and hands the mixer `processed[slot ^ 1]` from the previous
/// frame, while the helper processes `raw[slot]` into `processed[slot]`.
/// `UnsafeCell` lets `buffer()` hand out raw pointers to `processed[slot ^ 1]`
/// without taking a mutable borrow.
struct Work {
    reader: parameters::Reader,
    stereo: bool,
    raw: Box<[[[f32; FRAMES]; 2]; 2]>,
    processed: Box<[UnsafeCell<[[f32; FRAMES]; 2]>; 2]>,
    /// Sequence number of the frame copied into `raw[slot]`.
    captured: [u64; 2],
    /// Whether `processed[slot]` holds valid DSP output for `raw[slot]`.
    done: [bool; 2],
    processed_mode: bool,
}
/// Shared handoff block transferred between the callback (when helper is idle)
/// and the helper (from `queue` until `IDLE`).
struct Handoff {
    seq: u64,
    frames: usize,
    cycle_start: u64,
    started: u64,
    native_success: bool,
    jobs: Vec<Job>,
    work: Vec<Option<Work>>,
    counters: Counters,
    done_seq: u64,
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
    /// Precomputed per-phase visit lists for sparse cycles (processed/failed
    /// paths every cycle, plus each passthrough path once per `METER_STRIDE`).
    /// Cleared and refilled in place to avoid allocations.
    visits: [Vec<usize>; METER_STRIDE as usize],
    /// Remaining full-scan cycles before switching back to sparse visits.
    full_left: u32,
    full: bool,
    phase: usize,
    publications: u64,
    faults: u64,
    /// Completed sparse cycles owed to paths that were skipped.
    skipped_frames: u64,
    counters: Counters,
    cycle: u32,
    seq: u64,
    idle: bool,
    previous_ready: bool,
    sampler: Sampler,
    /// Cached copy of `gauges::enabled()` to avoid atomic loads per path.
    gauged: bool,
}
pub struct Pipeline {
    front: UnsafeCell<Front>,
    handoff: UnsafeCell<Handoff>,
    helper: Helper,
    publications: Arc<AtomicU64>,
}
// SAFETY: `front` is callback-thread only; `handoff` ownership alternates
// strictly between callback and helper via `Helper::state` Acquire/Release.
unsafe impl Sync for Pipeline {}
unsafe impl Send for Pipeline {}
impl Drop for Pipeline {
    fn drop(&mut self) {
        // Stop and join the helper before `handoff` drops (fields drop in declaration order).
        self.helper.stop();
    }
}
/// Computes peak/RMS/clipping for an unprocessed path. Written as plain loops
/// rather than a flattened iterator so LLVM keeps the inner loop tight.
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
    /// Prefetches the `Port` structs and referenced buffers/DSP state for the
    /// paths visited in this sparse cycle. Kept `#[inline(never)]` so its cost
    /// shows up cleanly in CPU profiles.
    ///
    /// # Safety
    /// Caller must own `handoff` (helper idle).
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
    /// Returns true once every port in `plan` has a unique live registration on
    /// `client` (hosts often register the process callback before all ports).
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
            // Callback-owned outputs process the same frame in-place, so dry
            // and wet are sample-aligned.
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
    /// Records the callback's exit timestamp so the helper can forecast the
    /// next handoff time.
    pub fn ended(&self, at: u64) {
        self.helper.shared.ended.store(at, Ordering::Relaxed);
    }
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
