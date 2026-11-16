//! Repair mixer: for a mixer a flash left unable to keep its setup, or to
//! start. The user holds the power button for 15 seconds so the bootloader
//! shows the mixer's storage over USB; the administrator helper then checks
//! it and restores damaged files (rc2fx_installer's `repair`). Reached from
//! the first screen whenever RC2FX isn't running.
//!
//! The page is a step sidebar on the left and the current step on the
//! right. The installer will move to the same layout; [`step_sidebar`] is
//! shared for that.
use super::*;
use rc2fx_installer::{RepairFindings, RepairResult};

/// How often the page looks for the mixer's storage while waiting.
const LOOK_EVERY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Stage {
    /// Waiting for the mixer's storage to appear.
    Waiting,
    Checking,
    Findings(RepairFindings),
    Repairing,
    Done(RepairResult),
    Failed(String),
}

pub(super) struct Repair {
    pub stage: Stage,
    last_look: Option<Instant>,
    /// The size of the mixer's storage while it is visible.
    pub disk: Option<u64>,
}

impl Repair {
    pub fn new() -> Self {
        Self {
            stage: Stage::Waiting,
            last_look: None,
            disk: None,
        }
    }

    pub(super) fn busy(&self) -> bool {
        matches!(self.stage, Stage::Checking | Stage::Repairing)
    }
}

/// Requests that belong to the repair page, not the install flow.
pub(super) fn is_repair(request: &Request) -> bool {
    matches!(request, Request::RepairInspect | Request::Repair { .. })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mark {
    Done,
    Now,
    Failed,
    Pending,
}

/// The steps of a guided task down the left: done, current, failed or to
/// come. Repair uses it now; the installer will too.
pub(super) fn step_sidebar(steps: &[(&str, Mark)]) -> Div {
    let mut list = div().v_flex().gap_1().w(px(220.)).flex_shrink_0().pr_6();
    for (n, (title, mark)) in steps.iter().enumerate() {
        let (circle, text) = match mark {
            Mark::Done => (
                div()
                    .bg(rgb(surface().raised))
                    .border_color(rgb(ACCENT))
                    .child(Icon::new(IconName::Check).text_color(rgb(ACCENT)).size_3()),
                MUTED,
            ),
            Mark::Now => (
                div()
                    .border_color(rgb(ACCENT))
                    .text_color(rgb(ACCENT))
                    .child((n + 1).to_string()),
                TEXT,
            ),
            Mark::Failed => (
                div().border_color(rgb(DANGER)).child(
                    Icon::new(IconName::CircleX)
                        .text_color(rgb(DANGER))
                        .size_3(),
                ),
                TEXT,
            ),
            Mark::Pending => (
                div()
                    .border_color(rgb(surface().line))
                    .text_color(rgb(FAINT))
                    .child((n + 1).to_string()),
                FAINT,
            ),
        };
        list = list.child(
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .py_2()
                .child(
                    circle
                        .size(px(22.))
                        .flex_shrink_0()
                        .rounded_full()
                        .border_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs(),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(text))
                        .when(*mark == Mark::Now, |d| d.font_semibold())
                        .child(title.to_string()),
                ),
        );
    }
    list
}

fn panel() -> Div {
    div()
        .v_flex()
        .gap_2()
        .mt_4()
        .p_4()
        .rounded_xl()
        .bg(rgb(surface().panel))
        .border_1()
        .border_color(rgb(surface().line))
}

fn row(color: u32, text: impl Into<SharedString>) -> Div {
    div()
        .h_flex()
        .items_start()
        .gap_3()
        .py_1()
        .text_sm()
        .text_color(rgb(MUTED))
        .child(
            div()
                .mt(px(6.))
                .size(px(8.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(color)),
        )
        .child(div().flex_1().min_w_0().child(text.into()))
}

fn heading(text: impl Into<SharedString>) -> Div {
    div().text_xl().font_semibold().child(text.into())
}

impl Rc2FxApp {
    /// Open the installer straight on the repair page.
    pub(crate) fn open_repair(&mut self, cx: &mut Context<Self>) {
        self.open_installer(cx);
        if let Some(installer) = self.installer.as_mut() {
            installer.repair = Some(Repair::new());
        }
        self.installer_page = true;
        cx.notify();
    }

    /// Switch the open installer to the repair page.
    pub(super) fn show_repair(&mut self, cx: &mut Context<Self>) {
        if let Some(installer) = self.installer.as_mut().filter(|i| !i.busy()) {
            installer.repair = Some(Repair::new());
        }
        cx.notify();
    }

    /// Look for the mixer's storage while waiting, and start the check once
    /// it is there and access is granted. Returns whether to redraw.
    pub(crate) fn poll_repair(&mut self) -> bool {
        let Some(installer) = self.installer.as_mut() else {
            return false;
        };
        let allowed = installer.access.allows_flashing();
        let Some(repair) = installer.repair.as_mut() else {
            return false;
        };
        if repair.stage != Stage::Waiting
            || repair.last_look.is_some_and(|t| t.elapsed() < LOOK_EVERY)
        {
            return false;
        }
        repair.last_look = Some(Instant::now());
        // Sysfs, IOKit or SetupAPI: readable without administrator access.
        let disk = rc2fx_installer::platform::host()
            .recovery_disk()
            .ok()
            .flatten();
        let changed = disk != repair.disk;
        repair.disk = disk;
        if disk.is_some() && allowed {
            repair.stage = Stage::Checking;
            installer.progress = None;
            if installer.runner.send(Request::RepairInspect).is_err() {
                repair.stage = Stage::Failed(
                    "The installer stopped unexpectedly. Close and reopen it.".into(),
                );
            }
            return true;
        }
        changed
    }

    /// A finished repair request.
    pub(super) fn repair_outcome(
        installer: &mut Installer,
        request: &Request,
        result: std::result::Result<Reply, String>,
    ) {
        let Some(repair) = installer.repair.as_mut() else {
            return;
        };
        repair.stage = match (request, result) {
            (_, Err(e)) if e == rc2fx_installer::CANCELLED => Stage::Waiting,
            (_, Err(e)) => Stage::Failed(e),
            (Request::RepairInspect, Ok(Reply::RepairFindings(f))) => Stage::Findings(f),
            (Request::Repair { .. }, Ok(Reply::Repaired(r))) => Stage::Done(r),
            _ => Stage::Failed("The installer gave an unexpected answer.".into()),
        };
        installer.progress = None;
    }

    fn start_repair(&mut self, cx: &mut Context<Self>) {
        let Some(installer) = self.installer.as_mut() else {
            return;
        };
        let Some(repair) = installer.repair.as_mut() else {
            return;
        };
        // What the user saw and approved; the helper checks each again.
        let Stage::Findings(findings) = &repair.stage else {
            return;
        };
        let files = findings.targets();
        repair.stage = Stage::Repairing;
        installer.progress = None;
        if installer.runner.send(Request::Repair { files }).is_err() {
            repair.stage =
                Stage::Failed("The installer stopped unexpectedly. Close and reopen it.".into());
        }
        cx.notify();
    }

    fn repair_again(&mut self, cx: &mut Context<Self>) {
        if let Some(repair) = self.installer.as_mut().and_then(|i| i.repair.as_mut()) {
            *repair = Repair::new();
        }
        cx.notify();
    }

    pub(super) fn repair_screen(
        &self,
        installer: &Installer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(repair) = installer.repair.as_ref() else {
            return div().into_any_element();
        };
        let access_step = installer.access != Access::NotNeeded;
        let need_access = !installer.access.allows_flashing();
        let reached = match &repair.stage {
            _ if need_access => 0,
            Stage::Waiting => 1,
            Stage::Checking | Stage::Failed(_) => 2,
            // Nothing to restore: what is left is leaving storage mode.
            Stage::Findings(f) if f.repairable() == 0 => 4,
            Stage::Findings(_) | Stage::Repairing => 3,
            Stage::Done(_) => 4,
        };
        let failed = matches!(repair.stage, Stage::Failed(_));
        let titles = [
            "Administrator access",
            "Storage mode",
            "Check",
            "Back up and repair",
            "Restart the mixer",
        ];
        let steps: Vec<(&str, Mark)> = titles
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > 0 || access_step)
            .map(|(i, title)| {
                let mark = match i.cmp(&reached) {
                    std::cmp::Ordering::Less => Mark::Done,
                    std::cmp::Ordering::Equal if failed => Mark::Failed,
                    std::cmp::Ordering::Equal => Mark::Now,
                    std::cmp::Ordering::Greater => Mark::Pending,
                };
                (*title, mark)
            })
            .collect();
        let body = if need_access {
            self.access_card(
                installer,
                "Administrator access lets RC2FX read the mixer's storage while it is in storage mode, and restore damaged files.",
                cx,
            )
        } else {
            self.repair_body(installer, repair, cx)
        };
        let busy = repair.busy();
        div()
            .v_flex()
            .size_full()
            .relative()
            .font_family("Inter")
            .bg(onboarding::background())
            .child(onboarding::backdrop())
            .text_color(rgb(TEXT))
            .child(
                div()
                    .id("repair-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_8()
                    .py_8()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(860.))
                            .mx_auto()
                            .v_flex()
                            .gap_6()
                            .child(div().text_2xl().font_semibold().child("Repair mixer"))
                            .child(
                                div()
                                    .h_flex()
                                    .items_start()
                                    .child(step_sidebar(&steps))
                                    .child(div().flex_1().min_w_0().child(body)),
                            ),
                    ),
            )
            .child(
                div().flex_shrink_0().px_8().pb_6().child(
                    div()
                        .w_full()
                        .max_w(px(860.))
                        .mx_auto()
                        .h_flex()
                        .justify_end()
                        .child(
                            Button::new("repair-close")
                                .ghost()
                                .label("Close")
                                .disabled(busy)
                                .on_click(cx.listener(|app, _, _, cx| app.close_installer(cx))),
                        ),
                ),
            )
            .into_any_element()
    }

    fn repair_body(&self, installer: &Installer, repair: &Repair, cx: &mut Context<Self>) -> Div {
        let working = |what: &str| {
            let line = installer
                .progress
                .as_ref()
                .map(progress_text)
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| what.to_owned());
            let mut body = div()
                .v_flex()
                .gap_3()
                .child(heading(what.to_owned()))
                .child(
                    div()
                        .h_flex()
                        .gap_3()
                        .items_center()
                        .child(Spinner::new().color(rgb(ACCENT).into()))
                        .child(paragraph(line, MUTED)),
                );
            if let Some(p) = installer.progress.as_ref().filter(|p| p.total > 0) {
                body = body.child(
                    ProgressBar::new("repair-progress")
                        .value((p.completed as f32 / p.total as f32 * 100.).clamp(0., 100.)),
                );
            }
            body
        };
        match &repair.stage {
            Stage::Waiting => div()
                .v_flex()
                .gap_3()
                .child(heading("Put the mixer in storage mode"))
                .child(paragraph(
                    "RC2FX checks the mixer's system for files a flash damaged, and restores them. Your settings are copied to this computer before anything is written.",
                    MUTED,
                ))
                .child(
                    panel()
                        .child(paragraph("1. Connect the mixer's USB 1 port to this computer.", MUTED))
                        .child(paragraph("2. Switch the mixer off.", MUTED))
                        .child(paragraph(
                            "3. Press and hold the power button for about 15 seconds, until the screen shows only the RØDE logo.",
                            MUTED,
                        ))
                        .child(
                            div()
                                .h_flex()
                                .gap_3()
                                .items_center()
                                .mt_2()
                                .pt_3()
                                .border_t_1()
                                .border_color(rgb(surface().line))
                                .child(Spinner::new().color(rgb(ACCENT).into()))
                                .child(paragraph("Waiting for the mixer's storage…", MUTED)),
                        ),
                ),
            Stage::Checking => working("Checking the mixer"),
            Stage::Repairing => working("Repairing the mixer"),
            Stage::Failed(e) => div()
                .v_flex()
                .gap_3()
                .child(heading("The check didn't finish"))
                .child(paragraph(e.clone(), DANGER))
                .child(
                    div().h_flex().child(
                        Button::new("repair-again")
                            .label("Check again")
                            .on_click(cx.listener(|app, _, _, cx| app.repair_again(cx))),
                    ),
                ),
            Stage::Findings(findings) => self.repair_findings(findings, cx),
            Stage::Done(result) => repair_done(result),
        }
    }

    fn repair_findings(&self, findings: &RepairFindings, cx: &mut Context<Self>) -> Div {
        let repairable = findings.repairable();
        let damaged: usize = findings.roots.iter().map(|r| r.damaged.len()).sum();
        let title = match (damaged, repairable) {
            (0, _) => "No damaged files found".to_owned(),
            (_, 0) => "Found damaged files RC2FX can't restore".to_owned(),
            (_, 1) => "Found 1 damaged file".to_owned(),
            (_, n) => format!("Found {n} damaged files"),
        };
        let mut list = panel().child(row(
            if findings.settings_ok { ACCENT } else { WARN },
            if findings.settings_ok {
                "Settings partition looks healthy: your setup is still there."
            } else {
                "Settings partition looks damaged. RC2FX doesn't repair it, but copies it to this computer first."
            },
        ));
        for root in &findings.roots {
            if root.damaged.is_empty() {
                list = list.child(row(
                    ACCENT,
                    format!(
                        "System partition {}: no damaged files (compared with {}).",
                        root.index, root.reference
                    ),
                ));
            }
            for file in &root.damaged {
                list = list.child(row(
                    if file.why.is_none() { WARN } else { DANGER },
                    match &file.why {
                        None => format!(
                            "System partition {}: {} is damaged; an identical copy restores it.",
                            root.index, file.path
                        ),
                        Some(why) => format!(
                            "System partition {}: {} is damaged, and {why}.",
                            root.index, file.path
                        ),
                    },
                ));
            }
        }
        let explanation = if damaged == 0 {
            "Nothing on the mixer's system needs restoring. If it still forgets its setup, its settings partition may be the problem."
        } else if repairable == 0 {
            "RC2FX only restores a file from an identical copy, so it leaves these alone. Flashing the mixer again replaces the whole system."
        } else {
            "Restoring writes only the damaged files, with byte-identical copies, after copying the settings partition to this computer."
        };
        div()
            .v_flex()
            .gap_3()
            .child(heading(title))
            .child(paragraph(explanation, MUTED))
            .child(list)
            .when(repairable == 0, |d| d.child(restart_steps()))
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .mt_2()
                    .when(repairable > 0, |d| {
                        d.child(
                            Button::new("repair-start")
                                .primary()
                                .label("Back up settings and repair")
                                .on_click(cx.listener(|app, _, _, cx| app.start_repair(cx))),
                        )
                    })
                    .child(
                        Button::new("repair-recheck")
                            .label("Check again")
                            .on_click(cx.listener(|app, _, _, cx| app.repair_again(cx))),
                    ),
            )
    }
}

fn repair_done(result: &RepairResult) -> Div {
    let mut list = panel();
    for path in &result.repaired {
        list = list.child(row(ACCENT, format!("Restored {path}")));
    }
    for (path, why) in &result.not_repaired {
        list = list.child(row(DANGER, format!("{path}: {why}")));
    }
    div()
        .v_flex()
        .gap_3()
        .child(heading(if result.repaired.is_empty() {
            "Nothing was restored".to_owned()
        } else {
            "Repaired. One last step.".to_owned()
        }))
        .child(list)
        .child(restart_steps())
        .child(paragraph(
            format!("Settings partition copied to {}", result.backup),
            FAINT,
        ))
}

/// Leaving storage mode: the mixer doesn't start while USB 1 holds it there.
fn restart_steps() -> Div {
    panel()
        .child(paragraph("To leave storage mode:", TEXT))
        .child(paragraph("1. Unplug USB 1 from this computer.", MUTED))
        .child(paragraph(
            "2. Disconnect the mixer's power for 30 seconds, then turn it on normally.",
            MUTED,
        ))
        .child(paragraph(
            "3. Don't redo the setup if it asks: a repaired mixer brings it back on this start.",
            MUTED,
        ))
}
