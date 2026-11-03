use super::*;

fn restart_candidate(state: &DeviceState) -> bool {
    state.engine.restart_eligible() && state.engine.generation < u64::MAX
}

impl Rc2FxApp {
    pub(super) fn processing_transition_label(&self) -> &'static str {
        match self.session.saved.as_ref().map(|s| s.routing.status) {
            Some(RoutingPhase::Starting) => "Enabling processing",
            Some(RoutingPhase::Bypassing) => "Disabling processing",
            _ if self.restart_pending.is_some() => "Restart requested",
            _ if self
                .session
                .saved
                .as_ref()
                .is_some_and(|s| s.engine.mode == EngineMode::Native) =>
            {
                "Updating channel processing"
            }
            _ => "Restarting processing",
        }
    }
    pub(super) fn restart_busy(&self) -> bool {
        self.restart_pending.is_some()
            || self
                .session
                .saved
                .as_ref()
                .is_some_and(DeviceState::processing_transition_pending)
    }

    fn restart_ready(&self) -> bool {
        self.session.connected
            && self.stream_connected
            // A stopped engine may emit no more status changes. The live event stream
            // connection plus an observed generation is sufficient to offer
            // the action; the server rechecks generation and recovery.
            && self.engine_seen.is_some()
            && !self.transition_busy()
            && self.pending.is_none()
            && self.routing_pending.is_none()
            && !self.presets.writing()
            && !self.file_busy
            && self.session.saved.as_ref().is_some_and(restart_candidate)
    }

    fn restart_engine(&mut self, cx: &mut Context<Self>) {
        if !self.restart_ready() {
            return;
        }
        match self.session.begin_restart() {
            Ok(pending) => {
                if self.command(Command::RestartEngine {
                    generation: self.generation,
                    request: pending.request.clone(),
                }) {
                    self.restart_pending = Some(pending);
                    self.meter = None;
                    self.meter_progress.clear();
                    self.levels.clear();
                    self.message =
                        "Requesting processing restart with the mixer's saved settings…".into();
                }
            }
            Err(error) => self.message = error.to_string(),
        }
        cx.notify();
    }

    pub(super) fn restart_reply(&mut self, result: std::result::Result<DeviceState, String>) {
        let Some(pending) = self.restart_pending.take() else {
            return;
        };
        let previous = self.session.saved.as_ref().map(|s| s.engine.generation);
        match result.and_then(|state| {
            self.session
                .acknowledge_restart(&pending, state)
                .map_err(|e| e.to_string())
        }) {
            Ok(()) => {
                self.message =
                    "Restart accepted • waiting for current processing and routing status".into()
            }
            Err(error) => {
                self.message = format!(
                    "{error} Checking the mixer state; the request will not be retried automatically."
                )
            }
        }
        if previous != self.session.saved.as_ref().map(|s| s.engine.generation) {
            self.engine_seen = None;
            self.meter = None;
            self.meter_progress.clear();
            self.levels.clear();
        }
        // Reads reconcile an interrupted stream or uncertain request result.
        self.command(Command::Refresh {
            generation: self.generation,
        });
    }

    pub(super) fn restart_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div().v_flex().gap_2();
        if self.selection_busy() {
            return panel;
        }
        if self.restart_busy() {
            let detail = match self.session.saved.as_ref().map(|s| s.routing.status) {
                Some(RoutingPhase::Bypassing) => {
                    "Restoring stock audio and stopping custom processing. Your saved settings stay on the mixer."
                }
                Some(RoutingPhase::Starting) => {
                    "Starting custom processing with the mixer's saved settings. Controls will become available when it is ready."
                }
                _ if self
                    .session
                    .saved
                    .as_ref()
                    .is_some_and(|s| s.engine.mode == EngineMode::Native) =>
                {
                    "Updating only the changed channels. Other channels continue playing."
                }
                _ => {
                    "The mixer is checking audio recovery and restarting with its saved settings. Settings changes are paused until it finishes."
                }
            };
            panel = panel
                .p_4()
                .rounded_xl()
                .bg(rgb(surface().panel))
                .border_1()
                .border_color(rgb(surface().line))
                .child(
                    div()
                        .font_semibold()
                        .child(self.processing_transition_label()),
                )
                .child(div().text_sm().text_color(rgb(MUTED)).child(detail));
            if let Some(saved) = &self.session.saved {
                panel = panel.child(div().text_sm().child(saved.engine.message.clone()));
            }
        } else if self.session.saved.as_ref().is_some_and(restart_candidate) {
            panel = panel.p_4().rounded_xl().bg(rgb(surface().panel)).border_1().border_color(rgb(surface().line))
                .child(Button::new("restart-engine").label("Restart processing").disabled(!self.restart_ready())
                    .on_click(cx.listener(|app, _, _, cx| app.restart_engine(cx))))
                .child(div().text_sm().text_color(rgb(MUTED)).child("Restart using the mixer's saved settings. Your local draft is kept. Audio recovery is checked again before processing starts."));
        }
        panel
    }
}

#[cfg(test)]
mod tests {
    use rc2fx_core::{EngineMode, EnginePhase, EngineStatus};
    #[test]
    fn only_explicit_backend_eligibility_on_a_failed_jack_engine_offers_restart() {
        let mut engine = EngineStatus {
            generation: 4,
            restart_allowed: true,
            mode: EngineMode::Jack,
            status: EnginePhase::Error,
            applied_revision: 1,
            sample_rate: 48000,
            block_size: 112,
            message: String::new(),
            audio_advancing: false,
        };
        assert!(engine.restart_eligible());
        for phase in [
            EnginePhase::Starting,
            EnginePhase::Restarting,
            EnginePhase::Running,
            EnginePhase::Offline,
        ] {
            engine.status = phase;
            assert!(!engine.restart_eligible());
        }
        engine.status = EnginePhase::Error;
        engine.restart_allowed = false;
        assert!(!engine.restart_eligible());
        engine.restart_allowed = true;
        engine.mode = EngineMode::Simulation;
        assert!(!engine.restart_eligible());
    }
}
