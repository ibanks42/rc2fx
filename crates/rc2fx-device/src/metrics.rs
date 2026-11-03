//! Per-path telemetry with one borrowed callback producer and one control reader.
//! Floating-point data crosses a fixed SPSC ring; only its indices are atomic.
//! No allocation, lock, retry loop, clock probe, formatting or destructor belongs
//! to `Producer::record`. Live readings are coherent but may lag or be dropped.
use rc2fx_dsp::Meter;
use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    num::NonZeroU64,
    sync::atomic::{AtomicUsize, Ordering},
};

#[cfg(not(target_has_atomic = "ptr"))]
compile_error!("Callback metrics require lock-free pointer-width atomics");

pub const WINDOW_FRAMES: u64 = 2400;
pub const CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Timing {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub overruns: u64,
    pub clock_errors: u64,
    pub histogram: [u64; 8],
    pub saturated: bool,
}
impl Timing {
    fn add(&mut self, elapsed: Option<NonZeroU64>, budget: u64) {
        let Some(elapsed) = elapsed.map(NonZeroU64::get) else {
            add(&mut self.clock_errors, 1, &mut self.saturated);
            return;
        };
        // Exactly the C fractions, with overflow-safe 3/4 and 2x arithmetic.
        let bounds = [
            budget / 16,
            budget / 8,
            budget / 4,
            budget / 2,
            (budget / 4) * 3 + (budget % 4) * 3 / 4,
            budget,
            budget.saturating_mul(2),
        ];
        let bin = bounds
            .iter()
            .position(|&bound| elapsed <= bound)
            .unwrap_or(7);
        add(&mut self.histogram[bin], 1, &mut self.saturated);
        add(&mut self.total_ns, elapsed, &mut self.saturated);
        add(&mut self.calls, 1, &mut self.saturated);
        self.max_ns = self.max_ns.max(elapsed);
        if elapsed > budget {
            add(&mut self.overruns, 1, &mut self.saturated);
        }
    }
    /// Control-side wire conversion. CPU timing remains a separate diagnostic;
    /// the current core Performance.callback field represents wall time only.
    pub fn to_core(self) -> rc2fx_core::Timing {
        rc2fx_core::Timing {
            calls: self.calls,
            total_ns: self.total_ns,
            max_ns: self.max_ns,
            overruns: self.overruns,
            clock_errors: self.clock_errors,
            histogram: self.histogram,
        }
    }
}
fn add(value: &mut u64, increment: u64, saturated: &mut bool) {
    if let Some(sum) = value.checked_add(increment) {
        *value = sum;
    } else {
        *value = u64::MAX;
        *saturated = true;
    }
}

/// Observed callback entry-to-entry intervals, not execution duration or a
/// causal measure of scheduler lateness. Counts above the rounded expected
/// period include ordinary jitter and never change processing/fault policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cadence {
    pub expected_period_ns: u64,
    pub attempts: u64,
    pub intervals: u64,
    pub baselines: u64,
    pub unmeasured: u64,
    pub unavailable: u64,
    pub clock_errors: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub above_period: u64,
    pub above_two_periods: u64,
    /// <= P/2, <= P, <= 2P, <= 4P, > 4P. P is expected_period_ns.
    pub histogram: [u64; 5],
    pub saturated: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Start {
    Unmeasured,
    Unavailable,
    Invalid,
    At(u64),
}
impl Cadence {
    fn record(&mut self, start: Start, previous: &mut Option<u64>) {
        add(&mut self.attempts, 1, &mut self.saturated);
        let current = match start {
            Start::At(value) => value,
            other => {
                let counter = match other {
                    Start::Unmeasured => &mut self.unmeasured,
                    Start::Unavailable => &mut self.unavailable,
                    Start::Invalid => &mut self.clock_errors,
                    Start::At(_) => unreachable!(),
                };
                add(counter, 1, &mut self.saturated);
                *previous = None;
                return;
            }
        };
        let Some(before) = previous.take() else {
            add(&mut self.baselines, 1, &mut self.saturated);
            *previous = Some(current);
            return;
        };
        let Some(gap) = current.checked_sub(before).filter(|gap| *gap > 0) else {
            // Neither a zero/backwards delta nor its following interval may
            // become a valid gap. The next valid start establishes a baseline.
            add(&mut self.clock_errors, 1, &mut self.saturated);
            return;
        };
        *previous = Some(current);
        let period = self.expected_period_ns;
        let bounds = [
            period / 2,
            period,
            period.saturating_mul(2),
            period.saturating_mul(4),
        ];
        let bin = bounds.iter().position(|bound| gap <= *bound).unwrap_or(4);
        add(&mut self.histogram[bin], 1, &mut self.saturated);
        add(&mut self.intervals, 1, &mut self.saturated);
        add(&mut self.total_ns, gap, &mut self.saturated);
        self.max_ns = self.max_ns.max(gap);
        if gap > period {
            add(&mut self.above_period, 1, &mut self.saturated);
        }
        if gap > period.saturating_mul(2) {
            add(&mut self.above_two_periods, 1, &mut self.saturated);
        }
    }
}

/// None means invalid/unresolved measurement, never a zero-cost observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elapsed {
    wall_ns: Option<NonZeroU64>,
    thread_cpu_ns: Option<NonZeroU64>,
    start: Start,
}
impl Elapsed {
    pub fn from_durations(wall_ns: Option<u64>, thread_cpu_ns: Option<u64>) -> Self {
        let wall_ns = wall_ns.and_then(NonZeroU64::new);
        let thread_cpu_ns = thread_cpu_ns
            .and_then(NonZeroU64::new)
            .filter(|cpu| wall_ns.is_none_or(|wall| cpu <= &wall));
        Self {
            wall_ns,
            thread_cpu_ns,
            // Synthetic elapsed durations do not invent a callback start.
            start: Start::Unmeasured,
        }
    }
    pub fn wall_ns(self) -> Option<u64> {
        self.wall_ns.map(NonZeroU64::get)
    }
    pub fn thread_cpu_ns(self) -> Option<u64> {
        self.thread_cpu_ns.map(NonZeroU64::get)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub callback_attempts: u64,
    pub callbacks: u64,
    pub processed_frames: u64,
    pub failed_callbacks: u64,
    pub invalid_meters: u64,
    pub unmeasured_callbacks: u64,
    pub dropped_windows: u64,
    pub wall: Timing,
    pub thread_cpu: Option<Timing>,
    pub cadence: Cadence,
    pub saturated: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub revision: u64,
    pub frames: u64,
    pub meter: Meter,
}
impl Window {
    /// Control-side presentation; the callback never calculates RMS.
    pub fn rms(self) -> f64 {
        if self.meter.samples == 0 {
            0.0
        } else {
            (self.meter.sum_squares / self.meter.samples as f64).sqrt()
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub window: Window,
    pub totals: Totals,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FinalSnapshot {
    pub totals: Totals,
    pub partial_window: Option<Window>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordResult {
    Accumulated,
    Published,
    Dropped,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidBudget;
impl std::fmt::Display for InvalidBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("callback budget must be nonzero")
    }
}
impl std::error::Error for InvalidBudget {}

struct Queue {
    readings: [UnsafeCell<Option<Reading>>; CAPACITY],
    written: AtomicUsize,
    read: AtomicUsize,
}
// Exactly one non-cloneable Producer writes free slots after acquiring `read`;
// exactly one non-cloneable Consumer copies occupied slots after acquiring
// `written`. Mutable endpoint methods prevent two simultaneous same-side calls.
unsafe impl Sync for Queue {}
struct Accumulator {
    budget_ns: u64,
    totals: Totals,
    partial: Option<Window>,
    previous_start: Option<u64>,
}
/// Storage stays with the control owner; a live producer prevents an unsafely
/// concurrent final snapshot through the normal Rust borrowing rules.
///
/// ```compile_fail
/// use rc2fx_device::metrics::Slot;
/// let mut slot = Slot::new(1_000, false).unwrap();
/// let (mut producer, _) = slot.split();
/// let final_snapshot = slot.final_snapshot();
/// producer.record(None, 1, 112, None);
/// ```
pub struct Slot {
    queue: Queue,
    accumulator: Accumulator,
}
impl Slot {
    pub fn new(callback_budget_ns: u64, measure_thread_cpu: bool) -> Result<Self, InvalidBudget> {
        if callback_budget_ns == 0 {
            return Err(InvalidBudget);
        }
        Ok(Self {
            queue: Queue {
                readings: std::array::from_fn(|_| UnsafeCell::new(None)),
                written: AtomicUsize::new(0),
                read: AtomicUsize::new(0),
            },
            accumulator: Accumulator {
                budget_ns: callback_budget_ns,
                totals: Totals {
                    thread_cpu: measure_thread_cpu.then(Timing::default),
                    cadence: Cadence {
                        expected_period_ns: callback_budget_ns,
                        ..Cadence::default()
                    },
                    ..Totals::default()
                },
                partial: None,
                previous_start: None,
            },
        })
    }
    pub fn split(&mut self) -> (Producer<'_>, Consumer<'_>) {
        let budget_ns = self.accumulator.budget_ns;
        // Returning and reopening a producer is not continuous registration.
        // Preserve cumulative counts but do not measure its inactive gap.
        self.accumulator.previous_start = None;
        (
            Producer {
                queue: &self.queue,
                accumulator: &mut self.accumulator,
            },
            Consumer {
                queue: &self.queue,
                budget_ns,
            },
        )
    }
    /// All endpoint borrows must have ended. Thus this is a quiescent exact
    /// cumulative copy, including a final partial window or a full dropped ring.
    pub fn final_snapshot(&self) -> FinalSnapshot {
        FinalSnapshot {
            totals: self.accumulator.totals,
            partial_window: self.accumulator.partial,
        }
    }
}
pub struct Producer<'a> {
    queue: &'a Queue,
    accumulator: &'a mut Accumulator,
}
pub struct Consumer<'a> {
    queue: &'a Queue,
    budget_ns: u64,
}
impl Producer<'_> {
    /// Record the DSP result and the actual revision used by that callback.
    /// None meter is a failed callback: timing is still recorded, audio progress
    /// is not. None elapsed explicitly counts an uninstrumented callback;
    /// Some invalid duration counts clockErrors in each enabled timing stream.
    pub fn record(
        &mut self,
        meter: Option<Meter>,
        revision: u64,
        frames: u32,
        elapsed: Option<Elapsed>,
    ) -> RecordResult {
        let state = &mut self.accumulator;
        let totals = &mut state.totals;
        add(&mut totals.callback_attempts, 1, &mut totals.saturated);
        totals.cadence.record(
            elapsed.map_or(Start::Unmeasured, |value| value.start),
            &mut state.previous_start,
        );
        if let Some(elapsed) = elapsed {
            totals.wall.add(elapsed.wall_ns, state.budget_ns);
            if let Some(cpu) = &mut totals.thread_cpu {
                cpu.add(elapsed.thread_cpu_ns, state.budget_ns);
            }
        } else {
            add(&mut totals.unmeasured_callbacks, 1, &mut totals.saturated);
        }
        let Some(meter) = meter else {
            add(&mut totals.failed_callbacks, 1, &mut totals.saturated);
            return RecordResult::Rejected;
        };
        if revision == 0 || frames == 0 || !valid_meter(meter) {
            add(&mut totals.invalid_meters, 1, &mut totals.saturated);
            return RecordResult::Rejected;
        }
        if state
            .partial
            .is_none_or(|partial| partial.revision != revision)
        {
            state.partial = Some(Window {
                revision,
                frames: 0,
                meter: Meter::default(),
            });
        }
        let window = state
            .partial
            .as_mut()
            .expect("initialized immediately above");
        let squares = window.meter.sum_squares + meter.sum_squares;
        if !squares.is_finite() {
            add(&mut totals.invalid_meters, 1, &mut totals.saturated);
            state.partial = None;
            return RecordResult::Rejected;
        }
        window.meter.input_peak = window.meter.input_peak.max(meter.input_peak);
        window.meter.output_peak = window.meter.output_peak.max(meter.output_peak);
        window.meter.gain_reduction = window.meter.gain_reduction.max(meter.gain_reduction);
        window.meter.voice = window.meter.voice.max(meter.voice);
        window.meter.gate_open = window.meter.gate_open.max(meter.gate_open);
        window.meter.sum_squares = squares;
        add(
            &mut window.meter.samples,
            meter.samples,
            &mut totals.saturated,
        );
        add(
            &mut window.meter.clipped,
            meter.clipped,
            &mut totals.saturated,
        );
        add(&mut window.frames, u64::from(frames), &mut totals.saturated);
        add(&mut totals.callbacks, 1, &mut totals.saturated);
        add(
            &mut totals.processed_frames,
            u64::from(frames),
            &mut totals.saturated,
        );
        if window.frames < WINDOW_FRAMES {
            return RecordResult::Accumulated;
        }
        let written = self.queue.written.load(Ordering::Relaxed);
        let read = self.queue.read.load(Ordering::Acquire);
        let result = if written.wrapping_sub(read) < CAPACITY {
            let reading = Reading {
                window: *window,
                totals: *totals,
            };
            // Free slot is exclusively producer-owned until the release store.
            unsafe {
                *self.queue.readings[written % CAPACITY].get() = Some(reading);
            }
            self.queue
                .written
                .store(written.wrapping_add(1), Ordering::Release);
            RecordResult::Published
        } else {
            add(&mut totals.dropped_windows, 1, &mut totals.saturated);
            RecordResult::Dropped
        };
        state.partial = None;
        result
    }
}
impl Consumer<'_> {
    pub fn budget_ns(&self) -> u64 {
        self.budget_ns
    }
    pub fn pop(&mut self) -> Option<Reading> {
        let read = self.queue.read.load(Ordering::Relaxed);
        if read == self.queue.written.load(Ordering::Acquire) {
            return None;
        }
        // Occupied slot remains consumer-owned until its release store.
        let reading = unsafe { *self.queue.readings[read % CAPACITY].get() };
        self.queue
            .read
            .store(read.wrapping_add(1), Ordering::Release);
        reading
    }
    /// At most CAPACITY copies, even if a producer continues concurrently.
    /// Never reuse a previous revision's amplitudes as fresh current telemetry.
    pub fn drain_latest(&mut self, revision: u64) -> Option<Reading> {
        let mut latest = None;
        for _ in 0..CAPACITY {
            let Some(reading) = self.pop() else {
                break;
            };
            if reading.window.revision == revision {
                latest = Some(reading);
            }
        }
        latest
    }
}
fn valid_meter(meter: Meter) -> bool {
    meter.samples != 0
        && meter.clipped <= meter.samples
        && [
            meter.input_peak,
            meter.output_peak,
            meter.gain_reduction,
            meter.sum_squares,
        ]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
        && [meter.voice, meter.gate_open]
            .iter()
            .all(|value| (0.0..=1.0).contains(value))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockInfo {
    pub wall_resolution_ns: Option<u64>,
    pub thread_cpu_resolution_ns: Option<u64>,
}
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    info: ClockInfo,
}
/// Deliberately !Send/!Sync: a thread-CPU span must finish on its starting thread.
///
/// ```compile_fail
/// use rc2fx_device::metrics::Clock;
/// let clock = Clock::probe(true);
/// let started = clock.start();
/// std::thread::spawn(move || clock.finish(started));
/// ```
pub struct Stamp {
    wall: Option<u64>,
    cpu: Option<u64>,
    _same_thread: PhantomData<*mut ()>,
}
impl Clock {
    /// Control-side capability query. Reported resolution is not an overhead or
    /// accuracy guarantee; actual zero/backwards readings remain clock errors.
    pub fn probe(measure_thread_cpu: bool) -> Self {
        Self {
            info: ClockInfo {
                wall_resolution_ns: resolution(false),
                thread_cpu_resolution_ns: measure_thread_cpu.then(|| resolution(true)).flatten(),
            },
        }
    }
    pub fn info(self) -> ClockInfo {
        self.info
    }
    pub fn start(self) -> Stamp {
        Stamp {
            wall: self.info.wall_resolution_ns.and_then(|_| now(false)),
            cpu: self.info.thread_cpu_resolution_ns.and_then(|_| now(true)),
            _same_thread: PhantomData,
        }
    }
    pub fn finish(self, start: Stamp) -> Elapsed {
        // CPU span lies inside the monotonic span, including clock-call cost.
        let cpu = self.info.thread_cpu_resolution_ns.and_then(|_| now(true));
        let wall = self.info.wall_resolution_ns.and_then(|_| now(false));
        let mut elapsed = Elapsed::from_durations(delta(start.wall, wall), delta(start.cpu, cpu));
        elapsed.start = if self.info.wall_resolution_ns.is_none() {
            Start::Unavailable
        } else {
            start.wall.map_or(Start::Invalid, Start::At)
        };
        elapsed
    }
}
fn delta(start: Option<u64>, end: Option<u64>) -> Option<u64> {
    start
        .zip(end)
        .and_then(|(start, end)| end.checked_sub(start))
        .filter(|duration| *duration != 0)
}
#[cfg(target_os = "linux")]
fn time_ns(value: libc::timespec) -> Option<u64> {
    if value.tv_sec < 0 || !(0..1_000_000_000).contains(&value.tv_nsec) {
        return None;
    }
    (value.tv_sec as u64)
        .checked_mul(1_000_000_000)?
        .checked_add(value.tv_nsec as u64)
}
#[cfg(target_os = "linux")]
fn clock_id(cpu: bool) -> libc::clockid_t {
    if cpu {
        libc::CLOCK_THREAD_CPUTIME_ID
    } else {
        libc::CLOCK_MONOTONIC
    }
}
#[cfg(target_os = "linux")]
fn now(cpu: bool) -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(clock_id(cpu), &mut value) } != 0 {
        return None;
    }
    time_ns(value)
}
#[cfg(target_os = "linux")]
fn resolution(cpu: bool) -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_getres(clock_id(cpu), &mut value) } != 0 {
        return None;
    }
    time_ns(value).filter(|value| *value != 0)
}
#[cfg(not(target_os = "linux"))]
fn now(_: bool) -> Option<u64> {
    None
}
#[cfg(not(target_os = "linux"))]
fn resolution(_: bool) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests;
