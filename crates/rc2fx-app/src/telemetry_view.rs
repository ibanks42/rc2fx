//! The Settings page's TELEMETRY section, shown when Show telemetry is on.
//! It answers whether the mixer is healthy and, if not, where: a verdict, four
//! figures, any problems, then one line per processed path with fresh meters.
//! Details opens every figure the mixer reports.
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
    /// Faults from the mixer starting, which the figures here leave out.
    pub(super) startup: Option<String>,
    /// Processed paths whose source sends no audio right now.
    pub(super) idle: Vec<String>,
    /// Details is open: every figure, instead of the compact paths.
    pub(super) details: bool,
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
fn level(label: &'static str, widths: (f32, f32), peak: f64, mono: &SharedString) -> Div {
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
                .w(px(widths.0))
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
                .w(px(widths.1))
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

#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict {
    Healthy,
    Attention,
    Fault,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Attention => "Needs attention",
            Self::Fault => "Not processing",
        }
    }

    fn color(self) -> u32 {
        match self {
            Self::Healthy => ACCENT,
            Self::Attention => WARN,
            Self::Fault => DANGER,
        }
    }
}

/// The audio callback and xruns are shared by every path: one report.
fn shared(paths: &[PathView]) -> Option<&Performance> {
    paths
        .iter()
        .find_map(|path| path.meter.performance.as_ref())
}

/// The suppression workers that ran, one per path that has one.
fn workers(paths: &[PathView]) -> Vec<&Performance> {
    paths
        .iter()
        .filter_map(|path| path.meter.performance.as_ref())
        .filter(|p| p.worker_wall.calls > 0)
        .collect()
}

/// `name` for each non-zero count: "3 overruns".
fn counted(counts: &[(u64, &str)]) -> Vec<String> {
    counts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, name)| format!("{} {name}", grouped(*count)))
        .collect()
}

/// What needs looking at and where, beyond the figures above it. Empty when
/// there is nothing.
fn problems(paths: &[PathView]) -> Vec<String> {
    let mut list = Vec::new();
    if let Some(p) = shared(paths) {
        let found = counted(&[
            (p.callback.overruns, "overruns"),
            (p.callback.clock_errors, "clock errors"),
        ]);
        if !found.is_empty() {
            list.push(format!("Audio callback: {}", found.join(", ")));
        }
    }
    for path in paths {
        let meter = &path.meter;
        let worker = meter.performance.as_ref();
        let mut found = Vec::new();
        if meter.worker_failed {
            found.push("noise worker failed".to_owned());
        }
        if let Some(warning) = path.warning {
            found.push(warning.label().to_owned());
        }
        found.extend(counted(&[
            (meter.clipped, "clipped"),
            (meter.missed_deadlines, "fallbacks"),
            (meter.stale_results, "stale results"),
            (meter.input_drops, "input drops"),
            (meter.output_drops, "output drops"),
            (
                worker.map_or(0, |p| p.worker_wall.overruns),
                "worker overruns",
            ),
            (
                worker.map_or(0, |p| {
                    p.worker_wall.clock_errors + p.worker_cpu.clock_errors
                }),
                "worker clock errors",
            ),
        ]));
        if !found.is_empty() {
            list.push(format!("{}: {}", path.name, found.join(", ")));
        }
    }
    list
}

/// Fault when audio isn't being processed; Attention for any count above zero,
/// so nothing is passed over.
fn verdict(engine: &EngineStatus, routing: &RoutingStatus, paths: &[PathView]) -> Verdict {
    let running = engine.status == EnginePhase::Running && engine.audio_advancing;
    if !running
        || routing.status == RoutingPhase::Error
        || paths.iter().any(|path| path.meter.worker_failed)
    {
        return Verdict::Fault;
    }
    let shared_faults = shared(paths).is_some_and(|p| {
        p.xruns > 0 || p.helper_late.unwrap_or(0) > 0 || p.helper_stalls.unwrap_or(0) > 0
    });
    if shared_faults || !problems(paths).is_empty() {
        Verdict::Attention
    } else {
        Verdict::Healthy
    }
}

/// Each figure's name and what it means.
const CALLBACK: (&str, &str) = (
    "Callback load",
    "How much of each audio block's deadline the mixer's audio work uses on average. Near 100 % it risks missing the deadline. The peak is the slowest block.",
);
const XRUNS: (&str, &str) = (
    "Xruns",
    "Audio blocks the system missed, across all paths. This should stay at 0. Any from the mixer starting are listed separately.",
);
const SUPPRESSION: (&str, &str) = (
    "Suppression load",
    "How much of each frame's time budget noise suppression uses on average, for the channel with the slowest peak. Near 100 % it risks running late, and a late frame uses the original audio. This is not how much noise is removed.",
);
const HELPER: (&str, &str) = (
    "Helper",
    "Helper work that finished after its audio cycle, then callbacks that found the helper still busy. Both should stay at 0.",
);

/// One of the four figures.
#[derive(Debug, PartialEq)]
struct Tile {
    label: &'static str,
    help: &'static str,
    value: String,
    caption: String,
    color: u32,
}

/// Share of a budget used on average, and the peak against it.
fn load(timing: &Timing, budget_ns: u64, epoch: u64) -> (String, String) {
    let mean_ns = timing.total_ns.checked_div(timing.calls).unwrap_or(0);
    (
        format!("{:.0} %", mean_ns as f64 * 100. / budget_ns.max(1) as f64),
        format!("peak {} of {}", peak(timing, epoch), ms(budget_ns)),
    )
}

fn tiles(paths: &[PathView], epoch: u64, startup: Option<&str>) -> [Tile; 4] {
    let missing = |(label, help)| Tile {
        label,
        help,
        value: "–".into(),
        caption: "Not reported".into(),
        color: FAINT,
    };
    let tone = |count: u64, color| if count > 0 { color } else { TEXT };
    let Some(p) = shared(paths) else {
        return [CALLBACK, XRUNS, SUPPRESSION, HELPER].map(missing);
    };
    let (value, caption) = load(&p.callback, p.callback_budget_ns, epoch);
    let callback = Tile {
        label: CALLBACK.0,
        help: CALLBACK.1,
        value,
        caption,
        color: tone(p.callback.overruns, WARN),
    };
    let xruns = Tile {
        label: XRUNS.0,
        help: XRUNS.1,
        value: grouped(p.xruns),
        caption: startup.unwrap_or("Audio blocks missed").to_owned(),
        color: tone(p.xruns, DANGER),
    };
    let workers = workers(paths);
    let suppression = match workers.iter().max_by_key(|p| p.worker_wall.max_ns) {
        Some(worst) => {
            let (value, caption) = load(&worst.worker_wall, worst.worker_budget_ns, epoch);
            Tile {
                label: SUPPRESSION.0,
                help: SUPPRESSION.1,
                value,
                caption,
                color: tone(workers.iter().map(|p| p.worker_wall.overruns).sum(), WARN),
            }
        }
        None => Tile {
            caption: "Not running".into(),
            ..missing(SUPPRESSION)
        },
    };
    let helper = match (p.helper_late, p.helper_stalls) {
        (None, None) => missing(HELPER),
        (late, stalls) => {
            let (late, stalls) = (late.unwrap_or(0), stalls.unwrap_or(0));
            Tile {
                label: HELPER.0,
                help: HELPER.1,
                value: format!("{} · {}", grouped(late), grouped(stalls)),
                caption: "Late · stalls".into(),
                color: tone(late + stalls, WARN),
            }
        }
    };
    [callback, xruns, suppression, helper]
}

fn tile(tile: Tile) -> Div {
    div()
        .v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .p_3()
        .rounded_lg()
        .bg(rgb(surface().raised))
        .child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(tile.label),
                )
                .child(ui::info_badge(
                    SharedString::from(format!("telemetry-{}", tile.label)),
                    tile.help,
                )),
        )
        .child(
            div()
                .text_size(px(ui::SUBHEADING_TEXT))
                .font_semibold()
                .text_color(rgb(tile.color))
                .child(tile.value),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(tile.caption),
        )
}

/// The verdict and what it rests on.
fn status_line(engine: &EngineStatus, routing: &RoutingStatus, snapshot: &Snapshot) -> Div {
    let verdict = verdict(engine, routing, &snapshot.paths);
    let mut parts = vec![
        mode_label(engine.mode).to_owned(),
        phase_label(engine.status).to_owned(),
        routing_label(routing.status).to_owned(),
    ];
    if routing.channels > 0 {
        parts.push(format!("{} channels", routing.channels));
    }
    if snapshot.synthetic {
        parts.push("simulated".to_owned());
    }
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(8.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(verdict.color())),
        )
        .child(div().font_semibold().child(verdict.label()))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(parts.join(" · ")),
        )
}

/// One path on one line: its levels and added delay.
fn path_line(path: &PathView, sample_rate: u32, mono: &SharedString) -> Div {
    let meter = &path.meter;
    let delay = if meter.latency_samples > 0 {
        format!(
            "+{:.1} ms",
            f64::from(meter.latency_samples) * 1000. / f64::from(sample_rate.max(1))
        )
    } else {
        "No delay".into()
    };
    div()
        .h_flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .w(px(110.))
                .flex_shrink_0()
                .truncate()
                .text_sm()
                .font_medium()
                .child(path.name.clone()),
        )
        .child(level("In", (20., 60.), meter.input_peak, mono))
        .child(level("Out", (24., 60.), meter.output_peak, mono))
        .child(
            div()
                .w(px(60.))
                .flex_shrink_0()
                .text_right()
                .text_xs()
                .text_color(rgb(if meter.latency_samples > 0 {
                    TEXT
                } else {
                    FAINT
                }))
                .child(delay),
        )
}

/// "Since the engine started: 35 s of audio · 15,292 blocks · 48 kHz, ...".
fn footer(engine: &EngineStatus, paths: &[PathView], epoch: u64) -> String {
    let block_ms = f64::from(engine.block_size) * 1000. / f64::from(engine.sample_rate.max(1));
    let format = format!(
        "{:.0} kHz, {}-sample blocks ({block_ms:.2} ms)",
        f64::from(engine.sample_rate) / 1000.,
        engine.block_size
    );
    let since = if epoch > 0 {
        "Since the reset"
    } else {
        "Since the engine started"
    };
    match paths.iter().max_by_key(|path| path.meter.processed_frames) {
        Some(path) => format!(
            "{since}: {} of audio · {} blocks · {format}",
            duration(path.meter.processed_frames, engine.sample_rate),
            grouped(path.meter.callbacks)
        ),
        None => format,
    }
}

/// The engine's state, then the timing the mixer reports for it.
fn engine_rows(
    engine: &EngineStatus,
    routing: &RoutingStatus,
    synthetic: bool,
    epoch: u64,
    paths: &[PathView],
    startup: Option<&str>,
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

    let performance = shared(paths);
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
        labelled("Xruns", "Audio blocks the system missed, for all paths.").when_some(
            startup,
            |label, line| {
                label.child(
                    div()
                        .text_xs()
                        .text_color(rgb(FAINT))
                        .child(SharedString::from(line.to_owned())),
                )
            },
        ),
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
    let workers = workers(paths);
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
            .child(level("Input", (48., 72.), meter.input_peak, mono))
            .child(level("Output", (48., 72.), meter.output_peak, mono)),
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

/// The TELEMETRY section; `aside` holds its header controls and `toggle` opens
/// or closes Details.
pub(super) fn telemetry_section(
    snapshot: &Snapshot,
    mono: &SharedString,
    aside: Div,
    toggle: impl IntoElement,
) -> Div {
    let sample_rate = snapshot
        .engine
        .as_ref()
        .map_or(48_000, |(engine, _)| engine.sample_rate);
    let mut summary = div().v_flex().gap_3().py_3();
    if let Some((engine, routing)) = &snapshot.engine {
        summary = summary.child(status_line(engine, routing, snapshot));
    }
    if let Some(notice) = snapshot.notice {
        summary = summary.child(value(notice, MUTED));
    }
    if !snapshot.paths.is_empty() {
        summary = summary.child(div().h_flex().gap_2().children(
            tiles(&snapshot.paths, snapshot.epoch, snapshot.startup.as_deref()).map(tile),
        ));
    }
    let problems = problems(&snapshot.paths);
    if !problems.is_empty() {
        summary = summary.child(
            div()
                .v_flex()
                .gap_1()
                .text_xs()
                .text_color(rgb(WARN))
                .children(problems.into_iter().map(|line| div().child(line))),
        );
    }
    let mut body = div().child(summary);
    if snapshot.details {
        let mut rows = Vec::new();
        if let Some((engine, routing)) = &snapshot.engine {
            rows.extend(engine_rows(
                engine,
                routing,
                snapshot.synthetic,
                snapshot.epoch,
                &snapshot.paths,
                snapshot.startup.as_deref(),
            ));
        }
        for path in &snapshot.paths {
            rows.push(path_block(path, sample_rate, snapshot.epoch, mono));
        }
        body = body.child(
            div()
                .border_t_1()
                .border_color(rgb(surface().line))
                .children(rows),
        );
    } else if !snapshot.paths.is_empty() {
        body = body.child(
            div()
                .v_flex()
                .gap_2()
                .py_3()
                .border_t_1()
                .border_color(rgb(surface().line))
                .children(
                    snapshot
                        .paths
                        .iter()
                        .map(|path| path_line(path, sample_rate, mono)),
                ),
        );
    }
    let mut notes = Vec::new();
    if let Some((engine, _)) = &snapshot.engine {
        notes.push(footer(engine, &snapshot.paths, snapshot.epoch));
    }
    if !snapshot.idle.is_empty() {
        notes.push(format!("Idle: {}", snapshot.idle.join(", ")));
    }
    let closing = div()
        .h_flex()
        .items_center()
        .gap_3()
        .py_2()
        .border_t_1()
        .border_color(rgb(surface().line))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(notes.join(" · ")),
        )
        .child(toggle);
    section("TELEMETRY", aside, body.child(closing))
}

impl Rc2FxApp {
    pub(super) fn telemetry_snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot {
            engine: None,
            notice: None,
            synthetic: false,
            epoch: 0,
            paths: Vec::new(),
            startup: None,
            idle: Vec::new(),
            details: self.telemetry_details,
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
                    let mut shown = meter.clone();
                    self.startup.settle(&mut shown);
                    snapshot.paths.push(PathView {
                        name: name(&meter.id),
                        meter: shown,
                        warning: self.audio_warning_for(&meter.id),
                    });
                }
                None => snapshot.idle.push(name(&target.id)),
            }
        }
        snapshot.startup = self.startup.line();
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
    use super::{
        ACCENT, DANGER, EnginePhase, EngineStatus, FAINT, PathView, RoutingPhase, RoutingStatus,
        TEXT, Verdict, WARN, db_label, duration, footer, grouped, problems, tiles, timing_summary,
        verdict,
    };
    use rc2fx_core::{EngineMode, TargetMeter, Timing};

    fn engine() -> EngineStatus {
        EngineStatus {
            generation: 1,
            restart_allowed: false,
            mode: EngineMode::Native,
            status: EnginePhase::Running,
            applied_revision: 1,
            sample_rate: 48_000,
            block_size: 112,
            message: String::new(),
            audio_advancing: true,
        }
    }

    fn routing() -> RoutingStatus {
        serde_json::from_value(serde_json::json!({
            "status": "active", "channels": 4, "recovered": 0, "message": ""
        }))
        .unwrap()
    }

    /// A clean path; `change` makes it less so.
    fn path(change: impl FnOnce(&mut TargetMeter)) -> Vec<PathView> {
        let timing = |calls: u64, mean: u64, peak: u64| {
            serde_json::json!({"calls": calls, "totalNs": calls * mean, "maxNs": peak,
                "overruns": 0, "clockErrors": 0, "histogram": [0, 0, 0, 0, 0, 0, 0, 0]})
        };
        let mut meter: TargetMeter = serde_json::from_value(serde_json::json!({
            "id": "mic-1", "inputPeak": 0.5, "outputPeak": 0.5, "rms": 0.1,
            "gainReduction": 0.0, "voice": 0.0, "gateOpen": 1.0, "clipped": 0,
            "spectrum": [], "callbacks": 15_292, "processedFrames": 1_680_000,
            "suppressionFrames": 0, "missedDeadlines": 0, "staleResults": 0,
            "inputDrops": 0, "outputDrops": 0, "latencySamples": 0, "workerFailed": false,
            "performance": {
                "callbackBudgetNs": 2_000_000, "workerBudgetNs": 10_000_000, "xruns": 0,
                "helperLate": 0, "helperStalls": 0,
                "callback": timing(100, 500_000, 900_000),
                "workerWall": timing(10, 2_000_000, 4_000_000),
                "workerCpu": timing(10, 1_000_000, 2_000_000),
            }
        }))
        .unwrap();
        change(&mut meter);
        vec![PathView {
            name: "Host".into(),
            meter,
            warning: None,
        }]
    }

    #[test]
    fn a_clean_mixer_is_healthy_and_lists_no_problems() {
        let paths = path(|_| {});
        assert_eq!(verdict(&engine(), &routing(), &paths), Verdict::Healthy);
        assert!(problems(&paths).is_empty());
        assert_eq!(Verdict::Healthy.color(), ACCENT);
    }

    #[test]
    fn any_count_above_zero_needs_attention_and_says_where() {
        let paths = path(|m| m.missed_deadlines = 24);
        assert_eq!(verdict(&engine(), &routing(), &paths), Verdict::Attention);
        assert_eq!(problems(&paths), ["Host: 24 fallbacks"]);
        let paths = path(|m| m.performance.as_mut().unwrap().xruns = 1);
        assert_eq!(verdict(&engine(), &routing(), &paths), Verdict::Attention);
        let paths = path(|m| m.performance.as_mut().unwrap().helper_stalls = Some(2));
        assert_eq!(verdict(&engine(), &routing(), &paths), Verdict::Attention);
        let paths = path(|m| m.performance.as_mut().unwrap().callback.overruns = 3);
        assert_eq!(problems(&paths), ["Audio callback: 3 overruns"]);
    }

    #[test]
    fn audio_not_being_processed_is_a_fault() {
        let mut stalled = engine();
        stalled.audio_advancing = false;
        assert_eq!(verdict(&stalled, &routing(), &path(|_| {})), Verdict::Fault);
        let mut stopped = engine();
        stopped.status = EnginePhase::Error;
        assert_eq!(verdict(&stopped, &routing(), &path(|_| {})), Verdict::Fault);
        let mut broken = routing();
        broken.status = RoutingPhase::Error;
        assert_eq!(verdict(&engine(), &broken, &path(|_| {})), Verdict::Fault);
        let paths = path(|m| m.worker_failed = true);
        assert_eq!(verdict(&engine(), &routing(), &paths), Verdict::Fault);
    }

    #[test]
    fn tiles_show_load_against_budget_and_colour_only_real_faults() {
        let [callback, xruns, suppression, helper] = tiles(&path(|_| {}), 0, None);
        assert_eq!(callback.value, "25 %");
        assert_eq!(callback.caption, "peak 0.90 ms of 2.00 ms");
        assert_eq!(callback.color, TEXT);
        assert_eq!((xruns.value.as_str(), xruns.color), ("0", TEXT));
        assert_eq!(suppression.label, "Suppression load");
        assert_eq!(suppression.value, "20 %");
        assert_eq!(suppression.caption, "peak 4.00 ms of 10.00 ms");
        assert_eq!((helper.value.as_str(), helper.color), ("0 · 0", TEXT));
        let paths = path(|m| {
            let p = m.performance.as_mut().unwrap();
            p.xruns = 1234;
            p.helper_late = Some(25);
        });
        let [_, xruns, _, helper] = tiles(&paths, 0, Some("12 xruns during startup"));
        assert_eq!((xruns.value.as_str(), xruns.color), ("1,234", DANGER));
        assert_eq!(xruns.caption, "12 xruns during startup");
        assert_eq!((helper.value.as_str(), helper.color), ("25 · 0", WARN));
    }

    #[test]
    fn every_tile_explains_itself_even_with_nothing_reported() {
        let reported = path(|_| {});
        let silent = path(|m| m.performance = None);
        for paths in [&reported, &silent] {
            assert!(tiles(paths, 0, None).iter().all(|t| !t.help.is_empty()));
        }
    }

    #[test]
    fn tiles_say_when_the_mixer_reports_nothing() {
        let paths = path(|m| m.performance = None);
        let all = tiles(&paths, 0, None);
        assert!(all.iter().all(|t| t.value == "–" && t.color == FAINT));
        let paths = path(|m| {
            let p = m.performance.as_mut().unwrap();
            p.worker_wall.calls = 0;
        });
        assert_eq!(tiles(&paths, 0, None)[2].caption, "Not running");
    }

    #[test]
    fn the_footer_carries_the_totals_once() {
        assert_eq!(
            footer(&engine(), &path(|_| {}), 0),
            "Since the engine started: 35 s of audio · 15,292 blocks · 48 kHz, 112-sample blocks (2.33 ms)"
        );
        assert!(footer(&engine(), &path(|_| {}), 1).starts_with("Since the reset:"));
        assert!(footer(&engine(), &[], 0).starts_with("48 kHz"));
    }

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
            startup: Some("12 xruns during startup".into()),
            idle: vec!["Bluetooth".into()],
            details: !reported,
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
                            Button::new("telemetry-details")
                                .ghost()
                                .small()
                                .label("Details"),
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
