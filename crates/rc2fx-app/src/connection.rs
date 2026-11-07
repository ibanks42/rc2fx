use super::*;
use std::{fs, path::PathBuf};

fn preferences_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    Some(base.join("RC2FX"))
}

/// The view preferences file: what the channel list shows and whether the
/// Telemetry section is on.
fn view_path() -> Option<PathBuf> {
    Some(preferences_dir()?.join("view.json"))
}

fn remembered_view() -> serde_json::Map<String, serde_json::Value> {
    read_view(view_path().as_deref())
}

fn read_view(path: Option<&std::path::Path>) -> serde_json::Map<String, serde_json::Value> {
    path.and_then(|path| fs::read_to_string(path).ok())
        .filter(|text| text.len() <= 4096)
        .and_then(|text| rc2fx_core::from_json::<serde_json::Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// Saves one view preference, keeping the others.
fn remember_view(key: &str, value: serde_json::Value) {
    let mut view = remembered_view();
    view.insert(key.to_owned(), value);
    if let Some(path) = view_path()
        && let Some(dir) = path.parent()
        && fs::create_dir_all(dir).is_ok()
    {
        // A preference that fails to save only costs the choice next time.
        let _ = interchange::write_export(&path, &serde_json::Value::Object(view).to_string());
    }
}

/// Whether the Telemetry section was last on.
pub(super) fn remembered_telemetry() -> bool {
    remembered_view()
        .get("showTelemetry")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn remember_telemetry(on: bool) {
    remember_view("showTelemetry", on.into());
}

pub(super) fn remembered_telemetry_details() -> bool {
    remembered_view()
        .get("telemetryDetails")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn remember_telemetry_details(on: bool) {
    remember_view("telemetryDetails", on.into());
}

/// The appearance last chosen; Dusk when none was.
pub(super) fn remembered_appearance() -> desk::Appearance {
    remembered_view()
        .get("appearance")
        .and_then(serde_json::Value::as_str)
        .and_then(desk::Appearance::from_key)
        .unwrap_or(desk::Appearance::Dusk)
}

pub(super) fn remember_appearance(appearance: desk::Appearance) {
    remember_view("appearance", appearance.key().into());
}

pub(super) fn remembered_duo() -> bool {
    duo_enabled(&remembered_view())
}

fn duo_enabled(view: &serde_json::Map<String, serde_json::Value>) -> bool {
    view.get("experimentalDuoFlash")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Unlike view-only preferences, report a failed save rather than implying
/// that the experimental opt-in will survive restarting the app.
pub(super) fn remember_duo(allow: bool) -> std::result::Result<(), String> {
    let path = view_path().ok_or("No application settings folder")?;
    save_duo(&path, allow)
}

fn save_duo(path: &std::path::Path, allow: bool) -> std::result::Result<(), String> {
    save_flag(path, "experimentalDuoFlash", allow)
}

/// Whether Developer mode was left on. It only shows developer controls in
/// Settings; it grants nothing by itself.
pub(super) fn remembered_developer() -> bool {
    developer_enabled(&remembered_view())
}

fn developer_enabled(view: &serde_json::Map<String, serde_json::Value>) -> bool {
    view.get("developerMode")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Like the Duo opt-in, a failed save is reported rather than ignored.
pub(super) fn remember_developer(on: bool) -> std::result::Result<(), String> {
    let path = view_path().ok_or("No application settings folder")?;
    save_flag(&path, "developerMode", on)
}

fn save_flag(path: &std::path::Path, key: &str, on: bool) -> std::result::Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("No settings parent")?).map_err(|e| e.to_string())?;
    let mut view = read_view(Some(path));
    view.insert(key.into(), on.into());
    interchange::write_export(path, &serde_json::Value::Object(view).to_string())
        .map_err(|e| e.to_string())
}

/// A sunken box for exact text, such as a command.
pub(super) fn code_box(mono: SharedString) -> Div {
    div()
        .relative()
        .px(px(6.))
        .py(px(4.))
        .rounded(px(10.))
        .bg(rgb(surface().sunken))
        .border_1()
        .border_color(rgb(surface().line))
        .font_family(mono)
        .text_size(px(12.))
        .text_color(rgb(MUTED))
}

impl Rc2FxApp {
    /// While disconnected: whether the app is looking for the mixer, why the
    /// last attempt failed, or a way to connect again after Disconnect.
    fn connection_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (color, text) = connection_status(self.auto_connect, &self.message);
        div()
            .v_flex()
            .gap_4()
            .items_center()
            .text_center()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .size(px(7.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgb(color)),
                    )
                    .child(div().min_w_0().text_color(rgb(MUTED)).child(text)),
            )
            .when(!self.auto_connect, |panel| {
                panel.child(
                    Button::new("connect")
                        .primary()
                        .label("Connect")
                        .disabled(self.connecting)
                        .on_click(cx.listener(|app, _, _, cx| app.connect(cx))),
                )
            })
    }

    pub(super) fn connection_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.auto_connect && cfg!(target_os = "linux") && usb_permission(&self.message) {
            return self.usb_access_screen(cx).into_any_element();
        }
        let found = self.auto_connect && mixer_found(&self.message);
        let copy = div()
            .v_flex()
            .items_center()
            .w_full()
            .max_w(px(620.))
            .mx_auto()
            .px_6()
            .pb(px(80.))
            .child(onboarding::headline(
                if found { "Found it." } else { "Plug in your" },
                if found { "Now, the noise." } else { "RØDECaster." },
            ))
            .child(
                div()
                    .mt(px(22.))
                    .max_w(px(540.))
                    .text_center()
                    .text_size(px(ui::LEAD_TEXT))
                    .line_height(relative(1.55))
                    .text_color(rgb(MUTED))
                    .child(if found {
                        "Your RØDECaster doesn't have RC2FX yet. It adds noise suppression, EQ and compression, running on the mixer itself."
                    } else {
                        "Connect it to this computer with a USB cable."
                    }),
            )
            .when(found, |copy| {
                copy.child(
                    div()
                        .mt_6()
                        .v_flex()
                        .items_center()
                        .gap_4()
                        .when(rc2fx_installer::helper::needed(), |actions| {
                            actions.child(div().text_xs().text_color(rgb(FAINT))
                                .child("Asks for your password once"))
                        })
                        .child(
                            Button::new("open-installer")
                                .primary()
                                .large()
                                .h(px(ui::LARGE_BUTTON_HEIGHT))
                                .px(px(28.))
                                .rounded(px(26.))
                                .font_semibold()
                                .label("Install RC2FX")
                                .on_click(cx.listener(|app, _, _, cx| app.open_installer(cx))),
                        ),
                )
            })
            .when(!found, |copy| {
                copy.child(div().mt_6().child(self.connection_panel(cx)))
                    .when(!self.message.is_empty(), |copy| {
                        copy.child(
                            Button::new("open-installer")
                                .ghost()
                                .mt_4()
                                .label("Open installer")
                                .on_click(cx.listener(|app, _, _, cx| app.open_installer(cx))),
                        )
                    })
            });
        div()
            .size_full()
            .v_flex()
            .relative()
            .font_family("Inter")
            .bg(onboarding::background())
            .child(onboarding::backdrop())
            .text_color(rgb(TEXT))
            .child(
                div()
                    .id("connection-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .v_flex()
                            .w_full()
                            .min_h(relative(1.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(200.))
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(!self.reduced_motion, |hero| {
                                        hero.child(
                                            div()
                                                .w(relative(0.6))
                                                .h_full()
                                                .child(self.onboarding_signature.clone()),
                                        )
                                    }),
                            )
                            .child(copy),
                    ),
            )
            .into_any_element()
    }

    /// A small Copy button that says Copied for a moment after a click.
    pub(super) fn copy_button(
        &self,
        id: &'static str,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let text = text.into();
        let done = self.copied == Some(id);
        div()
            .id(id)
            .px(px(10.))
            .py(px(4.))
            .rounded(px(7.))
            .bg(rgb(surface().sunken))
            .border_1()
            .border_color(rgb(surface().line_strong))
            .cursor_pointer()
            .font_family("Inter")
            .text_size(px(12.))
            .text_color(rgb(if done { ACCENT } else { MUTED }))
            .hover(|s| s.text_color(rgb(TEXT)))
            .child(if done { "Copied" } else { "Copy" })
            .on_click(cx.listener(move |app, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                app.copied = Some(id);
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    let _ = this.update(cx, |app, cx| {
                        if app.copied == Some(id) {
                            app.copied = None;
                            cx.notify();
                        }
                    });
                })
                .detach();
            }))
    }

    /// Linux without the udev rule: what RC2FX needs, exactly what it will
    /// add (the file, shown byte for byte), and how to undo it, before the
    /// system's own password prompt.
    fn usb_access_screen(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = Theme::global(cx).mono_font_family.clone();
        let asking = self.usb_rule == UsbRule::Asking;
        let open = self.usb_rule_open;
        let point = |text: &'static str| {
            div()
                .h_flex()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .mt(px(8.))
                        .size(px(5.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(rgb(FAINT)),
                )
                // min_w_0 lets a long line shrink to the card and wrap.
                .child(div().flex_1().min_w_0().child(text))
        };
        // Exact text in a read-only box (select it or copy it), with a Copy
        // button pinned to the box's corner, outside what scrolls. Copy puts
        // the constant on the clipboard, never the field's contents.
        let code_box = || code_box(mono.clone());
        let card = div()
            .w_full()
            .max_w(px(640.))
            .mx_auto()
            .mt(px(36.))
            .p(px(28.))
            .rounded(px(20.))
            .bg(rgb(surface().panel))
            .border_1()
            .border_color(rgb(surface().line))
            .text_size(px(14.5))
            .line_height(relative(1.55))
            .text_color(rgb(MUTED))
            .child(
                "Linux only lets apps open USB devices you've allowed. RC2FX needs this once, so it can read your mixer's settings and levels and save your changes over USB.",
            )
            .child(div().mt(px(22.)).mb(px(10.)).font_semibold().text_color(rgb(TEXT)).child("What this does"))
            .child(
                div()
                    .v_flex()
                    .gap_2()
                    .child(point("Adds one file: /etc/udev/rules.d/70-rc2fx.rules"))
                    .child(point("It only covers RØDECaster mixers, and only gives access to whoever is logged in at this computer."))
                    .child(point("It doesn't cover update mode. Installing firmware still asks for permission separately."))
                    .child(point("Nothing else on your system changes, and RC2FX never sees or keeps your password.")),
            )
            .child(
                div()
                    .id("usb-rule-show")
                    .mt(px(18.))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(rgb(MUTED))
                    .hover(|s| s.text_color(rgb(TEXT)))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                    .child("Show the exact file")
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.usb_rule_open = !open;
                        cx.notify();
                    })),
            )
            .when(open, |card| {
                card.child(
                    code_box()
                        .mt(px(10.))
                        .child(
                            gpui_kit::component::input::Textarea::new(&self.usb_rule_text)
                                .readonly(true)
                                .appearance(false)
                                .h(px(220.))
                                .aria_label("The udev rule RC2FX installs"),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(px(8.))
                                .right(px(8.))
                                .child(self.copy_button("copy-usb-rule", rc2fx_installer::helper::USB_RULE, cx)),
                        ),
                )
            })
            .child(div().mt(px(18.)).text_size(px(13.)).child("To undo it later:"))
            .child(
                code_box()
                    .mt(px(6.))
                    .pr(px(76.))
                    .child(Input::new(&self.usb_undo_text).readonly(true).appearance(false))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right(px(8.))
                            .flex()
                            .items_center()
                            .child(self.copy_button("copy-usb-undo", USB_UNDO, cx)),
                    ),
            )
            .when_some(
                match &self.usb_rule {
                    UsbRule::Failed(error) => Some(error.clone()),
                    _ => None,
                },
                |card, error| card.child(div().mt(px(18.)).text_size(px(13.5)).text_color(rgb(WARN)).child(error)),
            )
            .child(
                div()
                    .mt(px(24.))
                    .h_flex()
                    .items_center()
                    .justify_end()
                    .gap_4()
                    .child(div().text_size(px(13.)).text_color(rgb(FAINT)).child(if asking {
                        "Answer your system's password prompt to continue."
                    } else {
                        "Your system will ask for your password."
                    }))
                    .child(
                        Button::new("allow-usb")
                            .primary()
                            .large()
                            .h(px(ui::LARGE_BUTTON_HEIGHT))
                            .px(px(28.))
                            .rounded(px(26.))
                            .font_semibold()
                            .label(if matches!(self.usb_rule, UsbRule::Failed(_)) { "Try again" } else { "Allow access" })
                            .disabled(asking)
                            .on_click(cx.listener(|app, _, _, cx| app.allow_usb(cx))),
                    ),
            );
        div()
            .size_full()
            .v_flex()
            .relative()
            .font_family("Inter")
            .bg(onboarding::background())
            .child(onboarding::backdrop())
            .text_color(rgb(TEXT))
            .child(
                div()
                    .id("usb-access-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_8()
                    .pt(px(24.))
                    .pb(px(48.))
                    .child(
                        onboarding::headline("Let RC2FX talk", "to your mixer.")
                            .text_size(px(ui::DISPLAY_TEXT_SMALL)),
                    )
                    .child(card),
            )
    }

    /// Install the udev rule through the system's password prompt, then
    /// connect.
    fn allow_usb(&mut self, cx: &mut Context<Self>) {
        if self.usb_rule == UsbRule::Asking {
            return;
        }
        self.usb_rule = UsbRule::Asking;
        cx.notify();
        let request = cx
            .background_executor()
            .spawn(async { rc2fx_installer::helper::request_usb_rule() });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok(()) => {
                        app.usb_rule = UsbRule::Idle;
                        app.begin_connection(false, cx);
                    }
                    Err(error) => app.usb_rule = UsbRule::Failed(error.0),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// How to remove the rule again, as the screen shows and copies it.
pub(super) const USB_UNDO: &str = "sudo rm /etc/udev/rules.d/70-rc2fx.rules";

/// The USB permission screen's request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UsbRule {
    Idle,
    /// The system's password prompt is open.
    Asking,
    /// Refused or failed; the reason, in words.
    Failed(String),
}

/// Linux refused the app the mixer's USB 1 control interface: the one
/// failure the udev rule fixes (rc2fx_installer::helper::USB_PERMISSION).
pub(super) fn usb_permission(last_result: &str) -> bool {
    last_result.starts_with(rc2fx_installer::helper::USB_PERMISSION)
}

/// This exact discovery failure is emitted only after finding one supported
/// mixer without the RC2FX interface (control::control_interface). Do not infer
/// "found" from arbitrary failures/timeouts or a substring of an error.
pub(super) fn mixer_found(last_result: &str) -> bool {
    last_result == "The mixer is connected, but RC2FX isn't running on it"
}

// Retry activity must not replace the last completed observation with a
// transient 'Looking…' result. Starting a new explicit connection clears it.
fn connection_status(auto_connect: bool, last_result: &str) -> (u32, String) {
    if !auto_connect {
        (FAINT, "Disconnected".to_owned())
    } else if last_result.is_empty() {
        (ACCENT, "Listening…".to_owned())
    } else {
        (WARN, format!("Last check: {last_result}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACCENT, FAINT, WARN, connection_status, developer_enabled, duo_enabled, mixer_found,
        read_view, save_duo, save_flag,
    };
    use std::fs;

    #[test]
    fn the_undo_command_removes_exactly_the_installed_file() {
        assert_eq!(
            super::USB_UNDO,
            format!("sudo rm {}", rc2fx_installer::helper::USB_RULE_PATH)
        );
    }

    #[test]
    fn only_a_refused_open_offers_the_usb_rule() {
        assert!(super::usb_permission(
            "USB permission required: Permission denied (os error 13)"
        ));
        for other in [
            "",
            "Couldn't open the mixer's USB control: No such file or directory (os error 2)",
            "The mixer is connected, but RC2FX isn't running on it",
        ] {
            assert!(!super::usb_permission(other));
        }
    }

    #[test]
    fn found_copy_requires_the_specific_device_discovery_outcome() {
        assert!(mixer_found(
            "The mixer is connected, but RC2FX isn't running on it"
        ));
        for other in [
            "",
            "timed out",
            "No RØDECaster Pro II connected",
            "More than one RØDECaster Pro II is connected; connect only one",
        ] {
            assert!(!mixer_found(other));
        }
    }

    #[test]
    fn a_retry_keeps_the_last_connection_observation_visible() {
        let failure = "The mixer is connected, but RC2FX isn't running on it";
        assert_eq!(connection_status(true, "").0, ACCENT);
        // Before, during and after a background retry with no new result:
        for _ in 0..3 {
            assert_eq!(
                connection_status(true, failure),
                (WARN, format!("Last check: {failure}"))
            );
        }
        assert_eq!(connection_status(false, failure).0, FAINT);
        assert_eq!(
            connection_status(true, "USB check failed").1,
            "Last check: USB check failed"
        );
    }

    #[test]
    fn experimental_duo_is_off_by_default_and_persists_without_losing_view_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RC2FX/view.json");
        assert!(!duo_enabled(&read_view(Some(&path))));
        save_duo(&path, true).unwrap();
        assert!(duo_enabled(&read_view(Some(&path))));
        fs::write(
            &path,
            r#"{"channelFilter":"outputs","showTelemetry":true,"experimentalDuoFlash":true}"#,
        )
        .unwrap();
        save_duo(&path, false).unwrap();
        let view = read_view(Some(&path));
        assert!(!duo_enabled(&view));
        assert_eq!(view["channelFilter"], "outputs");
        assert_eq!(view["showTelemetry"], true);
        for invalid in [
            r#"{"experimentalDuoFlash":"true"}"#,
            r#"{"experimentalDuoFlash":1}"#,
            "broken",
            "[]",
        ] {
            fs::write(&path, invalid).unwrap();
            assert!(!duo_enabled(&read_view(Some(&path))));
        }
    }

    #[test]
    fn developer_mode_is_off_by_default_and_persists_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("RC2FX/view.json");
        assert!(!developer_enabled(&read_view(Some(&path))));
        save_duo(&path, true).unwrap();
        save_flag(&path, "developerMode", true).unwrap();
        let view = read_view(Some(&path));
        assert!(developer_enabled(&view) && duo_enabled(&view));
        save_flag(&path, "developerMode", false).unwrap();
        assert!(!developer_enabled(&read_view(Some(&path))));
        fs::write(&path, r#"{"developerMode":"yes"}"#).unwrap();
        assert!(!developer_enabled(&read_view(Some(&path))));
    }

    #[test]
    fn a_failed_developer_save_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        assert!(save_flag(&blocker.join("view.json"), "developerMode", true).is_err());
    }
}
