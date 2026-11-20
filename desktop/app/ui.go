package app

// Shared UI primitives (color helpers, toggles, pills, readouts, and confirm dialogs).

import (
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"
)

func rgb(hex uint32) gk.Color { return rgba(hex<<8 | 0xff) }

func rgba(hex uint32) gk.Color {
	c := hslaOf(hex)
	return gk.HSLA(c[0], c[1], c[2], c[3])
}

func rgbAlpha(hex uint32, alpha float32) gk.Color {
	c := hslaOf(hex<<8 | 0xff)
	return gk.HSLA(c[0], c[1], c[2], min(max(alpha, 0), 1))
}

func gradient() gk.LinearGradient {
	return gk.NewLinearGradient(90, gk.NewLinearColorStop(rgb(Accent), 0), gk.NewLinearColorStop(rgb(Violet), 1))
}

// PNG icon bytes embedded by cmd/rc2fx and applied to every window via windowIdentity.
var windowIcon []byte

func SetWindowIcon(png []byte) { windowIcon = png }

func windowIdentity(c gk.WindowConfig) gk.WindowConfig {
	c.AppID = appID
	c.Icon = windowIcon
	return c
}

func white(alpha float32) gk.Color { return gk.HSLA(0, 0, 1, alpha) }

func txt(s string) gk.Element { return gk.Text(s) }

func px(v float32) gk.Length { return gk.Px(v) }

func hover() gk.StyleRefinement { return gk.NewStyleRefinementDefault() }

func dot(on bool, size float32) gk.DivEl {
	d := gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(size))
	if on {
		return d.Bg(gradient())
	}
	return d.Style(s.Border1).BorderColor(rgb(Dim))
}

func toggle(id gk.ID, on, small, disabled bool) gk.StatefulDivEl {
	w, h, knob := float32(40), float32(24), float32(18)
	if small {
		w, h, knob = 32, 20, 14
	}
	knobLeft := float32(3)
	if on {
		knobLeft = w - knob - 3
	}
	knobColor := uint32(0x7c8494)
	if on {
		knobColor = 0xffffff
	}
	t := gk.Div(s.Relative, s.FlexShrink0, s.RoundedFull).W(px(w)).H(px(h))
	if on {
		t = t.Bg(gradient())
	} else {
		t = t.Bg(rgb(0x262b3a))
	}
	if disabled {
		t = t.Opacity(0.4)
	} else {
		t = t.Style(s.CursorPointer)
	}
	return t.Child(gk.Div(s.Absolute, s.RoundedFull).Top(px(3)).Left(px(knobLeft)).
		Size(px(knob)).Bg(rgb(knobColor))).ID(id)
}

func pills() gk.DivEl {
	return gk.HFlex(s.FlexShrink0, s.Border1).P(px(3)).Gap(px(2)).Rounded(px(10)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line))
}

func pill(id gk.ID, label string, on bool) gk.StatefulDivEl {
	p := gk.HFlex(s.ItemsCenter, s.CursorPointer, s.WhitespaceNowrap).Gap(px(6)).
		Px(px(12)).Py(px(6)).Rounded(px(7)).TextSize(px(12.5))
	if on {
		p = p.TextColor(rgb(Text)).Bg(white(0.09))
	} else {
		p = p.TextColor(rgb(Muted)).Hover(hover().TextColor(rgb(Text)))
	}
	return p.Child(txt(label)).ID(id)
}

func section(title string, trailing gk.Element) gk.DivEl {
	d := gk.HFlex(s.ItemsCenter, s.Gap3).Mt(px(6)).Mb(px(16)).
		Child(gk.Div(s.FontSemibold).TextSize(px(11)).TextColor(rgb(Faint)).
			Child(txt(strings.ToUpper(title)))).
		Child(gk.Div(s.Flex1).H(px(1)).Bg(rgb(surface().Line)))
	if trailing != nil {
		d = d.Child(trailing)
	}
	return d
}

func choice(id gk.ID, title, note string, on bool) gk.StatefulDivEl {
	c := gk.Div(s.Flex1, s.MinW0, s.Border1, s.CursorPointer).Px(px(14)).Py(px(12)).Rounded(px(12))
	if on {
		c = c.Bg(white(0.06)).BorderColor(rgb(0x3b4560))
	} else {
		c = c.BorderColor(rgb(surface().LineStrong)).Hover(hover().BorderColor(rgb(0x3a4258)))
	}
	head := gk.HFlex(s.ItemsCenter, s.Gap2, s.FontMedium).TextSize(px(14))
	if on {
		head = head.TextColor(rgb(Text)).Child(dot(true, 6))
	} else {
		head = head.TextColor(rgb(Muted))
	}
	return c.Child(head.Child(txt(title))).
		Child(gk.Div().Mt(px(3)).TextSize(px(12.5)).TextColor(rgb(Faint)).Child(txt(note))).
		ID(id)
}

func hint(t string) gk.DivEl {
	return gk.Div().Mt(px(10)).TextSize(px(12.5)).LineHeight(gk.Relative(1.5)).
		TextColor(rgb(Faint)).Child(txt(t))
}

func grid(columns int, items []gk.Element) gk.DivEl {
	columns = max(columns, 1)
	rows := gk.VFlex()
	for i := 0; i < len(items); i += columns {
		row := gk.HFlex(s.ItemsStart).Gap(px(36))
		for j := 0; j < columns; j++ {
			cell := gk.Div(s.Flex1, s.MinW0).FlexBasis(px(0))
			if i+j < len(items) {
				cell = cell.Child(items[i+j])
			}
			row = row.Child(cell)
		}
		rows = rows.Child(row)
	}
	return rows
}

type Fact struct {
	Label string
	Value string
	Bar   *float32
}

func newFact(label, value string) Fact { return Fact{Label: label, Value: value} }

func (f Fact) withBar(fraction float32) Fact {
	v := min(max(fraction, 0), 1)
	f.Bar = &v
	return f
}

func facts(live, synthetic bool, rows []Fact, extra gk.Element) gk.DivEl {
	heading := "LIVE ON THE MIXER"
	if synthetic && live {
		heading = "LIVE ON THE MIXER · SIMULATED"
	}
	column := gk.VFlex(s.FlexShrink0, s.BorderL1).Gap(px(14)).W(px(280)).Pl(px(28)).
		BorderColor(rgb(surface().Line)).TextSize(px(13)).TextColor(rgb(Faint)).
		Child(gk.HFlex(s.ItemsCenter, s.Gap2, s.FontSemibold).TextSize(px(11)).
			Child(dot(live, 6)).Child(txt(heading)))
	for _, fact := range rows {
		value, color := "-", Dim
		if live {
			value, color = fact.Value, Text
		}
		column = column.Child(gk.HFlex(s.JustifyBetween, s.Gap3).
			Child(txt(fact.Label)).
			Child(gk.Div(s.WhitespaceNowrap, s.FontMedium).TextColor(rgb(color)).Child(txt(value))))
		if live && fact.Bar != nil {
			column = column.Child(gk.Div(s.WFull).Mt(px(-6)).H(px(4)).Rounded(px(2)).Bg(rgb(0x232838)).
				Child(gk.Div(s.HFull).W(gk.Relative(*fact.Bar)).Rounded(px(2)).Bg(gradient())))
		}
	}
	if live {
		if extra != nil {
			column = column.Child(extra)
		}
		return column
	}
	return column.Child(gk.Div().TextSize(px(12)).LineHeight(gk.Relative(1.5)).
		Child(txt("No fresh readings from the mixer. These aren't zero; the mixer just hasn't reported them in the last second.")))
}

type Confirm struct {
	Eyebrow         string // optional small header above Title
	Title           string
	Detail          string
	Cancel          string // empty for notice-only dialogs
	Action          string
	Link, LinkLabel string
}

type confirmKind int

const (
	confirmKindOther confirmKind = iota
	confirmKindRestart
	confirmKindClose
)

// confirmState tracks the single active modal dialog backed by App.confirmDialog
// (allocated once in init to avoid leaking gk.Built handles).
type confirmState struct {
	kind    confirmKind
	spec    Confirm
	onOK    func(cx *gk.Cx)
	onClose func(cx *gk.Cx)
}

// confirm opens the shared modal dialog. Only one confirm dialog can be open at
// a time; if one is already active, confirm returns false without invoking either
// callback so the existing dialog's onClose cleanup isn't lost.
func (a *App) confirm(cx *gk.Cx, kind confirmKind, spec Confirm, onOK, onClose func(cx *gk.Cx)) bool {
	if !a.claimConfirm(kind, spec, onOK, onClose) {
		return false
	}
	gk.OpenDialog(cx, a.confirmDialog)
	return true
}

func (a *App) claimConfirm(kind confirmKind, spec Confirm, onOK, onClose func(cx *gk.Cx)) bool {
	if a.confirmOpen != nil {
		return false
	}
	a.confirmOpen = &confirmState{kind: kind, spec: spec, onOK: onOK, onClose: onClose}
	return true
}

func (a *App) releaseConfirm(c *confirmState) bool {
	if a.confirmOpen != c || c == nil {
		return false
	}
	a.confirmOpen = nil
	return true
}

func (a *App) confirmShowing(kind confirmKind) bool {
	return a.confirmOpen != nil && a.confirmOpen.kind == kind
}

func (a *App) restartAsked() bool { return a.confirmShowing(confirmKindRestart) }

func (a *App) closingPrompt() bool { return a.confirmShowing(confirmKindClose) }

func (a *App) confirmView(cx *gk.Cx) gk.DialogEl {
	c := a.confirmOpen
	if c == nil {
		c = &confirmState{}
	}
	spec := c.spec
	footer := gk.DialogFooter(s.Pt5)
	if spec.Cancel != "" {
		footer = footer.Child(gk.DialogClose().Trigger(gk.Button("dialog-close").Ghost().Large().
			H(px(44)).Style(s.Px5).Rounded(float32(22)).Label(spec.Cancel)))
	}
	footer = footer.Child(gk.DialogAction().Child(gk.Button("dialog-action").Primary().Large().
		H(px(44)).Style(s.Px5).Rounded(float32(22)).FontSemibold().Label(spec.Action)))
	title := gk.VFlex(s.Gap1).Pr(px(40))
	if spec.Eyebrow != "" {
		title = title.Child(gk.Div(s.TextXs, s.FontMedium).TextColor(rgb(Faint)).Child(txt(spec.Eyebrow)))
	}
	title = title.Child(gk.Div(s.TextXl, s.FontSemibold).Child(txt(spec.Title)))
	detail := gk.VFlex(s.Gap3).
		Child(gk.Div(s.TextSm).LineHeight(gk.Relative(1.6)).TextColor(rgb(Muted)).Child(txt(spec.Detail)))
	if spec.Link != "" {
		detail = detail.Child(gk.Div(s.TextSm).Child(gk.Link("dialog-link").Href(spec.Link).Child(txt(spec.LinkLabel))))
	}
	return gk.Dialog(s.P6, s.Border1).W(560).Rounded(px(20)).Bg(rgb(surface().Panel)).
		BorderColor(rgb(surface().LineStrong)).TextColor(rgb(Text)).FontFamily("Inter").
		OverlayClosable(false).
		Title(title).
		Child(detail).
		Footer(footer).
		OnOk(func(cx *gk.Cx, _ gk.ClickEvent) bool {
			if c.onOK != nil {
				c.onOK(cx)
			}
			return true
		}).
		OnClose(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.releaseConfirm(c)
			if c.onClose != nil {
				c.onClose(cx)
			}
			a.notify(cx)
		})
}
