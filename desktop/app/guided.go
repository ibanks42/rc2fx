package app

// Shared wizard layout for Install, Remove, Update, Fix, and Repair flows:
// header, left step sidebar, independently scrollable body, and pinned footer.

import (
	"fmt"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

type guidedTask struct {
	title     string
	count     string
	steps     []installflow.TaskStep
	body      gk.Element
	scroll    gk.ScrollHandle
	unplug    installflow.Unplug
	secondary []gk.ButtonEl
	primary   gk.ButtonEl
}

const (
	taskHeaderHeight float32 = 76
	taskFooterHeight float32 = 76
	taskInset        float32 = 36
	taskSidebarWidth float32 = 228
	taskButtonHeight float32 = 44
)

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

func stepCount(stage, total int) string {
	if stage >= total {
		return "Finished"
	}
	return fmt.Sprintf("Step %d of %d", stage+1, total)
}

func taskButton(id gk.ID, label string, primary bool) gk.ButtonEl {
	b := gk.Button(id)
	if primary {
		b = b.Primary().FontSemibold()
	} else {
		b = b.Ghost()
	}
	return b.Large().H(px(taskButtonHeight)).Px(px(20)).Rounded(float32(taskButtonHeight / 2)).Label(label)
}

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

func taskHeading(text string) gk.DivEl {
	return gk.Div(s.TextXl, s.FontSemibold).Child(txt(text))
}

func doneHeading(text string) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Icon(gk.IconCheck).TextColor(rgb(Accent))).
		Child(taskHeading(text))
}

func prose(text string, color uint32) gk.DivEl {
	return gk.Div(s.TextSm).LineHeight(gk.Relative(1.6)).TextColor(rgb(color)).Child(txt(text))
}

func taskPanel() gk.DivEl {
	return gk.VFlex(s.Gap1, s.RoundedXl, s.Border1).Px(px(16)).Py(px(15)).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line))
}

func warnPanel() gk.DivEl {
	return gk.VFlex(s.Gap1, s.RoundedXl, s.Border1).Px(px(16)).Py(px(15)).
		Bg(rgb(surface().Panel)).BorderColor(rgbAlpha(Warn, 0.35))
}

func eyebrow(text string) gk.DivEl {
	return gk.Div(s.TextXs, s.FontSemibold).Mb(px(8)).TextColor(rgb(Faint)).Child(txt(upper(text)))
}

func bullet(children ...gk.Element) gk.DivEl {
	return gk.HFlex(s.ItemsStart, s.TextSm).Gap(px(12)).LineHeight(gk.Relative(1.55)).TextColor(rgb(Muted)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(8)).Size(px(5)).Bg(rgb(Faint))).
		Child(gk.HFlex(s.Flex1, s.MinW0, s.FlexWrap, s.ItemsBaseline).Gap(px(6)).Children(children...))
}

func bullets(points ...gk.Element) gk.DivEl {
	return gk.VFlex().Gap(px(9)).Children(points...)
}

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

func seenRow(color uint32, text string, strong bool) gk.DivEl {
	textColor := Muted
	if strong {
		textColor = Text
	}
	return gk.HFlex(s.ItemsStart, s.Gap3, s.Py1, s.TextSm).TextColor(rgb(textColor)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(6)).Size(px(8)).Bg(rgb(color))).
		Child(gk.Div(s.Flex1, s.MinW0).Child(txt(text)))
}

func (a *App) spinnerLine(text string) gk.DivEl {
	return a.waitLine(text, false)
}

func upper(text string) string {
	b := []rune(text)
	for i, c := range b {
		if 'a' <= c && c <= 'z' {
			b[i] = c - 'a' + 'A'
		}
	}
	return string(b)
}
