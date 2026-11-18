package app

// presets.rs: presets saved on the mixer: save the current settings, then
// load or delete each one.

import (
	"fmt"
	"slices"
	"strings"
	"unicode"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

// presetPendingKind is which preset request is out.
type presetPendingKind int

const (
	presetPendingList presetPendingKind = iota
	presetPendingCreate
	presetPendingLoad
	presetPendingDelete
)

// presetPending is presets.rs Pending; revision and original belong to a
// load.
type presetPending struct {
	kind     presetPendingKind
	revision uint64
	original []core.Target
}

// PresetLibrary is the preset list and the request that is out.
type PresetLibrary struct {
	items         []core.PresetSummary
	name          gk.InputState
	pending       *presetPending
	confirmDelete *string
}

// newPresetLibrary is PresetLibrary::new (makes its name input: call in
// init).
func newPresetLibrary(cx *gk.Cx) PresetLibrary {
	return PresetLibrary{name: gk.NewInputState(cx).Placeholder("Preset name").Build()}
}

func (p *PresetLibrary) reset() {
	p.items = nil
	p.pending = nil
	p.confirmDelete = nil
}

func (p *PresetLibrary) busy() bool { return p.pending != nil }

func (p *PresetLibrary) loadingRevision() *uint64 {
	if p.pending != nil && p.pending.kind == presetPendingLoad {
		revision := p.pending.revision
		return &revision
	}
	return nil
}

func (p *PresetLibrary) writing() bool {
	return p.busy() && p.pending.kind != presetPendingList
}

func (a *App) presetsReady() bool {
	return a.session.Connected && a.streamConnected && a.pending == nil && !a.presets.busy() &&
		!a.fileBusy && !a.transitionBusy()
}

func (a *App) listPresets() {
	if !a.session.Connected || a.presets.busy() {
		return
	}
	if a.command(transport.CommandPreset{Generation: a.generation, Action: transport.PresetActionList{}}) {
		a.presets.pending = &presetPending{kind: presetPendingList}
	}
}

func (a *App) createPreset(cx *gk.Cx) {
	if !a.presetsReady() || a.session.Dirty() || a.session.Conflict {
		return
	}
	name := strings.TrimFunc(a.presets.name.Value(cx), unicode.IsSpace)
	if name == "" || len(name) > 128 {
		a.message = "Enter a preset name of 1–128 bytes."
		a.notify(cx)
		return
	}
	saved := a.session.Saved
	if saved == nil {
		return
	}
	if a.command(transport.CommandPreset{Generation: a.generation,
		Action: transport.PresetActionCreate{Name: name, ExpectedRevision: saved.Config.Revision}}) {
		a.presets.pending = &presetPending{kind: presetPendingCreate}
		a.message = "Saving a preset of the mixer’s current settings…"
	}
	a.notify(cx)
}

func (a *App) loadPreset(cx *gk.Cx, id string) {
	if !a.presetsReady() || a.session.Dirty() || a.session.Conflict {
		return
	}
	saved := a.session.Saved
	if saved == nil {
		return
	}
	revision := saved.Config.Revision
	original := core.CloneTargets(a.session.Draft)
	if a.command(transport.CommandPreset{Generation: a.generation,
		Action: transport.PresetActionApply{ID: id, ExpectedRevision: revision}}) {
		a.presets.pending = &presetPending{kind: presetPendingLoad, revision: revision, original: original}
		a.message = "Loading preset on the mixer…"
	}
	a.notify(cx)
}

func (a *App) deletePreset(cx *gk.Cx, id string) {
	if !a.presetsReady() || a.presets.confirmDelete == nil || *a.presets.confirmDelete != id {
		return
	}
	if a.command(transport.CommandPreset{Generation: a.generation, Action: transport.PresetActionDelete{ID: id}}) {
		a.presets.pending = &presetPending{kind: presetPendingDelete}
		a.message = "Deleting preset…"
	}
	a.notify(cx)
}

// refreshAfterPreset asks for the mixer's settings after a preset reply
// that leaves them uncertain.
func (a *App) refreshAfterPreset() {
	a.awaitingRefresh = a.command(transport.CommandRefresh{Generation: a.generation}) || a.awaitingRefresh
}

func (a *App) presetReply(cx *gk.Cx, reply transport.PresetReply, err *transport.PresetError) {
	pending := a.presets.pending
	if pending == nil {
		return
	}
	a.presets.pending = nil
	wasLoading := pending.kind == presetPendingLoad
	previous := core.CloneTargets(a.session.Draft)
	unexpected := false
	if err != nil {
		a.message = err.Message
		if err.Conflict || wasLoading {
			a.session.Conflict = true
			a.refreshAfterPreset()
		}
	} else {
		switch r := reply.(type) {
		case transport.PresetReplyList:
			if pending.kind != presetPendingList {
				unexpected = true
				break
			}
			a.presets.items = r.Presets
		case transport.PresetReplyCreated:
			if pending.kind != presetPendingCreate {
				unexpected = true
				break
			}
			a.message = fmt.Sprintf("Preset ‘%s’ saved on the mixer", r.Preset.Name)
			a.presets.name.SetValue(cx, "")
			a.listPresets()
		case transport.PresetReplyDeleted:
			if pending.kind != presetPendingDelete {
				unexpected = true
				break
			}
			a.presets.items = slices.DeleteFunc(a.presets.items, func(p core.PresetSummary) bool { return p.ID == r.ID })
			a.presets.confirmDelete = nil
			a.message = "Preset deleted • current mixer settings are unchanged"
		case transport.PresetReplyApplied:
			if pending.kind != presetPendingLoad {
				unexpected = true
				break
			}
			prevGen, hadGen := savedGeneration(a.session.Saved)
			state, perr := core.DeviceStateFromJSON(string(r.State))
			if perr == nil {
				perr = a.session.AcknowledgePreset(pending.revision, pending.original, state)
			}
			if now, has := savedGeneration(a.session.Saved); has != hadGen || now != prevGen {
				a.engineSeen = nil
				a.meter = nil
				clear(a.meterProgress)
				a.levels.Clear()
			}
			switch {
			case perr != nil:
				a.message = perr.Error()
				a.session.Conflict = true
			case a.session.Conflict:
				a.message = "Preset loaded. Your newer edits were kept and are saved on top of it."
			default:
				a.message = "Preset saved to mixer • waiting for applied telemetry"
			}
			// A stream event can precede the request's reply, including a
			// later controller's write. Refresh after resolving our snapshot.
			a.refreshAfterPreset()
		default:
			unexpected = true
		}
	}
	if unexpected {
		a.message = "Unexpected preset response; reload mixer settings."
		a.session.Conflict = true
		a.refreshAfterPreset()
	}
	if !core.TargetsEqual(previous, a.session.Draft) {
		a.selected = min(a.selected, len(a.session.Draft)-1)
		a.importDraft = nil
		a.rebuildEditors(cx)
	}
}

// presetPanel is the presets saved on the mixer: save the current settings,
// then load or delete each one.
func (a *App) presetPanel(cx *gk.Cx) gk.Element {
	ready := a.presetsReady()
	canSnapshot := ready && !a.session.Dirty() && !a.session.Conflict
	refresh := gk.Button("refresh-presets").Ghost().Small().Icon(gk.IconRefreshCw).Tooltip("Refresh").
		Disabled(!a.session.Connected || a.presets.busy()).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.listPresets()
			a.notify(cx)
		})
	muted := func(text string) gk.DivEl {
		return gk.Div(s.Px4, s.Py3, s.TextSm).TextColor(rgb(Muted)).Child(txt(text))
	}
	if !a.session.Connected {
		return settingsSection("PRESETS", refresh, gk.Div().Child(muted("Not connected.")))
	}
	body := gk.Div().Child(row(gk.Div().Child(gk.Input(a.presets.name)),
		gk.Button("create-preset").Label("Save preset").
			Disabled(!canSnapshot || len(a.presets.items) >= 128).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.createPreset(cx) })))
	if a.session.Dirty() || a.session.Conflict {
		body = body.Child(gk.Div(s.Px4, s.Py2, s.TextSm, s.BorderB1).TextColor(rgb(Warn)).
			BorderColor(rgb(surface().Line)).
			Child(txt("Presets can be saved or loaded once your edits are saved to the mixer.")))
	}
	list := gk.VFlex().MaxH(px(320))
	if len(a.presets.items) == 0 {
		if a.presets.busy() {
			list = list.Child(muted("Loading…"))
		} else {
			list = list.Child(muted("No presets yet."))
		}
	}
	last := max(len(a.presets.items)-1, 0)
	for index, preset := range a.presets.items {
		id := preset.ID
		confirm := a.presets.confirmDelete != nil && *a.presets.confirmDelete == id
		actions := gk.HFlex(s.Gap1, s.ItemsCenter)
		if confirm {
			actions = actions.
				Child(gk.Button(gk.Key("cancel-delete", index)).Ghost().Label("Cancel").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
						a.presets.confirmDelete = nil
						a.notify(cx)
					})).
				Child(gk.Button(gk.Key("delete-preset", index)).Danger().Label("Delete").
					Disabled(!ready).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.deletePreset(cx, id) }))
		} else {
			actions = actions.
				Child(gk.Button(gk.Key("load-preset", index)).Label("Load").
					Disabled(!canSnapshot).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.loadPreset(cx, id) })).
				Child(gk.Button(gk.Key("delete-preset", index)).Ghost().Icon(gk.IconDelete).Tooltip("Delete").
					Disabled(!ready).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
						a.presets.confirmDelete = &id
						a.notify(cx)
					}))
		}
		r := row(txt(preset.Name), actions)
		if index == last {
			r = r.Style(s.BorderB0)
		}
		list = list.Child(r)
	}
	return settingsSection("PRESETS", refresh, body.Child(list.ID("preset-list").OverflowYScroll()))
}
