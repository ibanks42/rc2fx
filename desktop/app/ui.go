package app

// ui.rs: the channel editor's building blocks: status dots, toggles, pill
// groups, section headings, choice cards, the "Live on the mixer" readouts
// and the app's confirm dialog. The sizes are in layout.go, dbfs in
// colors.go, the window identity's app id in layout.go.

import (
	"strings"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"
)

// rgb is gpui's rgb(0xRRGGBB) as a colour a builder takes.
func rgb(hex uint32) gk.Color { return rgba(hex<<8 | 0xff) }

// rgba is gpui's rgba(0xRRGGBBAA).
func rgba(hex uint32) gk.Color {
	c := hslaOf(hex)
	return gk.HSLA(c[0], c[1], c[2], c[3])
}

// rgbAlpha is Hsla::from(rgb(hex)).opacity(alpha).
func rgbAlpha(hex uint32, alpha float32) gk.Color {
	c := hslaOf(hex<<8 | 0xff)
	return gk.HSLA(c[0], c[1], c[2], min(max(alpha, 0), 1))
}

// gradient is RC2FX's cyan→violet: only where RC2FX is processing.
func gradient() gk.LinearGradient {
	return gk.NewLinearGradient(90, gk.NewLinearColorStop(rgb(Accent), 0), gk.NewLinearColorStop(rgb(Violet), 1))
}

// windowIcon is the waveform icon every window shows
// (desktop/packaging/rc2fx.png, 64×64), as PNG bytes: cmd/rc2fx embeds
// it and hands it over with SetWindowIcon before Run.
var windowIcon []byte

// SetWindowIcon sets the PNG every window opens with (see windowIdentity).
func SetWindowIcon(png []byte) { windowIcon = png }

// windowIdentity is ui.rs window_options: the desktop entry and every window
// share one identity (app id "rc2fx", StartupWMClass=rc2fx) and the waveform
// icon.
func windowIdentity(c gk.WindowConfig) gk.WindowConfig {
	c.AppID = appID
	c.Icon = windowIcon
	return c
}

// white is hsla(0., 0., 1., alpha), the app's translucent hover and selection
// washes.
func white(alpha float32) gk.Color { return gk.HSLA(0, 0, 1, alpha) }

// txt is a text child, Rust's `.child("…")`.
func txt(s string) gk.Element { return gk.Text(s) }

// px is gpui's px().
func px(v float32) gk.Length { return gk.Px(v) }

// hover is a hover style: Rust's `.hover(|s| s…)`.
func hover() gk.StyleRefinement { return gk.NewStyleRefinementDefault() }

// dot is a status dot: the gradient when on, a hollow ring when off.
func dot(on bool, size float32) gk.DivEl {
	d := gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(size))
	if on {
		return d.Bg(gradient())
	}
	return d.Style(s.Border1).BorderColor(rgb(Dim))
}

// toggle is the design's switch: a gradient track with a white knob when on.
// Callers add the click handler.
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

// pills is a row of pill buttons on a sunken track.
func pills() gk.DivEl {
	return gk.HFlex(s.FlexShrink0, s.Border1).P(px(3)).Gap(px(2)).Rounded(px(10)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line))
}

// pill is one pill in pills(). Callers add the click handler.
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

// section is a small uppercase heading with a rule after it, and an optional
// control (nil for none) at the end of the rule. (ui.rs section; settings.rs
// has its own, settingsSection.)
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

// choice is a choice card: title and a short note; the chosen one is lifted
// and marked with the gradient dot. Callers add the click handler.
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

// hint is a muted explanatory line under a control.
func hint(t string) gk.DivEl {
	return gk.Div().Mt(px(10)).TextSize(px(12.5)).LineHeight(gk.Relative(1.5)).
		TextColor(rgb(Faint)).Child(txt(t))
}

// grid lays items out columns to a row, every cell the same width, with the
// design's column gap. A short last row keeps its cells' width.
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

// Fact is one "Live on the mixer" reading: label, value and an optional 0..1
// bar.
type Fact struct {
	Label string
	Value string
	Bar   *float32
}

func newFact(label, value string) Fact { return Fact{Label: label, Value: value} }

// withBar is Fact::bar.
func (f Fact) withBar(fraction float32) Fact {
	v := min(max(fraction, 0), 1)
	f.Bar = &v
	return f
}

// facts is the readouts column beside a stage's controls. Without fresh
// readings every value is a dash and the note says they are missing, not
// zero. extra is nil for none.
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

// Confirm is what a styled dialog says and offers.
type Confirm struct {
	// Eyebrow is a small line above the title (what it's about); "" for none.
	Eyebrow string
	Title   string
	Detail  string
	// Cancel is the button that keeps things as they are; "" for a notice,
	// which offers only Action.
	Cancel string
	Action string
	// Link, when set, is a web page the dialog offers under its detail, as
	// LinkLabel.
	Link, LinkLabel string
}

// confirmKind is which question the open confirm dialog asks, for the
// prompts the rest of the app waits on.
type confirmKind int

const (
	// confirmKindOther is any prompt nothing else waits on.
	confirmKindOther confirmKind = iota
	// confirmKindRestart is askRestart's question.
	confirmKindRestart
	// confirmKindClose is confirmClose's "Close without saving?".
	confirmKindClose
)

// confirmState is the open confirm dialog: one at a time, drawn by the one
// gk.Built made in init (a Built is kit state: it cannot be made per call
// without leaking it).
type confirmState struct {
	kind    confirmKind
	spec    Confirm
	onOK    func(cx *gk.Cx)
	onClose func(cx *gk.Cx)
}

// confirm is the app's dialog (the installer's flash confirmation is the
// model): dark, rounded, Inter, white primary pill. The OS keeps only its own
// password prompt. onOK runs for the action; onClose whenever the dialog
// closes, the action included. Clicking outside does nothing. (ui.rs confirm,
// which took the window; here it is a method so the dialog can be made once.)
//
// Only one dialog is open at a time: while one is open a new one is refused
// and confirm returns false without running either callback, so the caller
// undoes whatever it set up for it. Replacing the open one would lose its
// onClose, and with it the state its caller clears there.
func (a *App) confirm(cx *gk.Cx, kind confirmKind, spec Confirm, onOK, onClose func(cx *gk.Cx)) bool {
	if !a.claimConfirm(kind, spec, onOK, onClose) {
		return false
	}
	gk.OpenDialog(cx, a.confirmDialog)
	return true
}

// claimConfirm takes the one dialog slot for a new prompt; false when a
// prompt already holds it.
func (a *App) claimConfirm(kind confirmKind, spec Confirm, onOK, onClose func(cx *gk.Cx)) bool {
	if a.confirmOpen != nil {
		return false
	}
	a.confirmOpen = &confirmState{kind: kind, spec: spec, onOK: onOK, onClose: onClose}
	return true
}

// releaseConfirm frees the slot when c's dialog closes; false when c no
// longer holds it.
func (a *App) releaseConfirm(c *confirmState) bool {
	if a.confirmOpen != c || c == nil {
		return false
	}
	a.confirmOpen = nil
	return true
}

// confirmShowing is whether the open dialog asks kind's question.
func (a *App) confirmShowing(kind confirmKind) bool {
	return a.confirmOpen != nil && a.confirmOpen.kind == kind
}

// restartAsked: the restart question is open (askRestart).
func (a *App) restartAsked() bool { return a.confirmShowing(confirmKindRestart) }

// closingPrompt: "Close without saving?" is open (confirmClose).
func (a *App) closingPrompt() bool { return a.confirmShowing(confirmKindClose) }

// confirmView draws a.confirmOpen; made into a gk.Built in init.
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
