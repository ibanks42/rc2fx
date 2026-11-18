package app

// The guided task: one layout for Install, Remove and (next) Update and
// Repair. The title and "Step N of M" across the top, every step down the
// left with done / now / to come / failed marks, the current step's body in
// the middle, and a footer pinned to the bottom: whether the mixer may be
// unplugged on the left, the buttons on the right with the primary one last.
// The body scrolls inside its own area, so nothing it holds moves the footer.

import (
	"fmt"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

// guidedTask is what one page of a guided task shows.
type guidedTask struct {
	title string
	// count is "Step N of M", or "Finished".
	count string
	steps []installflow.TaskStep
	body  gk.Element
	// scroll is the body's scroll position (kit state the caller keeps).
	scroll gk.ScrollHandle
	unplug installflow.Unplug
	// secondary are the buttons left of the primary one, in order.
	secondary []gk.ButtonEl
	primary   gk.ButtonEl
}

// Guided-task sizes, from the approved mock-up.
const (
	taskHeaderHeight float32 = 76
	taskFooterHeight float32 = 76
	taskInset        float32 = 36
	taskSidebarWidth float32 = 228
	taskButtonHeight float32 = 44
)

// guidedScreen is a guided task filling the window.
func (a *App) guidedScreen(t guidedTask) gk.Element {
	header := gk.HFlex(s.FlexShrink0, s.ItemsBaseline, s.JustifyBetween).H(px(taskHeaderHeight)).
		Px(px(taskInset)).Pt(px(26)).
		Child(gk.Div(s.Text2xl, s.FontSemibold).Child(txt(t.title))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Faint)).Child(txt(t.count)))
	body := gk.VFlex(s.Flex1, s.MinW0, s.HFull).Gap(px(16)).Pr(px(4)).Pb(px(20)).
		ID("task-body").TrackScroll(t.scroll).OverflowYScroll().
		Child(t.body)
	middle := gk.HFlex(s.Flex1, s.MinH0, s.ItemsStart).Px(px(taskInset)).Pt(px(8)).
		Child(gk.Div(s.FlexShrink0, s.HFull, s.BorderR1).W(px(taskSidebarWidth)).Pr(px(24)).Mr(px(32)).
			BorderColor(rgb(surface().Line)).
			Child(stepSidebar(t.steps))).
		Child(body)
	buttons := gk.HFlex(s.FlexShrink0, s.ItemsCenter).Gap(px(8))
	for _, b := range t.secondary {
		buttons = buttons.Child(b)
	}
	buttons = buttons.Child(t.primary)
	footer := gk.HFlex(s.FlexShrink0, s.ItemsCenter, s.BorderT1).H(px(taskFooterHeight)).Px(px(taskInset)).
		Gap(px(12)).BorderColor(rgb(surface().Line)).Bg(rgbAlpha(surface().Sunken, 0.5)).
		Child(unplugNote(t.unplug)).
		Child(buttons)
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(header).
		Child(middle).
		Child(footer)
}

// unplugNote is the footer's "Safe to unplug" line.
func unplugNote(u installflow.Unplug) gk.DivEl {
	var color uint32
	var answer string
	switch u.Kind {
	case installflow.UnplugYes:
		color, answer = Accent, "yes"
	case installflow.UnplugPowerOnly:
		color, answer = Violet, "power only"
	default:
		color, answer = Warn, "no"
	}
	line := gk.HFlex(s.Flex1, s.MinW0, s.ItemsCenter, s.TextSm).Gap(px(9)).TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(7)).Bg(rgb(color))).
		Child(txt("Safe to unplug:")).
		Child(gk.Div(s.FontSemibold).TextColor(rgb(Text)).Child(txt(answer)))
	if u.Reason != "" {
		line = line.Child(gk.Div(s.MinW0, s.Truncate).Child(txt("— " + u.Reason)))
	}
	return line
}

// stepCount is the header's count for stage of total, or "Finished".
func stepCount(stage, total int) string {
	if stage >= total {
		return "Finished"
	}
	return fmt.Sprintf("Step %d of %d", stage+1, total)
}

// taskButton is a guided task's footer button: a ghost unless primary.
func taskButton(id gk.ID, label string, primary bool) gk.ButtonEl {
	b := gk.Button(id)
	if primary {
		b = b.Primary().FontSemibold()
	} else {
		b = b.Ghost()
	}
	return b.Large().H(px(taskButtonHeight)).Px(px(20)).Rounded(float32(taskButtonHeight / 2)).Label(label)
}

// stepSidebar is the steps of a guided task down the left: done, current,
// failed or to come.
func stepSidebar(steps []installflow.TaskStep) gk.DivEl {
	list := gk.VFlex(s.Gap1)
	for n, step := range steps {
		var circle gk.DivEl
		var text uint32
		switch step.Mark {
		case installflow.MarkDone:
			circle = gk.Div().Bg(rgb(surface().Raised)).BorderColor(rgb(Accent)).
				Child(gk.Icon(gk.IconCheck).TextColor(rgb(Accent)).Size(px(12)))
			text = Muted
		case installflow.MarkNow:
			circle = gk.Div().BorderColor(rgb(Accent)).TextColor(rgb(Accent)).Child(txt(fmt.Sprint(n + 1)))
			text = Text
		case installflow.MarkFailed:
			circle = gk.Div().BorderColor(rgb(Danger)).
				Child(gk.Icon(gk.IconCircleX).TextColor(rgb(Danger)).Size(px(12)))
			text = Text
		default:
			circle = gk.Div().BorderColor(rgb(surface().Line)).TextColor(rgb(Faint)).Child(txt(fmt.Sprint(n + 1)))
			text = Faint
		}
		title := gk.Div(s.TextSm).TextColor(rgb(text))
		if step.Mark == installflow.MarkNow {
			title = title.Style(s.FontSemibold)
		}
		list = list.Child(gk.HFlex(s.ItemsCenter, s.Gap3, s.Py2).
			Child(circle.Style(s.FlexShrink0, s.RoundedFull, s.Border1, s.Flex, s.ItemsCenter, s.JustifyCenter, s.TextXs).
				Size(px(22))).
			Child(title.Child(txt(step.Title))))
	}
	return list
}

// taskHeading is the body's heading.
func taskHeading(text string) gk.DivEl {
	return gk.Div(s.TextXl, s.FontSemibold).Child(txt(text))
}

// doneHeading is a result's heading, with a check before it.
func doneHeading(text string) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Icon(gk.IconCheck).TextColor(rgb(Accent))).
		Child(taskHeading(text))
}

// prose is a paragraph of the body; color is Muted for most, Text to stand
// out, Faint for asides.
func prose(text string, color uint32) gk.DivEl {
	return gk.Div(s.TextSm).LineHeight(gk.Relative(1.6)).TextColor(rgb(color)).Child(txt(text))
}

// taskPanel is a bordered box in the body.
func taskPanel() gk.DivEl {
	return gk.VFlex(s.Gap1, s.RoundedXl, s.Border1).Px(px(16)).Py(px(15)).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line))
}

// warnPanel is a panel that needs attention.
func warnPanel() gk.DivEl {
	return gk.VFlex(s.Gap1, s.RoundedXl, s.Border1).Px(px(16)).Py(px(15)).
		Bg(rgb(surface().Panel)).BorderColor(rgbAlpha(Warn, 0.35))
}

// eyebrow is a small uppercase label over a panel's contents.
func eyebrow(text string) gk.DivEl {
	return gk.Div(s.TextXs, s.FontSemibold).Mb(px(8)).TextColor(rgb(Faint)).Child(txt(upper(text)))
}

// bullet is one point of a list: a small dot, then its contents.
func bullet(children ...gk.Element) gk.DivEl {
	return gk.HFlex(s.ItemsStart, s.TextSm).Gap(px(12)).LineHeight(gk.Relative(1.55)).TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(8)).Size(px(5)).Bg(rgb(Faint))).
		Child(gk.HFlex(s.Flex1, s.MinW0, s.FlexWrap, s.ItemsBaseline).Gap(px(6)).Children(children...))
}

// bullets is a list of points.
func bullets(points ...gk.Element) gk.DivEl {
	return gk.VFlex().Gap(px(9)).Children(points...)
}

// numbered is a numbered list of what to do, in order.
func numbered(items ...string) gk.DivEl {
	list := gk.VFlex().Gap(px(9))
	for n, item := range items {
		list = list.Child(gk.HFlex(s.ItemsStart, s.TextSm).Gap(px(12)).LineHeight(gk.Relative(1.55)).
			TextColor(rgb(Muted)).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull, s.Flex, s.ItemsCenter, s.JustifyCenter, s.TextXs).
				Size(px(20)).Bg(rgb(surface().Raised)).Child(txt(fmt.Sprint(n + 1)))).
			Child(gk.Div(s.Flex1, s.MinW0).Child(txt(item))))
	}
	return list
}

// seenRow is a line of what the app sees: a dot colored by whether it is
// as it should be.
func seenRow(color uint32, text string, strong bool) gk.DivEl {
	textColor := Muted
	if strong {
		textColor = Text
	}
	return gk.HFlex(s.ItemsStart, s.Gap3, s.Py1, s.TextSm).TextColor(rgb(textColor)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(6)).Size(px(8)).Bg(rgb(color))).
		Child(gk.Div(s.Flex1, s.MinW0).Child(txt(text)))
}

// spinnerLine is something under way: a spinner, then what.
func spinnerLine(text string) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Spinner().Color(rgb(Accent))).
		Child(prose(text, Muted))
}

// upper is an ASCII-only upper case, enough for the app's own labels.
func upper(text string) string {
	b := []rune(text)
	for i, c := range b {
		if 'a' <= c && c <= 'z' {
			b[i] = c - 'a' + 'A'
		}
	}
	return string(b)
}
