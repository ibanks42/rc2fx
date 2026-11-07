//! The settings page: connection, presets, files and diagnostics.

use super::*;

/// Width of the settings page's centred column.
const COLUMN: f32 = 760.;

/// A titled group of rows: a small uppercase heading,
/// optional controls beside it, then the rows.
pub(super) fn section(title: &'static str, aside: impl IntoElement, body: Div) -> Div {
    div()
        .v_flex()
        .child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .min_h(px(28.))
                .mb(px(6.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_semibold()
                        .text_color(rgb(FAINT))
                        .child(title.to_uppercase()),
                )
                .child(aside),
        )
        .child(body.v_flex())
}

/// One row of a section: what it is on the left, its controls on the right.
pub(super) fn row(label: impl IntoElement, controls: impl IntoElement) -> Div {
    div()
        .h_flex()
        .items_center()
        .gap(px(24.))
        .min_h(px(56.))
        .py(px(16.))
        .border_b_1()
        .border_color(rgb(surface().line))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(15.))
                .font_medium()
                .child(label),
        )
        .child(div().flex_shrink_0().child(controls))
}

/// A row's title and the sentence under it.
pub(super) fn labelled(title: impl Into<SharedString>, detail: impl Into<SharedString>) -> Div {
    let detail = detail.into();
    div()
        .v_flex()
        .gap(px(3.))
        .child(title.into())
        .when(!detail.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(13.5))
                    .font_normal()
                    .line_height(relative(1.45))
                    .text_color(rgb(MUTED))
                    .child(detail),
            )
        })
}

/// A secondary action: an outlined pill.
pub(super) fn ghost(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id)
        .ghost()
        .h(px(38.))
        .px(px(16.))
        .rounded(px(19.))
        .border_1()
        .border_color(rgb(surface().line_strong))
        .label(label)
}

impl Rc2FxApp {
    pub(super) fn set_settings_open(&mut self, open: bool) {
        if open && !self.settings_open {
            self.list_presets();
            self.read_ssh();
        }
        self.settings_open = open;
        self.presets.open = open;
        self.diagnostics_open = false;
        // Settings, a channel or the installer: one page at a time.
        self.installer_page = false;
    }

    fn mixer_section(&self, cx: &mut Context<Self>) -> Div {
        let mut rows = div().child(row(
            labelled(
                "RØDECaster",
                if self.session.connected {
                    "Connected".to_owned()
                } else {
                    "Not connected".into()
                },
            ),
            ghost("disconnect", "Disconnect")
                .disabled(!self.session.connected && !self.connecting)
                .on_click(cx.listener(|app, _, window, cx| app.disconnect(window, cx))),
        ));
        rows = rows.child(row(
            labelled(
                "Update, repair or remove RC2FX",
                "RC2FX is built into official RØDE firmware. The installer saves your settings first and carries them over.",
            ),
            ghost("open-installer-settings", "Open installer")
                .on_click(cx.listener(|app, _, _, cx| app.toggle_installer_page(cx))),
        ));
        if let Some(processing) = self.processing_row(cx) {
            rows = rows.child(processing);
        }
        section("Mixer", div(), rows)
    }

    /// Switch between Settings and its Diagnostics page. They share one
    /// scroll position, so each starts at its top.
    fn show_diagnostics(&mut self, open: bool) {
        self.diagnostics_open = open;
        self.content_scroll.set_offset(Point::default());
    }

    fn about_section(&self) -> Div {
        let rows = div().child(row(
            labelled(
                "Licenses",
                "RC2FX is MIT licensed. Third-party notices and credits are included.",
            ),
            ghost("open-licenses", "View licenses")
                .on_click(|_, window, cx| crate::licenses::open(window, cx)),
        ));
        section("About", div(), rows)
    }

    fn files_section(&self, cx: &mut Context<Self>) -> Div {
        section(
            "Files",
            div(),
            div().child(
                row(
                    labelled("Settings file", "Open a settings file into your draft, or save this draft to share or keep."),
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            ghost("open-settings-file", "Open…")
                                .disabled(
                                    self.file_busy
                                        || self.pending.is_some()
                                        || self.presets.writing()
                                        || self.connecting
                                        || self.transition_busy(),
                                )
                                .on_click(
                                    cx.listener(|app, _, window, cx| app.open_draft(window, cx)),
                                ),
                        )
                        .child(
                            ghost("save-settings-file", "Save…")
                                .disabled(self.file_busy)
                                .on_click(
                                    cx.listener(|app, _, window, cx| app.save_draft(window, cx)),
                                ),
                        ),
                )
                .border_b_0(),
            ),
        )
    }

    /// This app's colors: a preference of this computer, not the mixer's.
    fn appearance_section(&self, cx: &mut Context<Self>) -> Div {
        let current = desk::appearance();
        let mut choices = div().h_flex().gap_2();
        for (index, appearance) in desk::Appearance::ALL.into_iter().enumerate() {
            let note = match appearance {
                desk::Appearance::Dusk => "Deep blue-grey · default",
                desk::Appearance::Graphite => "Neutral dark grey",
                desk::Appearance::Black => "Near black",
            };
            choices = choices.child(
                ui::choice(
                    ("appearance", index),
                    appearance.label(),
                    note,
                    appearance == current,
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    desk::set_appearance(appearance, cx);
                    connection::remember_appearance(appearance);
                    // The events window draws with the same colors.
                    cx.refresh_windows();
                })),
            );
        }
        section(
            "Appearance",
            div(),
            div()
                .py(px(16.))
                .border_b_1()
                .border_color(rgb(surface().line))
                .child(
                    labelled("Colors", "")
                        .text_size(px(15.))
                        .font_medium()
                        .mb(px(14.)),
                )
                .child(choices),
        )
    }

    fn diagnostics_tools(&self, cx: &mut Context<Self>) -> Div {
        section(
            "Troubleshooting",
            div(),
            div()
                .child(row(
                    labelled("Events", "What the mixer reported: overruns, path changes and recoveries, newest first. Opens in its own window."),
                    ghost("open-events", "Open events").on_click(cx.listener(|app, _, _, cx| app.open_events(cx))),
                ))
                .child(row(
                    labelled("Show telemetry", "Levels, delay, counters and timing for every processed path, shown below."),
                    ui::toggle("show-telemetry", self.telemetry_open, false, false).on_click(cx.listener(
                        |app, _, _, cx| {
                            app.telemetry_open = !app.telemetry_open;
                            connection::remember_telemetry(app.telemetry_open);
                            cx.notify();
                        },
                    )),
                ))
                .child(
                    row(
                        labelled("Diagnostics file", "The mixer's state and recent timing history, for reporting a problem."),
                        ghost("export-diagnostics", "Export diagnostics…")
                            .disabled(
                                self.diagnostics_pending || self.file_busy || !self.session.connected,
                            )
                            .on_click(cx.listener(|app, _, _, cx| app.request_diagnostics(cx))),
                    )
                    .border_b_0(),
                ),
        )
    }

    /// One quiet row at the foot of Settings; troubleshooting lives behind it.
    fn diagnostics_entry(&self, cx: &mut Context<Self>) -> Div {
        section(
            "Diagnostics",
            div(),
            div().child(row(
                labelled("Diagnostics", ""),
                ghost("open-diagnostics", "Open").on_click(cx.listener(|app, _, _, cx| {
                    app.show_diagnostics(true);
                    cx.notify();
                })),
            )),
        )
    }

    fn diagnostics_page(&self, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .gap(px(40.))
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .pt(px(18.))
            .pb_6()
            .child(
                div()
                    .v_flex()
                    .gap(px(10.))
                    .child(
                        div()
                            .id("diagnostics-back")
                            .flex_shrink_0()
                            .cursor_pointer()
                            .text_size(px(13.5))
                            .text_color(rgb(MUTED))
                            .hover(|s| s.text_color(rgb(TEXT)))
                            .child("‹ Settings")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.show_diagnostics(false);
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TITLE_TEXT))
                            .line_height(relative(1.))
                            .font_bold()
                            .child("Diagnostics"),
                    ),
            )
            .child(self.diagnostics_tools(cx))
            .when(self.telemetry_open, |page| {
                let mono = Theme::global(cx).mono_font_family.clone();
                page.child(telemetry_view::telemetry_section(
                    &self.telemetry_snapshot(),
                    &mono,
                    self.telemetry_aside(cx),
                    Button::new("telemetry-details")
                        .ghost()
                        .small()
                        .label(if self.telemetry_details {
                            "Hide details"
                        } else {
                            "Details"
                        })
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.telemetry_details = !app.telemetry_details;
                            connection::remember_telemetry_details(app.telemetry_details);
                            cx.notify();
                        })),
                ))
            })
    }

    pub(super) fn settings_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.diagnostics_open {
            return self.diagnostics_page(cx);
        }
        div()
            .v_flex()
            .gap(px(40.))
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .pt(px(18.))
            .pb_6()
            .child(
                div()
                    .text_size(px(ui::TITLE_TEXT))
                    .line_height(relative(1.))
                    .font_bold()
                    .child("Settings"),
            )
            .child(self.mixer_section(cx))
            .child(self.versions_section(cx))
            .child(self.about_section())
            .child(self.preset_panel(cx))
            .child(self.files_section(cx))
            .child(self.appearance_section(cx))
            .child(self.developer_section(cx))
            .child(self.diagnostics_entry(cx))
    }
}
