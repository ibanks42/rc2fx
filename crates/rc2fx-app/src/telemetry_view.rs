//! The Diagnostics page's TELEMETRY section, shown when Show telemetry is on.
//! It answers whether RC2FX is keeping up, in words a person reads at a
//! glance: a row of status badges, four yes/no checks, then one line per
//! processed channel. Technical details opens the raw figures, each with an
//! explanation. Only audible problems are flagged, and only while they are
//! recent: older ones become history, and the mixer starting is left out.
//! Built from a plain snapshot so it renders the same with preview data.

use super::*;
use gpui_kit::component::{tag::Tag, tooltip::Tooltip};
use settings::section;
use std::time::SystemTime;

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
    /// Dropouts in the last minute, and when the last one was.
    pub(super) xruns: telemetry::Recent,
    /// Now, for "20 min ago".
    pub(super) now: SystemTime,
    /// Processed paths whose source sends no audio right now.
    pub(super) idle: Vec<String>,
    /// Technical details is open.
    pub(super) details: bool,
}

pub(super) struct PathView {
    pub(super) name: String,
    /// 1 for mono, 2 for stereo; 0 when the saved settings don't say.
    pub(super) channels: u8,
    pub(super) meter: TargetMeter,
    /// Its audible faults in the last minute, and when each was last seen.
    pub(super) recent: telemetry::PathRecent,
}

/// Peak level in dBFS, floored at −120.
fn dbfs(peak: f64) -> f64 {
    20.0 * peak.max(1e-6).log10()
}

/// A level for a column already headed dB: "-14.9", or "silent".
fn db_value(peak: f64) -> String {
    if peak <= 1e-6 {
        "silent".into()
    } else {
        format!("{:.1}", dbfs(peak))
    }
}

/// Milliseconds without a unit, for figures that share one: "0.72".
fn ms_bare(ns: u64) -> String {
    format!("{:.2}", ns as f64 / 1_000_000.)
}

fn ms(ns: u64) -> String {
    format!("{} ms", ms_bare(ns))
}

fn mean_ns(timing: &Timing) -> u64 {
    timing.total_ns.checked_div(timing.calls).unwrap_or(0)
}

/// Average share of a budget used: 0.31 for 31 %.
fn share(timing: &Timing, budget_ns: u64) -> f64 {
    mean_ns(timing) as f64 / budget_ns.max(1) as f64
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

/// "1 stall", "3 stalls".
fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{} {}", grouped(count), if count == 1 { one } else { many })
}

fn phase_label(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Starting => "Starting",
        EnginePhase::Restarting => "Restarting",
        EnginePhase::Running => "Running",
        EnginePhase::Offline => "Offline",
        EnginePhase::Error => "Engine error",
    }
}

fn routing_label(phase: RoutingPhase) -> &'static str {
    match phase {
        RoutingPhase::Unmanaged => "Unmanaged",
        RoutingPhase::Starting => "Connecting",
        RoutingPhase::Active => "Active",
        RoutingPhase::Bypassing => "Restoring stock audio",
        RoutingPhase::Bypassed => "Stock audio",
        RoutingPhase::Error => "Routing needs attention",
    }
}

fn value(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex_shrink_0()
        .text_sm()
        .text_color(rgb(color))
        .child(text.into())
}

fn dot(color: u32) -> Div {
    div()
        .size(px(8.))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(color))
}

/// What the status badge says.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    Healthy,
    /// Something audible in the last minute, named: "3 dropouts just now".
    Recent(String),
    /// A channel's noise suppression failed; it stays off until a restart.
    Failed(String),
    /// RC2FX's processing has stopped.
    Stopped,
}

impl Status {
    fn label(&self) -> String {
        match self {
            Self::Healthy => "Healthy".into(),
            Self::Recent(what) | Self::Failed(what) => what.clone(),
            Self::Stopped => "RC2FX stopped".into(),
        }
    }

    fn color(&self) -> u32 {
        match self {
            Self::Healthy => ACCENT,
            Self::Recent(_) => WARN,
            Self::Failed(_) | Self::Stopped => DANGER,
        }
    }

    /// Whether a next step, such as a restart, belongs beside it.
    fn needs_action(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Stopped)
    }
}

/// "Mic 4", or "2 channels" when there are several.
fn where_(names: &[&str]) -> String {
    match names {
        [one] => (*one).to_owned(),
        many => format!("{} channels", many.len()),
    }
}

/// The audio callback, xruns and helper are shared by every path: one report.
fn shared(paths: &[PathView]) -> Option<&Performance> {
    paths
        .iter()
        .find_map(|path| path.meter.performance.as_ref())
}

/// The native helper's counters; JACK has none.
fn helper(paths: &[PathView]) -> Option<(u64, u64)> {
    paths
        .iter()
        .filter_map(|path| path.meter.performance.as_ref())
        .find(|p| p.helper_late.is_some() || p.helper_stalls.is_some())
        .map(|p| (p.helper_late.unwrap_or(0), p.helper_stalls.unwrap_or(0)))
}

/// The paths whose suppression worker ran.
fn workers(paths: &[PathView]) -> Vec<&PathView> {
    paths
        .iter()
        .filter(|path| {
            path.meter
                .performance
                .as_ref()
                .is_some_and(|p| p.worker_wall.calls > 0)
        })
        .collect()
}

/// The worker using the most of its budget on average.
fn busiest<'a>(workers: &[&'a PathView]) -> Option<&'a Performance> {
    workers
        .iter()
        .filter_map(|path| path.meter.performance.as_ref())
        .max_by(|a, b| {
            share(&a.worker_wall, a.worker_budget_ns)
                .total_cmp(&share(&b.worker_wall, b.worker_budget_ns))
        })
}

/// Clock time and age: "22:34 (20 min ago)".
fn when(at: SystemTime, now: SystemTime) -> String {
    format!(
        "{} ({})",
        chrono::DateTime::<chrono::Local>::from(at).format("%H:%M"),
        events::ago(at, now)
    )
}

/// What a path's Issues cell says: audible problems in the last minute
/// (a warning), else when the last one was (quiet), else nothing.
fn issues(path: &PathView, now: SystemTime) -> Option<(String, bool)> {
    if path.meter.worker_failed {
        return Some(("Suppression failed".into(), true));
    }
    let r = &path.recent;
    let recent: Vec<String> = [
        (r.late.count, "late"),
        (r.dropped.count, "dropped"),
        (r.clipped.count, "clipped"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, name)| format!("{} {name}", grouped(count)))
    .collect();
    if !recent.is_empty() {
        return Some((recent.join(" · "), true));
    }
    [r.late.last, r.dropped.last, r.clipped.last]
        .into_iter()
        .flatten()
        .max()
        .map(|at| (format!("Last {}", when(at, now)), false))
}

/// A test for one audible problem on a path, and its name.
type Audible = (fn(&PathView) -> bool, &'static str);

/// Stopped when processing isn't running; otherwise the most audible recent
/// problem, by name. Old problems and inaudible counters don't count.
fn status(engine: &EngineStatus, routing: &RoutingStatus, snapshot: &Snapshot) -> Status {
    let running = engine.status == EnginePhase::Running && engine.audio_advancing;
    if !running || routing.status == RoutingPhase::Error {
        return Status::Stopped;
    }
    let paths = &snapshot.paths;
    let named = |hit: fn(&PathView) -> bool| -> Vec<&str> {
        paths
            .iter()
            .filter(|p| hit(p))
            .map(|p| p.name.as_str())
            .collect()
    };
    let failed = named(|p| p.meter.worker_failed);
    if !failed.is_empty() {
        return Status::Failed(format!("Noise suppression failed on {}", where_(&failed)));
    }
    if snapshot.xruns.count > 0 {
        return Status::Recent(format!(
            "{} just now",
            plural(snapshot.xruns.count, "dropout", "dropouts")
        ));
    }
    let audible: [Audible; 3] = [
        (|p| p.recent.late.count > 0, "Late audio"),
        (|p| p.recent.dropped.count > 0, "Dropped audio"),
        (|p| p.recent.clipped.count > 0, "Clipping"),
    ];
    for (hit, what) in audible {
        let names = named(hit);
        if !names.is_empty() {
            return Status::Recent(format!("{what} on {} just now", where_(&names)));
        }
    }
    Status::Healthy
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum CheckState {
    Ok,
    Warn,
    Off,
}

impl CheckState {
    fn color(self) -> u32 {
        match self {
            Self::Ok => ACCENT,
            Self::Warn => WARN,
            Self::Off => FAINT,
        }
    }
}

/// One yes/no question and its answer: "31 % load", then what went wrong
/// in the last minute (a warning), or when it last did (quiet history).
#[derive(Debug, PartialEq)]
struct Check {
    label: &'static str,
    answer: String,
    problem: Option<String>,
    history: Option<String>,
    state: CheckState,
}

impl Check {
    fn ok(label: &'static str, answer: String) -> Self {
        Self {
            label,
            answer,
            problem: None,
            history: None,
            state: CheckState::Ok,
        }
    }

    /// Warns while `problems` is not empty.
    fn new(label: &'static str, answer: String, problems: Vec<String>) -> Self {
        let mut check = Self::ok(label, answer);
        if !problems.is_empty() {
            check.problem = Some(problems.join(" · "));
            check.state = CheckState::Warn;
        }
        check
    }

    fn off(label: &'static str, answer: &str) -> Self {
        Self {
            state: CheckState::Off,
            ..Self::ok(label, answer.to_owned())
        }
    }

    /// Notes when the problem last happened, unless it is happening now.
    fn last(mut self, at: Option<SystemTime>, now: SystemTime) -> Self {
        if self.problem.is_none() {
            self.history = at.map(|at| format!("last {}", when(at, now)));
        }
        self
    }
}

fn load_label(timing: &Timing, budget_ns: u64) -> String {
    format!("{:.0} % load", share(timing, budget_ns) * 100.)
}

/// The four checks, always in this order so the layout never moves. Only
/// audible problems warn, and only while recent.
fn checks(snapshot: &Snapshot) -> [Check; 4] {
    const AUDIO: &str = "Audio keeping up";
    const DROPOUTS: &str = "No dropouts";
    const SUPPRESSION: &str = "Noise suppression on time";
    const HELPER: &str = "Background helper on time";
    let (paths, now) = (&snapshot.paths, snapshot.now);
    let Some(p) = shared(paths) else {
        return [AUDIO, DROPOUTS, SUPPRESSION, HELPER].map(|l| Check::off(l, "Not reported"));
    };
    let audio = Check::ok(AUDIO, load_label(&p.callback, p.callback_budget_ns));
    let xruns = snapshot.xruns;
    let dropouts = if xruns.count > 0 {
        Check::new(
            DROPOUTS,
            plural(xruns.count, "dropout", "dropouts"),
            vec!["in the last minute".into()],
        )
    } else {
        Check::ok(DROPOUTS, "0 dropouts".into()).last(xruns.last, now)
    };
    let workers = workers(paths);
    let suppression = match busiest(&workers) {
        Some(worst) => {
            let mut problems = Vec::new();
            let late: u64 = workers.iter().map(|w| w.recent.late.count).sum();
            let dropped: u64 = workers.iter().map(|w| w.recent.dropped.count).sum();
            if workers.iter().any(|w| w.meter.worker_failed) {
                problems.push("failed".to_owned());
            }
            if late > 0 {
                problems.push(format!("{} late", grouped(late)));
            }
            if dropped > 0 {
                problems.push(format!("{} dropped", grouped(dropped)));
            }
            let last = workers
                .iter()
                .flat_map(|w| [w.recent.late.last, w.recent.dropped.last])
                .flatten()
                .max();
            Check::new(
                SUPPRESSION,
                load_label(&worst.worker_wall, worst.worker_budget_ns),
                problems,
            )
            .last(last, now)
        }
        None => Check::off(SUPPRESSION, "Not running"),
    };
    // Late helper work is not audible on its own: shown, never a warning.
    let helper = match helper(paths) {
        Some((late, _)) => Check::ok(HELPER, format!("{} late", grouped(late))),
        None => Check::off(HELPER, "Not reported"),
    };
    [audio, dropouts, suppression, helper]
}

fn check_row(check: Check, first: bool) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .py_2()
        .when(!first, |row| {
            row.border_t_1().border_color(rgb(surface().line))
        })
        .child(dot(check.state.color()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_medium()
                .child(check.label),
        )
        .child(
            div()
                .h_flex()
                .flex_shrink_0()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .text_color(rgb(if check.state == CheckState::Off {
                            FAINT
                        } else {
                            TEXT
                        }))
                        .child(check.answer),
                )
                .when_some(check.problem, |answer, problem| {
                    answer.child(
                        div()
                            .text_color(rgb(WARN))
                            .child(SharedString::from(format!("· {problem}"))),
                    )
                })
                .when_some(check.history, |answer, history| {
                    answer.child(
                        div()
                            .text_color(rgb(FAINT))
                            .child(SharedString::from(format!("· {history}"))),
                    )
                }),
        )
}

/// A small rounded badge: a quiet name, then its value.
fn badge(name: Option<&'static str>, text: impl Into<SharedString>, color: u32) -> Tag {
    Tag::secondary().small().rounded_full().child(
        div()
            .h_flex()
            .items_center()
            .gap_1()
            .text_xs()
            .when_some(name, |row, name| {
                row.child(div().text_color(rgb(MUTED)).child(name))
            })
            .child(
                div()
                    .font_semibold()
                    .text_color(rgb(color))
                    .child(text.into()),
            ),
    )
}

/// The status, how long audio has run, the format, and anything unusual;
/// `next_step` sits beside a status the user can act on.
fn badges(
    engine: &EngineStatus,
    routing: &RoutingStatus,
    snapshot: &Snapshot,
    next_step: Option<AnyElement>,
) -> Div {
    let status = status(engine, routing, snapshot);
    let block_ms = f64::from(engine.block_size) * 1000. / f64::from(engine.sample_rate.max(1));
    let frames = snapshot
        .paths
        .iter()
        .map(|path| path.meter.processed_frames)
        .max();
    div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(
            Tag::secondary().small().rounded_full().child(
                div()
                    .h_flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(status.color()))
                    .child(dot(status.color()))
                    .child(status.label()),
            ),
        )
        .when_some(next_step.filter(|_| status.needs_action()), |row, step| {
            row.child(step)
        })
        .when(engine.status != EnginePhase::Running, |row| {
            row.child(badge(None, phase_label(engine.status), WARN))
        })
        .when(
            routing.status != RoutingPhase::Active && engine.status == EnginePhase::Running,
            |row| {
                row.child(badge(
                    None,
                    routing_label(routing.status),
                    if routing.status == RoutingPhase::Error {
                        DANGER
                    } else {
                        WARN
                    },
                ))
            },
        )
        .when_some(frames, |row, frames| {
            row.child(badge(
                Some(if snapshot.epoch > 0 {
                    "Since reset"
                } else {
                    "Up"
                }),
                duration(frames, engine.sample_rate),
                TEXT,
            ))
        })
        .child(badge(
            None,
            format!(
                "{:.0} kHz · {block_ms:.2} ms blocks",
                f64::from(engine.sample_rate) / 1000.
            ),
            TEXT,
        ))
        .when(snapshot.synthetic, |row| {
            row.child(badge(None, "Simulated", WARN))
        })
}

/// Column widths of the channel table: name, delay, issues; the two meters
/// share what is left.
const NAME_WIDTH: f32 = 170.;
const DELAY_WIDTH: f32 = 64.;
const ISSUES_WIDTH: f32 = 150.;

fn header_cell(text: &'static str) -> Div {
    div().text_xs().text_color(rgb(FAINT)).child(text)
}

/// A level bar from −60 to 0 dBFS, red at or over full scale, and its value.
fn level(peak: f64, mono: &SharedString) -> Div {
    let db = dbfs(peak);
    let width = ((db + 60.) / 60.).clamp(0., 1.) as f32;
    let color = if db >= 0. { DANGER } else { ACCENT };
    div()
        .h_flex()
        .items_center()
        .gap_2()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .flex_1()
                .min_w(px(48.))
                .h(px(6.))
                .rounded_full()
                .bg(rgb(surface().sunken))
                .child(
                    div()
                        .h_full()
                        .w(relative(width))
                        .rounded_full()
                        .bg(rgb(color)),
                ),
        )
        .child(
            div()
                .w(px(44.))
                .flex_shrink_0()
                .text_right()
                .text_xs()
                .font_family(mono.clone())
                .text_color(rgb(if db >= 0. { DANGER } else { TEXT }))
                .child(db_value(peak)),
        )
}

/// "Mono", "Stereo", or "4 ch"; nothing when unknown.
fn format_label(channels: u8) -> Option<String> {
    match channels {
        0 => None,
        1 => Some("Mono".into()),
        2 => Some("Stereo".into()),
        n => Some(format!("{n} ch")),
    }
}

fn channel_row(path: &PathView, sample_rate: u32, now: SystemTime, mono: &SharedString) -> Div {
    let meter = &path.meter;
    let delay = (meter.latency_samples > 0).then(|| {
        format!(
            "{:.1} ms",
            f64::from(meter.latency_samples) * 1000. / f64::from(sample_rate.max(1))
        )
    });
    let issue_cell = match issues(path, now) {
        None => div().child(div().text_color(rgb(FAINT)).child("—")),
        Some((text, recent)) => {
            let text: SharedString = text.into();
            div().child(
                div()
                    .id(SharedString::from(format!("telemetry-issues-{}", meter.id)))
                    .truncate()
                    .text_color(rgb(if recent { WARN } else { FAINT }))
                    .child(text.clone())
                    .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx)),
            )
        }
    };
    div()
        .h_flex()
        .items_center()
        .gap_4()
        .py_2()
        .border_t_1()
        .border_color(rgb(surface().line))
        .child(
            div()
                .w(px(NAME_WIDTH))
                .flex_shrink_0()
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
                .when_some(format_label(path.channels), |cell, label| {
                    cell.child(Tag::secondary().outline().small().child(label))
                }),
        )
        .child(level(meter.input_peak, mono))
        .child(level(meter.output_peak, mono))
        .child(
            div()
                .w(px(DELAY_WIDTH))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(if delay.is_some() { TEXT } else { FAINT }))
                .child(delay.unwrap_or_else(|| "—".into())),
        )
        .child(
            div()
                .w(px(ISSUES_WIDTH))
                .flex_shrink_0()
                .h_flex()
                .justify_end()
                .text_xs()
                .child(issue_cell),
        )
}

fn channel_table(snapshot: &Snapshot, sample_rate: u32, mono: &SharedString) -> Div {
    div()
        .v_flex()
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_4()
                .pb_1()
                .child(header_cell("Channel").w(px(NAME_WIDTH)).flex_shrink_0())
                .child(header_cell("Input dB").flex_1())
                .child(header_cell("Output dB").flex_1())
                .child(header_cell("Delay").w(px(DELAY_WIDTH)).flex_shrink_0())
                .child(
                    header_cell("Issues")
                        .w(px(ISSUES_WIDTH))
                        .flex_shrink_0()
                        .text_right(),
                ),
        )
        .children(
            snapshot
                .paths
                .iter()
                .map(|path| channel_row(path, sample_rate, snapshot.now, mono)),
        )
}

/// One technical figure: its name, value, and what it means.
#[derive(Debug, PartialEq)]
struct Fact {
    label: &'static str,
    value: String,
    help: &'static str,
}

const NOT_REPORTED: &str = "Not reported";

/// Every figure behind the checks, raw. Startup faults are already left out.
fn facts(engine: &EngineStatus, paths: &[PathView]) -> Vec<Fact> {
    let fact = |label, value: String, help| Fact { label, value, help };
    let pair = |a: u64, b: u64| format!("{} · {}", grouped(a), grouped(b));
    let recent = |ns: Option<u64>| ns.map_or_else(|| "–".to_owned(), ms_bare);
    let block_ms = f64::from(engine.block_size) * 1000. / f64::from(engine.sample_rate.max(1));
    let p = shared(paths);
    let workers = workers(paths);
    let worst = busiest(&workers);
    let sum = |f: fn(&TargetMeter) -> u64| workers.iter().map(|w| f(&w.meter)).sum::<u64>();
    let suppression = |value: Option<String>| {
        value.unwrap_or_else(|| {
            if p.is_some() {
                "Not running"
            } else {
                NOT_REPORTED
            }
            .to_owned()
        })
    };
    vec![
        fact(
            "Sample rate · block",
            format!(
                "{:.0} kHz · {} ({block_ms:.2} ms)",
                f64::from(engine.sample_rate) / 1000.,
                engine.block_size
            ),
            "How often RC2FX gets audio from the mixer: each block holds this many samples. The block's length is the deadline for processing it.",
        ),
        fact(
            "Callback avg · worst",
            p.map_or_else(
                || NOT_REPORTED.to_owned(),
                |p| {
                    format!(
                        "{} · {} ms",
                        ms_bare(mean_ns(&p.callback)),
                        recent(p.callback_recent_max_ns)
                    )
                },
            ),
            "Time RC2FX's audio work takes for one block. The average covers everything since the engine started or the last reset; worst is the slowest block in the last 10 seconds. Both must stay under the block's length.",
        ),
        fact(
            "Callback overruns · clock errors",
            p.map_or_else(
                || NOT_REPORTED.to_owned(),
                |p| pair(p.callback.overruns, p.callback.clock_errors),
            ),
            "Overruns: blocks whose work took longer than the block's length. Clock errors: times the clock could not be read, so a block went unmeasured. Both should stay at 0.",
        ),
        fact(
            "Suppression avg · worst · limit",
            suppression(worst.map(|w| {
                format!(
                    "{} · {} · {}",
                    ms_bare(mean_ns(&w.worker_wall)),
                    recent(w.worker_recent_max_ns),
                    ms(w.worker_budget_ns)
                )
            })),
            "Time noise suppression takes to return one frame, for the busiest channel. The average covers everything since the engine started or the last reset; worst is the slowest frame in the last 10 seconds. A frame must come back within the limit, or it plays unprocessed.",
        ),
        fact(
            "Suppression CPU avg",
            suppression(worst.map(|w| ms(mean_ns(&w.worker_cpu)))),
            "Processor time noise suppression actually used per frame, for the busiest channel. Lower than the turnaround above because the worker also waits its turn for the processor.",
        ),
        fact(
            "Suppression late · stale",
            suppression(worst.map(|_| pair(sum(|m| m.missed_deadlines), sum(|m| m.stale_results)))),
            "Late: frames whose result was not ready in time, so the original audio played. Stale: results that came back after their turn and were thrown away. All channels together.",
        ),
        fact(
            "Suppression drops in · out",
            suppression(worst.map(|_| pair(sum(|m| m.input_drops), sum(|m| m.output_drops)))),
            "Frames lost on the way to the suppression worker (in) or on the way back (out) because its queue was full. All channels together. Should stay at 0.",
        ),
        fact(
            "Helper late · stalls",
            helper(paths).map_or_else(
                || NOT_REPORTED.to_owned(),
                |(late, stalls)| pair(late, stalls),
            ),
            "The helper does work that can't happen inside the audio callback. Late: helper work that finished after its cycle. Stalls: callbacks that found the helper still busy. Both should stay at 0.",
        ),
        fact(
            "Blocks · suppression frames",
            pair(
                paths.iter().map(|p| p.meter.callbacks).max().unwrap_or(0),
                paths.iter().map(|p| p.meter.suppression_frames).sum(),
            ),
            "How many audio blocks, and noise suppression frames across all channels, have been processed since the engine started or the last reset.",
        ),
        fact(
            "Generation · revision",
            pair(engine.generation, engine.applied_revision),
            "Generation goes up each time the audio engine starts or restarts. Revision is the version of your settings the engine is running. Useful when reporting a problem.",
        ),
    ]
}

/// One figure; hovering anywhere on the row explains it, so no icon has to
/// follow labels of different lengths.
fn fact_row(fact: Fact, mono: &SharedString) -> Stateful<Div> {
    let help = fact.help;
    div()
        .id(SharedString::from(format!("telemetry-fact-{}", fact.label)))
        .h_flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_2()
        .py_1()
        .rounded_md()
        .hover(|row| row.bg(rgb(surface().raised)))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(rgb(MUTED))
                .text_decoration_1()
                .text_decoration_solid()
                .text_decoration_color(rgb(surface().line_strong))
                .child(fact.label),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .font_family(mono.clone())
                .text_color(rgb(TEXT))
                .child(fact.value),
        )
        .tooltip(move |window, cx| {
            Tooltip::element(move |_, _| div().w(px(ui::INFO_WIDTH)).py_1().child(help))
                .build(window, cx)
        })
}

/// The facts in two columns, reading down the left first.
fn fact_grid(facts: Vec<Fact>, mono: &SharedString) -> Div {
    let half = facts.len().div_ceil(2);
    let mut left = div().v_flex().flex_1().min_w_0();
    let mut right = div().v_flex().flex_1().min_w_0();
    for (index, fact) in facts.into_iter().enumerate() {
        if index < half {
            left = left.child(fact_row(fact, mono));
        } else {
            right = right.child(fact_row(fact, mono));
        }
    }
    div()
        .h_flex()
        .items_start()
        .gap_6()
        .child(left)
        .child(right)
}

/// The TELEMETRY section; `aside` holds its header controls, `toggle` opens
/// or closes Technical details, and `next_step` (such as a restart) shows
/// beside a status the user can act on.
pub(super) fn telemetry_section(
    snapshot: &Snapshot,
    mono: &SharedString,
    aside: Div,
    toggle: impl IntoElement,
    next_step: Option<AnyElement>,
) -> Div {
    let sample_rate = snapshot
        .engine
        .as_ref()
        .map_or(48_000, |(engine, _)| engine.sample_rate);
    let mut body = div().v_flex().gap_3().py_3();
    if let Some((engine, routing)) = &snapshot.engine {
        body = body.child(badges(engine, routing, snapshot, next_step));
    }
    if let Some(notice) = snapshot.notice {
        body = body.child(value(notice, MUTED));
    }
    if !snapshot.paths.is_empty() {
        let [first, rest @ ..] = checks(snapshot);
        body = body
            .child(
                div()
                    .v_flex()
                    .px_4()
                    .py_1()
                    .rounded_lg()
                    .bg(rgb(surface().raised))
                    .child(check_row(first, true))
                    .children(rest.into_iter().map(|check| check_row(check, false))),
            )
            .child(
                div()
                    .pt_2()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(MUTED))
                    .child("Channels"),
            )
            .child(channel_table(snapshot, sample_rate, mono));
    }
    if !snapshot.idle.is_empty() {
        body = body.child(
            div()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(format!("No audio right now: {}", snapshot.idle.join(", "))),
        );
    }
    let mut closing = div()
        .v_flex()
        .gap_2()
        .py_2()
        .border_t_1()
        .border_color(rgb(surface().line))
        .child(div().h_flex().child(toggle));
    if snapshot.details
        && let Some((engine, _)) = &snapshot.engine
    {
        closing = closing.child(fact_grid(facts(engine, &snapshot.paths), mono));
    }
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
            xruns: telemetry::Recent::default(),
            now: SystemTime::now(),
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
        let target = |id: &str| self.session.draft.iter().find(|target| target.id == id);
        let name = |id: &str| target(id).map_or_else(|| id.to_owned(), |t| self.target_name(t));
        let channels = |id: &str| target(id).map_or(0, |t| t.channels);
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
                        channels: channels(&meter.id),
                        meter: shown,
                        recent: self.recent_faults.path(&meter.id),
                    });
                }
                None => snapshot.idle.push(name(&target.id)),
            }
        }
        snapshot.xruns = self.recent_faults.xruns();
        if snapshot.paths.is_empty() {
            snapshot.idle.clear();
            snapshot.notice = Some(if state.engine.status != EnginePhase::Running {
                "The engine is not running, so no channel reports audio."
            } else {
                "No processed channel has fresh audio telemetry yet."
            });
        }
        snapshot
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the standard one.
    use super::{
        CheckState, EnginePhase, EngineStatus, PathView, RoutingPhase, RoutingStatus, Snapshot,
        Status, checks, db_value, duration, facts, grouped, issues, status,
    };
    use crate::telemetry::{PathRecent, Recent};
    use rc2fx_core::{EngineMode, TargetMeter};
    use std::time::{Duration, SystemTime};

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

    /// A clean path; `change` makes it less so. Its lifetime counters are
    /// not what telemetry warns about: only `recent` is.
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
                "callbackRecentMaxNs": 950_000, "workerRecentMaxNs": 6_260_000,
                "callback": timing(100, 500_000, 192_000_000),
                "workerWall": timing(10, 2_000_000, 4_000_000),
                "workerCpu": timing(10, 1_000_000, 2_000_000),
            }
        }))
        .unwrap();
        change(&mut meter);
        vec![PathView {
            name: "Mic 4".into(),
            channels: 1,
            meter,
            recent: PathRecent::default(),
        }]
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(10_000)
    }

    fn snapshot(paths: Vec<PathView>, xruns: Recent) -> Snapshot {
        Snapshot {
            engine: Some((engine(), routing())),
            notice: None,
            synthetic: false,
            epoch: 0,
            paths,
            xruns,
            now: now(),
            idle: Vec::new(),
            details: false,
        }
    }

    fn state(s: &Snapshot) -> Status {
        status(&engine(), &routing(), s)
    }

    #[test]
    fn a_clean_mixer_is_healthy_and_every_check_passes() {
        let s = snapshot(path(|_| {}), Recent::default());
        assert_eq!(state(&s), Status::Healthy);
        assert_eq!(issues(&s.paths[0], s.now), None);
        let all = checks(&s);
        assert!(
            all.iter()
                .all(|c| c.state == CheckState::Ok && c.history.is_none())
        );
        let answers: Vec<_> = all.iter().map(|c| c.answer.as_str()).collect();
        assert_eq!(answers, ["25 % load", "0 dropouts", "20 % load", "0 late"]);
    }

    #[test]
    fn lifetime_counters_alone_never_warn() {
        // What the mixer counted before this app watched, or inaudible
        // timing: an over-limit frame, overruns, late helper work.
        let s = snapshot(
            path(|m| {
                m.missed_deadlines = 12;
                let p = m.performance.as_mut().unwrap();
                p.xruns = 3;
                p.worker_wall.overruns = 1;
                p.callback.overruns = 4;
                p.helper_late = Some(5);
            }),
            Recent::default(),
        );
        assert_eq!(state(&s), Status::Healthy);
        assert_eq!(issues(&s.paths[0], s.now), None);
        assert!(checks(&s).iter().all(|c| c.state == CheckState::Ok));
    }

    #[test]
    fn a_recent_problem_is_named_in_the_status_and_its_check() {
        let mut paths = path(|_| {});
        paths[0].recent.late = Recent {
            count: 2,
            last: Some(now()),
        };
        let s = snapshot(paths, Recent::default());
        assert_eq!(
            state(&s),
            Status::Recent("Late audio on Mic 4 just now".into())
        );
        let [_, _, suppression, _] = checks(&s);
        assert_eq!(
            (suppression.state, suppression.problem.as_deref()),
            (CheckState::Warn, Some("2 late"))
        );
        assert_eq!(issues(&s.paths[0], s.now), Some(("2 late".into(), true)));
        let s = snapshot(
            path(|_| {}),
            Recent {
                count: 3,
                last: None,
            },
        );
        assert_eq!(state(&s), Status::Recent("3 dropouts just now".into()));
        assert_eq!(checks(&s)[1].problem.as_deref(), Some("in the last minute"));
    }

    #[test]
    fn an_older_problem_is_quiet_history() {
        let earlier = now() - Duration::from_secs(20 * 60);
        let mut paths = path(|_| {});
        paths[0].recent.dropped = Recent {
            count: 0,
            last: Some(earlier),
        };
        let s = snapshot(
            paths,
            Recent {
                count: 0,
                last: Some(earlier),
            },
        );
        assert_eq!(state(&s), Status::Healthy);
        let [_, dropouts, suppression, _] = checks(&s);
        assert_eq!(dropouts.state, CheckState::Ok);
        assert!(dropouts.history.unwrap().ends_with("(20 min ago)"));
        assert!(suppression.history.is_some());
        let (text, recent) = issues(&s.paths[0], s.now).unwrap();
        assert!(text.starts_with("Last ") && !recent);
    }

    #[test]
    fn stopped_processing_and_a_failed_worker_are_red() {
        let s = snapshot(path(|_| {}), Recent::default());
        let mut stalled = engine();
        stalled.audio_advancing = false;
        assert_eq!(status(&stalled, &routing(), &s), Status::Stopped);
        assert_eq!(Status::Stopped.label(), "RC2FX stopped");
        let mut broken = routing();
        broken.status = RoutingPhase::Error;
        assert_eq!(status(&engine(), &broken, &s), Status::Stopped);
        let s = snapshot(path(|m| m.worker_failed = true), Recent::default());
        assert_eq!(
            state(&s),
            Status::Failed("Noise suppression failed on Mic 4".into())
        );
        assert!(state(&s).needs_action() && !Status::Healthy.needs_action());
    }

    #[test]
    fn checks_hold_their_place_when_nothing_is_reported() {
        let silent = snapshot(path(|m| m.performance = None), Recent::default());
        let all = checks(&silent);
        assert!(
            all.iter()
                .all(|c| c.state == CheckState::Off && c.answer == "Not reported")
        );
        let idle = snapshot(
            path(|m| m.performance.as_mut().unwrap().worker_wall.calls = 0),
            Recent::default(),
        );
        assert_eq!(checks(&idle)[2].answer, "Not running");
        let jack = snapshot(
            path(|m| {
                let p = m.performance.as_mut().unwrap();
                p.helper_late = None;
                p.helper_stalls = None;
            }),
            Recent::default(),
        );
        assert_eq!(checks(&jack)[3].state, CheckState::Off);
    }

    #[test]
    fn technical_details_show_recent_worsts_not_the_startup_peak() {
        let all = facts(&engine(), &path(|_| {}));
        let value = |label: &str| {
            all.iter()
                .find(|f| f.label == label)
                .map(|f| f.value.as_str())
                .unwrap()
        };
        assert_eq!(value("Sample rate · block"), "48 kHz · 112 (2.33 ms)");
        assert_eq!(value("Callback avg · worst"), "0.50 · 0.95 ms");
        assert_eq!(
            value("Suppression avg · worst · limit"),
            "2.00 · 6.26 · 10.00 ms"
        );
        assert_eq!(value("Suppression CPU avg"), "1.00 ms");
        assert_eq!(value("Blocks · suppression frames"), "15,292 · 0");
        assert!(all.iter().all(|f| !f.value.contains("192")));
        assert!(all.iter().all(|f| !f.help.is_empty()));
        // A mixer from before recent worsts were reported.
        let old = path(|m| {
            let p = m.performance.as_mut().unwrap();
            p.callback_recent_max_ns = None;
        });
        let all = facts(&engine(), &old);
        assert_eq!(all[1].value, "0.50 · – ms");
    }

    #[test]
    fn numbers_read_plainly() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(duration(43 * 48_000, 48_000), "43 s");
        assert_eq!(duration((25 * 60 + 3) * 48_000, 48_000), "25 min 03 s");
        assert_eq!(duration((2 * 3600 + 5 * 60) * 48_000, 48_000), "2 h 05 min");
        assert_eq!(db_value(0.), "silent");
        assert_eq!(db_value(1.), "0.0");
        assert_eq!(db_value(0.1), "-20.0");
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
        details: bool,
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
                "callbackRecentMaxNs": 640_000,
                "workerRecentMaxNs": if worker { 6_260_000 } else { 0 },
                "callback": timing(663_214, 410_000, 1_020_000, 0),
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

    fn snapshot(reported: bool, details: bool) -> Snapshot {
        let mut usb = path("usb-main", 0, false);
        usb.output_peak = 1.0;
        usb.clipped = 12;
        let mut chat = path("send-chat", 1184, true);
        chat.input_peak = 0.;
        chat.output_peak = 0.;
        let mut paths = vec![
            PathView {
                name: "Microphone 1".into(),
                channels: 1,
                meter: path("mic-1", 1184, true),
                recent: telemetry::PathRecent {
                    late: telemetry::Recent {
                        count: 2,
                        last: Some(SystemTime::now()),
                    },
                    ..Default::default()
                },
            },
            PathView {
                name: "USB main".into(),
                channels: 2,
                meter: usb,
                recent: Default::default(),
            },
            PathView {
                name: "Chat Mix Out".into(),
                channels: 2,
                meter: chat,
                recent: Default::default(),
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
            xruns: telemetry::Recent {
                count: 0,
                last: Some(SystemTime::now() - Duration::from_secs(20 * 60)),
            },
            now: SystemTime::now(),
            idle: vec!["Bluetooth".into()],
            details,
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
                            &snapshot(self.reported, self.details),
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
                                        .outline()
                                        .small()
                                        .icon(gpui_kit::assets::IconName::ExternalLink)
                                        .label("Events"),
                                )
                                .child(events::reset_control(
                                    "telemetry-reset",
                                    "Reset…",
                                    false,
                                    true,
                                    |_| {},
                                    |_| {},
                                    |_| {},
                                )),
                            Switch::new("telemetry-details")
                                .small()
                                .checked(self.details)
                                .label("Technical details")
                                .on_click(cx.listener(|preview, on: &bool, _, cx| {
                                    preview.details = *on;
                                    cx.notify();
                                })),
                            None,
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
                gpui_kit::open_window(options, cx, |_, cx| {
                    cx.new(|_| Preview {
                        reported,
                        details: false,
                    })
                })
                .expect("Open telemetry preview window");
                cx.activate(true);
            });
    }
}
