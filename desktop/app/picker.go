package app

import (
	"slices"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
)

// pickerItem is one entry of a version menu: its line
// in the menu, the shorter text the closed menu shows, and what choosing it
// picks.
type pickerItem[V any] struct {
	title, short string
	value        V
}

// picker is a select menu whose items and choice come from the installer's
// model. The model stays the source of truth: choosing sends
// the pick to it, and each sync brings the menu back in step with it. The
// kit's items carry key(value) as their value.
type picker[V any] struct {
	state  gk.SelectState
	source gk.RowSource
	sub    gk.Subscription
	key    func(V) string
	items  []pickerItem[V]
	// keys is what the menu was last given (title and key per item), and
	// selected the key last chosen (nil: none).
	keys     []string
	selected *string
}

func newPicker[V any](a *App, cx *gk.Cx, key func(V) string, pick func(m *installflow.Model, v V)) *picker[V] {
	p := &picker[V]{key: key}
	// A menu line with room above and below, so entries don't crowd.
	p.source = gk.NewSearchSource(cx, nil, gk.SearchRow(func(cx *gk.Cx, at gk.RowAt) gk.Element {
		if at.Row < 0 || at.Row >= len(p.items) {
			return gk.Div()
		}
		return gk.Div().Py(px(4)).Child(txt(p.items[at.Row].title))
	}))
	p.state = gk.NewSelectState(cx, p.source, gk.NoRow).Build()
	p.sub = p.state.OnSelectEvent(cx, func(cx *gk.Cx, e gk.SelectEvent) {
		confirm, ok := e.(gk.SelectEventConfirm)
		if !ok {
			return
		}
		value, ok := confirm.Value.Get()
		if !ok {
			return
		}
		for _, item := range p.items {
			if p.key(item.value) == value {
				v := item.value
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
					pick(m, v)
					return nil
				})
				return
			}
		}
	})
	return p
}

func (p *picker[V]) sync(cx *gk.Cx, items []pickerItem[V], selected *V) {
	keys := make([]string, len(items))
	for n, item := range items {
		keys[n] = item.title + "\x00" + p.key(item.value)
	}
	changed := !slices.Equal(keys, p.keys)
	p.items = items
	if changed {
		p.keys = keys
		search := make([]gk.SearchItem, len(items))
		for n, item := range items {
			search[n] = gk.SearchItem{Value: p.key(item.value), Title: item.title}
			if item.short != item.title {
				search[n].Display = gk.Some(item.short)
			}
		}
		gk.SetSearchItems(cx, p.source, search)
		p.state.SetItems(cx, p.source)
	}
	var key *string
	if selected != nil {
		k := p.key(*selected)
		key = &k
	}
	if changed || !opt.Equal(key, p.selected) {
		if key != nil {
			p.state.SetSelectedValue(cx, *key)
		} else {
			p.state.SetSelectedIndex(cx, gk.NoRow)
		}
		p.selected = key
	}
}

func (p *picker[V]) release(free func(...any)) {
	if p == nil {
		return
	}
	free(p.sub, p.state, p.source)
}

// pickerSelect is a version menu, or nothing before its first sync. A
// select fills its parent and draws nothing in a parent without a definite
// width, so it sits in a box of its own width; its menu matches it.
// versionSelectWidth fits the longest version name with its channel.
const versionSelectWidth = 260

func pickerSelect[V any](p *picker[V], id string, disabled bool) gk.Element {
	slot := gk.Div(s.FlexNone).W(px(versionSelectWidth))
	if p != nil {
		slot = slot.Child(gk.Select(p.state).ID(id).Placeholder("None available").Disabled(disabled))
	}
	return slot
}
