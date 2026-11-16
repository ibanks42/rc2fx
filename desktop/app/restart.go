package app

// restart.rs: restarting processing on the mixer when its engine stopped,
// and the panel shown while processing changes state.

import (
	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func (a *App) restartCandidate() bool {
	return a.session.Saved != nil && edit.RestartCandidate(a.session.Saved)
}

func (a *App) processingTransitionLabel() string {
	saved := a.session.Saved
	switch {
	case saved != nil && saved.Routing.Status == core.RoutingPhaseStarting:
		return "Enabling processing"
	case saved != nil && saved.Routing.Status == core.RoutingPhaseBypassing:
		return "Disabling processing"
	case a.restartPending != nil:
		return "Restart requested"
	case saved != nil && saved.Engine.Mode == core.EngineModeNative:
		return "Updating channel processing"
	}
	return "Restarting processing"
}

func (a *App) restartBusy() bool {
	return a.restartPending != nil || (a.session.Saved != nil && a.session.Saved.ProcessingTransitionPending())
}

func (a *App) restartReady() bool {
	return a.session.Connected && a.streamConnected &&
		// A stopped engine may emit no more status changes. The live event
		// stream connection plus an observed generation is sufficient to
		// offer the action; the server rechecks generation and recovery.
		a.engineSeen != nil &&
		!a.transitionBusy() && a.pending == nil && a.routingPending == nil && !a.presets.writing() &&
		!a.fileBusy && a.restartCandidate()
}

func (a *App) restartEngine(cx *gk.Cx) {
	if !a.restartReady() {
		return
	}
	pending, err := a.session.BeginRestart()
	if err != nil {
		a.message = err.Error()
	} else if a.command(transport.CommandRestartEngine{Generation: a.generation, Request: pending.Request}) {
		a.restartPending = &pending
		a.meter = nil
		clear(a.meterProgress)
		a.levels.Clear()
		a.message = "Requesting processing restart with the mixer's saved settings…"
	}
	a.notify(cx)
}

func (a *App) restartReply(state core.DeviceState, err error) {
	if a.restartPending == nil {
		return
	}
	pending := *a.restartPending
	a.restartPending = nil
	previous, hadPrevious := savedGeneration(a.session.Saved)
	if err == nil {
		err = a.session.AcknowledgeRestart(pending, state)
	}
	if err == nil {
		a.message = "Restart accepted • waiting for current processing and routing status"
	} else {
		a.message = err.Error() + " Checking the mixer state; the request will not be retried automatically."
	}
	if now, has := savedGeneration(a.session.Saved); has != hadPrevious || now != previous {
		a.engineSeen = nil
		a.meter = nil
		clear(a.meterProgress)
		a.levels.Clear()
	}
	// Reads reconcile an interrupted stream or uncertain request result.
	a.command(transport.CommandRefresh{Generation: a.generation})
}

// telemetryNextStep is the telemetry status's next step when processing has
// stopped: a restart, when the mixer allows one. nil when there is nothing
// to do.
func (a *App) telemetryNextStep(cx *gk.Cx) gk.Element {
	if a.restartBusy() || !a.restartCandidate() {
		return nil
	}
	return gk.Button("telemetry-restart").Small().Outline().Label("Restart processing").
		Disabled(!a.restartReady()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.restartEngine(cx) })
}

func (a *App) restartPanel(cx *gk.Cx) gk.Element {
	panel := gk.VFlex(s.Gap2)
	if a.selectionBusy() {
		return panel
	}
	saved := a.session.Saved
	switch {
	case a.restartBusy():
		var detail string
		switch {
		case saved != nil && saved.Routing.Status == core.RoutingPhaseBypassing:
			detail = "Restoring stock audio and stopping custom processing. Your saved settings stay on the mixer."
		case saved != nil && saved.Routing.Status == core.RoutingPhaseStarting:
			detail = "Starting custom processing with the mixer's saved settings. Controls will become available when it is ready."
		case saved != nil && saved.Engine.Mode == core.EngineModeNative:
			detail = "Updating only the changed channels. Other channels continue playing."
		default:
			detail = "The mixer is checking audio recovery and restarting with its saved settings. Settings changes are paused until it finishes."
		}
		panel = panel.Style(s.P4, s.RoundedXl, s.Border1).Bg(rgb(surface().Panel)).
			BorderColor(rgb(surface().Line)).
			Child(gk.Div(s.FontSemibold).Child(txt(a.processingTransitionLabel()))).
			Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).Child(txt(detail)))
		if saved != nil {
			panel = panel.Child(gk.Div(s.TextSm).Child(txt(saved.Engine.Message)))
		}
	case a.restartCandidate():
		panel = panel.Style(s.P4, s.RoundedXl, s.Border1).Bg(rgb(surface().Panel)).
			BorderColor(rgb(surface().Line)).
			Child(gk.Button("restart-engine").Label("Restart processing").Disabled(!a.restartReady()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.restartEngine(cx) })).
			Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).
				Child(txt("Restart using the mixer's saved settings. Your local draft is kept. Audio recovery is checked again before processing starts.")))
	}
	return panel
}
