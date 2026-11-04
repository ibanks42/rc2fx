//! The Settings page's TELEMETRY section, shown when Show telemetry is on:
//! the engine's state and timing, then one block per processed path with
//! fresh meters.
//! Built from a plain snapshot so it renders the same with preview data.

use super::*;
use gpui_kit::component::tooltip::Tooltip;
use settings::{row, section};

/// Everything the section shows, read from the app at render time.
pub(super) struct Snapshot {
    /// The mixer's engine and routing state, once known.
    pub(super) engine: Option<(EngineStatus, RoutingStatus)>,
    /// Why no path has fresh meters, when none has.
    pub(super) notice: Option<&'static str>,
    pub(super) synthetic: bool,
    /// The meters' diagnostics epoch: above 0 after a reset.
    pub(super) epoch: u64,
    pub(super) paths: Vec<PathView>,
    /// Processed paths whose source sends no audio right now.
    pub(super) idle: Vec<String>,
}

pub(super) struct PathView {
    pub(super) name: String,
    pub(super) meter: TargetMeter,
    pub(super) warning: Option<telemetry::Warning>,
}

/// Peak level in dBFS, floored at −120.
fn dbfs(peak: f64) -> f64 {
    20.0 * peak.max(1e-6).log10()
}

fn db_label(peak: f64) -> String {
    if peak <= 1e-6 {
        "silent".into()
    } else {
        format!("{:.1} dB", dbfs(peak))
    }
}

fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.)
}

/// 1234567 as "1,234,567".
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Audio time from a frame count: "43 s", "25 min 43 s", "2 h 05 min".
fn duration(frames: u64, sample_rate: u32) -> String {
    let seconds = frames / u64::from(sample_rate.max(1));
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min {:02} s", seconds / 60, seconds % 60),
        _ => format!("{} h {:02} min", seconds / 3600, seconds / 60 % 60),
    }
}

fn mode_label(mode: EngineMode) -> &'static str {
    match mode {
        EngineMode::Native => "Native",
        EngineMode::Jack => "JACK",
        EngineMode::Simulation => "Simulation",
        EngineMode::Offline => "Offline",
    }
}

fn phase_label(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Starting => "Starting",
        EnginePhase::Restarting => "Restarting",
        EnginePhase::Running => "Running",
        EnginePhase::Offline => "Offline",
        EnginePhase::Error => "Error",
    }
}

fn routing_label(phase: RoutingPhase) -> &'static str {
    match phase {
        RoutingPhase::Unmanaged => "Unmanaged",
        RoutingPhase::Starting => "Connecting",
        RoutingPhase::Active => "Active",
        RoutingPhase::Bypassing => "Restoring stock audio",
        RoutingPhase::Bypassed => "Stock audio",
        RoutingPhase::Error => "Needs attention",
    }
}

/// A label with a quieter explanation under it, for the left of a row.
fn labelled(label: &'static str, detail: impl Into<SharedString>) -> Div {
    div()
        .v_flex()
        .gap_1()
        .child(label)
        .child(div().text_xs().text_color(rgb(MUTED)).child(detail.into()))
}

fn value(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex_shrink_0()
        .text_sm()
        .text_color(rgb(color))
        .child(text.into())
}

/// One counter: quiet at zero, highlighted once it counts something.
fn counter(label: &'static str, count: u64, color: u32) -> Div {
    let chip = div()
        .h_flex()
        .gap_1()
        .px_2()
        .py(px(2.))
        .rounded_md()
        .text_xs()
        .child(label)
        .child(grouped(count));
    if count == 0 {
        chip.text_color(rgb(FAINT))
    } else {
        chip.bg(rgb(surface().raised)).text_color(rgb(color))
    }
}

/// A level bar from −60 to 0 dBFS, red at or over full scale, with its label
/// and value.
fn level(label: &'static str, peak: f64, mono: &SharedString) -> Div {
    let db = dbfs(peak);
    let width = ((db + 60.) / 60.).clamp(0., 1.) as f32;
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .w(px(48.))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(80.))
                .h(px(6.))
                .rounded_full()
                .bg(rgb(surface().sunken))
                .child(
                    div()
                        .h_full()
                        .w(relative(width))
                        .rounded_full()
                        .bg(rgb(if db >= 0. { DANGER } else { ACCENT })),
                ),
        )
        .child(
            div()
                .w(px(72.))
                .flex_shrink_0()
                .text_right()
                .text_xs()
                .font_family(mono.clone())
                .text_color(rgb(if db >= 0. { DANGER } else { TEXT }))
                .child(db_label(peak)),
        )
}

/// "0.41 ms avg · 1.02 ms peak · budget 2.33 ms (18 %)".
/// A timing's peak. After a diagnostics reset (`epoch` above 0) the mixer
/// reports a peak only once one beats the peak from before the reset, so 0
/// with calls is "nothing slower since the reset": "-".
fn peak(timing: &Timing, epoch: u64) -> String {
    if epoch > 0 && timing.max_ns == 0 && timing.calls > 0 {
        "-".into()
    } else {
        ms(timing.max_ns)
    }
}

/// Whether `peak` shows "-" for this timing.
fn unbeaten(timing: &Timing, epoch: u64) -> bool {
    epoch > 0 && timing.max_ns == 0 && timing.calls > 0
}

const UNBEATEN: &str = "Nothing slower than before the reset";

fn timing_summary(timing: &Timing, budget_ns: u64, epoch: u64) -> String {
    let mean_ns = timing.total_ns.checked_div(timing.calls).unwrap_or(0);
    let share = if budget_ns == 0 {
        String::new()
    } else {
        format!(" ({:.0} % avg)", mean_ns as f64 * 100. / budget_ns as f64)
    };
    format!(
        "{} avg · {} peak · budget {}{share}",
        ms(mean_ns),
        peak(timing, epoch),
        ms(budget_ns)
    )
}

/// A timing reading for the right of a row: the times, then the budget and
/// the counters that mean it was missed.
/// `unbeaten` names the reading when its peak is "-", for its explanation.
fn timing_value(
    times: String,
    unbeaten: Option<&'static str>,
    budget: String,
    counters: impl IntoIterator<Item = Div>,
) -> Div {
    let times = match unbeaten {
        Some(id) => div().child(
            value(times, TEXT)
                .id(id)
                .tooltip(|window, cx| Tooltip::new(UNBEATEN).build(window, cx)),
        ),
        None => value(times, TEXT),
    };
    div()
        .v_flex()
        .items_end()
        .gap_1()
        .flex_shrink_0()
        .child(times)
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_1()
                .child(div().pr_1().text_xs().text_color(rgb(MUTED)).child(budget))
                .children(counters),
        )
}

fn not_reported() -> Div {
    value("Not reported by the mixer", FAINT)
}

/// The engine's state, then the timing the mixer reports for it.
fn engine_rows(
    engine: &EngineStatus,
    routing: &RoutingStatus,
    synthetic: bool,
    epoch: u64,
    paths: &[PathView],
) -> Vec<Div> {
    let healthy = engine.status == EnginePhase::Running && engine.audio_advancing;
    let block_ms = f64::from(engine.block_size) * 1000. / f64::from(engine.sample_rate.max(1));
    let mut state = format!(
        "{} · {}",
        mode_label(engine.mode),
        phase_label(engine.status)
    );
    if engine.status == EnginePhase::Running {
        state.push_str(if engine.audio_advancing {
            " · audio advancing"
        } else {
            " · audio stalled"
        });
    }
    let mut rows = vec![row(
        labelled(
            "Engine",
            format!(
                "{:.0} kHz · {}-sample blocks ({block_ms:.2} ms) · generation {} · revision {}",
                f64::from(engine.sample_rate) / 1000.,
                engine.block_size,
                engine.generation,
                engine.applied_revision
            ),
        ),
        value(state, if healthy { ACCENT } else { WARN }),
    )];
    if synthetic {
        rows.push(row(
            labelled("Simulation", "These meters are generated, not measured."),
            value("Synthetic", WARN),
        ));
    }
    let mut routed = routing_label(routing.status).to_owned();
    if routing.channels > 0 {
        routed.push_str(&format!(" · {} channels", routing.channels));
    }
    if routing.recovered > 0 {
        routed.push_str(&format!(" · {} recovered", routing.recovered));
    }
    rows.push(row(
        labelled(
            "Audio routing",
            if routing.message.trim().is_empty() {
                "Where RC2FX sits in the mixer's audio.".to_owned()
            } else {
                routing.message.clone()
            },
        ),
        value(
            routed,
            if routing.status == RoutingPhase::Error {
                DANGER
            } else {
                MUTED
            },
        ),
    ));

    // The audio callback and xruns are shared by every path: one report.
    let performance = paths
        .iter()
        .find_map(|path| path.meter.performance.as_ref());
    rows.push(row(
        labelled(
            "Audio callback",
            "Time spent per audio block, against its deadline.",
        ),
        match performance {
            Some(p) => {
                let mean_ns = p.callback.total_ns / p.callback.calls.max(1);
                timing_value(
                    format!("{} avg · {} peak", ms(mean_ns), peak(&p.callback, epoch)),
                    unbeaten(&p.callback, epoch).then_some("callback-peak"),
                    format!(
                        "budget {} · {:.0} % used on average",
                        ms(p.callback_budget_ns),
                        mean_ns as f64 * 100. / p.callback_budget_ns.max(1) as f64
                    ),
                    [
                        counter("overruns", p.callback.overruns, WARN),
                        counter("clock errors", p.callback.clock_errors, WARN),
                    ],
                )
            }
            None => not_reported(),
        },
    ));
    rows.push(row(
        labelled("Xruns", "Audio blocks the system missed, for all paths."),
        match performance {
            Some(p) => counter("xruns", p.xruns, DANGER),
            None => not_reported(),
        },
    ));
    // The native helper's counters are shared too; JACK has none.
    let helper = paths
        .iter()
        .filter_map(|path| path.meter.performance.as_ref())
        .find(|p| p.helper_late.is_some() || p.helper_stalls.is_some());
    rows.push(row(
        labelled(
            "Helper",
            "Helper work finished after its cycle, and callbacks that found it busy.",
        ),
        match helper {
            Some(p) => div()
                .h_flex()
                .gap_1()
                .child(counter("late", p.helper_late.unwrap_or(0), WARN))
                .child(counter("stalls", p.helper_stalls.unwrap_or(0), WARN)),
            None => not_reported(),
        },
    ));
    // The suppression workers run per path; show the worst of them.
    let workers: Vec<_> = paths
        .iter()
        .filter_map(|path| path.meter.performance.as_ref())
        .filter(|p| p.worker_wall.calls > 0)
        .collect();
    let worker = match workers.iter().max_by_key(|p| p.worker_wall.max_ns) {
        Some(worst) => timing_value(
            format!("{} peak", peak(&worst.worker_wall, epoch)),
            unbeaten(&worst.worker_wall, epoch).then_some("worker-peak"),
            format!("budget {}", ms(worst.worker_budget_ns)),
            [
                counter(
                    "overruns",
                    workers.iter().map(|p| p.worker_wall.overruns).sum(),
                    WARN,
                ),
                counter(
                    "clock errors",
                    workers
                        .iter()
                        .map(|p| p.worker_wall.clock_errors + p.worker_cpu.clock_errors)
                        .sum(),
                    WARN,
                ),
            ],
        ),
        None => not_reported(),
    };
    rows.push(row(
        labelled(
            "Suppression workers",
            "Slowest turnaround of any path's noise suppression.",
        ),
        worker,
    ));
    rows
}

/// One path: name and delay, levels, dynamics, counters and timing.
fn path_block(path: &PathView, sample_rate: u32, epoch: u64, mono: &SharedString) -> Div {
    let meter = &path.meter;
    let delay = if meter.latency_samples > 0 {
        format!(
            "{:.1} ms added delay",
            f64::from(meter.latency_samples) * 1000. / f64::from(sample_rate.max(1))
        )
    } else {
        "No added delay".into()
    };
    let mut block = div()
        .v_flex()
        .gap_3()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(rgb(surface().line))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_semibold()
                        .child(path.name.clone()),
                )
                .child(div().flex_1())
                .child(value(
                    delay,
                    if meter.latency_samples > 0 {
                        TEXT
                    } else {
                        MUTED
                    },
                )),
        );
    if let Some(warning) = path.warning {
        let detail = warning.detail();
        block = block.child(
            div()
                .id(SharedString::from(format!(
                    "telemetry-warning-{}",
                    meter.id
                )))
                .text_xs()
                .text_color(rgb(WARN))
                .child(warning.label())
                .tooltip(move |window, cx| Tooltip::new(detail).build(window, cx)),
        );
    }
    block = block.child(
        div()
            .h_flex()
            .gap_6()
            .child(level("Input", meter.input_peak, mono))
            .child(level("Output", meter.output_peak, mono)),
    );

    let suppression = meter.suppression_frames > 0
        || meter.missed_deadlines > 0
        || meter.stale_results > 0
        || meter.input_drops > 0
        || meter.output_drops > 0
        || meter.worker_failed;
    let mut dynamics = vec![format!("Output RMS {}", db_label(meter.rms))];
    if meter.gain_reduction > 0.05 {
        dynamics.push(format!("gain reduction {:.1} dB", meter.gain_reduction));
    }
    if suppression {
        dynamics.push(format!("voice {:.0} %", meter.voice * 100.));
        dynamics.push(format!("gate {:.0} % open", meter.gate_open * 100.));
    }
    block = block.child(
        div()
            .text_xs()
            .text_color(rgb(MUTED))
            .child(dynamics.join(" · ")),
    );

    let mut counters = div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(counter("clipped", meter.clipped, DANGER));
    if suppression {
        counters = counters
            .child(counter("fallbacks", meter.missed_deadlines, WARN))
            .child(counter("stale results", meter.stale_results, WARN))
            .child(counter("input drops", meter.input_drops, WARN))
            .child(counter("output drops", meter.output_drops, WARN));
    }
    if meter.worker_failed {
        counters = counters.child(
            div()
                .px_2()
                .py(px(2.))
                .rounded_md()
                .bg(rgb(DANGER_BG))
                .text_xs()
                .text_color(rgb(DANGER))
                .child("noise worker failed"),
        );
    }
    block = block.child(counters);

    if let Some(p) = &meter.performance
        && p.worker_wall.calls > 0
    {
        block = block.child(div().text_xs().text_color(rgb(MUTED)).child(format!(
            "Suppression worker {} · CPU {} avg",
            timing_summary(&p.worker_wall, p.worker_budget_ns, epoch),
            ms(p.worker_cpu.total_ns / p.worker_cpu.calls.max(1))
        )));
    }
    let mut totals = vec![
        format!("{} of audio", duration(meter.processed_frames, sample_rate)),
        format!("{} blocks", grouped(meter.callbacks)),
    ];
    if meter.suppression_frames > 0 {
        totals.push(format!(
            "{} suppression frames",
            grouped(meter.suppression_frames)
        ));
    }
    block.child(div().text_xs().text_color(rgb(FAINT)).child(format!(
        "{}: {}",
        if epoch > 0 {
            "Since the reset"
        } else {
            "Since the engine started"
        },
        totals.join(" · ")
    )))
}

/// The TELEMETRY section; `aside` holds its header controls.
pub(super) fn telemetry_section(snapshot: &Snapshot, mono: &SharedString, aside: Div) -> Div {
    let mut rows = Vec::new();
    let sample_rate = snapshot
        .engine
        .as_ref()
        .map_or(48_000, |(engine, _)| engine.sample_rate);
    if let Some((engine, routing)) = &snapshot.engine {
        rows.extend(engine_rows(
            engine,
            routing,
            snapshot.synthetic,
            snapshot.epoch,
            &snapshot.paths,
        ));
    }
    if let Some(notice) = snapshot.notice {
        rows.push(row(value(notice, MUTED), div()));
    }
    for path in &snapshot.paths {
        rows.push(path_block(path, sample_rate, snapshot.epoch, mono));
    }
    if !snapshot.idle.is_empty() {
        rows.push(row(
            labelled("Idle paths", "No audio from the source right now."),
            value(snapshot.idle.join(", "), MUTED),
        ));
    }
    // The panel's border closes the last row.
    if let Some(last) = rows.pop() {
        rows.push(last.border_b_0());
    }
    section("TELEMETRY", aside, div().children(rows))
}

impl Rc2FxApp {
    pub(super) fn telemetry_snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot {
            engine: None,
            notice: None,
            synthetic: false,
            epoch: 0,
            paths: Vec::new(),
            idle: Vec::new(),
        };
        if !self.session.connected {
            snapshot.notice = Some("Connect to the mixer to see its telemetry.");
            return snapshot;
        }
        let Some(state) = self.session.saved.as_ref() else {
            snapshot.notice = Some("Waiting for the mixer's state.");
            return snapshot;
        };
        snapshot.engine = Some((state.engine.clone(), state.routing.clone()));
        snapshot.epoch = self
            .meter
            .as_ref()
            .map_or(0, |meter| meter.diagnostics_epoch);
        if !self.stream_connected {
            snapshot.notice = Some("Connection interrupted · telemetry paused.");
            return snapshot;
        }
        let name = |id: &str| {
            self.session
                .draft
                .iter()
                .find(|target| target.id == id)
                .map_or_else(|| id.to_owned(), |target| self.target_name(target))
        };
        // Stock passthrough paths carry untouched audio: only processed ones.
        for target in self
            .meter
            .iter()
            .flat_map(|meter| &meter.targets)
            .filter(|target| state.config.is_selected(&target.id))
        {
            match self.fresh_meter_for(&target.id) {
                Some((meter, synthetic)) => {
                    snapshot.synthetic |= synthetic;
                    snapshot.paths.push(PathView {
                        name: name(&meter.id),
                        meter: meter.clone(),
                        warning: self.audio_warning_for(&meter.id),
                    });
                }
                None => snapshot.idle.push(name(&target.id)),
            }
        }
        if snapshot.paths.is_empty() {
            snapshot.idle.clear();
            snapshot.notice = Some(if state.engine.status != EnginePhase::Running {
                "The engine is not running, so no path reports audio."
            } else {
                "No processed path has fresh audio telemetry yet."
            });
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the standard one.
    use super::{db_label, duration, grouped, timing_summary};
    use rc2fx_core::Timing;

    #[test]
    fn numbers_read_plainly() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(duration(43 * 48_000, 48_000), "43 s");
        assert_eq!(duration((25 * 60 + 3) * 48_000, 48_000), "25 min 03 s");
        assert_eq!(duration((2 * 3600 + 5 * 60) * 48_000, 48_000), "2 h 05 min");
        assert_eq!(db_label(0.), "silent");
        assert_eq!(db_label(1.), "0.0 dB");
        assert_eq!(db_label(0.1), "-20.0 dB");
    }

    #[test]
    fn timing_states_its_share_of_the_budget() {
        let timing = Timing {
            calls: 4,
            total_ns: 4_000_000,
            max_ns: 2_500_000,
            overruns: 0,
            clock_errors: 0,
            histogram: [0; 8],
        };
        assert_eq!(
            timing_summary(&timing, 2_000_000, 0),
            "1.00 ms avg · 2.50 ms peak · budget 2.00 ms (50 % avg)"
        );
    }
}

/// `rc2fx --telemetry-preview [old]`: the section alone, filled with made-up
/// telemetry, for looking at the layout. Debug builds only; it opens no
/// connection of any kind.
#[cfg(debug_assertions)]
pub(super) mod preview {
    use super::*;

    pub(crate) const FLAG: &str = "--telemetry-preview";

    /// Whether the made-up mixer reports timing (current native) or not (a
    /// mixer from before it did: `old`).
    struct Preview {
        reported: bool,
    }

    fn path(id: &str, latency: u32, worker: bool) -> TargetMeter {
        let timing = |calls: u64, mean_ns: u64, max_ns: u64, overruns: u64| {
            serde_json::json!({
                "calls": calls, "totalNs": calls * mean_ns, "maxNs": max_ns,
                "overruns": overruns, "clockErrors": 0, "histogram": [0, 0, 0, 0, 0, 0, 0, 0],
            })
        };
        TargetMeter {
            id: id.into(),
            input_peak: 0.32,
            output_peak: 0.21,
            rms: 0.04,
            gain_reduction: if worker { 3.4 } else { 0. },
            voice: if worker { 0.91 } else { 0. },
            gate_open: if worker { 1. } else { 0. },
            clipped: 0,
            spectrum: Vec::new(),
            callbacks: 663_214,
            processed_frames: 74_279_968,
            suppression_frames: if worker { 154_747 } else { 0 },
            missed_deadlines: if worker { 3 } else { 0 },
            stale_results: 0,
            input_drops: 0,
            output_drops: 0,
            latency_samples: latency,
            worker_failed: false,
            // Read like the mixer's report, so optional fields may come and go.
            performance: serde_json::from_value(serde_json::json!({
                "callbackBudgetNs": 2_333_333,
                "workerBudgetNs": 10_000_000,
                "xruns": 2,
                "helperLate": 4,
                "helperStalls": 0,
                "callback": timing(663_214, 410_000, 1_020_000, 0),
                // Not beaten since the reset: shown as "-".
                "workerWall": if worker {
                    timing(154_747, 3_100_000, 0, 1)
                } else {
                    timing(0, 0, 0, 0)
                },
                "workerCpu": if worker {
                    timing(154_747, 2_900_000, 6_000_000, 0)
                } else {
                    timing(0, 0, 0, 0)
                },
            }))
            .ok(),
        }
    }

    fn snapshot(reported: bool) -> Snapshot {
        let mut usb = path("usb-main", 0, false);
        usb.output_peak = 1.0;
        usb.clipped = 12;
        let mut chat = path("send-chat", 1184, true);
        chat.input_peak = 0.;
        chat.output_peak = 0.;
        let mut paths = vec![
            PathView {
                name: "Microphone 1".into(),
                meter: path("mic-1", 1184, true),
                warning: Some(telemetry::Warning::SuppressionFallback),
            },
            PathView {
                name: "USB main".into(),
                meter: usb,
                warning: None,
            },
            PathView {
                name: "Chat Mix Out".into(),
                meter: chat,
                warning: None,
            },
        ];
        if !reported {
            for path in &mut paths {
                path.meter.performance = None;
            }
        }
        Snapshot {
            engine: Some((
                EngineStatus {
                    generation: 5,
                    restart_allowed: false,
                    mode: EngineMode::Native,
                    status: EnginePhase::Running,
                    applied_revision: 235,
                    sample_rate: 48_000,
                    block_size: 112,
                    message: String::new(),
                    audio_advancing: true,
                },
                RoutingStatus {
                    status: RoutingPhase::Active,
                    channels: 4,
                    recovered: 0,
                    message: String::new(),
                },
            )),
            notice: None,
            synthetic: false,
            epoch: u64::from(reported),
            paths,
            idle: vec!["Bluetooth".into()],
        }
    }

    impl Render for Preview {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let mono = Theme::global(cx).mono_font_family.clone();
            div()
                .id("telemetry-preview")
                .size_full()
                .overflow_y_scroll()
                .bg(rgb(surface().bg))
                .text_color(rgb(TEXT))
                .px_6()
                .py_5()
                .child(
                    div()
                        .v_flex()
                        .gap_6()
                        .w_full()
                        .max_w(px(760.))
                        .mx_auto()
                        .child(telemetry_section(
                            &snapshot(self.reported),
                            &mono,
                            // The real header's look; nothing here acts.
                            div()
                                .h_flex()
                                .items_center()
                                .gap_3()
                                .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                                    "Live · {}",
                                    events::since_label(
                                        self.reported.then(|| {
                                            std::time::SystemTime::now()
                                                - Duration::from_secs(14 * 60)
                                        }),
                                        u64::from(self.reported),
                                    )
                                )))
                                .child(
                                    Button::new("show-events")
                                        .ghost()
                                        .small()
                                        .label("Show events…"),
                                )
                                .child(events::reset_control(
                                    "telemetry-reset",
                                    false,
                                    true,
                                    |_| {},
                                    |_| {},
                                    |_| {},
                                )),
                        )),
                )
        }
    }

    pub(crate) fn run(reported: bool) {
        gpui_kit::application()
            .with_assets(gpui_kit::assets::Assets)
            .with_quit_mode(QuitMode::LastWindowClosed)
            .run(move |cx| {
                gpui_kit::init(cx);
                Theme::change(ThemeMode::Dark, None, cx);
                desk::apply_theme(cx);
                let options = WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(900.), px(1400.)),
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("RC2FX telemetry preview".into()),
                        ..Default::default()
                    }),
                    ..ui::window_options()
                };
                gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Preview { reported }))
                    .expect("Open telemetry preview window");
                cx.activate(true);
            });
    }
}
