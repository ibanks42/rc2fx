package app

// Third-party licenses dialog (notice data and markdown formatting live in app/licenses).

import (
	"fmt"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/licenses"
)

type licensesView struct {
	expanded map[int]bool
	// Lazily parsed on first expand (in click handlers, since render can't allocate
	// kit state) so collapsing/reopening preserves scroll and selection.
	textViews map[int]gk.TextViewState
	view      gk.View
	theme     gk.BaseTheme
}

var licensesDialog struct {
	made  bool
	built gk.Built[gk.DialogEl]
	view  *licensesView
}

func newLicensesView(cx *gk.Cx) *licensesView {
	l := &licensesView{expanded: map[int]bool{}, textViews: map[int]gk.TextViewState{}, theme: gk.NewBaseThemeGlobal(cx)}
	l.view = gk.NewView(nil, l.render)
	return l
}

func (l *licensesView) reset(cx *gk.Cx) {
	// Snapshot the current global theme on open so appearance changes take effect.
	gk.Release(cx, l.theme)
	l.theme = gk.NewBaseThemeGlobal(cx)
	for index, state := range l.textViews {
		gk.Release(cx, state)
		delete(l.textViews, index)
	}
	clear(l.expanded)
	if len(licenses.Sections()) == 1 {
		l.open(cx, 0)
	}
}

func (l *licensesView) open(cx *gk.Cx, index int) {
	l.expanded[index] = true
	if _, ok := l.textViews[index]; !ok {
		markdown := licenses.NoticeMarkdown(licenses.Sections()[index].Text)
		l.textViews[index] = gk.NewTextViewStateMarkdown(cx, markdown).Build()
	}
}

func (l *licensesView) toggle(cx *gk.Cx, index int) {
	if l.expanded[index] {
		delete(l.expanded, index)
	} else {
		l.open(cx, index)
	}
}

func (l *licensesView) render(cx *gk.Cx) gk.Element {
	list := gk.VFlex(s.Gap1).MaxH(px(440)).ID("licenses-list").OverflowYScroll()
	for index, section := range licenses.Sections() {
		open := l.expanded[index]
		arrow := "▸"
		if open {
			arrow = "▾"
		}
		item := gk.VFlex(s.BorderB1).BorderColor(rgb(surface().Line)).
			// Use a button so section headers are keyboard-focusable.
			Child(gk.Button(gk.Key("license-section", index)).Ghost().Style(s.WFull, s.JustifyStart).
				Label(fmt.Sprintf("%s %s", arrow, section.Title)).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { l.toggle(cx, index) }))
		if state, ok := l.textViews[index]; open && ok {
			item = item.Child(noticeBox(state, l.theme))
		}
		list = list.Child(item)
	}
	return list
}

func noticeBox(state gk.TextViewState, theme gk.BaseTheme) gk.DivEl {
	style := gk.NewBaseTextViewStyleFromTheme(theme).WithForeground(rgb(Muted)).WithParagraphGap(0)
	return gk.Div(s.FlexShrink0, s.My2, s.Border1).H(px(320)).P(px(18)).Rounded(px(12)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line)).TextSize(px(13.5)).
		Child(gk.BaseTextView(state).Style(style).Selectable(true).Scrollable(true).
			OnLinkClick(func(*gk.Cx, gk.Event83) {}))
}

func openLicenses(cx *gk.Cx) {
	d := &licensesDialog
	if !d.made {
		d.view = newLicensesView(cx)
		view := d.view.view
		d.built = gk.NewBuilt(cx, func(cx *gk.Cx) gk.DialogEl {
			return styledDialog(720).
				Title(gk.Div(s.TextXl, s.FontSemibold).Child(txt("Licenses"))).
				ChildView(view).
				Footer(gk.DialogFooter(s.Pt5).Child(gk.DialogClose().Trigger(gk.Button("licenses-close").Ghost().Large().
					H(px(44)).Style(s.Px5).Rounded(float32(22)).Label("Close"))))
		})
		d.made = true
	}
	d.view.reset(cx)
	gk.Notify(cx, d.view.view)
	gk.OpenDialog(cx, d.built)
}
