//! The Diagnostic events window: the mixer's recent audio events, newest
//! first, in a window of its own. The main window owns the connection and
//! the event log; this window only shows the log and asks for a reset.

use super::*;
use gpui_kit::component::tooltip::Tooltip;
use std::{
    collections::{HashSet, VecDeque},
    time::SystemTime,
};

/// How many events the window keeps.
pub(super) const CAPACITY: usize = 2048;
/// How often an open window asks the mixer for new events.
pub(super) const POLL: Duration = Duration::from_millis(500);

/// The filter groups, in chip order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Group {
    Xruns,
    Helper,
    Timing,
    Fallbacks,
    Drops,
    Stale,
    Other,
}

impl Group {
    pub(super) const ALL: [Self; 7] = [
        Self::Xruns,
        Self::Helper,
        Self::Timing,
        Self::Fallbacks,
        Self::Drops,
        Self::Stale,
        Self::Other,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Xruns => "Xruns",
            Self::Helper => "Helper stalls / late",
            Self::Timing => "Callback timing",
            Self::Fallbacks => "Fallbacks / missed deadlines",
            Self::Drops => "Drops",
            Self::Stale => "Stale results",
            Self::Other => "Other",
        }
    }

    pub(super) fn of(kind: DiagnosticsKind) -> Self {
        use DiagnosticsKind::*;
        match kind {
            Xrun => Self::Xruns,
            HelperBusy | HelperStall | HelperLateFinish => Self::Helper,
            LateEnd | Overrun | LateStart | Gap | CycleSkip | VendorJobLate => Self::Timing,
            MissedDeadline | LateFrame => Self::Fallbacks,
            InputDrops | OutputDrops => Self::Drops,
            StaleResults => Self::Stale,
            UsbGadget | Clipped | Lost | Reset | Other => Self::Other,
        }
    }
}

fn kind_label(kind: DiagnosticsKind) -> &'static str {
    use DiagnosticsKind::*;
    match kind {
        Xrun => "xrun",
        HelperBusy => "helper busy",
        HelperStall => "helper stall",
        HelperLateFinish => "helper late",
        LateEnd => "late end",
        Overrun => "overrun",
        LateStart => "late start",
        Gap => "gap",
        CycleSkip => "cycle skip",
        VendorJobLate => "vendor job late",
        UsbGadget => "USB gadget",
        MissedDeadline => "missed deadline",
        LateFrame => "late frame",
        StaleResults => "stale results",
        InputDrops => "input drops",
        OutputDrops => "output drops",
        Clipped => "clipped",
        Lost => "lost",
        Reset => "reset",
        Other => "other",
    }
}

/// The color of a kind's chip.
fn kind_color(kind: DiagnosticsKind) -> u32 {
    use DiagnosticsKind::*;
    match kind {
        Xrun | Lost | Clipped => DANGER,
        UsbGadget | Reset | Other => MUTED,
        _ => WARN,
    }
}

fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.)
}

/// What JACK's xrun delay does and does not mean.
pub(super) const XRUN_NOTE: &str = "This is JACK’s last backend delay, not a measurement tied to this notification. It does not establish audio loss or audibility.";

/// What an event measured, one phrase per figure, for its expanded row.
/// `period_ns` is the engine's block period, when known. Each kind's
/// `valueNs` measures something different; none is a bare "value".
fn details_lines(
    kind: DiagnosticsKind,
    details: &EventDetails,
    period_ns: Option<u64>,
) -> Vec<String> {
    use DiagnosticsKind::*;
    let into_cycle = |ns: u64| match period_ns {
        Some(period) => format!("{} into the {} cycle", ms(ns), ms(period)),
        None => format!("{} into its cycle", ms(ns)),
    };
    let mut lines = Vec::new();
    if let Some(ns) = details.value_ns {
        lines.push(match kind {
            Xrun => format!("notified {}", into_cycle(ns)),
            HelperBusy => format!("the helper had been running {}", ms(ns)),
            HelperStall => format!("waited {}, then gave up", ms(ns)),
            HelperLateFinish => format!("finished {}", into_cycle(ns)),
            LateEnd => format!("ended {}", into_cycle(ns)),
            LateStart => format!("started {}", into_cycle(ns)),
            Overrun => match period_ns {
                Some(period) => format!("ran {} against a {} period", ms(ns), ms(period)),
                None => format!("ran {}", ms(ns)),
            },
            Gap => format!("callbacks {} apart", ms(ns)),
            CycleSkip => format!("cycle start moved {}", ms(ns)),
            VendorJobLate => format!("finished {} after the next callback started", ms(ns)),
            UsbGadget if ns == 0 => return lines,
            _ => format!("measured {}", ms(ns)),
        });
    }
    if let Some(us) = details.delayed_us {
        lines.push(format!("last backend delay reported by JACK: {us} µs"));
    }
    match (details.delta, details.interval_ns) {
        (Some(delta), Some(ns)) => lines.push(format!("{delta} more in the {} before", ms(ns))),
        (Some(delta), None) => lines.push(format!("{delta} more")),
        (None, Some(ns)) => lines.push(format!("over {}", ms(ns))),
        (None, None) => {}
    }
    lines
}

/// The row's sentence. The mixer's own wording is kept except for xruns,
/// whose figures it reads ambiguously.
fn message(kind: DiagnosticsKind, mixer: String) -> String {
    if kind == DiagnosticsKind::Xrun {
        return "JACK xrun notification · impact unknown".into();
    }
    // "the callback ended late" reads as a sentence once capitalised.
    let mut chars = mixer.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => mixer,
    }
}

pub(super) enum Row {
    Event {
        seq: u64,
        kind: DiagnosticsKind,
        /// The path's display name, when the event belongs to one.
        path: Option<String>,
        /// When it happened, on this computer's clock.
        at: SystemTime,
        message: String,
        /// What it measured, one phrase per figure.
        details: Vec<String>,
    },
    /// The mixer dropped events, older than the ones after it, before they
    /// could be read.
    Lost { count: u64 },
}

/// The events read so far, shared by the main window (which fills it) and
/// the events window (which shows it).
pub(super) struct EventLog {
    /// Newest first.
    rows: VecDeque<Row>,
    /// The sequence number to ask for next; None until the first read.
    pub(super) next_seq: Option<u64>,
    hidden: HashSet<Group>,
    expanded: Option<u64>,
    pub(super) connected: bool,
    /// When the counters and events were last reset, if this app saw it.
    pub(super) since: Option<SystemTime>,
    /// The mixer's diagnostics epoch, from its meters: above 0 after a reset.
    pub(super) epoch: u64,
    /// The engine's block period, which cycle positions are measured in.
    pub(super) period_ns: Option<u64>,
    pub(super) confirm_reset: bool,
    pub(super) error: Option<String>,
}

impl EventLog {
    pub(super) fn new() -> Self {
        Self {
            rows: VecDeque::new(),
            next_seq: None,
            hidden: HashSet::new(),
            expanded: None,
            connected: false,
            since: None,
            epoch: 0,
            period_ns: None,
            confirm_reset: false,
            error: None,
        }
    }

    /// Forgets every event, as after a reset or a new connection.
    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.next_seq = None;
        self.expanded = None;
    }

    /// Adds events read in one reply, oldest first, and any the mixer lost
    /// before them.
    pub(super) fn receive(&mut self, lost: u64, events: Vec<Row>) {
        if lost > 0 {
            self.rows.push_front(Row::Lost { count: lost });
        }
        for row in events {
            if let Row::Event {
                kind: DiagnosticsKind::Reset,
                at,
                ..
            } = row
            {
                // The mixer clears its list at a reset; so does the window.
                self.rows.clear();
                self.since = Some(at);
            }
            self.rows.push_front(row);
        }
        self.rows.truncate(CAPACITY);
    }

    /// One page from the mixer, received at `received`; `name` gives a
    /// path's display name. The one place the wire format meets the window.
    pub(super) fn receive_page(
        &mut self,
        page: DiagnosticsEvents,
        received: SystemTime,
        name: impl Fn(&str) -> String,
    ) {
        let rows = page
            .events
            .into_iter()
            .map(|event| {
                let path = event.path.as_deref().map(|id| (id, name(id)));
                // The mixer names paths by id ("3 output drops on mic-1").
                let mixer = match &path {
                    Some((id, name)) => event.message.replace(id, name),
                    None => event.message,
                };
                Row::Event {
                    seq: event.seq,
                    kind: event.kind,
                    message: message(event.kind, mixer),
                    details: details_lines(event.kind, &event.details, self.period_ns),
                    path: path.map(|(_, name)| name),
                    at: host_time(event.at_ns, page.now_ns, received),
                }
            })
            .collect();
        self.receive(page.lost, rows);
        self.next_seq = Some(page.next_seq);
    }

    fn count(&self, group: Group) -> usize {
        self.rows
            .iter()
            .filter(|row| matches!(row, Row::Event { kind, .. } if Group::of(*kind) == group))
            .count()
    }

    fn visible(&self, row: &Row) -> bool {
        match row {
            Row::Event { kind, .. } => !self.hidden.contains(&Group::of(*kind)),
            Row::Lost { .. } => true,
        }
    }
}

/// A mixer time (nanoseconds on its clock) on this computer's clock, given
/// the mixer's clock when the reply was made and when it arrived.
pub(super) fn host_time(at_ns: u64, now_ns: u64, received: SystemTime) -> SystemTime {
    received
        .checked_sub(Duration::from_nanos(now_ns.saturating_sub(at_ns)))
        .unwrap_or(received)
}

fn clock(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(at)
        .format("%H:%M:%S%.3f")
        .to_string()
}

/// "just now", "12 s ago", "4 min ago", "2 h ago".
fn ago(at: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(at).unwrap_or_default().as_secs();
    match seconds {
        0 => "just now".into(),
        1..60 => format!("{seconds} s ago"),
        60..3600 => format!("{} min ago", seconds / 60),
        _ => format!("{} h ago", seconds / 3600),
    }
}

/// "since 20:41:07 (reset)", "since the last reset" (one this app did not
/// see) or "since the engine started"; `epoch` is the meter's diagnostics
/// epoch.
pub(super) fn since_label(since: Option<SystemTime>, epoch: u64) -> String {
    match since {
        Some(at) => format!(
            "since {} (reset)",
            chrono::DateTime::<chrono::Local>::from(at).format("%H:%M:%S")
        ),
        None if epoch > 0 => "since the last reset".into(),
        None => "since the engine started".into(),
    }
}

fn kind_chip(kind: DiagnosticsKind) -> Div {
    let color = kind_color(kind);
    div()
        .px_2()
        .py(px(2.))
        .rounded_md()
        .bg(rgb(if color == DANGER {
            DANGER_BG
        } else {
            surface().raised
        }))
        .text_xs()
        .text_color(rgb(color))
        .truncate()
        .child(kind_label(kind))
}

/// The reset control: a button, then a confirmation in its place.
pub(super) fn reset_control(
    id: &'static str,
    confirming: bool,
    connected: bool,
    ask: impl Fn(&mut App) + 'static,
    cancel: impl Fn(&mut App) + 'static,
    reset: impl Fn(&mut App) + 'static,
) -> Div {
    if confirming {
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Reset counters and events on the mixer?"),
            )
            .child(
                Button::new((id, 0usize))
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(move |_, _, cx| cancel(cx)),
            )
            .child(
                Button::new((id, 1usize))
                    .danger()
                    .small()
                    .label("Reset")
                    .disabled(!connected)
                    .on_click(move |_, _, cx| reset(cx)),
            )
    } else {
        div().child(
            Button::new((id, 2usize))
                .ghost()
                .small()
                .label("Reset diagnostics…")
                .disabled(!connected)
                .on_click(move |_, _, cx| ask(cx)),
        )
    }
}

/// The events window's view.
pub(super) struct EventsView {
    log: Entity<EventLog>,
    /// The main window's app, which sends the reset; None in the preview.
    app: Option<WeakEntity<Rc2FxApp>>,
    _ticks: Task<()>,
}

impl EventsView {
    fn new(
        log: Entity<EventLog>,
        app: Option<WeakEntity<Rc2FxApp>>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&log, |_, _, cx| cx.notify()).detach();
        // "12 s ago" moves on even when nothing new arrives.
        let ticks = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            log,
            app,
            _ticks: ticks,
        }
    }

    fn header(&self, log: &EventLog) -> Div {
        let set_confirm = |log: Entity<EventLog>, value: bool| {
            move |cx: &mut App| {
                log.update(cx, |log, cx| {
                    log.confirm_reset = value;
                    cx.notify();
                })
            }
        };
        let app = self.app.clone();
        let reset = move |cx: &mut App| {
            if let Some(app) = app.as_ref().and_then(WeakEntity::upgrade) {
                app.update(cx, |app, cx| app.reset_diagnostics(cx));
            }
        };
        div()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(div().text_2xl().font_semibold().child("Events"))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                        "{} · newest first · the last {CAPACITY} kept",
                        since_label(log.since, log.epoch)
                    ))),
            )
            .child(reset_control(
                "events-reset",
                log.confirm_reset,
                log.connected && self.app.is_some(),
                set_confirm(self.log.clone(), true),
                set_confirm(self.log.clone(), false),
                reset,
            ))
    }

    fn filters(&self, log: &EventLog) -> Div {
        let mut chips = div().h_flex().flex_wrap().items_center().gap_2();
        for (index, group) in Group::ALL.into_iter().enumerate() {
            let shown = !log.hidden.contains(&group);
            let count = log.count(group);
            let entity = self.log.clone();
            chips = chips.child(
                div()
                    .id(("event-filter", index))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .cursor_pointer()
                    .text_xs()
                    .bg(rgb(if shown {
                        surface().selected
                    } else {
                        surface().bg
                    }))
                    .text_color(rgb(if shown { TEXT } else { FAINT }))
                    .child(group.label())
                    .child(
                        div()
                            .text_color(rgb(if count > 0 && shown { TEXT } else { FAINT }))
                            .child(count.to_string()),
                    )
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |log, cx| {
                            if !log.hidden.remove(&group) {
                                log.hidden.insert(group);
                            }
                            cx.notify();
                        })
                    }),
            );
        }
        chips
    }

    fn table(&self, log: &EventLog, mono: &SharedString) -> Div {
        let now = SystemTime::now();
        let header = div()
            .h_flex()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(rgb(surface().line))
            .text_xs()
            .text_color(rgb(MUTED))
            .child(div().w(px(132.)).flex_shrink_0().child("Time"))
            .child(div().w(px(150.)).flex_shrink_0().child("Kind"))
            .child(div().w(px(140.)).flex_shrink_0().child("Path"))
            .child(div().flex_1().child("Message"));
        let mut rows = Vec::new();
        for row in log.rows.iter().filter(|row| log.visible(row)) {
            rows.push(match row {
                Row::Lost { count } => div()
                    .h_flex()
                    .gap_3()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(surface().line))
                    .bg(rgb(DANGER_BG))
                    .text_sm()
                    .child(div().w(px(132.)).flex_shrink_0())
                    .child(div().flex_1().text_color(rgb(DANGER)).child(format!(
                        "{count} event{} lost before they could be read",
                        if *count == 1 { "" } else { "s" }
                    ))),
                Row::Event {
                    seq,
                    kind,
                    path,
                    at,
                    message,
                    details,
                } => {
                    let seq = *seq;
                    let expanded = log.expanded == Some(seq);
                    let entity = self.log.clone();
                    let mut cell = div()
                        .v_flex()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(surface().line))
                        .child(
                            div()
                                .id(("event", seq))
                                .h_flex()
                                .items_start()
                                .gap_3()
                                .cursor_pointer()
                                .text_sm()
                                .child(time_cell(*at, now, mono))
                                .child(div().w(px(150.)).flex_shrink_0().child(kind_chip(*kind)))
                                .child(
                                    div()
                                        .w(px(140.))
                                        .flex_shrink_0()
                                        .truncate()
                                        .text_color(rgb(if path.is_some() { TEXT } else { FAINT }))
                                        .child(path.clone().unwrap_or_else(|| "-".into())),
                                )
                                .child(div().flex_1().min_w_0().child(message.clone()))
                                .when(!details.is_empty(), |line| {
                                    line.child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(rgb(FAINT))
                                            .child(if expanded { "Hide" } else { "Details" }),
                                    )
                                })
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |log, cx| {
                                        log.expanded = if log.expanded == Some(seq) {
                                            None
                                        } else {
                                            Some(seq)
                                        };
                                        cx.notify();
                                    })
                                }),
                        );
                    if expanded && !details.is_empty() {
                        let mut lines = div()
                            .id(("event-details", seq))
                            .v_flex()
                            .gap_1()
                            .ml(px(132. + 12.))
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .bg(rgb(surface().sunken))
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .children(details.iter().map(|line| div().child(line.clone())));
                        if *kind == DiagnosticsKind::Xrun {
                            lines = lines
                                .tooltip(|window, cx| Tooltip::new(XRUN_NOTE).build(window, cx));
                        }
                        cell = cell.child(lines);
                    }
                    cell
                }
            });
        }
        let empty = rows.is_empty();
        if let Some(last) = rows.pop() {
            rows.push(last.border_b_0());
        }
        div()
            .v_flex()
            .border_t_1()
            .border_color(rgb(surface().line))
            .child(header)
            .children(rows)
            .when(empty, |table| {
                table.child(div().px_4().py_4().text_sm().text_color(rgb(MUTED)).child(
                    if !log.connected {
                        "Connect to the mixer to read its events."
                    } else if log.rows.is_empty() {
                        "No events yet."
                    } else {
                        "No events in the chosen groups."
                    },
                ))
            })
    }
}

fn time_cell(at: SystemTime, now: SystemTime, mono: &SharedString) -> Div {
    div()
        .v_flex()
        .w(px(132.))
        .flex_shrink_0()
        .child(div().font_family(mono.clone()).text_xs().child(clock(at)))
        .child(div().text_xs().text_color(rgb(FAINT)).child(ago(at, now)))
}

impl Render for EventsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = Theme::global(cx).mono_font_family.clone();
        let log = self.log.clone();
        let log = log.read(cx);
        let mut page = div()
            .v_flex()
            .gap_4()
            .child(self.header(log))
            .child(self.filters(log));
        if let Some(error) = &log.error {
            page = page.child(div().text_sm().text_color(rgb(WARN)).child(error.clone()));
        }
        div()
            .id("events-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(surface().bg))
            .text_color(rgb(TEXT))
            .px_6()
            .py_5()
            .child(page.child(self.table(log, &mono)))
    }
}

/// Opens the events window, or brings it forward when it is open already.
pub(super) fn open(
    existing: Option<AnyWindowHandle>,
    log: Entity<EventLog>,
    app: Option<WeakEntity<Rc2FxApp>>,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return Some(handle);
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1040.), px(720.)),
            cx,
        ))),
        window_min_size: Some(size(px(760.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("RC2FX diagnostic events".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |_, cx| {
        cx.new(|cx| EventsView::new(log, app, cx))
    })
    .ok()
    .map(|(handle, _)| handle)
}

/// `rc2fx --telemetry-preview events`: the events window alone, with
/// made-up events. Debug builds only; it opens no connection of any kind.
#[cfg(debug_assertions)]
pub(super) mod preview {
    use super::*;

    /// A page as the mixer would send it, `now_ns` being its clock now.
    fn page(now_ns: u64, lost: u64, events: serde_json::Value) -> DiagnosticsEvents {
        serde_json::from_value(serde_json::json!({
            "nowNs": now_ns, "nextSeq": 45, "lost": lost, "more": false, "events": events,
        }))
        .expect("preview page")
    }

    fn log() -> EventLog {
        const S: u64 = 1_000_000_000;
        let now = SystemTime::now();
        let mut log = EventLog::new();
        log.connected = true;
        log.epoch = 1;
        log.period_ns = Some(2_333_333);
        let name = |id: &str| {
            target_layout::output_label(id)
                .unwrap_or(if id == "mic-1" { "Microphone 1" } else { id })
                .to_owned()
        };
        log.receive_page(
            page(
                5_000 * S,
                0,
                serde_json::json!([
                    {"seq": 1, "kind": "reset", "atNs": 4_155 * S, "message": "Diagnostics reset"},
                    {"seq": 2, "kind": "xrun", "atNs": 4_388 * S,
                     "message": "JACK xrun (jackd reported 0 us of backend delay)",
                     "details": {"valueNs": 688_000, "delayedUs": 0}},
                    {"seq": 3, "kind": "helperLateFinish", "path": "mic-1", "atNs": 4_598 * S,
                     "message": "the helper finished after its frame's cycle",
                     "details": {"valueNs": 2_743_566}},
                    {"seq": 4, "kind": "missedDeadline", "path": "send-chat", "atNs": 4_905 * S,
                     "message": "Suppression missed a frame; the delayed original played"},
                    {"seq": 5, "kind": "lateEnd", "atNs": 4_931 * S,
                     "message": "the callback ended late",
                     "details": {"valueNs": 2_100_000}},
                ]),
            ),
            now - Duration::from_secs(60),
            name,
        );
        log.receive_page(
            page(
                9_000 * S,
                37,
                serde_json::json!([
                    {"seq": 42, "kind": "outputDrops", "path": "mic-1", "atNs": 8_988 * S,
                     "message": "3 output drops on Microphone 1",
                     "details": {"delta": 3, "intervalNs": 500_000_000}},
                    {"seq": 43, "kind": "staleResults", "path": "send-chat", "atNs": 8_996 * S,
                     "message": "1 stale result on Chat Mix Out",
                     "details": {"delta": 1, "intervalNs": 500_000_000}},
                    {"seq": 44, "kind": "helperStall", "atNs": 8_999 * S,
                     "message": "the callback passed its frame through (helper stall)",
                     "details": {"valueNs": 200_000}},
                ]),
            ),
            now,
            name,
        );
        log.expanded = Some(2);
        log
    }

    pub(crate) fn run() {
        gpui_kit::application()
            .with_assets(gpui_kit::assets::Assets)
            .with_quit_mode(QuitMode::LastWindowClosed)
            .run(|cx| {
                gpui_kit::init(cx);
                Theme::change(ThemeMode::Dark, None, cx);
                desk::apply_theme(cx);
                let log = cx.new(|_| log());
                open(None, log, None, cx).expect("Open events preview window");
                cx.activate(true);
            });
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the standard one.
    use super::{EventLog, Group, Row, ago, details_lines, host_time, message};
    use rc2fx_core::{DiagnosticsEvents, DiagnosticsKind, EventDetails};
    use std::time::{Duration, SystemTime};

    fn page(lost: u64, events: serde_json::Value) -> DiagnosticsEvents {
        serde_json::from_value(serde_json::json!({
            "nowNs": 10_000_000_000u64, "nextSeq": 99, "lost": lost, "more": false,
            "events": events,
        }))
        .unwrap()
    }

    fn event(seq: u64, kind: &str) -> serde_json::Value {
        serde_json::json!({"seq": seq, "kind": kind, "atNs": seq, "message": ""})
    }

    fn order(log: &EventLog) -> Vec<i64> {
        log.rows
            .iter()
            .map(|row| match row {
                Row::Event { seq, .. } => *seq as i64,
                Row::Lost { count } => -(*count as i64),
            })
            .collect()
    }

    #[test]
    fn kinds_fall_into_their_groups() {
        use DiagnosticsKind::*;
        assert_eq!(Group::of(Xrun), Group::Xruns);
        assert_eq!(Group::of(HelperStall), Group::Helper);
        assert_eq!(Group::of(HelperLateFinish), Group::Helper);
        assert_eq!(Group::of(Gap), Group::Timing);
        assert_eq!(Group::of(MissedDeadline), Group::Fallbacks);
        assert_eq!(Group::of(LateFrame), Group::Fallbacks);
        assert_eq!(Group::of(OutputDrops), Group::Drops);
        assert_eq!(Group::of(StaleResults), Group::Stale);
        assert_eq!(Group::of(Reset), Group::Other);
    }

    #[test]
    fn pages_arrive_newest_first_capped_with_lost_markers() {
        let mut log = EventLog::new();
        let now = SystemTime::now();
        let name = |id: &str| format!("name of {id}");
        log.receive_page(
            page(
                0,
                serde_json::json!([event(1, "xrun"), event(2, "xrun"), event(3, "xrun")]),
            ),
            now,
            name,
        );
        assert_eq!(log.next_seq, Some(99));
        log.receive_page(
            page(
                5,
                serde_json::json!([{"seq": 9, "kind": "staleResults", "path": "mic-1",
                "atNs": 1, "message": "3 drops on mic-1", "details": {"delta": 3}}]),
            ),
            now,
            name,
        );
        assert_eq!(order(&log), [9, -5, 3, 2, 1]);
        assert_eq!(log.count(Group::Xruns), 3);
        let Row::Event {
            path,
            details,
            message,
            ..
        } = &log.rows[0]
        else {
            unreachable!()
        };
        assert_eq!(path.as_deref(), Some("name of mic-1"));
        assert_eq!(message, "3 drops on name of mic-1");
        assert_eq!(details, &["3 more"]);
        let many: Vec<_> = (10..3000).map(|seq| event(seq, "gap")).collect();
        log.receive_page(page(0, many.into()), now, name);
        assert_eq!(log.rows.len(), super::CAPACITY);
    }

    #[test]
    fn a_reset_clears_the_window_and_dates_its_counters() {
        let mut log = EventLog::new();
        let now = SystemTime::now();
        log.receive_page(
            page(0, serde_json::json!([event(1, "xrun")])),
            now,
            str::to_owned,
        );
        log.receive_page(
            page(0, serde_json::json!([event(2, "reset"), event(3, "gap")])),
            now,
            str::to_owned,
        );
        assert_eq!(order(&log), [3, 2]);
        assert_eq!(
            log.since,
            Some(host_time(2, 10_000_000_000, now)),
            "the reset's own time"
        );
    }

    #[test]
    fn figures_say_what_they_measure() {
        use DiagnosticsKind::*;
        let period = Some(2_333_333);
        let value = |ns| EventDetails {
            value_ns: Some(ns),
            ..EventDetails::default()
        };
        let xrun = |delayed| EventDetails {
            value_ns: Some(688_000),
            delayed_us: Some(delayed),
            ..EventDetails::default()
        };
        assert_eq!(
            details_lines(Xrun, &xrun(0), period),
            [
                "notified 0.69 ms into the 2.33 ms cycle",
                "last backend delay reported by JACK: 0 µs"
            ]
        );
        assert_eq!(
            details_lines(Xrun, &xrun(1840), None),
            [
                "notified 0.69 ms into its cycle",
                "last backend delay reported by JACK: 1840 µs"
            ]
        );
        assert_eq!(
            details_lines(Xrun, &value(688_000), period),
            ["notified 0.69 ms into the 2.33 ms cycle"]
        );
        assert!(details_lines(Xrun, &EventDetails::default(), None).is_empty());
        assert_eq!(
            details_lines(LateEnd, &value(2_100_000), period),
            ["ended 2.10 ms into the 2.33 ms cycle"]
        );
        assert_eq!(
            details_lines(HelperStall, &value(200_000), period),
            ["waited 0.20 ms, then gave up"]
        );
        assert_eq!(
            details_lines(Gap, &value(230_000_000), period),
            ["callbacks 230.00 ms apart"]
        );
        assert!(details_lines(UsbGadget, &value(0), period).is_empty());
        assert_eq!(
            message(LateEnd, "the callback ended late".into()),
            "The callback ended late"
        );
    }

    #[test]
    fn xrun_impact_is_unknown_for_zero_nonzero_and_missing_backend_delay() {
        for delayed in [Some(0), Some(1840), None] {
            let mut log = EventLog::new();
            log.receive_page(
                page(
                    0,
                    serde_json::json!([{
                        "seq": 1, "kind": "xrun", "atNs": 1,
                        "message": "untrusted mixer interpretation",
                        "details": {"delayedUs": delayed}
                    }]),
                ),
                SystemTime::now(),
                str::to_owned,
            );
            let Row::Event {
                message, details, ..
            } = &log.rows[0]
            else {
                panic!("expected an xrun row");
            };
            assert_eq!(message, "JACK xrun notification · impact unknown");
            let expected: Vec<_> = delayed
                .into_iter()
                .map(|us| format!("last backend delay reported by JACK: {us} µs"))
                .collect();
            assert_eq!(details, &expected);
            assert_eq!(log.count(Group::Xruns), 1);
        }
    }

    #[test]
    fn mixer_times_map_onto_the_host_clock() {
        let received = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(
            host_time(7_000_000_000, 10_000_000_000, received),
            received - Duration::from_secs(3)
        );
        // A time after the reply's own clock is shown as the receipt.
        assert_eq!(host_time(11, 10, received), received);
        assert_eq!(
            ago(received - Duration::from_secs(12), received),
            "12 s ago"
        );
        assert_eq!(ago(received, received), "just now");
    }
}
