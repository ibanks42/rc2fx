package app

// Status banner shown while the mixer's audio processing is transitioning state.

import (
	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func (a *App) processingTransitionLabel() string {
	saved := a.session.Saved
	switch {
	case saved != nil && saved.Routing.Status == core.RoutingPhaseStarting:
		return "Enabling processing"
	case saved != nil && saved.Routing.Status == core.RoutingPhaseBypassing:
		return "Disabling processing"
	case saved != nil && saved.Engine.Mode == core.EngineModeNative:
		return "Updating channel processing"
	}
	return "Restarting processing"
}

func (a *App) restartBusy() bool {
	return a.session.Saved != nil && a.session.Saved.ProcessingTransitionPending()
}

func (a *App) restartPanel() gk.Element {
	panel := gk.VFlex(s.Gap2)
	if a.selectionBusy() || !a.restartBusy() {
		return panel
	}
	saved := a.session.Saved
	var detail string
	switch {
	case saved.Routing.Status == core.RoutingPhaseBypassing:
		detail = "Restoring stock audio and stopping custom processing. Your saved settings stay on the mixer."
	case saved.Routing.Status == core.RoutingPhaseStarting:
		detail = "Starting custom processing with the mixer's saved settings. Controls will become available when it is ready."
	case saved.Engine.Mode == core.EngineModeNative:
		detail = "Updating only the changed channels. Other channels continue playing."
	default:
		detail = "The mixer is checking audio recovery and restarting with its saved settings. Settings changes are paused until it finishes."
	}
	return panel.Style(s.P4, s.RoundedXl, s.Border1).Bg(rgb(surface().Panel)).
		BorderColor(rgb(surface().Line)).
		Child(gk.Div(s.FontSemibold).Child(txt(a.processingTransitionLabel()))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).Child(txt(detail))).
		Child(gk.Div(s.TextSm).Child(txt(saved.Engine.Message)))
}
