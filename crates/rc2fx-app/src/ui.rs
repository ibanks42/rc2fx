//! The channel editor's building blocks: status dots,
//! toggles, pill groups, section headings, choice cards and the "Live on the
//! mixer" readouts.

use super::*;
use gpui_kit::component::{
    WindowExt,
    dialog::{DialogAction, DialogClose, DialogFooter},
};
use std::rc::Rc;

/// RC2FX's cyan→violet: only where RC2FX is processing.
pub(super) fn gradient() -> Background {
    linear_gradient(
        90.,
        linear_color_stop(rgb(ACCENT), 0.),
        linear_color_stop(rgb(VIOLET), 1.),
    )
}

/// The draft's dimmest text (`--dim`): off values and empty readouts.
pub(super) const DIM: u32 = 0x4c5468;

/// A status dot: the gradient when on, a hollow ring when off.
pub(super) fn dot(on: bool, size: f32) -> Div {
    let dot = div().size(px(size)).flex_shrink_0().rounded_full();
    if on {
        dot.bg(gradient())
    } else {
        dot.border_1().border_color(rgb(DIM))
    }
}

/// The design's switch: a gradient track with a white knob when on. Callers
/// add the click handler.
pub(super) fn toggle(
    id: impl Into<ElementId>,
    on: bool,
    small: bool,
    disabled: bool,
) -> Stateful<Div> {
    let (w, h, knob) = if small {
        (32., 20., 14.)
    } else {
        (40., 24., 18.)
    };
    div()
        .id(id)
        .relative()
        .w(px(w))
        .h(px(h))
        .flex_shrink_0()
        .rounded_full()
        .when(on, |t| t.bg(gradient()))
        .when(!on, |t| t.bg(rgb(0x262b3a)))
        .when(disabled, |t| t.opacity(0.4))
        .when(!disabled, |t| t.cursor_pointer())
        .child(
            div()
                .absolute()
                .top(px(3.))
                .left(px(if on { w - knob - 3. } else { 3. }))
                .size(px(knob))
                .rounded_full()
                .bg(rgb(if on { 0xffffff } else { 0x7c8494 })),
        )
}

/// A row of pill buttons on a sunken track.
pub(super) fn pills() -> Div {
    div()
        .h_flex()
        .flex_shrink_0()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(10.))
        .bg(rgb(surface().sunken))
        .border_1()
        .border_color(rgb(surface().line))
}

/// One pill in `pills()`. Callers add the click handler.
pub(super) fn pill(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .py(px(6.))
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(px(12.5))
        .whitespace_nowrap()
        .text_color(rgb(if on { TEXT } else { MUTED }))
        .when(on, |p| p.bg(hsla(0., 0., 1., 0.09)))
        .when(!on, |p| p.hover(|s| s.text_color(rgb(TEXT))))
        .child(label.into())
}

/// A small uppercase heading with a rule after it, and an optional control
/// at the end of the rule.
pub(super) fn section(title: &'static str, trailing: Option<AnyElement>) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap_3()
        .mt(px(6.))
        .mb(px(16.))
        .child(
            div()
                .text_size(px(11.))
                .font_semibold()
                .text_color(rgb(FAINT))
                .child(title.to_uppercase()),
        )
        .child(div().flex_1().h(px(1.)).bg(rgb(surface().line)))
        .children(trailing)
}

/// A choice card: title and a short note; the chosen one is lifted and marked
/// with the gradient dot. Callers add the click handler.
pub(super) fn choice(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    note: impl Into<SharedString>,
    on: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(12.))
        .border_1()
        .cursor_pointer()
        .when(on, |c| {
            c.bg(hsla(0., 0., 1., 0.06)).border_color(rgb(0x3b4560))
        })
        .when(!on, |c| {
            c.border_color(rgb(surface().line_strong))
                .hover(|s| s.border_color(rgb(0x3a4258)))
        })
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .text_size(px(14.))
                .font_medium()
                .text_color(rgb(if on { TEXT } else { MUTED }))
                .when(on, |t| t.child(dot(true, 6.)))
                .child(title.into()),
        )
        .child(
            div()
                .mt(px(3.))
                .text_size(px(12.5))
                .text_color(rgb(FAINT))
                .child(note.into()),
        )
}

/// A muted explanatory line under a control.
pub(super) fn hint(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(10.))
        .text_size(px(12.5))
        .line_height(relative(1.5))
        .text_color(rgb(FAINT))
        .child(text.into())
}

/// Lays `items` out `columns` to a row, every cell the same width, with the
/// design's column gap. A short last row keeps its cells' width.
pub(super) fn grid(columns: usize, items: Vec<AnyElement>) -> Div {
    let columns = columns.max(1);
    let mut rows = div().v_flex();
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let mut row = div().h_flex().items_start().gap(px(36.));
        for _ in 0..columns {
            let cell = div().flex_1().flex_basis(px(0.)).min_w_0();
            row = row.child(match items.next() {
                Some(item) => cell.child(item),
                None => cell,
            });
        }
        rows = rows.child(row);
    }
    rows
}

/// One "Live on the mixer" reading: label, value and an optional 0..1 bar.
pub(super) struct Fact {
    pub label: &'static str,
    pub value: String,
    pub bar: Option<f32>,
}

impl Fact {
    pub fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            bar: None,
        }
    }

    pub fn bar(mut self, fraction: f32) -> Self {
        self.bar = Some(fraction.clamp(0., 1.));
        self
    }
}

/// The readouts column beside a stage's controls. Without fresh readings
/// every value is a dash and the note says they are missing, not zero.
pub(super) fn facts(
    live: bool,
    synthetic: bool,
    rows: Vec<Fact>,
    extra: Option<AnyElement>,
) -> Div {
    let mut column = div()
        .v_flex()
        .gap(px(14.))
        .w(px(280.))
        .flex_shrink_0()
        .pl(px(28.))
        .border_l_1()
        .border_color(rgb(surface().line))
        .text_size(px(13.))
        .text_color(rgb(FAINT))
        .child(
            div()
                .h_flex()
                .items_center()
                .gap_2()
                .text_size(px(11.))
                .font_semibold()
                .child(dot(live, 6.))
                .child(if synthetic && live {
                    "LIVE ON THE MIXER · SIMULATED"
                } else {
                    "LIVE ON THE MIXER"
                }),
        );
    for fact in rows {
        column = column.child(
            div()
                .h_flex()
                .justify_between()
                .gap_3()
                .child(fact.label)
                .child(
                    div()
                        .whitespace_nowrap()
                        .font_medium()
                        .text_color(rgb(if live { TEXT } else { DIM }))
                        .child(if live { fact.value } else { "-".into() }),
                ),
        );
        if let (true, Some(fraction)) = (live, fact.bar) {
            column = column.child(
                div()
                    .mt(px(-6.))
                    .h(px(4.))
                    .w_full()
                    .rounded(px(2.))
                    .bg(rgb(0x232838))
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded(px(2.))
                            .bg(gradient()),
                    ),
            );
        }
    }
    if live {
        column.children(extra)
    } else {
        column.child(
            div()
                .text_size(px(12.))
                .line_height(relative(1.5))
                .child("No fresh readings from the mixer. These aren't zero; the mixer just hasn't reported them in the last second."),
        )
    }
}

/// A reported peak as dBFS text.
pub(super) fn dbfs(peak: f64) -> String {
    let db = 20. * peak.max(1e-9).log10();
    if db <= -60. {
        "below −60 dBFS".into()
    } else {
        format!("{db:.0} dBFS").replace('-', "\u{2212}")
    }
}

/// What a styled dialog says and offers.
pub(super) struct Confirm {
    /// A small line above the title (what it's about); None for none.
    pub eyebrow: Option<String>,
    pub title: String,
    pub detail: String,
    /// The button that keeps things as they are; None for a notice, which
    /// offers only `action`.
    pub cancel: Option<&'static str>,
    pub action: String,
}

/// The app's dialog (the installer's flash confirmation is the model): dark,
/// rounded, Inter, white primary pill. The OS keeps only its own password
/// prompt. `on_ok` runs for the action; `on_close` whenever the dialog
/// closes, the action included. Clicking outside does nothing.
pub(super) fn confirm(
    window: &mut Window,
    cx: &mut App,
    spec: Confirm,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_ok = Rc::new(on_ok);
    let on_close = Rc::new(on_close);
    window.open_dialog(cx, move |dialog, _, _| {
        let on_ok = on_ok.clone();
        let on_close = on_close.clone();
        let mut footer = DialogFooter::new().pt_5();
        if let Some(cancel) = spec.cancel {
            footer = footer.child(DialogClose::new().trigger(move |button| {
                button
                    .ghost()
                    .large()
                    .h(px(44.))
                    .px_5()
                    .rounded(px(22.))
                    .label(cancel)
            }));
        }
        footer = footer.child(
            DialogAction::new().child(
                Button::new("dialog-action")
                    .primary()
                    .large()
                    .h(px(44.))
                    .px_5()
                    .rounded(px(22.))
                    .font_semibold()
                    .label(spec.action.clone()),
            ),
        );
        dialog
            .w(px(560.))
            .rounded(px(20.))
            .p_6()
            .bg(rgb(surface().panel))
            .border_1()
            .border_color(rgb(surface().line_strong))
            .text_color(rgb(TEXT))
            .font_family("Inter")
            .overlay_closable(false)
            .title(
                div()
                    .v_flex()
                    .gap_1()
                    .pr(px(40.))
                    .when_some(spec.eyebrow.clone(), |t, eyebrow| {
                        t.child(
                            div()
                                .text_xs()
                                .font_medium()
                                .text_color(rgb(FAINT))
                                .child(eyebrow),
                        )
                    })
                    .child(div().text_xl().font_semibold().child(spec.title.clone())),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(relative(1.6))
                    .text_color(rgb(MUTED))
                    .child(spec.detail.clone()),
            )
            .footer(footer)
            .on_ok(move |_, window, cx| {
                on_ok(window, cx);
                true
            })
            .on_close(move |_, window, cx| on_close(window, cx))
    });
}
