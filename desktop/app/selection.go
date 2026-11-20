package app

// Per-channel and bulk processing-path selection on the mixer.

import (
	"slices"
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func (a *App) selectionBusy() bool {
	return a.selectionPending != nil ||
		(a.session.Saved != nil && a.session.Saved.Selection.Status == core.SelectionPhaseChanging)
}

func (a *App) transitionBusy() bool { return a.restartBusy() || a.selectionBusy() }

func (a *App) selectionReady() bool {
	if !(a.session.Connected && a.streamConnected && a.engineSeen != nil && !a.transitionBusy() &&
		a.pending == nil && a.routingPending == nil && !a.presets.writing() && !a.fileBusy && !a.session.Conflict) {
		return false
	}
	if !a.session.SelectionDirty() &&
		!(a.session.Saved != nil && a.session.Saved.Selection.Status == core.SelectionPhaseError) {
		return false
	}
	_, err := a.session.BeginSelection()
	return err == nil
}

func (a *App) changeSelection(cx *gk.Cx) {
	if !a.selectionReady() {
		return
	}
	pending, err := a.session.BeginSelection()
	if err != nil {
		a.message = err.Error()
	} else if a.command(transport.CommandSelection{Generation: a.generation, Request: pending.Request}) {
		a.selectionPending = &pending
		a.selectionAsked = true
		a.meter = nil
		clear(a.meterProgress)
		a.levels.Clear()
		a.message = "Requesting processing paths. Local effect edits are kept; saved effects will be used."
	}
	a.notify(cx)
}

func (a *App) selectionReply(state core.DeviceState, err error) {
	previous, hadPrevious := savedGeneration(a.session.Saved)
	if a.selectionPending == nil {
		return
	}
	pending := *a.selectionPending
	a.selectionPending = nil
	if err == nil {
		err = a.session.AcknowledgeSelection(pending, state)
	}
	if err == nil {
		a.message = "Path change accepted • waiting for the mixer's result"
	} else {
		a.message = err.Error() + " Checking mixer state; no automatic retry."
	}
	if now, has := savedGeneration(a.session.Saved); has != hadPrevious || now != previous {
		a.engineSeen = nil
	}
	a.meter = nil
	clear(a.meterProgress)
	a.levels.Clear()
	a.command(transport.CommandRefresh{Generation: a.generation})
}

func (a *App) selectionFailed() bool {
	return a.selectionAsked && a.session.Saved != nil && a.session.Saved.Selection.Status == core.SelectionPhaseError
}

func (a *App) processed(id string) bool {
	return a.session.Saved != nil && a.session.Saved.Config.IsSelected(id)
}

func (a *App) pathNote(id string) (string, bool) {
	if a.session.Connected && !a.streamConnected {
		return "Unconfirmed", true
	}
	state := a.session.Saved
	if state == nil || !a.session.Connected || !state.Config.IsSelected(id) {
		return "", false
	}
	switch {
	case slices.Contains(state.Selection.ActiveTargetIDs, id):
		if state.Routing.Status != core.RoutingPhaseActive || state.Engine.Status != core.EnginePhaseRunning {
			return "Not routed", true
		}
		return "", false
	case state.Engine.Mode == core.EngineModeNative && state.Routing.Status == core.RoutingPhaseBypassed:
		return "RC2FX off", true
	default:
		return "Not running", true
	}
}

func (a *App) canSwitchPath(id string) bool {
	state := a.session.Saved
	return state != nil && state.Selection.Allowed && slices.Contains(state.Selection.QualifiedTargetIDs, id) &&
		a.session.Connected && a.streamConnected && a.engineSeen != nil && !a.transitionBusy() &&
		a.pending == nil && a.routingPending == nil && !a.presets.writing() && !a.fileBusy && !a.session.Conflict
}

func (a *App) switchPath(cx *gk.Cx, id string, on bool) {
	if !a.canSwitchPath(id) {
		return
	}
	saved := a.session.Saved
	if saved == nil {
		return
	}
	selected := slices.DeleteFunc(slices.Clone(saved.Config.SelectedTargets), func(v string) bool { return v == id })
	if on {
		selected = append(selected, id)
	}
	ids, err := saved.Config.CanonicalSelection(selected)
	if err != nil {
		a.message = err.Error()
		a.notify(cx)
		return
	}
	a.session.SelectionDraft = ids
	a.changeSelection(cx)
}

func (a *App) selectionNotice(cx *gk.Cx) (gk.DivEl, bool) {
	state := a.session.Saved
	if state == nil {
		return gk.DivEl{}, false
	}
	failed := a.selectionFailed()
	if a.selectionBusy() || !(a.session.SelectionDirty() || failed) {
		return gk.DivEl{}, false
	}
	changed := edit.ChangedPaths(state.Config.SelectedTargets, a.session.SelectionDraft)
	names := make([]string, 0, len(changed))
	for _, id := range changed {
		name := id
		for i := range state.Config.Targets {
			if state.Config.Targets[i].ID == id {
				name = a.targetName(&state.Config.Targets[i])
				break
			}
		}
		names = append(names, name)
	}
	title, detail := "Changing processed channels failed", state.Selection.Message
	if a.session.SelectionDirty() {
		title, detail = "Processed channels differ from the mixer", "Changes: "+strings.Join(names, ", ")
	}
	c := card().BorderColor(rgb(Warn)).
		Child(gk.Div(s.FontSemibold).TextColor(rgb(Warn)).Child(txt(title))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).Child(txt(detail)))
	if _, err := state.Selection.CheckCapacity(state.Config, a.session.SelectionDraft); err != nil {
		c = c.Child(gk.Div(s.TextSm).TextColor(rgb(Warn)).Child(txt(err.Error())))
	}
	apply := "Retry"
	if a.session.SelectionDirty() {
		apply = "Use these channels"
	}
	return c.Child(gk.HFlex(s.Gap2).
		Child(gk.Button("apply-path-selection").Primary().Label(apply).
			Disabled(!a.selectionReady()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.changeSelection(cx) })).
		Child(gk.Button("reset-path-selection").Ghost().Label("Keep the mixer's").
			Disabled(!a.session.SelectionDirty() || a.transitionBusy()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if saved := a.session.Saved; saved != nil {
					a.session.SelectionDraft = slices.Clone(saved.Config.SelectedTargets)
				}
				a.notify(cx)
			}))), true
}
