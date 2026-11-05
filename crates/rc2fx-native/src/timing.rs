//! Optional callback timing. Wall clocks cover every callback; thread CPU clocks
//! are sampled once in 16. Recording is bounded, allocation/lock/wait free. The
//! original callback includes buffer hooks; owned is pre/post processing only.
use rc2fx_native_control::{CallbackTiming, Timing};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub const CPU_SAMPLE_EVERY: u32 = 16;
pub const BUDGET_NS: u64 = super::pipeline::FRAMES as u64 * 1_000_000_000 / 48_000;
struct Counters {
    calls: AtomicU64,
    total: AtomicU64,
    max: AtomicU64,
    overruns: AtomicU64,
    errors: AtomicU64,
    bins: [AtomicU64; 8],
}
impl Counters {
    const fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            total: AtomicU64::new(0),
            max: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            bins: [const { AtomicU64::new(0) }; 8],
        }
    }
    fn record(&self, elapsed: Option<u64>) {
        let Some(elapsed) = elapsed else {
            self.errors.fetch_add(1, Relaxed);
            return;
        };
        let bounds = [
            BUDGET_NS / 16,
            BUDGET_NS / 8,
            BUDGET_NS / 4,
            BUDGET_NS / 2,
            BUDGET_NS * 3 / 4,
            BUDGET_NS,
            BUDGET_NS * 2,
        ];
        let bin = bounds
            .iter()
            .position(|bound| elapsed <= *bound)
            .unwrap_or(7);
        self.bins[bin].fetch_add(1, Relaxed);
        self.total.fetch_add(elapsed, Relaxed);
        // One callback writer per context; retired merges are control-serialized.
        if elapsed > self.max.load(Relaxed) {
            self.max.store(elapsed, Relaxed);
        }
        if elapsed > BUDGET_NS {
            self.overruns.fetch_add(1, Relaxed);
        }
        self.calls.fetch_add(1, Relaxed);
    }
    fn snapshot(&self) -> Timing {
        Timing {
            calls: self.calls.load(Relaxed),
            total_ns: self.total.load(Relaxed),
            max_ns: self.max.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            clock_errors: self.errors.load(Relaxed),
            histogram: std::array::from_fn(|i| self.bins[i].load(Relaxed)),
        }
    }
    fn merge(&self, value: Timing) {
        self.calls.fetch_add(value.calls, Relaxed);
        self.total.fetch_add(value.total_ns, Relaxed);
        if value.max_ns > self.max.load(Relaxed) {
            self.max.store(value.max_ns, Relaxed);
        }
        self.overruns.fetch_add(value.overruns, Relaxed);
        self.errors.fetch_add(value.clock_errors, Relaxed);
        for (counter, value) in self.bins.iter().zip(value.histogram) {
            counter.fetch_add(value, Relaxed);
        }
    }
    fn reset(&self) {
        for counter in [
            &self.calls,
            &self.total,
            &self.max,
            &self.overruns,
            &self.errors,
        ] {
            counter.store(0, Relaxed);
        }
        for counter in &self.bins {
            counter.store(0, Relaxed);
        }
    }
}
/// Two Timings added: calls, totals, overruns and clock errors sum, maxima
/// take the larger, histograms add bin by bin.
pub(crate) fn merge_timing(
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

pub struct Shared {
    attempts: AtomicU64,
    wrong_frames: AtomicU64,
    callback_wall: Counters,
    callback_cpu: Counters,
    owned_wall: Counters,
    owned_cpu: Counters,
    native_wall: Counters,
}
impl Shared {
    pub const fn new() -> Self {
        Self {
            attempts: AtomicU64::new(0),
            wrong_frames: AtomicU64::new(0),
            callback_wall: Counters::new(),
            callback_cpu: Counters::new(),
            owned_wall: Counters::new(),
            owned_cpu: Counters::new(),
            native_wall: Counters::new(),
        }
    }
    pub fn begin(&self, frames: u32) -> Frame {
        let attempt = self.attempts.fetch_add(1, Relaxed);
        if frames as usize != super::pipeline::FRAMES {
            self.wrong_frames.fetch_add(1, Relaxed);
        }
        let cpu = attempt.is_multiple_of(u64::from(CPU_SAMPLE_EVERY));
        Frame {
            start: Stamp::entry(cpu),
            cpu,
        }
    }
    pub fn finish(&self, frame: Frame, before_native: Stamp, after_native: Stamp) {
        self.record(frame, before_native, after_native, Stamp::exit(frame.cpu));
    }
    fn record(&self, frame: Frame, before: Stamp, after: Stamp, end: Stamp) {
        self.callback_wall.record(delta(frame.start.wall, end.wall));
        self.native_wall.record(delta(before.wall, after.wall));
        self.owned_wall.record(sum(
            delta(frame.start.wall, before.wall),
            delta(after.wall, end.wall),
        ));
        if frame.cpu {
            self.callback_cpu.record(delta(frame.start.cpu, end.cpu));
            self.owned_cpu.record(sum(
                delta(frame.start.cpu, before.cpu),
                delta(after.cpu, end.cpu),
            ));
        }
    }
    pub fn snapshot(&self) -> CallbackTiming {
        CallbackTiming {
            callback_budget_ns: BUDGET_NS,
            cpu_sample_every: CPU_SAMPLE_EVERY,
            attempts: self.attempts.load(Relaxed),
            unexpected_frame_sizes: self.wrong_frames.load(Relaxed),
            callback_wall: self.callback_wall.snapshot(),
            callback_cpu: self.callback_cpu.snapshot(),
            owned_wall: self.owned_wall.snapshot(),
            owned_cpu: self.owned_cpu.snapshot(),
            native_wall: self.native_wall.snapshot(),
        }
    }
    /// Adds another snapshot into this one: sums, maxima and summed bins.
    /// Used to keep a process-wide total across context rebuilds.
    pub fn merged_with(&self, other: CallbackTiming) -> CallbackTiming {
        let mut merged = self.snapshot();
        merged.attempts = merged.attempts.saturating_add(other.attempts);
        merged.unexpected_frame_sizes = merged
            .unexpected_frame_sizes
            .saturating_add(other.unexpected_frame_sizes);
        merged.callback_wall = merge_timing(merged.callback_wall, other.callback_wall);
        merged.callback_cpu = merge_timing(merged.callback_cpu, other.callback_cpu);
        merged.owned_wall = merge_timing(merged.owned_wall, other.owned_wall);
        merged.owned_cpu = merge_timing(merged.owned_cpu, other.owned_cpu);
        merged.native_wall = merge_timing(merged.native_wall, other.native_wall);
        merged
    }
    /// Only after native close has quiesced this context, under CONTROL.
    pub fn retire_into(&self, retired: &Shared) {
        let value = self.snapshot();
        retired.attempts.fetch_add(value.attempts, Relaxed);
        retired
            .wrong_frames
            .fetch_add(value.unexpected_frame_sizes, Relaxed);
        retired.callback_wall.merge(value.callback_wall);
        retired.callback_cpu.merge(value.callback_cpu);
        retired.owned_wall.merge(value.owned_wall);
        retired.owned_cpu.merge(value.owned_cpu);
        retired.native_wall.merge(value.native_wall);
        self.attempts.store(0, Relaxed);
        self.wrong_frames.store(0, Relaxed);
        self.callback_wall.reset();
        self.callback_cpu.reset();
        self.owned_wall.reset();
        self.owned_cpu.reset();
        self.native_wall.reset();
    }
}
#[derive(Clone, Copy)]
pub struct Frame {
    start: Stamp,
    cpu: bool,
}
impl Frame {
    pub fn before_native(&self) -> Stamp {
        Stamp::exit(self.cpu)
    }
    pub fn after_native(&self) -> Stamp {
        Stamp::entry(self.cpu)
    }
}
#[derive(Clone, Copy)]
pub struct Stamp {
    wall: Option<u64>,
    cpu: Option<u64>,
}
impl Stamp {
    pub(crate) fn wall_ns(self) -> Option<u64> {
        self.wall
    }
    fn entry(cpu: bool) -> Self {
        Self {
            wall: now(false),
            cpu: cpu.then(|| now(true)).flatten(),
        }
    }
    fn exit(cpu: bool) -> Self {
        let cpu = cpu.then(|| now(true)).flatten();
        Self {
            cpu,
            wall: now(false),
        }
    }
}
fn delta(start: Option<u64>, end: Option<u64>) -> Option<u64> {
    start.zip(end).and_then(|(s, e)| e.checked_sub(s))
}
fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.zip(b).and_then(|(a, b)| a.checked_add(b))
}
/// CLOCK_MONOTONIC in nanoseconds, or 0 if the clock is unavailable.
pub(crate) fn monotonic() -> u64 {
    now(false).unwrap_or(0)
}
fn now(cpu: bool) -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Supported Linux clocks, fixed writable stack storage, no allocation.
    let rc = unsafe {
        libc::clock_gettime(
            if cpu {
                libc::CLOCK_THREAD_CPUTIME_ID
            } else {
                libc::CLOCK_MONOTONIC
            },
            &mut time,
        )
    };
    if rc != 0 || time.tv_sec < 0 || !(0..1_000_000_000).contains(&time.tv_nsec) {
        return None;
    }
    (time.tv_sec as u64)
        .checked_mul(1_000_000_000)?
        .checked_add(time.tv_nsec as u64)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stamp(wall: u64, cpu: u64) -> Stamp {
        Stamp {
            wall: Some(wall),
            cpu: Some(cpu),
        }
    }
    #[test]
    fn separates_native_and_owned_work_and_does_not_invent_unsampled_cpu() {
        let counters = Shared::new();
        counters.record(
            Frame {
                start: stamp(10, 5),
                cpu: true,
            },
            stamp(30, 15),
            stamp(90, 35),
            stamp(120, 55),
        );
        let value = counters.snapshot();
        assert_eq!(value.callback_wall.total_ns, 110);
        assert_eq!(value.native_wall.total_ns, 60);
        assert_eq!(value.owned_wall.total_ns, 50);
        assert_eq!(value.callback_cpu.total_ns, 50);
        assert_eq!(value.owned_cpu.total_ns, 30);
        counters.record(
            Frame {
                start: stamp(10, 5),
                cpu: false,
            },
            stamp(30, 15),
            stamp(90, 35),
            stamp(120, 55),
        );
        assert_eq!(counters.snapshot().callback_cpu.calls, 1);
        assert_eq!(counters.snapshot().callback_cpu.clock_errors, 0);
    }
    #[test]
    fn invalid_clocks_are_visible_and_retirement_preserves_counters() {
        let counters = Shared::new();
        let retired = Shared::new();
        let bad = Stamp {
            wall: None,
            cpu: None,
        };
        counters.record(
            Frame {
                start: bad,
                cpu: true,
            },
            bad,
            bad,
            bad,
        );
        assert_eq!(counters.snapshot().callback_wall.clock_errors, 1);
        assert_eq!(counters.snapshot().callback_wall.calls, 0);
        counters.retire_into(&retired);
        assert_eq!(counters.snapshot().callback_wall.clock_errors, 0);
        assert_eq!(retired.snapshot().callback_wall.clock_errors, 1);
    }
    #[test]
    fn real_clocks_have_bounded_sample_population() {
        let counters = Shared::new();
        for _ in 0..32 {
            let frame = counters.begin(112);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        let value = counters.snapshot();
        assert_eq!(value.attempts, 32);
        assert_eq!(
            value.callback_wall.calls + value.callback_wall.clock_errors,
            32
        );
        assert_eq!(
            value.callback_cpu.calls + value.callback_cpu.clock_errors,
            2
        );
    }
    #[test]
    fn recording_has_no_owned_callback_allocations_or_reclamation() {
        use super::super::{
            CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
        };
        let counters = Shared::new();
        let before = [
            CALLBACK_ALLOCATIONS.load(Relaxed),
            CALLBACK_FREES.load(Relaxed),
            CALLBACK_REALLOCATIONS.load(Relaxed),
        ];
        let previous = INSIDE.with(|value| {
            value.replace(CallbackFrame {
                owner: 1,
                processing: true,
                pipeline: 0,
                started: 0,
            })
        });
        for _ in 0..1024 {
            let frame = counters.begin(112);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        INSIDE.with(|value| value.set(previous));
        assert_eq!(
            [
                CALLBACK_ALLOCATIONS.load(Relaxed),
                CALLBACK_FREES.load(Relaxed),
                CALLBACK_REALLOCATIONS.load(Relaxed)
            ],
            before
        );
    }
    #[test]
    #[ignore = "explicit local clock/recording overhead observation, not device qualification"]
    fn timing_overhead_receipt() {
        use std::{hint::black_box, time::Instant};
        let counters = Shared::new();
        for _ in 0..1024 {
            let frame = counters.begin(112);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        let mut observed = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            for _ in 0..100_000 {
                let frame = counters.begin(black_box(112));
                let before = frame.before_native();
                let after = frame.after_native();
                counters.finish(frame, before, after);
            }
            observed.push(start.elapsed().as_nanos() as f64 / 100_000.0);
        }
        observed.sort_by(f64::total_cmp);
        eprintln!(
            "native timing host-only observation: median batch mean {:.1} ns/callback; min {:.1}, max {:.1}; CPU sample1/16; five x100000 no-op callbacks",
            observed[2], observed[0], observed[4]
        );
        assert_eq!(counters.snapshot().attempts, 501024);
    }
}
