//! Home: a calm overview of the processed channels, one
//! click from each channel's editor.

use super::*;
use level_bar::Bar;
use live_line::{LineState, LineStyle};

impl Rc2FxApp {
    /// Indexes into the draft of the paths the mixer processes, in draft order.
    pub(super) fn processed_indexes(&self) -> Vec<usize> {
        let Some(state) = self.session.saved.as_ref() else {
            return Vec::new();
        };
        self.session
            .draft
            .iter()
            .enumerate()
            .filter(|(_, t)| state.config.is_selected(&t.id))
            .map(|(i, _)| i)
            .collect()
    }

    /// The headline and the sentence under it. Connection, path-change,
    /// restart, engine and routing states come from the status line's shared
    /// derivation (`system_status`, `routing_status`), in its order. "All
    /// clear" needs every processed path to report fresh audio: a path that
    /// is missing from the meters counts as not fresh. A warning outranks
    /// missing audio.
    pub(super) fn home_headline(&self, processed: &[usize]) -> (String, String) {
        if let Some((label, _)) = self.system_status() {
            let detail = if !self.stream_connected {
                "Mixer processing is independent of this app; it keeps running."
            } else if self
                .session
                .saved
                .as_ref()
                .and_then(telemetry::engine_error)
                .is_some()
            {
                "The notice above says what happened and what to do."
            } else {
                ""
            };
            return (label.into(), detail.into());
        }
        if let Some((label, _)) = self.routing_status() {
            return (label.into(), String::new());
        }
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        if processed.is_empty() {
            return (
                "No channels processed.".into(),
                "Choose a channel on the left to add RC2FX processing.".into(),
            );
        }
        if let Some(warning) = processed
            .iter()
            .find_map(|&i| self.audio_warning_for(&self.session.draft[i].id))
        {
            return ("Needs a look.".into(), warning.label().into());
        }
        let fresh = processed
            .iter()
            .filter(|&&i| self.fresh_meter_for(&self.session.draft[i].id).is_some())
            .count();
        if fresh < processed.len() {
            return (
                "Waiting for audio.".into(),
                format!(
                    "No fresh levels from {} of {} processed channel{}.",
                    processed.len() - fresh,
                    processed.len(),
                    plural(processed.len())
                ),
            );
        }
        (
            "All clear.".into(),
            format!(
                "RC2FX is processing {} channel{}.",
                processed.len(),
                plural(processed.len())
            ),
        )
    }

    /// What Home shows that can change with time alone: the headline, each
    /// processed path's freshness and each reported path's, for the overview's
    /// bars. The poll compares it every tick, so Home goes stale on its own
    /// when meters stop arriving (see `poll`).
    pub(super) fn home_key(&self) -> (String, String, Vec<bool>) {
        let processed = self.processed_indexes();
        let (headline, detail) = self.home_headline(&processed);
        let fresh = processed
            .iter()
            .map(|&i| self.session.draft[i].id.as_str())
            .chain(
                self.meter
                    .iter()
                    .flat_map(|m| m.targets.iter().map(|t| t.id.as_str())),
            )
            .map(|id| self.fresh_meter_for(id).is_some())
            .collect();
        (headline, detail, fresh)
    }

    /// Whether a page with live lines is on screen: Home or a channel's
    /// editor. The shell caches the app only then, and indicators are only
    /// claimed for the overlay then; Settings and the installer keep their
    /// every-frame rendering and draw no animated lines.
    pub(super) fn animated_page(&self) -> bool {
        self.session.connected && !self.settings_open && !self.installer_page
    }

    /// Whether the overlay can draw this indicator.
    fn can_animate(&self, source: &line_overlay::Source) -> bool {
        line_overlay::enabled()
            && self.animated_page()
            && !self.reduced_motion
            && !line_overlay::latched(&self.line_slots, source)
    }

    /// Whether the overlay is drawing every overview bar each frame, so a
    /// reading needs no redraw of the app. A hidden window pauses it.
    pub(super) fn bars_animate(&self, window: &Window) -> bool {
        line_overlay::enabled()
            && !self.reduced_motion
            && window.is_visible()
            && !line_overlay::any_latched(&self.line_slots)
    }

    /// One overview bar: claimed for the overlay like `indicator`, otherwise
    /// drawn still here.
    fn level_bar(&self, target_id: &str, bar: Bar) -> AnyElement {
        let level = self.overview_level(target_id);
        let source = line_overlay::Source::Path(target_id.to_owned());
        if level.is_some() && self.can_animate(&source) {
            return line_overlay::claim(&self.line_slots, source, line_overlay::Shape::Bar(bar))
                .into_any_element();
        }
        level_bar::still(bar, level).into_any_element()
    }

    /// One level indicator: claimed for the animated overlay when it can
    /// animate, otherwise drawn still here (never both; line_overlay.rs).
    pub(super) fn indicator(
        &self,
        source: line_overlay::Source,
        fresh: bool,
        style: LineStyle,
    ) -> AnyElement {
        if fresh && self.can_animate(&source) {
            return line_overlay::claim(&self.line_slots, source, line_overlay::Shape::Line(style))
                .into_any_element();
        }
        let line_overlay::Source::Path(id) = &source;
        let history = self.levels.get(id);
        let (level, state) = if fresh {
            (
                live_line::level_of(history, Instant::now()),
                LineState::Live,
            )
        } else {
            (0., LineState::Unavailable)
        };
        live_line::still_line(level, state, style).into_any_element()
    }

    /// Leaves Home for one channel's editor.
    pub(super) fn open_channel(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inputs.selected = None;
        self.selected = index;
        self.band = 0;
        self.channel = Channel::Left;
        self.import = None;
        self.import_error = None;
        self.home_open = false;
        self.set_settings_open(false);
        self.rebuild_editors(window, cx);
        cx.notify();
    }

    /// A channel's level for the overview, 0..1, or None without a fresh
    /// reading.
    pub(super) fn overview_level(&self, target_id: &str) -> Option<f32> {
        self.fresh_meter_for(target_id)?;
        Some(live_line::level_of(
            self.levels.get(target_id),
            Instant::now(),
        ))
    }

    /// What the overview says under a channel: its effects when processed.
    fn overview_chip(&self, target_id: &str) -> (String, bool) {
        let Some(target) = self.session.draft.iter().find(|t| t.id == target_id) else {
            return ("No RC2FX path".into(), false);
        };
        if !self.processed(target_id) {
            return ("Off".into(), false);
        }
        if target.bypass {
            return ("Bypassed".into(), true);
        }
        let on: Vec<&str> = desk::visible_effects(target)
            .into_iter()
            .filter(|e| target.effect_enabled(*e))
            .map(desk::short_effect)
            .collect();
        if on.is_empty() {
            ("No effects on".into(), true)
        } else {
            (on.join(" · "), true)
        }
    }

    /// Opens a show source from the overview, as the sidebar does.
    fn open_source(&mut self, source_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .show_sources()
            .find(|s| s.id == source_id)
            .map(|s| s.target_id.clone());
        let Some(target) = target else {
            return;
        };
        let index = self.session.draft.iter().position(|t| t.id == target);
        if let Some(index) = index {
            self.open_channel(index, window, cx);
        }
        self.inputs.selected = Some(source_id);
        cx.notify();
    }

    /// The overview: a strip per fader in the current show with its live
    /// level, then the headphones and outputs. Any of them opens its editor.
    pub(super) fn home_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let processed = self.processed_indexes();
        let (headline, detail) = self.home_headline(&processed);
        let sub = match headline.as_str() {
            "All clear." => "Click any channel to change its effects.".to_owned(),
            "No channels processed." => {
                "Nothing is processed yet. Pick a channel to add effects.".to_owned()
            }
            _ if detail.is_empty() => headline,
            _ => format!("{headline} {detail}"),
        };
        let mut faders: Vec<_> = self.show_sources().filter(|s| s.fader.is_some()).collect();
        faders.sort_by_key(|s| s.fader);
        let mut strips = div().h_flex().items_stretch().gap(px(10.));
        for source in &faders {
            let (chip, on) = self.overview_chip(&source.target_id);
            let id = source.id.clone();
            strips = strips.child(
                overview_card(("strip", source.source_id as usize), on)
                    .flex_1()
                    .min_w_0()
                    .v_flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(14.))
                    .pb(px(12.))
                    .px(px(8.))
                    .child(vertical_meter(
                        self.level_bar(&source.target_id, Bar::Vertical),
                    ))
                    .child(card_name(target_layout::display_name(&source.name), on).mt(px(4.)))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(FAINT))
                            .child(format!("Fader {}", source.fader.unwrap_or_default())),
                    )
                    .child(chip_label(chip, on))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.open_source(id.clone(), window, cx)
                    })),
            );
        }
        // A processed input with no fader in this show (taken out of the show,
        // or never on one) gets its own row, so it never silently vanishes.
        let on_faders: Vec<&str> = faders.iter().map(|s| s.target_id.as_str()).collect();
        let (mut elsewhere, mut any_elsewhere) = (div().h_flex().flex_wrap().gap(px(10.)), false);
        let mut outputs = div().h_flex().flex_wrap().gap(px(10.));
        for (index, target) in self.session.draft.iter().enumerate() {
            let output = matches!(
                target_layout::Group::of(target),
                target_layout::Group::Headphones | target_layout::Group::Outputs
            );
            if !output && !processed_off_faders(&target.id, processed.contains(&index), &on_faders)
            {
                continue;
            }
            let (chip, on) = self.overview_chip(&target.id);
            let card = overview_card(("output", index), on)
                .w(px(232.))
                .v_flex()
                .gap(px(8.))
                .px(px(14.))
                .py(px(11.))
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap(px(12.))
                        .child(card_name(self.target_name(target), on))
                        .child(horizontal_meter(
                            self.level_bar(&target.id, Bar::Horizontal),
                        )),
                )
                .child(div().h_flex().child(chip_label(chip, on)))
                .on_click(
                    cx.listener(move |app, _, window, cx| app.open_channel(index, window, cx)),
                );
            if output {
                outputs = outputs.child(card);
            } else {
                any_elsewhere = true;
                elsewhere = elsewhere.child(card);
            }
        }
        div()
            .v_flex()
            .w_full()
            .max_w(px(1180.))
            .mx_auto()
            .pb_6()
            .child(
                div()
                    .text_size(px(32.))
                    .font_bold()
                    .line_height(relative(1.1))
                    .child("Your mixer"),
            )
            .child(
                div()
                    .mt(px(6.))
                    .mb(px(26.))
                    .text_size(px(15.))
                    .text_color(rgb(MUTED))
                    .child(sub),
            )
            .child(strips)
            .when(any_elsewhere, |page| {
                page.child(section_label("PROCESSED, NOT IN THIS SHOW"))
                    .child(elsewhere)
            })
            .child(section_label("HEADPHONES AND OUTPUTS"))
            .child(outputs)
    }
}

/// Whether an input gets the overview's "not in this show" card: it is
/// processed and no fader strip shows it. Removing a source from the show
/// never hides a channel RC2FX still processes.
fn processed_off_faders(target_id: &str, processed: bool, on_faders: &[&str]) -> bool {
    processed && !on_faders.contains(&target_id)
}

/// A small heading between the overview's groups.
fn section_label(text: &'static str) -> Div {
    div()
        .mt(px(30.))
        .mb(px(10.))
        .text_size(px(11.))
        .font_semibold()
        .text_color(rgb(FAINT))
        .child(text)
}

/// A clickable overview card; processed ones get a tinted border.
fn overview_card(id: impl Into<ElementId>, processed: bool) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .rounded(px(12.))
        .border_1()
        .bg(rgb(surface().panel))
        .border_color(rgb(if processed { 0x2f4a5a } else { surface().line }))
        .hover(|s| {
            s.bg(rgb(surface().raised))
                .border_color(rgb(surface().line_strong))
        })
}

/// A channel name on the overview, with a dot when it's processed.
fn card_name(name: String, processed: bool) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(6.))
        .max_w_full()
        .min_w_0()
        .text_size(px(13.5))
        .font_medium()
        .when(processed, |d| {
            d.child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .flex_shrink_0()
                    .bg(ui::gradient()),
            )
        })
        .child(div().min_w_0().truncate().child(name))
}

/// "Off", or the effects a processed channel runs.
fn chip_label(text: String, processed: bool) -> Div {
    div()
        .max_w_full()
        .truncate()
        .px(px(9.))
        .py(px(1.))
        .rounded_full()
        .text_size(px(11.))
        .when(processed, |d| {
            d.bg(hsla(183. / 360., 0.73, 0.63, 0.12))
                .text_color(rgb(TEXT))
        })
        .when(!processed, |d| {
            d.border_1()
                .border_color(rgb(surface().line_strong))
                .text_color(rgb(FAINT))
        })
        .child(text)
}

/// A fader's level: a tall track around its bar.
fn vertical_meter(bar: AnyElement) -> Div {
    div()
        .relative()
        .w(px(10.))
        .h(px(190.))
        .rounded(px(6.))
        .overflow_hidden()
        .bg(rgb(surface().sunken))
        .child(bar)
}

/// An output's level: a short track around its bar.
fn horizontal_meter(bar: AnyElement) -> Div {
    div()
        .flex_1()
        .min_w(px(40.))
        .h(px(6.))
        .rounded(px(4.))
        .overflow_hidden()
        .bg(rgb(surface().sunken))
        .child(bar)
}

#[cfg(test)]
mod tests {
    use super::processed_off_faders;

    #[test]
    fn a_processed_input_without_a_fader_stays_on_the_overview() {
        let faders = ["usb-game", "usb-chat"];
        // Mic 4 was taken out of the show but is still processed.
        assert!(processed_off_faders("mic-1", true, &faders));
        // On a fader, its strip shows it; unprocessed, it isn't listed twice.
        assert!(!processed_off_faders("usb-game", true, &faders));
        assert!(!processed_off_faders("mic-1", false, &faders));
        // An empty or unread show still lists it.
        assert!(processed_off_faders("mic-1", true, &[]));
    }
}
