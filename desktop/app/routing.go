package app

// Global RC2FX processing toggle in Settings.

import (
	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/s"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func (a *App) routingReady(enabled bool) bool {
	if !a.session.Connected || !a.streamConnected || a.routingPending != nil || a.transitionBusy() {
		return false
	}
	saved := a.session.Saved
	if saved == nil || !saved.Engine.Mode.HasProcessingControls() {
		return false
	}
	if enabled {
		return saved.CanEnableProcessing() &&
			// Stopped DSP emits no meters; rely on a live stream and recovery eligibility instead.
			a.engineSeen != nil &&
			!a.session.Dirty() && !a.session.Conflict && a.pending == nil && !a.presets.writing() && !a.fileBusy
	}
	return saved.CanDisableProcessing()
}

func (a *App) changeRouting(cx *gk.Cx, enabled bool) {
	if !a.routingReady(enabled) {
		return
	}
	pending, err := a.session.BeginRouting(enabled)
	if err != nil {
		a.message = err.Error()
	} else if a.command(transport.CommandRouting{Generation: a.generation, Request: pending.Request}) {
		a.routingPending = &pending
		if enabled {
			a.message = "Starting mixer processing…"
		} else {
			a.message = "Returning to stock audio…"
		}
	}
	a.notify(cx)
}

func (a *App) routingReply(state core.DeviceState, err error) {
	if a.routingPending == nil {
		return
	}
	pending := *a.routingPending
	a.routingPending = nil
	previous, hadPrevious := savedGeneration(a.session.Saved)
	if err == nil {
		err = a.session.AcknowledgeRouting(pending, state)
	}
	if err == nil {
		a.message = "Processing request accepted • waiting for mixer status"
	} else {
		a.message = err.Error() + " Checking mixer status; the request will not be retried automatically."
	}
	if now, has := savedGeneration(a.session.Saved); has != hadPrevious || now != previous {
		a.engineSeen = nil
		a.meter = nil
		clear(a.meterProgress)
		a.levels.Clear()
	}
	a.command(transport.CommandRefresh{Generation: a.generation})
}

func (a *App) processingRow(cx *gk.Cx) (gk.DivEl, bool) {
	saved := a.session.Saved
	if saved == nil || !saved.Engine.Mode.HasProcessingControls() {
		return gk.DivEl{}, false
	}
	phase := saved.Routing.Status
	enabled := phase == core.RoutingPhaseActive || phase == core.RoutingPhaseStarting ||
		(saved.Engine.Mode == core.EngineModeNative && phase == core.RoutingPhaseError && saved.CanDisableProcessing())
	ready := a.routingReady(!enabled)
	var label string
	switch {
	case !a.streamConnected:
		label = "Connection interrupted"
	case a.transitionBusy():
		label = "Changing processing…"
	case a.routingPending != nil:
		label = "Updating…"
	case (a.pending != nil || a.presets.writing()) &&
		(phase == core.RoutingPhaseBypassing || phase == core.RoutingPhaseStarting):
		label = "Applying effects…"
	default:
		switch phase {
		case core.RoutingPhaseActive:
			label = "Enabled"
		case core.RoutingPhaseBypassed:
			label = "Disabled · stock audio"
		case core.RoutingPhaseStarting:
			label = "Starting processing…"
		case core.RoutingPhaseBypassing:
			label = "Restoring stock audio…"
		case core.RoutingPhaseError:
			label = "Needs attention"
		case core.RoutingPhaseUnmanaged:
			label = "Unavailable"
		}
	}
	hint := "Enable RC2FX effects processing."
	switch {
	case !a.streamConnected:
		hint += " Waiting for the mixer connection."
	case phase == core.RoutingPhaseBypassed && !ready:
		if a.session.Dirty() || a.session.Conflict {
			hint += " Waiting for your changes to save to the mixer."
		} else {
			hint += " Waiting for the mixer to finish saving and become ready."
		}
	case phase == core.RoutingPhaseError || phase == core.RoutingPhaseUnmanaged:
		hint += " " + saved.Routing.Message
	}
	// Only show status text for non-steady states; the switch already conveys on/off.
	steady := a.streamConnected && (label == "Enabled" || label == "Disabled · stock audio")
	sw := toggle("rc2fx-processing", enabled, false, !ready)
	if ready {
		sw = sw.OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.changeRouting(cx, !enabled) })
	}
	controls := gk.HFlex(s.ItemsCenter, s.Gap3)
	if !steady {
		controls = controls.Child(gk.Div().TextSize(px(13)).TextColor(rgb(Warn)).Child(txt(label)))
	}
	return row(labeled("Processing", hint), controls.Child(sw)), true
}
