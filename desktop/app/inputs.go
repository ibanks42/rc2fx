package app

// Polls the RØDECaster's show input inventory (names and fader assignments)
// and renders the placeholder panel for sources without an RC2FX path.

import (
	"slices"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

type ShowInputs struct {
	inventory *core.StockInventory
	selected  *string
	pending   bool
	checked   *time.Time
	error     *string
}

func (in *ShowInputs) cache() edit.InventoryCache {
	return edit.InventoryCache{Inventory: in.inventory, Error: in.error, Pending: in.pending}
}

func (in *ShowInputs) visibleSources() []*core.StockSource {
	c := in.cache()
	return c.VisibleSources()
}

func (in *ShowInputs) receive(inventory core.StockInventory, err error) bool {
	c := in.cache()
	replaced := c.Receive(inventory, err)
	in.inventory, in.error, in.pending = c.Inventory, c.Error, c.Pending
	return replaced
}

func (a *App) refreshInputs() {
	if a.session.Connected && a.streamConnected && !a.inputs.pending && a.pending == nil &&
		!a.presets.busy() && a.routingPending == nil &&
		(a.inputs.checked == nil || time.Since(*a.inputs.checked) >= 5*time.Second) {
		now := time.Now()
		a.inputs.checked = &now
		a.inputs.pending = a.command(transport.CommandInputs{Generation: a.generation})
	}
}

func (a *App) receiveInputs(cx *gk.Cx, inventory core.StockInventory, err error) {
	if !a.inputs.receive(inventory, err) || a.inputs.selected == nil {
		return
	}
	id := *a.inputs.selected
	var source *core.StockSource
	for _, src := range a.showSources() {
		if src.ID == id {
			source = src
			break
		}
	}
	if source == nil {
		a.inputs.selected = nil
		return
	}
	index := slices.IndexFunc(a.session.Draft, func(t core.Target) bool { return t.ID == source.TargetID })
	if index >= 0 && a.selected != index {
		a.selected = index
		a.rebuildEditors(cx)
	}
}

func (a *App) showSources() []*core.StockSource { return a.inputs.visibleSources() }

func (a *App) targetName(target *core.Target) string {
	for _, src := range a.showSources() {
		if src.TargetID == target.ID {
			return model.DisplayName(src.Name)
		}
	}
	if label, ok := model.OutputLabel(target.ID); ok {
		return label
	}
	return model.DisplayName(target.Name)
}

func (a *App) unprocessedSource() *core.StockSource {
	if a.inputs.selected == nil {
		return nil
	}
	for _, src := range a.showSources() {
		if src.ID == *a.inputs.selected {
			if slices.ContainsFunc(a.session.Draft, func(t core.Target) bool { return t.ID == src.TargetID }) {
				return nil
			}
			return src
		}
	}
	return nil
}

func (a *App) unprocessedPanel(source *core.StockSource) gk.Element {
	note := edit.AssignmentLabel(source) + " · saved on your RØDECaster"
	if a.inputs.error != nil {
		note = edit.AssignmentLabel(source) + " · last confirmed show; refresh unavailable"
	}
	why := "It's listed in your RØDE show, but its RC2FX processing path isn't installed. This listing doesn't mean a device is connected or a call is active."
	if source.SourceID == edit.SmartPadsSource {
		why = "SMART Pads audio is mixed internally by the RØDECaster. RC2FX doesn't have an independent processing path for it yet."
	}
	return gk.Div().Pt(px(22)).
		Child(gk.Div(s.FontBold).TextSize(px(TitleText)).LineHeight(gk.Relative(1)).
			Child(txt(model.DisplayName(source.Name)))).
		Child(gk.Div().Mt(px(10)).TextSize(px(13.5)).TextColor(rgb(Faint)).Child(txt(note))).
		Child(gk.VFlex(s.BorderT1, s.ItemsCenter, s.TextCenter).Mt(px(28)).Py(px(44)).
			BorderColor(rgb(surface().Line)).
			Child(gk.Div(s.FontBold).TextSize(px(SubheadingText)).Child(txt("No RC2FX path for this input yet."))).
			Child(gk.Div().Mt(px(10)).MaxW(px(480)).TextSize(px(14)).LineHeight(gk.Relative(1.55)).
				TextColor(rgb(Muted)).Child(txt(why))))
}

func sourceDetail(source *core.StockSource, note string) string {
	return edit.SourceDetail(source, note)
}
