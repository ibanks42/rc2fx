//! Diagnostics baselines and the event log, held by rc2fxd.
//!
//! Two jobs, both off the audio path (they run where the control side already
//! reads the engine: nine times a second while a client is subscribed, and at
//! the idle interval otherwise):
//!
//! * **Baselines.** Every cumulative figure the app shows is reported relative
//!   to the last diagnostics reset, never by writing to a counter the callback,
//!   helper or worker owns: this module keeps the reading taken at the reset
//!   and subtracts it. With no reset yet, the baseline is the engine's first
//!   reading. A lifetime maximum cannot be decomposed, so after a reset a
//!   maximum is reported only while it exceeds the pre-reset one; 0 then means
//!   "no sample since the reset has beaten the old maximum", not "instant".
//!   Until a reset the lifetime maximum is shown as it is, because nothing was
//!   cleared and the worst sample often comes before the first reading. Each
//!   reset bumps `Meter::diagnostics_epoch`, so the app rebaselines its own
//!   fault tracking on the epoch instead of seeing a decrease.
//!
//! * **Events.** A bounded ring (2048) with a monotonic sequence number, built
//!   from the engine's own event ring, each worker's miss and late histories,
//!   and the counter increases this module can only see as totals. A gap in the
//!   engine's sequence numbers becomes one `lost` entry, and events past a
//!   worker's kept history become one counted entry per reading, so the list
//!   never silently misses anything; a client that fell behind the ring is
//!   told how many entries it missed.

use rc2fx_core::{
    DiagnosticsEvent, DiagnosticsEvents, DiagnosticsKind, EVENTS_PAGE, EventDetails, Meter, Timing,
};
use rc2fx_native_control::{ContextStatus, NativeEvent, NativeEventKind, TargetStatus};
use std::collections::{BTreeMap, VecDeque};

/// Entries kept; older ones are counted as `lost` for a client that asks.
pub const LOG_CAPACITY: usize = 2048;

/// The mixer's CLOCK_MONOTONIC, the clock the engine's own event times use.
/// Other Unix hosts (macOS, for the tests) read their own monotonic clock.
pub fn monotonic_ns() -> u64 {
    #[cfg(unix)]
    {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } == 0 {
            (value.tv_sec as u64) * 1_000_000_000 + value.tv_nsec as u64
        } else {
            0
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

pub struct DiagnosticsLog {
    epoch: u64,
    generation: Option<u64>,
    /// The last raw reading; a reset also stores it as the new baseline. A
    /// reading without a meter keeps the path figures of the one before.
    last: Option<Reading>,
    baselines: Option<Reading>,
    /// The baseline came from a user's reset, not an engine's first reading. A
    /// new engine clears this while the epoch stays: it has had no reset.
    baseline_is_reset: bool,
    events: VecDeque<DiagnosticsEvent>,
    next_seq: u64,
    /// The engine ring's last sequence number seen.
    engine_seq: u64,
    /// Each path's read position in its worker's miss and late histories,
    /// keyed on the runtime epoch: a new epoch is a new worker whose
    /// histories start over.
    worker_seqs: BTreeMap<String, HistoryCursor>,
    last_reading_ns: Option<u64>,
}

/// How far one path's worker histories have been read.
#[derive(Clone, Copy, Default)]
struct HistoryCursor {
    runtime_epoch: u64,
    misses: HistoryRead,
    lates: HistoryRead,
}

/// One history's read position: records are read by their ordinal in the
/// worker's ring (never by frame number, since a miss on frame 0 is real),
/// and events past the kept ones are reported as counts.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct HistoryRead {
    /// Records listed, the next ordinal to read.
    listed: u64,
    /// Events beyond the kept records already reported as a count.
    counted: u64,
}

/// One raw reading of everything cumulative.
#[derive(Clone, Default)]
struct Reading {
    targets: BTreeMap<String, TargetReading>,
    callback: Timing,
    xruns: u64,
    helper_late: u64,
    helper_stalls: u64,
}

#[derive(Clone, Default)]
struct TargetReading {
    callbacks: u64,
    processed_frames: u64,
    suppression_frames: u64,
    clipped: u64,
    missed_deadlines: u64,
    stale_results: u64,
    input_drops: u64,
    output_drops: u64,
    worker_wall: Timing,
    worker_cpu: Timing,
}

impl Reading {
    fn of(meter: Option<&Meter>, native: &[ContextStatus]) -> Self {
        let mut reading = Reading::default();
        if let Some(context) = native.first() {
            reading.xruns = context.counters.xruns;
            reading.helper_late = context.counters.helper_late;
            reading.helper_stalls = context.counters.helper_stalls;
            if let Some(timing) = context.timing {
                reading.callback = timing_of(timing.callback_wall);
            }
        }
        for target in meter.map_or(&[][..], |meter| meter.targets.as_slice()) {
            let worker = target.performance.as_ref();
            reading.targets.insert(
                target.id.clone(),
                TargetReading {
                    callbacks: target.callbacks,
                    processed_frames: target.processed_frames,
                    suppression_frames: target.suppression_frames,
                    clipped: target.clipped,
                    missed_deadlines: target.missed_deadlines,
                    stale_results: target.stale_results,
                    input_drops: target.input_drops,
                    output_drops: target.output_drops,
                    worker_wall: worker
                        .map_or_else(Timing::default, |value| value.worker_wall.clone()),
                    worker_cpu: worker
                        .map_or_else(Timing::default, |value| value.worker_cpu.clone()),
                },
            );
        }
        reading
    }
}

fn timing_of(value: rc2fx_native_control::Timing) -> Timing {
    Timing {
        calls: value.calls,
        total_ns: value.total_ns,
        max_ns: value.max_ns,
        overruns: value.overruns,
        clock_errors: value.clock_errors,
        histogram: value.histogram,
    }
}

/// `now` minus `before`, saturating: a decrease is a new baseline, not a wrap.
fn since(now: u64, before: u64) -> u64 {
    now.saturating_sub(before)
}

/// A cumulative timing since its baseline. After a reset the maximum is
/// reported only while it is larger than the baseline's, because a lifetime
/// maximum cannot be decomposed into "since the reset".
fn timing_since(now: &Timing, before: &Timing, after_reset: bool) -> Timing {
    let mut histogram = [0; 8];
    for (index, slot) in histogram.iter_mut().enumerate() {
        *slot = since(now.histogram[index], before.histogram[index]);
    }
    Timing {
        calls: since(now.calls, before.calls),
        total_ns: since(now.total_ns, before.total_ns),
        max_ns: if !after_reset || now.max_ns > before.max_ns {
            now.max_ns
        } else {
            0
        },
        overruns: since(now.overruns, before.overruns),
        clock_errors: since(now.clock_errors, before.clock_errors),
        histogram,
    }
}

impl Default for DiagnosticsLog {
    fn default() -> Self {
        Self {
            epoch: 0,
            generation: None,
            last: None,
            baselines: None,
            baseline_is_reset: false,
            events: VecDeque::new(),
            // Sequence numbers are 1-based: `after = 0` asks for everything.
            next_seq: 1,
            engine_seq: 0,
            worker_seqs: BTreeMap::new(),
            last_reading_ns: None,
        }
    }
}

impl DiagnosticsLog {
    /// Takes one reading: appends events and remembers the raw figures. It does
    /// not change `meter`; `shown` gives the figures relative to the baseline,
    /// so the baseline is applied exactly once.
    pub fn observe(&mut self, meter: Option<&Meter>, native: &[ContextStatus], now_ns: u64) {
        // A new engine generation brings new counters: it is a new baseline.
        let generation = meter.map(|meter| meter.generation);
        if generation.is_some() && self.generation != generation {
            self.generation = generation;
            self.baselines = None;
            self.baseline_is_reset = false;
            self.worker_seqs.clear();
        }
        let mut reading = Reading::of(meter, native);
        // With no meter (the engine is settling) only the engine-wide figures
        // are new. The paths keep their last figures, so growth across the gap
        // still becomes events and a reset still has a baseline for each path.
        if meter.is_none()
            && let Some(last) = &self.last
        {
            reading.targets = last.targets.clone();
        }
        self.collect_events(&reading, native, now_ns);
        self.baselines.get_or_insert_with(|| reading.clone());
        self.last = Some(reading);
        self.last_reading_ns = Some(now_ns);
    }

    /// The user's reset: what the window shows restarts from this reading on.
    /// The log keeps its sequence numbers and records where the reset happened.
    pub fn reset(&mut self, at_ns: u64) {
        self.epoch = self.epoch.wrapping_add(1);
        if let Some(last) = &self.last {
            self.baselines = Some(last.clone());
            self.baseline_is_reset = true;
        }
        // The window starts over: the ring is cleared, and the reset entry
        // says where (sequence numbers keep increasing, so a client that was
        // reading continues from the same cursor).
        self.events.clear();
        self.push(DiagnosticsEvent {
            seq: 0,
            kind: DiagnosticsKind::Reset,
            path: None,
            at_ns,
            message: "Diagnostics reset".into(),
            details: EventDetails::default(),
        });
    }

    /// `meter` as the window should show it: cumulative figures relative to the
    /// current baseline, leaving `meter` itself untouched (the diagnostics
    /// export keeps the raw figures).
    pub fn shown(&self, meter: &Meter) -> Meter {
        let mut shown = meter.clone();
        shown.diagnostics_epoch = self.epoch;
        if let (Some(baseline), Some(reading)) = (&self.baselines, &self.last) {
            self.rewrite(&mut shown, reading, baseline);
        }
        shown
    }

    /// One page of the log, oldest first, at most `EVENTS_PAGE` entries.
    pub fn page(&mut self, after: u64, limit: usize, now_ns: u64) -> DiagnosticsEvents {
        let limit = limit.clamp(1, EVENTS_PAGE);
        let first = self
            .events
            .front()
            .map(|event| event.seq)
            .unwrap_or(self.next_seq);
        // Entries that fell out of the ring before this client's cursor.
        let cursor = after.saturating_add(1);
        let lost = first.saturating_sub(cursor);
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|event| event.seq > after)
            .take(limit)
            .cloned()
            .collect();
        let more = events
            .last()
            .is_some_and(|last| last.seq + 1 < self.next_seq.max(after + 1));
        DiagnosticsEvents {
            now_ns,
            next_seq: events
                .last()
                .map_or(self.next_seq.max(after), |last| last.seq + 1),
            lost,
            more,
            events,
        }
    }

    fn push(&mut self, mut event: DiagnosticsEvent) {
        event.seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.events.push_back(event);
        while self.events.len() > LOG_CAPACITY {
            self.events.pop_front();
        }
    }

    /// Rewrites `meter`'s cumulative figures relative to `baseline`.
    fn rewrite(&self, meter: &mut Meter, reading: &Reading, baseline: &Reading) {
        for target in &mut meter.targets {
            let (Some(current), Some(base)) = (
                reading.targets.get(&target.id),
                baseline.targets.get(&target.id),
            ) else {
                continue;
            };
            target.callbacks = since(target.callbacks, base.callbacks);
            target.processed_frames = since(target.processed_frames, base.processed_frames);
            target.suppression_frames = since(target.suppression_frames, base.suppression_frames);
            target.clipped = since(target.clipped, base.clipped);
            target.missed_deadlines = since(target.missed_deadlines, base.missed_deadlines);
            target.stale_results = since(target.stale_results, base.stale_results);
            target.input_drops = since(target.input_drops, base.input_drops);
            target.output_drops = since(target.output_drops, base.output_drops);
            let Some(performance) = target.performance.as_mut() else {
                continue;
            };
            performance.xruns = since(reading.xruns, baseline.xruns);
            let after_reset = self.baseline_is_reset;
            performance.callback = timing_since(&reading.callback, &baseline.callback, after_reset);
            performance.worker_wall =
                timing_since(&current.worker_wall, &base.worker_wall, after_reset);
            performance.worker_cpu =
                timing_since(&current.worker_cpu, &base.worker_cpu, after_reset);
            performance.helper_late = performance
                .helper_late
                .map(|_| since(reading.helper_late, baseline.helper_late));
            performance.helper_stalls = performance
                .helper_stalls
                .map(|_| since(reading.helper_stalls, baseline.helper_stalls));
        }
    }

    /// Appends everything that happened since the last reading.
    fn collect_events(&mut self, reading: &Reading, native: &[ContextStatus], now_ns: u64) {
        // 1. The engine's own ring. Every context reports the same process-wide
        //    tail, so the highest sequence number seen dedupes it.
        for context in native {
            for event in &context.events {
                if event.seq > self.engine_seq {
                    self.engine_event(event);
                }
            }
        }
        let interval = self
            .last_reading_ns
            .map_or(0, |before| now_ns.saturating_sub(before));
        // 2. Each worker's miss and late histories, per path.
        for context in native {
            for status in &context.targets {
                self.worker_history(status, true, now_ns, interval);
                self.worker_history(status, false, now_ns, interval);
            }
        }
        // 3. Counter increases with no per-event record, one entry each per
        //    reading, with the interval they accumulated over.
        let Some(previous) = self.last.clone() else {
            return;
        };
        for (id, target) in &reading.targets {
            let Some(old) = previous.targets.get(id) else {
                continue;
            };
            for (kind, now, before, noun) in [
                (
                    DiagnosticsKind::StaleResults,
                    target.stale_results,
                    old.stale_results,
                    "stale suppression results",
                ),
                (
                    DiagnosticsKind::InputDrops,
                    target.input_drops,
                    old.input_drops,
                    "input drops",
                ),
                (
                    DiagnosticsKind::OutputDrops,
                    target.output_drops,
                    old.output_drops,
                    "output drops",
                ),
                (
                    DiagnosticsKind::Clipped,
                    target.clipped,
                    old.clipped,
                    "clipped samples",
                ),
            ] {
                let delta = since(now, before);
                if delta == 0 {
                    continue;
                }
                self.push(DiagnosticsEvent {
                    seq: 0,
                    kind,
                    path: Some(id.clone()),
                    at_ns: now_ns,
                    message: format!("{delta} {noun} on {id}"),
                    details: EventDetails {
                        delta: Some(delta),
                        interval_ns: Some(interval),
                        ..EventDetails::default()
                    },
                });
            }
        }
    }

    fn engine_event(&mut self, event: &NativeEvent) {
        let kind = match event.kind {
            NativeEventKind::Xrun => DiagnosticsKind::Xrun,
            NativeEventKind::HelperBusy => DiagnosticsKind::HelperBusy,
            NativeEventKind::HelperStall => DiagnosticsKind::HelperStall,
            NativeEventKind::HelperLateFinish => DiagnosticsKind::HelperLateFinish,
            NativeEventKind::LateEnd => DiagnosticsKind::LateEnd,
            NativeEventKind::Overrun => DiagnosticsKind::Overrun,
            NativeEventKind::LateStart => DiagnosticsKind::LateStart,
            NativeEventKind::Gap => DiagnosticsKind::Gap,
            NativeEventKind::CycleSkip => DiagnosticsKind::CycleSkip,
            NativeEventKind::VendorJobLate => DiagnosticsKind::VendorJobLate,
            NativeEventKind::UsbGadget => DiagnosticsKind::UsbGadget,
            NativeEventKind::Other => DiagnosticsKind::Other,
        };
        // A jump in the engine's sequence numbers means entries nobody read:
        // report the count before this one so the list never looks complete.
        if self.engine_seq != 0 && event.seq > self.engine_seq.saturating_add(1) {
            let count = event.seq - self.engine_seq - 1;
            self.push(DiagnosticsEvent {
                seq: 0,
                kind: DiagnosticsKind::Lost,
                path: None,
                at_ns: event.at_ns,
                message: format!("{count} engine events were not read"),
                details: EventDetails {
                    delta: Some(count),
                    ..EventDetails::default()
                },
            });
        }
        self.engine_seq = event.seq;
        let mut message = describe(kind).to_owned();
        if kind == DiagnosticsKind::Xrun {
            message = match event.delayed_us {
                Some(us) => format!("JACK xrun (jackd reported {us} us of backend delay)"),
                None => "JACK xrun".into(),
            };
        }
        self.push(DiagnosticsEvent {
            seq: 0,
            kind,
            path: None,
            at_ns: event.at_ns,
            message,
            details: EventDetails {
                value_ns: event.value_ns,
                delayed_us: event.delayed_us,
                ..EventDetails::default()
            },
        });
    }

    fn worker_history(&mut self, status: &TargetStatus, misses: bool, now_ns: u64, interval: u64) {
        let path = status.id.as_str();
        let history = if misses {
            &status.misses
        } else {
            &status.lates
        };
        let Some(history) = history else {
            return;
        };
        let mut cursor = self.worker_seqs.get(path).copied().unwrap_or_default();
        // A new runtime epoch is a new worker: its histories start over.
        if cursor.runtime_epoch != status.runtime_epoch {
            cursor = HistoryCursor {
                runtime_epoch: status.runtime_epoch,
                ..HistoryCursor::default()
            };
        }
        let mut read = if misses { cursor.misses } else { cursor.lates };
        // A history never shrinks within one worker; if it did, start over
        // rather than skip what it holds now.
        if history.total < read.listed.saturating_add(read.counted) {
            read = HistoryRead::default();
        }
        let (kind, noun) = if misses {
            (DiagnosticsKind::MissedDeadline, "missed deadlines")
        } else {
            (DiagnosticsKind::LateFrame, "late frames")
        };
        // A record's position in the list is its ordinal in the ring only
        // when every kept record is readable. One still being written (the
        // total is published last, so this is not expected) leaves the
        // ordinals unknown: wait for the next reading.
        let kept = history.total.saturating_sub(history.lost);
        if history.events.len() as u64 >= kept {
            for record in history.events.iter().skip(read.listed as usize) {
                self.push(DiagnosticsEvent {
                    seq: 0,
                    kind,
                    path: Some(path.to_owned()),
                    at_ns: record.at_ns,
                    message: format!("{path}: {}", describe(kind)),
                    details: EventDetails::default(),
                });
            }
            read.listed = read.listed.max(history.events.len() as u64);
            // The worker keeps only its first records; later ones are only
            // counted. Report each increase as one counted entry.
            if history.lost > read.counted {
                let delta = history.lost - read.counted;
                self.push(DiagnosticsEvent {
                    seq: 0,
                    kind,
                    path: Some(path.to_owned()),
                    at_ns: now_ns,
                    message: format!(
                        "{path}: {delta} more {noun} (only the first {} are listed)",
                        history.events.len()
                    ),
                    details: EventDetails {
                        delta: Some(delta),
                        interval_ns: Some(interval),
                        ..EventDetails::default()
                    },
                });
                read.counted = history.lost;
            }
        }
        if misses {
            cursor.misses = read;
        } else {
            cursor.lates = read;
        }
        self.worker_seqs.insert(path.to_owned(), cursor);
    }
}

fn describe(kind: DiagnosticsKind) -> &'static str {
    match kind {
        DiagnosticsKind::Xrun => "JACK xrun",
        DiagnosticsKind::HelperBusy => "the helper was still busy",
        DiagnosticsKind::HelperStall => "the callback passed its frame through (helper stall)",
        DiagnosticsKind::HelperLateFinish => "the helper finished after its frame's cycle",
        DiagnosticsKind::LateEnd => "the callback ended late",
        DiagnosticsKind::Overrun => "the callback overran its period",
        DiagnosticsKind::LateStart => "the callback started late",
        DiagnosticsKind::Gap => "the callbacks were further apart than a period and a half",
        DiagnosticsKind::CycleSkip => "the cycle clock skipped",
        DiagnosticsKind::VendorJobLate => "a vendor job finished after the next callback",
        DiagnosticsKind::UsbGadget => "the USB gadget was rebuilt",
        DiagnosticsKind::MissedDeadline => "suppression missed its deadline",
        DiagnosticsKind::LateFrame => "a suppression result arrived late",
        DiagnosticsKind::StaleResults => "stale suppression results",
        DiagnosticsKind::InputDrops => "input drops",
        DiagnosticsKind::OutputDrops => "output drops",
        DiagnosticsKind::Clipped => "clipped samples",
        DiagnosticsKind::Lost => "events were lost before they could be read",
        DiagnosticsKind::Reset => "diagnostics reset",
        DiagnosticsKind::Other => "an engine event",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::{Performance, TargetMeter};
    use rc2fx_native_control::NativeHistory;

    fn timing(calls: u64, max: u64, bins: [u64; 8]) -> Timing {
        Timing {
            calls,
            total_ns: calls * 100,
            max_ns: max,
            overruns: 0,
            clock_errors: 0,
            histogram: bins,
        }
    }

    /// The engine-wide figures a native reading carries: callback timing and
    /// the xrun and helper counts. Production gets them from the context.
    fn context(meter: &Meter, events: Vec<NativeEvent>) -> ContextStatus {
        let performance = meter.targets[0].performance.as_ref().unwrap();
        ContextStatus {
            context: 1,
            targets: vec![],
            timing: Some(rc2fx_native_control::CallbackTiming {
                callback_budget_ns: performance.callback_budget_ns,
                callback_wall: rc2fx_native_control::Timing {
                    calls: performance.callback.calls,
                    total_ns: performance.callback.total_ns,
                    max_ns: performance.callback.max_ns,
                    overruns: performance.callback.overruns,
                    clock_errors: performance.callback.clock_errors,
                    histogram: performance.callback.histogram,
                },
                ..Default::default()
            }),
            counters: rc2fx_native_control::EngineCounters {
                xruns: performance.xruns,
                helper_late: performance.helper_late.unwrap_or(0),
                helper_stalls: performance.helper_stalls.unwrap_or(0),
            },
            events,
            recent_callback_wall_ns: None,
        }
    }

    fn observe(log: &mut DiagnosticsLog, meter: &mut Meter, at: u64) {
        let context = context(meter, Vec::new());
        log.observe(Some(meter), &[context], at);
        *meter = log.shown(meter);
    }

    fn meter(callback: Timing, worker: Timing) -> Meter {
        Meter {
            message_type: rc2fx_core::MeterType::Meter,
            revision: 1,
            generation: 1,
            synthetic: false,
            diagnostics_epoch: 0,
            running_for_ns: None,
            targets: vec![TargetMeter {
                id: "mic-1".into(),
                input_peak: 0.,
                output_peak: 0.,
                rms: 0.,
                gain_reduction: 0.,
                voice: 0.,
                gate_open: 0.,
                clipped: 3,
                spectrum: vec![],
                callbacks: 10,
                processed_frames: 100,
                suppression_frames: 5,
                missed_deadlines: 1,
                stale_results: 2,
                input_drops: 3,
                output_drops: 4,
                latency_samples: 920,
                worker_failed: false,
                performance: Some(Performance {
                    callback_budget_ns: 2_333_333,
                    worker_budget_ns: 9_333_333,
                    xruns: 2,
                    callback: callback.clone(),
                    worker_wall: worker.clone(),
                    worker_cpu: worker,
                    helper_late: Some(1),
                    helper_stalls: Some(0),
                    // Already a window, not a cumulative maximum: the
                    // baseline below must leave these alone.
                    callback_recent_max_ns: Some(750),
                    worker_recent_max_ns: Some(2_600),
                }),
            }],
        }
    }

    #[test]
    fn after_a_reset_the_shown_counters_are_zero_and_then_only_the_growth() {
        let mut log = DiagnosticsLog::default();
        let bins = [1, 1, 1, 1, 1, 1, 1, 1];
        let mut first = meter(timing(10, 500, bins), timing(4, 300, bins));
        observe(&mut log, &mut first, 1_000);
        // Nothing has been reset yet: the baseline is the first reading.
        assert_eq!(first.targets[0].callbacks, 0);
        assert_eq!(first.targets[0].clipped, 0);
        assert_eq!(first.diagnostics_epoch, 0);
        log.reset(1_500);
        // Growth shows only the growth.
        let mut second = meter(
            timing(14, 900, [2, 1, 1, 1, 1, 1, 1, 1]),
            timing(6, 400, bins),
        );
        second.targets[0].callbacks = 14;
        observe(&mut log, &mut second, 2_000);
        assert_eq!(second.targets[0].callbacks, 4);
        assert_eq!(second.targets[0].clipped, 0);
        let performance = second.targets[0].performance.as_ref().unwrap();
        assert_eq!(performance.callback.calls, 4);
        assert_eq!(
            performance.callback.max_ns, 900,
            "a new maximum is reported"
        );
        assert_eq!(performance.callback.histogram, [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(performance.worker_wall.max_ns, 400);
        // Recent maxima are already windows, not cumulative figures, so a
        // reset must not rebaseline them.
        assert_eq!(
            (
                performance.callback_recent_max_ns,
                performance.worker_recent_max_ns
            ),
            (Some(750), Some(2_600))
        );
        // A sample that does not beat the pre-reset maximum reports no
        // maximum: 0 means "nothing since the reset has beaten it".
        let mut fourth = meter(
            timing(16, 200, [2, 1, 1, 1, 1, 1, 1, 1]),
            timing(8, 400, bins),
        );
        fourth.targets[0].callbacks = 16;
        observe(&mut log, &mut fourth, 4_000);
        let callback = &fourth.targets[0].performance.as_ref().unwrap().callback;
        assert_eq!(
            callback.max_ns, 0,
            "no sample beat the 500 ns pre-reset max"
        );
    }

    #[test]
    fn before_any_reset_the_lifetime_maximum_stands() {
        // The worst callback came at startup, before the first reading.
        let (peak, bins) = (5_000_000, [0; 8]);
        let mut log = DiagnosticsLog::default();
        let mut first = meter(timing(10, peak, bins), timing(4, peak, bins));
        observe(&mut log, &mut first, 1_000);
        let mut second = meter(timing(50_000, peak, bins), timing(40, peak, bins));
        observe(&mut log, &mut second, 2_000);
        assert_eq!(second.diagnostics_epoch, 0);
        let performance = second.targets[0].performance.as_ref().unwrap();
        let callback = &performance.callback;
        assert_eq!(callback.max_ns, peak);
        assert_eq!(performance.worker_wall.max_ns, peak);
        assert_eq!(performance.worker_cpu.max_ns, peak);
        // Counters still run from the first reading, and the peak of that
        // window is never below its mean.
        assert_eq!(callback.calls, 49_990);
        assert!(callback.max_ns >= callback.total_ns / callback.calls);
    }

    #[test]
    fn a_new_engine_has_had_no_reset_even_after_an_old_one_did() {
        let (peak, bins) = (5_000_000, [0; 8]);
        let mut log = DiagnosticsLog::default();
        let mut old = meter(timing(10, 500, bins), timing(4, 300, bins));
        observe(&mut log, &mut old, 1_000);
        log.reset(1_500);
        let mut first = meter(timing(10, peak, bins), timing(4, 300, bins));
        first.generation = 2;
        observe(&mut log, &mut first, 2_000);
        let mut second = meter(timing(20, peak, bins), timing(8, 300, bins));
        second.generation = 2;
        observe(&mut log, &mut second, 3_000);
        assert_eq!(second.diagnostics_epoch, 1);
        let performance = second.targets[0].performance.as_ref().unwrap();
        assert_eq!(performance.callback.max_ns, peak);
        // A reset on the new engine clears that maximum like any other.
        log.reset(3_500);
        let mut third = meter(timing(30, peak, bins), timing(12, 300, bins));
        third.generation = 2;
        observe(&mut log, &mut third, 4_000);
        let performance = third.targets[0].performance.as_ref().unwrap();
        assert_eq!(performance.callback.max_ns, 0);
    }

    #[test]
    fn a_reset_on_large_counters_still_shows_the_growth() {
        // The controller observes the raw meter, then asks for what to show:
        // the baseline must apply once, or every count after a reset on a
        // long-running engine sits at zero (the app then sees no live path).
        let mut log = DiagnosticsLog::default();
        let mut raw = meter(timing(10, 500, [0; 8]), timing(4, 300, [0; 8]));
        raw.targets[0].callbacks = 1_000_000;
        raw.targets[0].processed_frames = 112_000_000;
        log.observe(Some(&raw), &[context(&raw, Vec::new())], 1_000);
        log.reset(1_500);
        raw.targets[0].callbacks = 1_000_010;
        raw.targets[0].processed_frames = 112_001_120;
        log.observe(Some(&raw), &[context(&raw, Vec::new())], 2_000);
        assert_eq!(
            raw.targets[0].callbacks, 1_000_010,
            "observe leaves the raw meter"
        );
        let shown = log.shown(&raw);
        assert_eq!(shown.targets[0].callbacks, 10);
        assert_eq!(shown.targets[0].processed_frames, 1_120);
        assert_eq!(shown.diagnostics_epoch, 1);
    }

    #[test]
    fn a_reset_rebaselines_and_bumps_the_epoch() {
        let mut log = DiagnosticsLog::default();
        let mut first = meter(timing(10, 500, [0; 8]), timing(4, 300, [0; 8]));
        observe(&mut log, &mut first, 1_000);
        assert_eq!(log.epoch, 0);
        log.reset(1_500);
        assert_eq!(log.epoch, 1);
        // The same raw values now show zero.
        let mut second = meter(timing(10, 500, [0; 8]), timing(4, 300, [0; 8]));
        observe(&mut log, &mut second, 2_000);
        assert_eq!(second.diagnostics_epoch, 1);
        assert_eq!(second.targets[0].callbacks, 0);
        assert_eq!(second.targets[0].performance.as_ref().unwrap().xruns, 0);
        assert_eq!(
            second.targets[0].performance.as_ref().unwrap().helper_late,
            Some(0)
        );
        // And growth after the reset is counted from there.
        let mut third = meter(timing(12, 700, [0; 8]), timing(5, 350, [0; 8]));
        third.targets[0].callbacks = 12;
        observe(&mut log, &mut third, 3_000);
        assert_eq!(third.targets[0].callbacks, 2);
        assert_eq!(
            third.targets[0]
                .performance
                .as_ref()
                .unwrap()
                .callback
                .max_ns,
            700
        );
    }

    #[test]
    fn a_new_generation_clears_the_baselines() {
        let mut log = DiagnosticsLog::default();
        let mut first = meter(timing(10, 500, [0; 8]), timing(4, 300, [0; 8]));
        observe(&mut log, &mut first, 1_000);
        let mut next = meter(timing(2, 100, [0; 8]), timing(1, 50, [0; 8]));
        next.generation = 2;
        next.targets[0].callbacks = 3;
        observe(&mut log, &mut next, 2_000);
        // A new generation is its own baseline: the counters restart at zero,
        // not at a negative or huge delta.
        assert_eq!(next.targets[0].callbacks, 0);
        assert_eq!(
            next.targets[0].performance.as_ref().unwrap().callback.calls,
            0
        );
    }

    #[test]
    fn counter_increases_become_one_entry_each() {
        let mut log = DiagnosticsLog::default();
        let mut first = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        observe(&mut log, &mut first, 1_000);
        let mut second = meter(timing(2, 100, [0; 8]), timing(2, 100, [0; 8]));
        second.targets[0].stale_results += 2;
        second.targets[0].input_drops += 1;
        second.targets[0].clipped += 5;
        observe(&mut log, &mut second, 1_250);
        let page = log.page(0, EVENTS_PAGE, 2_000);
        let kinds: Vec<_> = page.events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiagnosticsKind::StaleResults,
                DiagnosticsKind::InputDrops,
                DiagnosticsKind::Clipped
            ]
        );
        assert_eq!(page.events[0].details.delta, Some(2));
        assert_eq!(page.events[0].path.as_deref(), Some("mic-1"));
        assert!(
            page.events[0].message.contains("stale"),
            "{}",
            page.events[0].message
        );
        // A steady reading adds nothing.
        let before = log.events.len();
        let mut third = meter(timing(3, 100, [0; 8]), timing(3, 100, [0; 8]));
        third.targets[0].stale_results = second.targets[0].stale_results;
        third.targets[0].input_drops = second.targets[0].input_drops;
        third.targets[0].clipped = second.targets[0].clipped;
        observe(&mut log, &mut third, 1_500);
        assert_eq!(log.events.len(), before);
    }

    #[test]
    fn a_reset_while_the_engine_settles_still_baselines_every_path() {
        let mut log = DiagnosticsLog::default();
        let raw = meter(timing(10, 500, [0; 8]), timing(4, 300, [0; 8]));
        let engine = context(&raw, Vec::new());
        log.observe(Some(&raw), std::slice::from_ref(&engine), 1_000);
        // The engine settles: a reading with no meter, only engine figures.
        log.observe(None, std::slice::from_ref(&engine), 1_250);
        log.reset(1_500);
        let mut later = raw.clone();
        later.targets[0].callbacks += 7;
        later.targets[0].clipped += 2;
        log.observe(Some(&later), &[context(&later, Vec::new())], 2_000);
        let shown = log.shown(&later);
        assert_eq!(shown.diagnostics_epoch, 1);
        // Only the growth since the reset, not the lifetime counts.
        assert_eq!(shown.targets[0].callbacks, 7);
        assert_eq!(shown.targets[0].clipped, 2);
        assert_eq!(shown.targets[0].processed_frames, 0);
        assert_eq!(shown.targets[0].performance.as_ref().unwrap().xruns, 0);
    }

    #[test]
    fn counter_increases_across_a_reading_without_a_meter_are_kept() {
        let mut log = DiagnosticsLog::default();
        let raw = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        let engine = context(&raw, Vec::new());
        log.observe(Some(&raw), std::slice::from_ref(&engine), 1_000);
        log.observe(None, std::slice::from_ref(&engine), 1_250);
        assert!(
            log.events.is_empty(),
            "a reading without a meter adds nothing"
        );
        let mut later = raw.clone();
        later.targets[0].input_drops += 3;
        log.observe(Some(&later), &[context(&later, Vec::new())], 1_500);
        let page = log.page(0, EVENTS_PAGE, 2_000);
        let kinds: Vec<_> = page.events.iter().map(|event| event.kind).collect();
        assert_eq!(kinds, vec![DiagnosticsKind::InputDrops]);
        assert_eq!(page.events[0].details.delta, Some(3));
    }

    #[test]
    fn the_ring_drops_the_oldest_and_counts_what_a_client_missed() {
        let mut log = DiagnosticsLog::default();
        for index in 0..(LOG_CAPACITY + 10) {
            log.push(DiagnosticsEvent {
                seq: 0,
                kind: DiagnosticsKind::Overrun,
                path: None,
                at_ns: index as u64,
                message: format!("{index}"),
                details: EventDetails::default(),
            });
        }
        assert_eq!(log.events.len(), LOG_CAPACITY);
        let page = log.page(0, EVENTS_PAGE, 0);
        // Ten entries fell out before this client's cursor.
        assert_eq!(page.lost, 10);
        assert_eq!(page.events[0].at_ns, 10);
        assert!(page.more);
        assert_eq!(page.events.len(), EVENTS_PAGE);
        // Paging: each page continues from the last sequence number it saw.
        let mut after = page.events.last().unwrap().seq;
        let mut seen = page.events.len();
        loop {
            let next = log.page(after, EVENTS_PAGE, 0);
            assert_eq!(next.lost, 0);
            seen += next.events.len();
            if !next.more {
                break;
            }
            after = next.next_seq - 1;
        }
        assert_eq!(seen, LOG_CAPACITY);
    }

    fn native_event(seq: u64, kind: NativeEventKind) -> NativeEvent {
        NativeEvent {
            seq,
            at_ns: 1_000 + seq,
            kind,
            value_ns: Some(500),
            delayed_us: None,
        }
    }

    #[test]
    fn engine_events_arrive_once_and_a_sequence_gap_is_reported() {
        let mut log = DiagnosticsLog::default();
        let meter = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        let mut context = ContextStatus {
            context: 1,
            targets: vec![],
            timing: None,
            counters: Default::default(),
            events: vec![native_event(1, NativeEventKind::Overrun)],
            recent_callback_wall_ns: None,
        };
        log.observe(Some(&meter), std::slice::from_ref(&context), 1_000);
        // The same tail again (as every context reports it) adds nothing.
        log.observe(Some(&meter), std::slice::from_ref(&context), 1_100);
        assert_eq!(log.events.len(), 1);
        // A gap: the engine produced 2 and 3 while nobody read them.
        context.events = vec![native_event(4, NativeEventKind::Xrun)];
        log.observe(Some(&meter), std::slice::from_ref(&context), 1_200);
        let page = log.page(0, EVENTS_PAGE, 2_000);
        assert_eq!(
            page.events
                .iter()
                .map(|event| event.kind)
                .collect::<Vec<_>>(),
            vec![
                DiagnosticsKind::Overrun,
                DiagnosticsKind::Lost,
                DiagnosticsKind::Xrun
            ]
        );
        assert_eq!(page.events[1].details.delta, Some(2));
        assert_eq!(page.events[2].details.value_ns, Some(500));
    }

    fn history(events: Vec<(u64, u64)>, total: u64) -> NativeHistory {
        NativeHistory {
            total,
            lost: total.saturating_sub(events.len() as u64),
            events: events
                .into_iter()
                .map(
                    |(sequence, at_ns)| rc2fx_native_control::NativeEventRecord { sequence, at_ns },
                )
                .collect(),
        }
    }

    fn history_status(misses: NativeHistory, lates: NativeHistory) -> TargetStatus {
        TargetStatus {
            id: "mic-1".into(),
            mode: rc2fx_native_control::PathMode::Processed,
            runtime_epoch: 1,
            retired_workers: vec![],
            retired_worker_count: 0,
            published: 1,
            seen: 1,
            applied: 1,
            fingerprint: [0; 32],
            meter: None,
            worker: None,
            misses: Some(misses),
            lates: Some(lates),
            worker_recent_max_ns: None,
        }
    }

    fn history_context(status: &TargetStatus) -> ContextStatus {
        ContextStatus {
            context: 1,
            targets: vec![status.clone()],
            timing: None,
            counters: Default::default(),
            events: vec![],
            recent_callback_wall_ns: None,
        }
    }

    #[test]
    fn worker_histories_arrive_once_and_a_new_worker_starts_over() {
        let mut log = DiagnosticsLog::default();
        // Frames are 0-based: the cold first frame of a worker can miss.
        let mut status = history_status(history(vec![(0, 100)], 1), history(vec![(0, 200)], 1));
        let meter = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        log.observe(Some(&meter), &[history_context(&status)], 1_000);
        log.observe(Some(&meter), &[history_context(&status)], 1_100);
        assert_eq!(
            log.events.len(),
            2,
            "one miss and one late frame, read once"
        );
        // A new worker (new runtime epoch) whose frame numbers are higher than
        // the old cursor's: every one of its records is new, from the first.
        status.runtime_epoch = 2;
        status.misses = Some(history(vec![(0, 300), (5, 350)], 2));
        status.lates = Some(history(vec![], 0));
        log.observe(Some(&meter), &[history_context(&status)], 1_200);
        assert_eq!(log.events.len(), 4);
        // Then its next miss is new, even at a lower frame than the old
        // worker's records.
        status.misses = Some(history(vec![(0, 300), (5, 350), (7, 500)], 3));
        log.observe(Some(&meter), &[history_context(&status)], 1_300);
        assert_eq!(log.events.len(), 5);
        let page = log.page(0, EVENTS_PAGE, 2_000);
        assert_eq!(
            page.events
                .iter()
                .map(|event| (event.kind, event.at_ns))
                .collect::<Vec<_>>(),
            vec![
                (DiagnosticsKind::MissedDeadline, 100),
                (DiagnosticsKind::LateFrame, 200),
                (DiagnosticsKind::MissedDeadline, 300),
                (DiagnosticsKind::MissedDeadline, 350),
                (DiagnosticsKind::MissedDeadline, 500),
            ]
        );
        assert!(
            page.events
                .iter()
                .all(|event| event.details.delta.is_none())
        );
        assert_eq!(page.events[4].path.as_deref(), Some("mic-1"));
    }

    #[test]
    fn events_past_a_full_history_are_counted_not_dropped() {
        let mut log = DiagnosticsLog::default();
        let meter = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        let kept: Vec<(u64, u64)> = (0..64).map(|frame| (frame, 1_000 + frame)).collect();
        // The 64 kept records and 3 more that were only counted.
        let mut status = history_status(history(kept.clone(), 67), history(vec![], 0));
        log.observe(Some(&meter), &[history_context(&status)], 1_000);
        assert_eq!(log.events.len(), 65);
        // Five more, only counted: one entry for the five.
        status.misses = Some(history(kept.clone(), 72));
        log.observe(Some(&meter), &[history_context(&status)], 1_500);
        // Nothing new: nothing added.
        log.observe(Some(&meter), &[history_context(&status)], 1_600);
        assert_eq!(log.events.len(), 66);
        let page = log.page(0, EVENTS_PAGE, 2_000);
        let counted: Vec<_> = page
            .events
            .iter()
            .filter(|event| event.details.delta.is_some())
            .collect();
        assert_eq!(counted.len(), 2);
        assert!(
            counted
                .iter()
                .all(|event| event.kind == DiagnosticsKind::MissedDeadline)
        );
        assert_eq!(counted[0].details.delta, Some(3));
        assert_eq!(counted[1].details.delta, Some(5));
        assert_eq!(counted[1].details.interval_ns, Some(500));
        assert_eq!(counted[1].at_ns, 1_500);
        assert!(
            counted[1].message.contains("5 more missed deadlines"),
            "{}",
            counted[1].message
        );
    }

    #[test]
    fn a_record_still_being_written_waits_for_the_next_reading() {
        let mut log = DiagnosticsLog::default();
        let meter = meter(timing(1, 100, [0; 8]), timing(1, 100, [0; 8]));
        // Two counted, one readable: which ordinal it holds is unknown.
        let mut status = history_status(
            NativeHistory {
                total: 2,
                lost: 0,
                events: vec![rc2fx_native_control::NativeEventRecord {
                    sequence: 3,
                    at_ns: 100,
                }],
            },
            history(vec![], 0),
        );
        log.observe(Some(&meter), &[history_context(&status)], 1_000);
        assert_eq!(log.events.len(), 0);
        status.misses = Some(history(vec![(0, 50), (3, 100)], 2));
        log.observe(Some(&meter), &[history_context(&status)], 1_100);
        assert_eq!(log.events.len(), 2);
    }

    #[test]
    fn a_reset_clears_events_and_says_where_it_happened() {
        let mut log = DiagnosticsLog::default();
        log.push(DiagnosticsEvent {
            seq: 0,
            kind: DiagnosticsKind::Overrun,
            path: None,
            at_ns: 1,
            message: "before".into(),
            details: EventDetails::default(),
        });
        log.reset(1_500);
        let page = log.page(0, EVENTS_PAGE, 0);
        // The reset entry is recorded; earlier entries are cleared (the client
        // sees them as lost if it still asks from before).
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].kind, DiagnosticsKind::Reset);
        assert_eq!(page.events[0].seq, 2, "sequence numbers continue");
        assert_eq!(page.lost, 1);
    }
}
