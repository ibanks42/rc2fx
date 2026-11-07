use rc2fx_core::{DeviceState, EngineMode, EnginePhase, Meter, RoutingPhase, TargetMeter};
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};

/// How much audio counts as the mixer starting, from its first reading: JACK
/// coming up, the controller starting and threads moving. Measured faults fell
/// inside this on real hardware; a fault after it counts normally.
const STARTUP_AUDIO: Duration = Duration::from_secs(30);

/// Faults counted while the mixer started. They are kept out of the telemetry
/// headline and shown on one quiet line instead; the mixer's logs and counters
/// are untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct StartupFaults {
    pub(super) xruns: u64,
    overruns: u64,
    helper_late: u64,
    helper_stalls: u64,
}

impl StartupFaults {
    fn of(meter: &Meter) -> Self {
        meter
            .targets
            .iter()
            .find_map(|target| target.performance.as_ref())
            .map_or_else(Self::default, |p| Self {
                xruns: p.xruns,
                overruns: p.callback.overruns,
                helper_late: p.helper_late.unwrap_or(0),
                helper_stalls: p.helper_stalls.unwrap_or(0),
            })
    }

    /// "3 xruns, 1 overrun during startup", or None with nothing to report.
    fn line(self) -> Option<String> {
        let parts: Vec<_> = [
            (self.xruns, "xrun", "xruns"),
            (self.overruns, "overrun", "overruns"),
            (
                self.helper_late,
                "late helper finish",
                "late helper finishes",
            ),
            (self.helper_stalls, "helper stall", "helper stalls"),
        ]
        .into_iter()
        .filter(|(count, ..)| *count > 0)
        .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
        .collect();
        (!parts.is_empty()).then(|| format!("{} during startup", parts.join(", ")))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum StartupPhase {
    /// Audio is younger than the window: everything counted is startup.
    Open,
    /// First seen after the window: only the event log can say what fell
    /// inside it. Faults stay in the headline until it has.
    Unread { started: SystemTime },
    /// The startup faults are known, or none can be told apart.
    #[default]
    Closed,
}

/// Separates what the mixer counted while starting from what came after. The
/// shown counters start at the first reading of an engine generation, so audio
/// processed since then is a measured age, and a new generation is a restart.
#[derive(Default)]
pub(super) struct StartupWindow {
    /// The generation and diagnostics epoch the counters belong to.
    seen: Option<(u64, u64)>,
    phase: StartupPhase,
    faults: StartupFaults,
}

impl StartupWindow {
    pub(super) fn observe(&mut self, meter: &Meter, sample_rate: u32, now: SystemTime) {
        let frames = meter.targets.iter().map(|t| t.processed_frames).max();
        let (Some(frames), true) = (frames, sample_rate > 0) else {
            return;
        };
        let age = Duration::from_secs_f64(frames as f64 / f64::from(sample_rate));
        let key = (meter.generation, meter.diagnostics_epoch);
        if self.seen != Some(key) {
            let reset = self.seen.is_some_and(|(generation, _)| generation == key.0);
            *self = Self {
                seen: Some(key),
                ..Self::default()
            };
            // A diagnostics reset restarts the counters, not the mixer, and
            // one from before this app connected hides the true start.
            if !reset && meter.diagnostics_epoch == 0 {
                self.phase = if age < STARTUP_AUDIO {
                    StartupPhase::Open
                } else {
                    StartupPhase::Unread {
                        started: now.checked_sub(age).unwrap_or(now),
                    }
                };
            }
        }
        if self.phase == StartupPhase::Open {
            if age < STARTUP_AUDIO {
                self.faults = StartupFaults::of(meter);
            } else {
                self.phase = StartupPhase::Closed;
            }
        }
    }

    /// Whether the mixer is still inside the window.
    pub(super) fn open(&self) -> bool {
        self.phase == StartupPhase::Open
    }

    /// Whether the event log is needed to place the startup faults.
    pub(super) fn wants_events(&self) -> bool {
        matches!(self.phase, StartupPhase::Unread { .. })
    }

    /// The event log is read: `xruns` counts those in the range it is given.
    pub(super) fn read_events(&mut self, xruns: impl Fn(SystemTime, SystemTime) -> u64) {
        if let StartupPhase::Unread { started } = self.phase {
            self.faults.xruns = xruns(started, started + STARTUP_AUDIO);
            self.phase = StartupPhase::Closed;
        }
    }

    /// The event log can't be read: every fault stays in the headline.
    pub(super) fn give_up(&mut self) {
        if self.wants_events() {
            self.phase = StartupPhase::Closed;
        }
    }

    /// Takes the startup faults out of what the headline counts.
    pub(super) fn settle(&self, meter: &mut TargetMeter) {
        let Some(p) = meter.performance.as_mut() else {
            return;
        };
        let faults = &self.faults;
        p.xruns = p.xruns.saturating_sub(faults.xruns);
        p.callback.overruns = p.callback.overruns.saturating_sub(faults.overruns);
        p.helper_late = p.helper_late.map(|n| n.saturating_sub(faults.helper_late));
        p.helper_stalls = p
            .helper_stalls
            .map(|n| n.saturating_sub(faults.helper_stalls));
    }

    pub(super) fn line(&self) -> Option<String> {
        self.faults.line()
    }
}

pub(super) fn meter_matches_engine(state: &DeviceState, meter: &Meter) -> bool {
    let expected = if state.engine.mode == EngineMode::Native {
        &state.selection.qualified_target_ids
    } else {
        &state.selection.active_target_ids
    };
    state.engine.status == EnginePhase::Running
        && state.selection.status == rc2fx_core::SelectionPhase::Idle
        && meter.generation == state.engine.generation
        && meter.revision == state.engine.applied_revision
        && meter.targets.len() == expected.len()
        && meter.targets.iter().map(|t| &t.id).eq(expected.iter())
}

/// The desktop receive path. Invalid, stale-generation and unrelated-revision
/// telemetry must not alter the progress baseline or renew engine freshness.
pub(super) fn receive_meter(
    state: &DeviceState,
    meter: &Meter,
    progress: &mut HashMap<String, Progress>,
    engine_seen: &mut Option<Instant>,
    now: Instant,
) -> bool {
    if !meter.targets.iter().all(|target| target.validate().is_ok())
        || !meter_matches_engine(state, meter)
    {
        return false;
    }
    if observe_meter_progress(progress, meter, &state.selection.active_target_ids, now) {
        // State is published only when it changes. New work from every path
        // also confirms observation of this same engine/revision.
        *engine_seen = Some(now);
    }
    true
}

/// The meter/status rendering predicate after the caller has confirmed both
/// the selected connection and its event stream are connected.
pub(super) fn fresh_meter<'a>(
    state: &DeviceState,
    meter: &'a Meter,
    progress: &HashMap<String, Progress>,
    target_id: &str,
    engine_seen: Option<Instant>,
    now: Instant,
) -> Option<(&'a TargetMeter, bool)> {
    let visible = if state.engine.mode == EngineMode::Native {
        &state.selection.qualified_target_ids
    } else {
        &state.selection.active_target_ids
    };
    if !engine_seen
        .is_some_and(|seen| now.saturating_duration_since(seen) < Duration::from_secs(30))
        || state.engine.status != EnginePhase::Running
        || !visible.iter().any(|id| id == target_id)
        || !state.engine.audio_advancing
        || meter.revision != state.engine.applied_revision
        || meter.generation != state.engine.generation
    {
        return None;
    }
    let target = meter.targets.iter().find(|target| target.id == target_id)?;
    progress
        .get(target_id)?
        .fresh(now)
        .then_some((target, meter.synthetic))
}

/// Call only for a validated meter matching the current engine generation,
/// revision and complete target set. Receipt alone does not renew the engine
/// observation: every active path must advance from its previous fresh sample,
/// and at least one path must advance. A native passthrough path whose stock
/// source is idle (unplugged USB, disconnected Bluetooth) keeps its own
/// per-path freshness but does not hold back the engine observation.
fn observe_meter_progress(
    progress: &mut HashMap<String, Progress>,
    meter: &Meter,
    active: &[String],
    now: Instant,
) -> bool {
    progress.retain(|id, _| meter.targets.iter().any(|target| &target.id == id));
    let mut any_advanced = false;
    let mut active_advanced = true;
    for target in &meter.targets {
        let advanced = progress
            .entry(target.id.clone())
            .or_insert_with(|| Progress::new(target.processed_frames))
            .observe_meter((meter.revision, meter.diagnostics_epoch), target, now);
        any_advanced |= advanced;
        if active.contains(&target.id) {
            active_advanced &= advanced;
        }
    }
    any_advanced && active_advanced
}

pub(super) struct EngineErrorNotice<'a> {
    pub(super) message: &'a str,
    pub(super) recovery_detail: &'static str,
}

impl EngineErrorNotice<'_> {
    pub(super) const TITLE: &'static str = "Audio processing error";
}

/// Terminal failures outlive meter freshness. Read the saved engine/routing
/// state, never infer recovery from missing meters or a stopped worker.
pub(super) fn engine_error(state: &DeviceState) -> Option<EngineErrorNotice<'_>> {
    if state.engine.status != EnginePhase::Error {
        return None;
    }
    let recovery_detail = match state.engine.mode {
        EngineMode::Simulation => "Simulation does not confirm mixer audio or route recovery.",
        EngineMode::Offline => "No live audio routing is confirmed in offline mode.",
        EngineMode::Native => {
            "Native channel processing reported an error. Mixer audio delivery is not confirmed by this status."
        }
        EngineMode::Jack => match state.routing.status {
            RoutingPhase::Bypassed
                if state.routing.channels == 0
                    && state.routing.recovered == 0
                    && state.selection.active_target_ids.is_empty()
                    && state.engine.restart_allowed
                    && !state.engine.audio_advancing =>
            {
                "Original audio routes were restored."
            }
            RoutingPhase::Bypassing => {
                "Original audio route restoration was in progress; completion is not confirmed."
            }
            _ => "Original audio route recovery is not confirmed.",
        },
    };
    Some(EngineErrorNotice {
        message: if state.engine.message.trim().is_empty() {
            "The audio engine reported an error without further details."
        } else {
            &state.engine.message
        },
        recovery_detail,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Warning {
    WorkerFailed,
    AudioDeadline,
    SuppressionFallback,
    ClockFailure,
}

impl Warning {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WorkerFailed => "Noise worker failed · stopping processing",
            Self::AudioDeadline => "Recent audio deadline missed",
            Self::SuppressionFallback => "Recent suppression fallback",
            Self::ClockFailure => "Audio timing measurement failed",
        }
    }
    pub(super) fn detail(self) -> &'static str {
        match self {
            Self::WorkerFailed => {
                "Noise suppression has failed. The device is stopping RC2FX processing; original audio route recovery is reported separately. Export diagnostics before restarting processing."
            }
            Self::AudioDeadline => {
                "A new callback overrun or shared JACK xrun was reported. Audio is advancing, but this interval was not clean. The JACK counter does not identify the responsible channel. Export diagnostics to retain the device history."
            }
            Self::SuppressionFallback => {
                "A new late or dropped suppression frame was reported. The worker uses the delayed original signal when a result is unavailable. Export diagnostics if this keeps happening."
            }
            Self::ClockFailure => {
                "A timing clock failed. These measurements cannot establish whether processing met its deadline."
            }
        }
    }
}

#[derive(Clone, Copy)]
struct FaultCounters {
    callbacks: u64,
    suppression_frames: u64,
    suppression: [u64; 4],
    performance: Option<([u64; 2], [u64; 3])>,
}

impl FaultCounters {
    fn from_meter(m: &TargetMeter) -> Self {
        Self {
            callbacks: m.callbacks,
            suppression_frames: m.suppression_frames,
            suppression: [
                m.missed_deadlines,
                m.stale_results,
                m.input_drops,
                m.output_drops,
            ],
            performance: m.performance.as_ref().map(|p| {
                (
                    [p.callback.overruns, p.xruns],
                    [
                        p.callback.clock_errors,
                        p.worker_wall.clock_errors,
                        p.worker_cpu.clock_errors,
                    ],
                )
            }),
        }
    }
    fn suppression_reset_from(self, old: Self) -> bool {
        self.suppression_frames < old.suppression_frames
            || self
                .suppression
                .iter()
                .zip(old.suppression)
                .any(|(now, before)| *now < before)
    }
    fn changed_from(self, old: Self) -> Option<Warning> {
        // A decreasing counter is a new baseline, not an unsigned fault delta.
        let increased = |now: &[u64], before: &[u64]| {
            now.iter().zip(before).all(|(a, b)| a >= b)
                && now.iter().zip(before).any(|(a, b)| a > b)
        };
        if let (Some((audio, clock)), Some((old_audio, old_clock))) =
            (self.performance, old.performance)
        {
            if increased(&audio, &old_audio) {
                return Some(Warning::AudioDeadline);
            }
            if increased(&clock, &old_clock) {
                return Some(Warning::ClockFailure);
            }
        }
        if self.suppression_frames >= old.suppression_frames
            && increased(&self.suppression, &old.suppression)
        {
            return Some(Warning::SuppressionFallback);
        }
        None
    }
}

/// Receipt alone is not progress. The first sample and counter resets require
/// another increasing sample before the display treats telemetry as current.
pub(super) struct Progress {
    frames: u64,
    advanced_at: Option<Instant>,
    observed_at: Option<Instant>,
    /// The meter revision and diagnostics epoch the counters belong to; a
    /// change of either (a new config, or a diagnostics reset) starts a new
    /// baseline.
    baseline: Option<(u64, u64)>,
    counters: Option<FaultCounters>,
    recent_warning: Option<(Instant, Warning)>,
    recent_suppression: Option<Instant>,
    worker_failed: bool,
}

impl Progress {
    pub(super) fn new(frames: u64) -> Self {
        Self {
            frames,
            advanced_at: None,
            observed_at: None,
            baseline: None,
            counters: None,
            recent_warning: None,
            recent_suppression: None,
            worker_failed: false,
        }
    }
    pub(super) fn observe(&mut self, frames: u64, now: Instant) {
        if frames > self.frames {
            self.advanced_at = Some(now);
        } else if frames < self.frames {
            self.advanced_at = None;
        }
        self.frames = frames;
    }
    pub(super) fn observe_meter(
        &mut self,
        baseline: (u64, u64),
        meter: &TargetMeter,
        now: Instant,
    ) -> bool {
        let counters = FaultCounters::from_meter(meter);
        let receipt_gap = self
            .observed_at
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(1));
        let callback_reset = self
            .counters
            .is_some_and(|old| counters.callbacks < old.callbacks);
        let reset = self.baseline != Some(baseline)
            || meter.processed_frames < self.frames
            || callback_reset
            || receipt_gap;
        let advanced = !reset
            && meter.processed_frames > self.frames
            && self
                .counters
                .is_some_and(|old| counters.callbacks > old.callbacks)
            && !meter.worker_failed;
        if reset {
            self.recent_warning = None;
            self.recent_suppression = None;
            self.advanced_at = None;
            self.frames = meter.processed_frames;
        } else if let Some(old) = self.counters {
            if counters.suppression_reset_from(old) {
                self.recent_suppression = None;
            }
            if let Some(warning) = counters.changed_from(old) {
                if warning == Warning::SuppressionFallback {
                    self.recent_suppression = Some(now);
                } else {
                    self.recent_warning = Some((now, warning));
                }
            }
        }
        self.observe(meter.processed_frames, now);
        self.observed_at = Some(now);
        self.baseline = Some(baseline);
        self.counters = Some(counters);
        self.worker_failed = meter.worker_failed;
        advanced
    }
    /// An audio deadline missed while the mixer starts is expected, so it does
    /// not warn. Other faults still do.
    pub(super) fn forget_startup_deadline(&mut self) {
        if matches!(self.recent_warning, Some((_, Warning::AudioDeadline))) {
            self.recent_warning = None;
        }
    }
    pub(super) fn warning_for(&self, now: Instant, suppression_active: bool) -> Option<Warning> {
        if suppression_active && self.worker_failed {
            Some(Warning::WorkerFailed)
        } else {
            self.recent_warning
                .filter(|(at, _)| now.saturating_duration_since(*at) < Duration::from_secs(5))
                .map(|(_, warning)| warning)
                .or_else(|| {
                    (suppression_active
                        && self.recent_suppression.is_some_and(|at| {
                            now.saturating_duration_since(at) < Duration::from_secs(5)
                        }))
                    .then_some(Warning::SuppressionFallback)
                })
        }
    }
    #[cfg(test)]
    fn warning(&self, now: Instant) -> Option<Warning> {
        self.warning_for(now, true)
    }
    pub(super) fn fresh(&self, now: Instant) -> bool {
        self.advanced_at
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_state() -> DeviceState {
        DeviceState {
            selection: rc2fx_core::SelectionStatus {
                channel_capacity: None,
                status: rc2fx_core::SelectionPhase::Idle,
                allowed: false,
                qualified_target_ids: vec!["mic-1".into()],
                active_target_ids: vec!["mic-1".into()],
                pending_target_ids: vec![],
                message: String::new(),
            },
            config: rc2fx_core::Config::default(),
            engine: rc2fx_core::EngineStatus {
                generation: 1,
                restart_allowed: false,
                mode: EngineMode::Jack,
                status: EnginePhase::Error,
                applied_revision: 1,
                sample_rate: 48_000,
                block_size: 112,
                message: "native suppression worker failed for mic-1; stopping processing".into(),
                audio_advancing: false,
            },
            routing: rc2fx_core::RoutingStatus {
                status: RoutingPhase::Active,
                channels: 2,
                recovered: 0,
                message: String::new(),
            },
        }
    }

    #[test]
    fn stopped_engine_error_remains_visible_without_any_fresh_meter() {
        let state = failed_state();
        assert!(!state.engine.audio_advancing);
        let notice = engine_error(&state).unwrap();
        assert_eq!(notice.message, state.engine.message);
        assert_eq!(
            notice.recovery_detail,
            "Original audio route recovery is not confirmed."
        );
    }

    #[test]
    fn old_queued_meters_cannot_confirm_a_new_engine_at_the_same_revision() {
        let mut state = failed_state();
        state.engine.status = EnginePhase::Running;
        state.engine.generation = 2;
        let mut queued = Meter {
            diagnostics_epoch: 0,
            message_type: rc2fx_core::MeterType::Meter,
            revision: state.engine.applied_revision,
            generation: 1,
            synthetic: false,
            targets: vec![meter()],
        };
        assert!(!meter_matches_engine(&state, &queued));
        queued.generation = 2;
        assert!(meter_matches_engine(&state, &queued));
        let original = queued.targets.clone();
        queued.targets.clear();
        assert!(!meter_matches_engine(&state, &queued));
        queued.targets = original.clone();
        queued.targets.push(original[0].clone());
        assert!(!meter_matches_engine(&state, &queued));
        queued.targets = original;
        state.selection.active_target_ids = vec!["usb-main".into()];
        assert!(!meter_matches_engine(&state, &queued));
        state.selection.active_target_ids = vec!["mic-1".into()];
        state.selection.status = rc2fx_core::SelectionPhase::Changing;
        assert!(!meter_matches_engine(&state, &queued));
        state.selection.status = rc2fx_core::SelectionPhase::Idle;
        queued.revision += 1;
        assert!(!meter_matches_engine(&state, &queued));
        queued.revision = state.engine.applied_revision;
        for phase in [
            EnginePhase::Restarting,
            EnginePhase::Starting,
            EnginePhase::Error,
        ] {
            state.engine.status = phase;
            assert!(!meter_matches_engine(&state, &queued));
        }
    }

    #[test]
    fn recovery_requires_a_complete_confirmed_bypass() {
        let mut state = failed_state();
        let restored = "Original audio routes were restored.";
        for phase in [
            RoutingPhase::Unmanaged,
            RoutingPhase::Starting,
            RoutingPhase::Active,
            RoutingPhase::Bypassing,
            RoutingPhase::Error,
        ] {
            state.routing.status = phase;
            state.routing.recovered = state.routing.channels;
            assert_ne!(engine_error(&state).unwrap().recovery_detail, restored);
        }
        state.routing.status = RoutingPhase::Bypassed;
        for (channels, recovered) in [(0, 0), (2, 0), (2, 1), (2, 3)] {
            state.routing.channels = channels;
            state.routing.recovered = recovered;
            assert_ne!(engine_error(&state).unwrap().recovery_detail, restored);
        }
        state.routing.channels = 2;
        state.routing.recovered = 2;
        assert_ne!(engine_error(&state).unwrap().recovery_detail, restored);
        state.routing.channels = 0;
        state.routing.recovered = 0;
        state.selection.active_target_ids.clear();
        assert_ne!(engine_error(&state).unwrap().recovery_detail, restored);
        state.engine.restart_allowed = true;
        assert_eq!(engine_error(&state).unwrap().recovery_detail, restored);
        for mode in [EngineMode::Simulation, EngineMode::Offline] {
            state.engine.mode = mode;
            assert_ne!(engine_error(&state).unwrap().recovery_detail, restored);
        }
    }

    #[test]
    fn terminal_notice_clears_only_with_a_new_nonerror_state() {
        let mut state = failed_state();
        state.engine.message = " ".into();
        assert_eq!(
            engine_error(&state).unwrap().message,
            "The audio engine reported an error without further details."
        );
        for phase in [
            EnginePhase::Starting,
            EnginePhase::Running,
            EnginePhase::Offline,
        ] {
            state.engine.status = phase;
            assert!(engine_error(&state).is_none());
        }
    }

    #[test]
    fn receipt_and_repeated_counters_are_not_audio_progress() {
        let now = Instant::now();
        let mut progress = Progress::new(100);
        assert!(!progress.fresh(now));
        progress.observe(100, now + Duration::from_millis(50));
        assert!(!progress.fresh(now));
        progress.observe(212, now + Duration::from_millis(100));
        assert!(progress.fresh(now + Duration::from_millis(500)));
        progress.observe(212, now + Duration::from_millis(900));
        assert!(!progress.fresh(now + Duration::from_millis(1100)));
    }
    #[test]
    fn restart_needs_new_progress() {
        let now = Instant::now();
        let mut progress = Progress::new(10_000);
        progress.observe(10_112, now);
        assert!(progress.fresh(now));
        progress.observe(0, now);
        assert!(!progress.fresh(now));
        progress.observe(112, now);
        assert!(progress.fresh(now));
    }

    fn meter() -> TargetMeter {
        serde_json::from_value(serde_json::json!({
            "id":"mic-1", "inputPeak":0.1, "outputPeak":0.1, "rms":0.05,
            "gainReduction":0, "voice":0, "gateOpen":0, "clipped":0, "spectrum":[], "callbacks":100,
            "processedFrames":11200, "suppressionFrames":20, "missedDeadlines":7,
            "staleResults":7, "inputDrops":0, "outputDrops":0,
            "latencySamples":1184, "workerFailed":false
        }))
        .unwrap()
    }

    /// Native telemetry with a full Performance block: the counters the app
    /// now receives must not be mistaken for faults while they only grow
    /// slowly, and a rebuild (a decrease) must merely reset the baseline.
    #[test]
    fn steady_performance_data_raises_no_warning_and_a_reset_is_quiet() {
        use rc2fx_core::{Performance, Timing};
        let timing = |calls: u64, overruns: u64| Timing {
            calls,
            total_ns: calls * 100,
            max_ns: 200,
            overruns,
            clock_errors: 0,
            histogram: [0; 8],
        };
        let performance = |calls: u64, overruns: u64, worker_calls: u64| Performance {
            callback_budget_ns: 2_333_333,
            worker_budget_ns: 9_333_333,
            xruns: 0,
            callback: timing(calls, overruns),
            worker_wall: timing(worker_calls, 0),
            worker_cpu: timing(worker_calls, 0),
            helper_late: Some(0),
            helper_stalls: Some(0),
        };
        let now = Instant::now();
        let mut progress = Progress::new(11200);
        let step = |progress: &mut Progress, calls: u64, frames: u64, performance: Performance| {
            let mut target = TargetMeter {
                callbacks: calls,
                processed_frames: frames,
                missed_deadlines: 0,
                stale_results: 0,
                performance: Some(performance),
                ..meter()
            };
            target.id = "mic-1".into();
            progress.observe_meter((1, 0), &target, now)
        };
        // The first sample is a baseline, not a fault.
        step(&mut progress, 100, 11200, performance(100, 0, 20));
        // Steady growth, same interval: no warning at all.
        step(&mut progress, 200, 22400, performance(200, 0, 40));
        step(&mut progress, 300, 33600, performance(300, 0, 60));
        assert_eq!(progress.warning(now), None);
        // A real new overrun is still reported.
        step(&mut progress, 400, 44800, performance(400, 1, 80));
        assert_eq!(progress.warning(now), Some(Warning::AudioDeadline));
        // A rebuild resets the counters downwards. On its own sample that is a
        // new baseline, not an unsigned fault, and growth within the new
        // baseline is quiet too.
        let mut rebuilt = Progress::new(11200);
        step(&mut rebuilt, 100, 11200, performance(100, 2, 20));
        step(&mut rebuilt, 200, 22400, performance(50, 1, 10));
        assert_eq!(rebuilt.warning(now), None);
        step(&mut rebuilt, 300, 33600, performance(150, 1, 12));
        assert_eq!(rebuilt.warning(now), None);
    }

    fn complete_meter() -> Meter {
        Meter {
            diagnostics_epoch: 0,
            message_type: rc2fx_core::MeterType::Meter,
            revision: 235,
            generation: 5,
            synthetic: false,
            targets: ["mic-1", "usb-main", "send-chat"]
                .into_iter()
                .map(|id| TargetMeter {
                    id: id.into(),
                    ..meter()
                })
                .collect(),
        }
    }

    fn ids(meter: &Meter) -> Vec<String> {
        meter
            .targets
            .iter()
            .map(|target| target.id.clone())
            .collect()
    }

    #[test]
    fn idle_passthrough_paths_do_not_hold_back_active_paths() {
        let start = Instant::now();
        let mut meter = complete_meter();
        let mut progress = HashMap::new();
        let active = vec!["mic-1".to_owned()];
        assert!(!observe_meter_progress(
            &mut progress,
            &meter,
            &active,
            start
        ));
        // Only the active microphone advances; usb-main and send-chat idle.
        meter.targets[0].callbacks += 1;
        meter.targets[0].processed_frames += 112;
        let now = start + Duration::from_millis(100);
        assert!(observe_meter_progress(&mut progress, &meter, &active, now));
        assert!(progress["mic-1"].fresh(now));
        assert!(!progress["usb-main"].fresh(now));
        // A stalled active path still blocks renewal while another advances.
        meter.targets[1].callbacks += 1;
        meter.targets[1].processed_frames += 112;
        let later = now + Duration::from_millis(100);
        assert!(!observe_meter_progress(
            &mut progress,
            &meter,
            &active,
            later
        ));
        // With nothing active, any advancing path confirms the engine.
        meter.targets[2].callbacks += 1;
        meter.targets[2].processed_frames += 112;
        let last = later + Duration::from_millis(100);
        assert!(observe_meter_progress(&mut progress, &meter, &[], last));
    }

    fn running_state() -> DeviceState {
        let mut state = failed_state();
        state.engine.status = EnginePhase::Running;
        state.engine.generation = 5;
        state.engine.applied_revision = 235;
        state.engine.audio_advancing = true;
        state.selection.active_target_ids =
            vec!["mic-1".into(), "usb-main".into(), "send-chat".into()];
        state
    }

    #[test]
    fn native_bound_meters_keep_engine_fresh_after_path_disable_and_global_off() {
        for enabled in [false, true] {
            let start = Instant::now();
            let mut state = running_state();
            state.engine.mode = EngineMode::Native;
            state.selection.qualified_target_ids =
                vec!["mic-1".into(), "usb-main".into(), "send-chat".into()];
            state.config.selected_targets = vec!["mic-1".into()];
            state.selection.active_target_ids = if enabled {
                vec!["mic-1".into()]
            } else {
                vec![]
            };
            state.routing.status = if enabled {
                RoutingPhase::Active
            } else {
                RoutingPhase::Bypassed
            };
            let mut current = complete_meter();
            assert!(meter_matches_engine(&state, &current));
            let mut progress = HashMap::new();
            let mut seen = None;
            assert!(receive_meter(
                &state,
                &current,
                &mut progress,
                &mut seen,
                start
            ));
            let now = start + Duration::from_millis(100);
            for target in &mut current.targets {
                target.processed_frames += 4480;
                target.callbacks += 40;
            }
            assert!(receive_meter(
                &state,
                &current,
                &mut progress,
                &mut seen,
                now
            ));
            assert_eq!(seen, Some(now));
            assert!(fresh_meter(&state, &current, &progress, "usb-main", seen, now).is_some());
            let mut missing = current.clone();
            missing.targets.pop();
            assert!(!meter_matches_engine(&state, &missing));
            state.engine.mode = EngineMode::Jack;
            assert!(!meter_matches_engine(&state, &current));
        }
    }

    #[test]
    fn receive_to_usb_meter_render_remains_fresh_past_thirty_seconds() {
        let start = Instant::now();
        let state = running_state();
        let mut progress = HashMap::new();
        let mut engine_seen = Some(start);
        let mut current = complete_meter();
        assert!(receive_meter(
            &state,
            &current,
            &mut progress,
            &mut engine_seen,
            start
        ));
        assert!(fresh_meter(&state, &current, &progress, "usb-main", engine_seen, start).is_none());
        for n in 1..=700 {
            let now = start + Duration::from_millis(n * 100);
            for target in &mut current.targets {
                target.callbacks += 40;
                target.processed_frames += 4480;
            }
            assert!(receive_meter(
                &state,
                &current,
                &mut progress,
                &mut engine_seen,
                now
            ));
            let displayed =
                fresh_meter(&state, &current, &progress, "usb-main", engine_seen, now).unwrap();
            assert_eq!(displayed.0.id, "usb-main");
            assert!(!displayed.1);
        }
        let end = start + Duration::from_secs(70);
        assert!(
            fresh_meter(&state, &current, &progress, "usb-main", Some(start), end).is_none(),
            "the original stale engine timestamp reproduces the missing meters"
        );
        let observed = engine_seen;
        for case in [
            "old-generation",
            "future-generation",
            "old-revision",
            "future-revision",
            "wrong-target",
            "invalid-meter",
        ] {
            let mut bad = current.clone();
            match case {
                "old-generation" => bad.generation -= 1,
                "future-generation" => bad.generation += 1,
                "old-revision" => bad.revision -= 1,
                "future-revision" => bad.revision += 1,
                "wrong-target" => bad.targets[1].id = "usb-chat".into(),
                "invalid-meter" => bad.targets[1].rms = -1.0,
                _ => unreachable!(),
            }
            assert!(
                !receive_meter(
                    &state,
                    &bad,
                    &mut progress,
                    &mut engine_seen,
                    end + Duration::from_millis(100)
                ),
                "{case}"
            );
            assert_eq!(engine_seen, observed);
        }
        assert!(receive_meter(
            &state,
            &current,
            &mut progress,
            &mut engine_seen,
            end + Duration::from_millis(200)
        ));
        assert_eq!(
            engine_seen, observed,
            "repeated receipt must not renew observation"
        );
        assert!(
            fresh_meter(
                &state,
                &current,
                &progress,
                "usb-main",
                engine_seen,
                end + Duration::from_secs(1)
            )
            .is_none(),
            "silence expires the visible meter"
        );
    }

    #[test]
    #[ignore = "requires explicit locally saved device state and diagnostics; never contacts the mixer"]
    fn saved_device_meters_follow_the_desktop_receive_and_render_path() {
        let state_text =
            std::fs::read_to_string(std::env::var("RC2FX_TEST_STATE").unwrap()).unwrap();
        let state = DeviceState::from_json(&state_text).unwrap();
        let report: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("RC2FX_TEST_DIAGNOSTICS").unwrap()).unwrap(),
        )
        .unwrap();
        let meters: Vec<Meter> = report["samples"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|sample| sample.get("telemetry")?.get("meter"))
            .map(|raw| rc2fx_core::from_json::<Meter>(&raw.to_string()).unwrap())
            .filter(|meter| {
                meter.generation == state.engine.generation
                    && meter.revision == state.engine.applied_revision
            })
            .collect();
        assert!(
            meters.len() > 61,
            "need more than30seconds of unchanged saved-state replay"
        );
        let start = Instant::now();
        let mut progress = HashMap::new();
        let mut engine_seen = Some(start);
        for (n, meter) in meters.iter().enumerate() {
            // Replay real serialized payloads at a deterministic desktop receipt
            // cadence; this tests acceptance/display, not target timing.
            let now = start + Duration::from_millis(n as u64 * 500);
            assert!(
                receive_meter(&state, meter, &mut progress, &mut engine_seen, now),
                "rejected real meter{n}"
            );
            if n > 0 {
                let displayed = fresh_meter(&state, meter, &progress, "usb-main", engine_seen, now)
                    .expect("actual USBmain telemetry should render");
                assert_eq!(displayed.0.id, "usb-main");
            }
        }
        println!(
            "Accepted and rendered {} actual full meters at generation{} revision{}; unchanged-state replay{}ms",
            meters.len(),
            state.engine.generation,
            state.engine.applied_revision,
            (meters.len() - 1) * 500
        );
    }

    #[test]
    fn continuous_current_meters_keep_unchanged_engine_observation_fresh() {
        let start = Instant::now();
        let mut meter = complete_meter();
        let mut progress = HashMap::new();
        let mut engine_seen = start;
        assert!(!observe_meter_progress(
            &mut progress,
            &meter,
            &ids(&meter),
            start
        ));
        // The service sends no redundant engine/state events. More than twice
        // the UI's 30-second engine window must remain confirmed by real work.
        for n in 1..=700 {
            let now = start + Duration::from_millis(n * 100);
            for target in &mut meter.targets {
                target.callbacks += 40;
                target.processed_frames += 4480;
            }
            assert!(observe_meter_progress(
                &mut progress,
                &meter,
                &ids(&meter),
                now
            ));
            engine_seen = now;
            assert!(progress.values().all(|path| path.fresh(now)));
        }
        assert_eq!(engine_seen.duration_since(start), Duration::from_secs(70));
        // Repeated queued/cached receipts are not new observations, even while
        // the previous sample is still inside the meter freshness interval.
        for n in 1..=310 {
            let now = engine_seen + Duration::from_millis(n * 100);
            assert!(!observe_meter_progress(
                &mut progress,
                &meter,
                &ids(&meter),
                now
            ));
        }
        assert!(
            progress
                .values()
                .all(|path| !path.fresh(engine_seen + Duration::from_secs(31)))
        );
    }

    #[test]
    fn meter_engine_renewal_requires_every_path_and_a_stable_baseline() {
        let start = Instant::now();
        let baseline = complete_meter();
        for case in [
            "stalled-path",
            "frame-only",
            "counter-reset",
            "revision",
            "gap",
            "failed",
        ] {
            let mut progress = HashMap::new();
            let mut next = baseline.clone();
            assert!(!observe_meter_progress(
                &mut progress,
                &next,
                &ids(&next),
                start
            ));
            for target in &mut next.targets {
                target.callbacks += 1;
                target.processed_frames += 112;
            }
            let mut now = start + Duration::from_millis(100);
            match case {
                "stalled-path" => next.targets[1] = baseline.targets[1].clone(),
                "frame-only" => next.targets[1].callbacks = baseline.targets[1].callbacks,
                "counter-reset" => next.targets[1].callbacks = 1,
                "revision" => next.revision += 1,
                "gap" => now = start + Duration::from_secs(1),
                "failed" => next.targets[0].worker_failed = true,
                _ => unreachable!(),
            }
            assert!(
                !observe_meter_progress(&mut progress, &next, &ids(&next), now),
                "{case}"
            );
        }
        let mut empty = baseline;
        empty.targets.clear();
        assert!(!observe_meter_progress(
            &mut HashMap::new(),
            &empty,
            &[],
            start
        ));
    }
    #[test]
    fn old_faults_are_not_new_but_failed_workers_remain_visible() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), None);
        m.processed_frames += 112;
        m.missed_deadlines += 1;
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), Some(Warning::SuppressionFallback));
        p.observe_meter((1, 0), &m, now + Duration::from_secs(4));
        assert_eq!(p.warning(now + Duration::from_secs(5)), None);
        m.worker_failed = true;
        p.observe_meter((1, 0), &m, now + Duration::from_secs(6));
        assert_eq!(
            p.warning(now + Duration::from_secs(60)),
            Some(Warning::WorkerFailed)
        );
        m.worker_failed = false;
        p.observe_meter((2, 0), &m, now + Duration::from_secs(61));
        assert_eq!(p.warning(now + Duration::from_secs(61)), None);
    }
    #[test]
    fn a_diagnostics_reset_is_a_new_baseline_not_a_fault() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.processed_frames += 112;
        m.callbacks += 1;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(50));
        assert!(p.fresh(now + Duration::from_millis(50)));
        // After the reset every counter restarts near zero, the faults
        // included; new ones since then are what count.
        m.processed_frames = 224;
        m.callbacks = 2;
        m.missed_deadlines = 4;
        let reset = now + Duration::from_millis(100);
        p.observe_meter((1, 1), &m, reset);
        assert_eq!(p.warning(reset), None);
        m.processed_frames += 112;
        m.callbacks += 1;
        p.observe_meter((1, 1), &m, reset + Duration::from_millis(50));
        assert!(p.fresh(reset + Duration::from_millis(50)));
        assert_eq!(p.warning(reset + Duration::from_millis(50)), None);
        m.missed_deadlines += 1;
        p.observe_meter((1, 1), &m, reset + Duration::from_millis(100));
        assert_eq!(
            p.warning(reset + Duration::from_millis(100)),
            Some(Warning::SuppressionFallback)
        );
    }

    #[test]
    fn reset_and_revision_change_establish_new_baselines() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.suppression_frames = 0;
        m.missed_deadlines = 1;
        m.stale_results = 0;
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), None);
        m.missed_deadlines = 20;
        p.observe_meter((2, 0), &m, now);
        assert_eq!(p.warning(now), None);
        assert!(!p.fresh(now));
        m.processed_frames = 0;
        m.missed_deadlines = 30;
        p.observe_meter((2, 0), &m, now);
        assert_eq!(p.warning(now), None);
        assert!(!p.fresh(now));
        m.processed_frames = 112;
        m.missed_deadlines += 1;
        p.observe_meter((2, 0), &m, now);
        assert_eq!(p.warning(now), Some(Warning::SuppressionFallback));
    }
    /// A reading `seconds` of audio into `generation`, with `xruns` counted.
    fn reading(generation: u64, epoch: u64, seconds: u64, xruns: u64) -> Meter {
        let mut target = meter();
        target.processed_frames = seconds * 48_000;
        target.performance = Some(serde_json::from_value(serde_json::json!({
            "callbackBudgetNs":2333333, "workerBudgetNs":10000000, "xruns":xruns,
            "callback":{"calls":100,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerWall":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerCpu":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]}
        })).unwrap());
        Meter {
            message_type: rc2fx_core::MeterType::Meter,
            revision: 1,
            generation,
            synthetic: false,
            diagnostics_epoch: epoch,
            targets: vec![target],
        }
    }

    /// The xruns the headline shows for `m`.
    fn headline(window: &StartupWindow, m: &Meter) -> u64 {
        let mut target = m.targets[0].clone();
        window.settle(&mut target);
        target.performance.unwrap().xruns
    }

    #[test]
    fn a_startup_burst_leaves_the_headline_for_the_startup_line() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        let burst = reading(1, 0, 5, 12);
        w.observe(&burst, 48_000, now);
        assert!(w.open());
        assert_eq!(headline(&w, &burst), 0);
        assert_eq!(w.line().as_deref(), Some("12 xruns during startup"));
        let later = reading(1, 0, 40, 12);
        w.observe(&later, 48_000, now);
        assert!(!w.open());
        assert_eq!(headline(&w, &later), 0);
        assert_eq!(w.line().as_deref(), Some("12 xruns during startup"));
    }

    #[test]
    fn an_xrun_after_the_window_counts_normally() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        w.observe(&reading(1, 0, 29, 12), 48_000, now);
        let later = reading(1, 0, 60, 13);
        w.observe(&later, 48_000, now);
        assert_eq!(headline(&w, &later), 1);
        assert_eq!(w.line().as_deref(), Some("12 xruns during startup"));
    }

    #[test]
    fn a_mixer_restart_opens_the_window_again() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        w.observe(&reading(1, 0, 5, 12), 48_000, now);
        w.observe(&reading(1, 0, 60, 13), 48_000, now);
        let restarted = reading(2, 0, 2, 3);
        w.observe(&restarted, 48_000, now);
        assert!(w.open());
        assert_eq!(headline(&w, &restarted), 0);
        assert_eq!(w.line().as_deref(), Some("3 xruns during startup"));
    }

    #[test]
    fn a_diagnostics_reset_is_not_a_restart() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        w.observe(&reading(1, 0, 5, 12), 48_000, now);
        // The frames restart with the counters, but the mixer did not.
        let reset = reading(1, 1, 1, 1);
        w.observe(&reset, 48_000, now);
        assert!(!w.open());
        assert_eq!(headline(&w, &reset), 1);
        assert_eq!(w.line(), None);
        // Reset before this app connected: its age says nothing about startup.
        let mut w = StartupWindow::default();
        w.observe(&reset, 48_000, now);
        assert!(!w.open() && !w.wants_events());
        assert_eq!(headline(&w, &reset), 1);
    }

    #[test]
    fn connecting_after_the_window_places_xruns_from_the_event_log() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        let m = reading(1, 0, 100, 15);
        w.observe(&m, 48_000, now);
        assert!(w.wants_events());
        assert_eq!(headline(&w, &m), 15, "counted until the log says otherwise");
        w.read_events(|from, until| {
            assert_eq!(from, now - Duration::from_secs(100));
            assert_eq!(until, now - Duration::from_secs(70));
            12
        });
        assert!(!w.wants_events());
        assert_eq!(headline(&w, &m), 3);
        assert_eq!(w.line().as_deref(), Some("12 xruns during startup"));
    }

    #[test]
    fn an_unreadable_event_log_leaves_every_fault_in_the_headline() {
        let (now, mut w) = (SystemTime::now(), StartupWindow::default());
        let m = reading(1, 0, 100, 15);
        w.observe(&m, 48_000, now);
        w.give_up();
        assert!(!w.wants_events());
        assert_eq!(headline(&w, &m), 15);
        assert_eq!(w.line(), None);
    }

    #[test]
    fn startup_lines_read_plainly() {
        let faults = StartupFaults {
            xruns: 1,
            overruns: 2,
            helper_late: 0,
            helper_stalls: 1,
        };
        assert_eq!(
            faults.line().as_deref(),
            Some("1 xrun, 2 overruns, 1 helper stall during startup")
        );
        assert_eq!(StartupFaults::default().line(), None);
    }

    #[test]
    fn a_startup_xrun_does_not_warn_but_other_faults_do() {
        let now = Instant::now();
        let mut m = reading(1, 0, 1, 0).targets.remove(0);
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.performance.as_mut().unwrap().xruns += 1;
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), Some(Warning::AudioDeadline));
        p.forget_startup_deadline();
        assert_eq!(p.warning(now), None);
        m.performance.as_mut().unwrap().worker_cpu.clock_errors += 1;
        p.observe_meter((1, 0), &m, now);
        p.forget_startup_deadline();
        assert_eq!(p.warning(now), Some(Warning::ClockFailure));
    }

    #[test]
    fn optional_performance_is_baselined_and_new_events_are_visible() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.performance = Some(serde_json::from_value(serde_json::json!({
            "callbackBudgetNs":2333333, "workerBudgetNs":10000000, "xruns":4,
            "callback":{"calls":100,"totalNs":100000,"maxNs":1000,"overruns":2,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerWall":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerCpu":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]}
        })).unwrap());
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), None);
        m.performance.as_mut().unwrap().xruns += 1;
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), Some(Warning::AudioDeadline));
        m.performance.as_mut().unwrap().worker_cpu.clock_errors += 1;
        p.observe_meter((1, 0), &m, now);
        assert_eq!(p.warning(now), Some(Warning::ClockFailure));
    }

    #[test]
    fn first_meter_after_a_receipt_gap_is_a_baseline() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.processed_frames += 112;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(50));
        assert!(p.fresh(now + Duration::from_millis(50)));

        let resumed = now + Duration::from_secs(2);
        m.processed_frames += 48_000;
        m.missed_deadlines += 10;
        p.observe_meter((1, 0), &m, resumed);
        assert!(!p.fresh(resumed));
        assert_eq!(p.warning(resumed), None);
        m.processed_frames += 112;
        p.observe_meter((1, 0), &m, resumed + Duration::from_millis(50));
        assert!(p.fresh(resumed + Duration::from_millis(50)));
        assert_eq!(p.warning(resumed + Duration::from_millis(50)), None);
        m.missed_deadlines += 1;
        p.observe_meter((1, 0), &m, resumed + Duration::from_millis(100));
        assert_eq!(
            p.warning(resumed + Duration::from_millis(100)),
            Some(Warning::SuppressionFallback)
        );

        m.worker_failed = true;
        p.observe_meter((1, 0), &m, resumed + Duration::from_secs(2));
        assert_eq!(
            p.warning(resumed + Duration::from_secs(2)),
            Some(Warning::WorkerFailed)
        );
    }

    #[test]
    fn suppression_reset_clears_the_previous_workers_recent_warning() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.processed_frames += 112;
        m.missed_deadlines += 1;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(50));
        assert_eq!(p.warning(now), Some(Warning::SuppressionFallback));

        m.suppression_frames = 0;
        m.missed_deadlines = 0;
        m.stale_results = 0;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(100));
        assert_eq!(p.warning(now + Duration::from_millis(100)), None);
        m.suppression_frames = 1;
        m.missed_deadlines = 1;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(150));
        assert_eq!(
            p.warning(now + Duration::from_millis(150)),
            Some(Warning::SuppressionFallback)
        );
    }

    #[test]
    fn callback_reset_rebaselines_faults_even_if_frames_have_caught_up() {
        let now = Instant::now();
        let mut m = meter();
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.callbacks = 1;
        m.processed_frames += 112;
        m.missed_deadlines += 10;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(50));
        assert_eq!(p.warning(now + Duration::from_millis(50)), None);
        assert!(!p.fresh(now + Duration::from_millis(50)));
        m.callbacks += 1;
        m.processed_frames += 112;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(100));
        assert!(p.fresh(now + Duration::from_millis(100)));
    }

    #[test]
    fn inactive_suppression_does_not_mask_independent_audio_warnings() {
        let now = Instant::now();
        let mut m = meter();
        m.performance = Some(serde_json::from_value(serde_json::json!({
            "callbackBudgetNs":2333333, "workerBudgetNs":10000000, "xruns":0,
            "callback":{"calls":100,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerWall":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]},
            "workerCpu":{"calls":20,"totalNs":100000,"maxNs":1000,"overruns":0,"clockErrors":0,"histogram":[0,0,0,0,0,0,0,0]}
        })).unwrap());
        let mut p = Progress::new(m.processed_frames);
        p.observe_meter((1, 0), &m, now);
        m.worker_failed = true;
        m.performance.as_mut().unwrap().xruns += 1;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(50));
        assert_eq!(p.warning_for(now, true), Some(Warning::WorkerFailed));
        assert_eq!(p.warning_for(now, false), Some(Warning::AudioDeadline));

        // A later suppression-only event must not replace the recent callback
        // warning, even when that suppression event is not relevant to output.
        m.worker_failed = false;
        m.missed_deadlines += 1;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(100));
        assert_eq!(p.warning_for(now, false), Some(Warning::AudioDeadline));
        m.suppression_frames = 0;
        m.missed_deadlines = 0;
        m.stale_results = 0;
        p.observe_meter((1, 0), &m, now + Duration::from_millis(150));
        assert_eq!(p.warning_for(now, false), Some(Warning::AudioDeadline));
        assert_eq!(p.warning_for(now + Duration::from_secs(6), false), None);
    }
}
