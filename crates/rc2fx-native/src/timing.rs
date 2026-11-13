//! Callback timing. Wall clocks cover every callback. Thread CPU clocks are read
//! only while the bench gauges are on, once in `CPU_SAMPLE_EVERY` callbacks:
//! `CLOCK_THREAD_CPUTIME_ID` is a system call on the mixer (the arm64 vDSO does
//! not serve it), four of them per sampled callback. Recording is bounded,
//! allocation/lock/wait free. The original callback includes buffer hooks;
//! owned is pre/post processing only.
use rc2fx_native_control::{CallbackTiming, Timing};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// With the bench gauges on, the callbacks between CPU-clock samples. A prime,
/// so that the samples spread over every phase of the meter stride
/// (pipeline.rs) and of anything else periodic in the callback; a multiple of
/// the stride would put every sample on one phase.
pub const CPU_SAMPLE_EVERY: u32 = 509;
/// What `CallbackTiming::cpu_sample_every` reports: 0 while CPU clocks are not
/// read at all, so that an empty CPU population is not read as a sampled one.
fn reported_sample_every(cpu_clocks: bool) -> u32 {
    if cpu_clocks { CPU_SAMPLE_EVERY } else { 0 }
}
/// Adds to a counter that one thread writes: a plain load and store. An atomic
/// add is an exclusive-pair loop on the Cortex-A53, and buys nothing here.
fn add(counter: &AtomicU64, value: u64) {
    counter.store(counter.load(Relaxed).wrapping_add(value), Relaxed);
}
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
            add(&self.errors, 1);
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
        // One callback writer per context; retired merges are control-serialized.
        add(&self.bins[bin], 1);
        add(&self.total, elapsed);
        if elapsed > self.max.load(Relaxed) {
            self.max.store(elapsed, Relaxed);
        }
        if elapsed > BUDGET_NS {
            add(&self.overruns, 1);
        }
        add(&self.calls, 1);
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
    /// Starts timing one callback. `cpu_clocks`: the bench gauges are on, so
    /// this callback may be one that also reads the thread CPU clock.
    pub fn begin(&self, frames: u32, cpu_clocks: bool) -> Frame {
        // One callback writer per context (see Counters::record).
        let attempt = self.attempts.load(Relaxed);
        self.attempts.store(attempt.wrapping_add(1), Relaxed);
        if frames as usize != super::pipeline::FRAMES {
            self.wrong_frames.fetch_add(1, Relaxed);
        }
        let cpu = cpu_clocks && attempt.is_multiple_of(u64::from(CPU_SAMPLE_EVERY));
        Frame {
            start: Stamp::entry(cpu),
            cpu,
        }
    }
    /// Records the callback, and returns its exit stamp: CLOCK_MONOTONIC in
    /// nanoseconds, or 0 if the clock is unavailable (as `monotonic`).
    pub fn finish(&self, frame: Frame, before_native: Stamp, after_native: Stamp) -> u64 {
        let end = Stamp::exit(frame.cpu);
        self.record(frame, before_native, after_native, end);
        end.wall.unwrap_or(0)
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
            cpu_sample_every: reported_sample_every(super::gauges::enabled()),
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
// Clock reads made by this thread, [CLOCK_MONOTONIC, thread CPU]: tests count
// what a callback costs with the bench gauges off and on.
#[cfg(test)]
thread_local! {
    pub(crate) static CLOCK_READS: std::cell::Cell<[u64; 2]> = const { std::cell::Cell::new([0; 2]) };
}
fn now(cpu: bool) -> Option<u64> {
    #[cfg(test)]
    CLOCK_READS.with(|reads| {
        let mut counts = reads.get();
        counts[usize::from(cpu)] += 1;
        reads.set(counts);
    });
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
    fn clock_reads() -> [u64; 2] {
        CLOCK_READS.with(std::cell::Cell::get)
    }
    #[test]
    fn real_clocks_have_bounded_sample_population() {
        let callbacks = 2 * u64::from(CPU_SAMPLE_EVERY);
        let counters = Shared::new();
        let before = clock_reads();
        for _ in 0..callbacks {
            let frame = counters.begin(112, true);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        let value = counters.snapshot();
        assert_eq!(value.attempts, callbacks);
        assert_eq!(
            value.callback_wall.calls + value.callback_wall.clock_errors,
            callbacks
        );
        assert_eq!(
            value.callback_cpu.calls + value.callback_cpu.clock_errors,
            2
        );
        assert_eq!(value.owned_cpu.calls + value.owned_cpu.clock_errors, 2);
        // Four wall reads per callback, and four CPU reads per sampled one.
        let after = clock_reads();
        assert_eq!(after[0] - before[0], 4 * callbacks);
        assert_eq!(after[1] - before[1], 4 * 2);
    }
    /// Without the bench gauges no callback reads the thread CPU clock, which
    /// is a system call on the mixer; the wire format keeps its fields, empty.
    #[test]
    fn without_the_gauges_no_cpu_clock_is_read_and_the_fields_stay_empty() {
        let callbacks = 3 * u64::from(CPU_SAMPLE_EVERY);
        let counters = Shared::new();
        let before = clock_reads();
        for _ in 0..callbacks {
            let frame = counters.begin(112, false);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        let after = clock_reads();
        assert_eq!(after[0] - before[0], 4 * callbacks);
        assert_eq!(after[1], before[1], "no CPU clock read");
        let value = counters.snapshot();
        assert_eq!(value.callback_wall.calls, callbacks);
        assert_eq!(value.owned_wall.calls, callbacks);
        assert_eq!(value.native_wall.calls, callbacks);
        assert_eq!(value.callback_cpu, Timing::default());
        assert_eq!(value.owned_cpu, Timing::default());
        // 0 says CPU clocks are not sampled, where a cadence with an empty
        // population would say they are.
        assert_eq!(reported_sample_every(false), 0);
        assert_eq!(reported_sample_every(true), CPU_SAMPLE_EVERY);
        // The whole snapshot still serializes with every field of the wire
        // format, and reads back.
        let json = serde_json::to_value(value).unwrap();
        for key in [
            "callbackBudgetNs",
            "cpuSampleEvery",
            "attempts",
            "unexpectedFrameSizes",
            "callbackWall",
            "callbackCpu",
            "ownedWall",
            "ownedCpu",
            "nativeWall",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["callbackCpu"]["calls"], 0);
        let back: CallbackTiming = serde_json::from_value(json).unwrap();
        assert_eq!(back, value);
    }
    /// The cadence shares no factor with the meter stride, so CPU samples do
    /// not all land on one meter phase.
    #[test]
    fn cpu_samples_cover_every_meter_phase() {
        let phases: std::collections::HashSet<u32> =
            (0..8).map(|sample| sample * CPU_SAMPLE_EVERY % 8).collect();
        assert_eq!(phases.len(), 8);
    }
    /// The callback's end time is the exit stamp of its timing: the same
    /// clock read, not a second one.
    #[test]
    fn finish_returns_the_exit_stamp_it_recorded() {
        let counters = Shared::new();
        let frame = counters.begin(112, false);
        let before = frame.before_native();
        let after = frame.after_native();
        let reads = clock_reads();
        let ended = counters.finish(frame, before, after);
        assert_eq!(clock_reads()[0] - reads[0], 1, "one read for both");
        let start = frame.start.wall.unwrap();
        assert!(ended >= start && ended >= after.wall.unwrap());
        assert_eq!(counters.snapshot().callback_wall.total_ns, ended - start);
    }
    /// The counters as they were before: an atomic add for every update.
    struct Reference {
        calls: AtomicU64,
        total: AtomicU64,
        max: AtomicU64,
        overruns: AtomicU64,
        errors: AtomicU64,
        bins: [AtomicU64; 8],
    }
    impl Reference {
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
    }
    /// Plain load-and-store counting gives the same snapshot as atomic adds,
    /// for every bin boundary, clock errors, huge values that wrap the total,
    /// and after a retirement.
    #[test]
    fn single_writer_counters_match_the_atomic_ones() {
        let reference = Reference {
            calls: AtomicU64::new(0),
            total: AtomicU64::new(0),
            max: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            bins: [const { AtomicU64::new(0) }; 8],
        };
        let counters = Counters::new();
        let mut edges = vec![None, Some(0), Some(1), Some(u64::MAX), Some(u64::MAX - 1)];
        for bound in [
            BUDGET_NS / 16,
            BUDGET_NS / 8,
            BUDGET_NS / 4,
            BUDGET_NS / 2,
            BUDGET_NS * 3 / 4,
            BUDGET_NS,
            BUDGET_NS * 2,
        ] {
            edges.extend([Some(bound - 1), Some(bound), Some(bound + 1)]);
        }
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut random = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..20_000 {
            let value = match round % 7 {
                0 => edges[round / 7 % edges.len()],
                1 => None,
                2 => Some(random()),
                _ => Some(random() % (BUDGET_NS * 3)),
            };
            reference.record(value);
            counters.record(value);
            if round % 997 == 0 {
                assert_eq!(counters.snapshot(), reference.snapshot(), "round {round}");
            }
        }
        let expected = reference.snapshot();
        assert_eq!(counters.snapshot(), expected);
        assert!(expected.histogram.iter().all(|bin| *bin > 0));
        assert!(expected.clock_errors > 0 && expected.overruns > 0);
        // Retirement still sums into the retired total and clears the source.
        let retired = Counters::new();
        retired.merge(counters.snapshot());
        retired.merge(counters.snapshot());
        counters.reset();
        assert_eq!(counters.snapshot(), Timing::default());
        assert_eq!(
            retired.snapshot(),
            merge_timing_wrapping(expected, expected)
        );
    }
    /// `Counters::merge` adds with wrap-around, as its atomics do.
    fn merge_timing_wrapping(a: Timing, b: Timing) -> Timing {
        Timing {
            calls: a.calls.wrapping_add(b.calls),
            total_ns: a.total_ns.wrapping_add(b.total_ns),
            max_ns: a.max_ns.max(b.max_ns),
            overruns: a.overruns.wrapping_add(b.overruns),
            clock_errors: a.clock_errors.wrapping_add(b.clock_errors),
            histogram: std::array::from_fn(|i| a.histogram[i].wrapping_add(b.histogram[i])),
        }
    }
    /// Counted attempts are unchanged by the plain load and store.
    #[test]
    fn attempts_count_every_callback_and_choose_the_sampled_ones() {
        let callbacks = 2 * CPU_SAMPLE_EVERY + 3;
        let counters = Shared::new();
        let mut sampled = Vec::new();
        for attempt in 0..callbacks {
            let frame = counters.begin(if attempt == 5 { 64 } else { 112 }, true);
            if frame.cpu {
                sampled.push(attempt);
            }
        }
        assert_eq!(sampled, [0, CPU_SAMPLE_EVERY, 2 * CPU_SAMPLE_EVERY]);
        let value = counters.snapshot();
        assert_eq!(value.attempts, u64::from(callbacks));
        assert_eq!(value.unexpected_frame_sizes, 1);
        let unsampled = Shared::new();
        assert!((0..callbacks).all(|_| !unsampled.begin(112, false).cpu));
        assert_eq!(unsampled.snapshot().attempts, u64::from(callbacks));
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
                jack: crate::jack_cache::Snapshot::INACTIVE,
            })
        });
        for _ in 0..1024 {
            let frame = counters.begin(112, true);
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
            let frame = counters.begin(112, false);
            let before = frame.before_native();
            let after = frame.after_native();
            counters.finish(frame, before, after);
        }
        let mut observed = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            for _ in 0..100_000 {
                let frame = counters.begin(black_box(112), false);
                let before = frame.before_native();
                let after = frame.after_native();
                counters.finish(frame, before, after);
            }
            observed.push(start.elapsed().as_nanos() as f64 / 100_000.0);
        }
        observed.sort_by(f64::total_cmp);
        eprintln!(
            "native timing host-only observation: median batch mean {:.1} ns/callback; min {:.1}, max {:.1}; no CPU clocks (gauges off); five x100000 no-op callbacks",
            observed[2], observed[0], observed[4]
        );
        assert_eq!(counters.snapshot().attempts, 501024);
    }
}
