package app

// connection.rs, the drawing half: the connection screen, the Linux USB
// permission screen and its Copy buttons. The preferences file and the
// screen's decisions are in prefs.go.

import (
	"runtime"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"
)

// USBRuleKind is the USB permission screen's request state.
type USBRuleKind int

const (
	USBRuleIdle USBRuleKind = iota
	// USBRuleAsking: the system's password prompt is open.
	USBRuleAsking
	// USBRuleFailed: refused or failed; Error is the reason, in words.
	USBRuleFailed
)

// USBRule is connection.rs UsbRule.
type USBRule struct {
	Kind  USBRuleKind
	Error string
}

// codeBox is a sunken box for exact text, such as a command.
func codeBox(mono string) gk.DivEl {
	return gk.Div(s.Relative, s.Border1).Px(px(6)).Py(px(4)).Rounded(px(10)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().Line)).FontFamily(mono).
		TextSize(px(12)).TextColor(rgb(Muted))
}

// connectionPanel is, while disconnected: whether the app is looking for the
// mixer, why the last attempt failed, or a way to connect again after
// Disconnect.
func (a *App) connectionPanel(cx *gk.Cx) gk.Element {
	color, status := connectionStatus(a.autoConnect, a.message)
	panel := gk.VFlex(s.Gap4, s.ItemsCenter, s.TextCenter).
		Child(gk.HFlex(s.ItemsCenter, s.JustifyCenter, s.Gap2, s.TextSm).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(7)).Bg(rgb(color))).
			Child(gk.Div(s.MinW0).TextColor(rgb(Muted)).Child(txt(status))))
	if !a.autoConnect {
		panel = panel.Child(gk.Button("connect").Primary().Label("Connect").Disabled(a.connecting).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.connect(cx) }))
	}
	return panel
}

func (a *App) connectionScreen(cx *gk.Cx) gk.Element {
	if a.autoConnect && runtime.GOOS == "linux" && usbPermission(a.message) {
		return a.usbAccessScreen(cx)
	}
	found := a.autoConnect && mixerFound(a.message)
	first, second := "Plug in your", "RØDECaster."
	lead := "Connect it to this computer with a USB cable."
	if found {
		first, second = "Found it.", "Now, the noise."
		lead = "Your RØDECaster doesn't have RC2FX yet. It adds noise suppression, EQ and compression, running on the mixer itself."
	}
	copyBlock := gk.VFlex(s.ItemsCenter, s.WFull, s.MxAuto, s.Px6).MaxW(px(620)).Pb(px(80)).
		Child(headline(first, second)).
		Child(gk.Div(s.TextCenter).Mt(px(22)).MaxW(px(540)).TextSize(px(LeadText)).
			LineHeight(gk.Relative(1.55)).TextColor(rgb(Muted)).Child(txt(lead)))
	if found {
		actions := gk.VFlex(s.Mt6, s.ItemsCenter, s.Gap4)
		if installerNeeded() {
			actions = actions.Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt("Asks for your password once")))
		}
		actions = actions.Child(gk.Button("open-installer").Primary().Large().H(px(LargeButtonHeight)).
			Px(px(28)).Rounded(float32(26)).FontSemibold().Label("Install RC2FX").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openInstaller(cx) })).
			Child(a.repairLink())
		copyBlock = copyBlock.Child(actions)
	} else {
		copyBlock = copyBlock.Child(gk.Div(s.Mt6).Child(a.connectionPanel(cx)))
		if a.message != "" {
			copyBlock = copyBlock.Child(gk.Button("open-installer").Ghost().Style(s.Mt4).Label("Open installer").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openInstaller(cx) }))
		}
		copyBlock = copyBlock.Child(gk.Div(s.Mt2).Child(a.repairLink()))
	}
	hero := gk.Div(s.Flex1, s.WFull, s.Flex, s.ItemsCenter, s.JustifyCenter).MinH(px(200))
	if !a.reducedMotion {
		hero = hero.Child(gk.Div(s.HFull).W(gk.Relative(0.6)).ChildView(a.onboardingSignature))
	}
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(gk.Div(s.Flex1, s.MinH0).ID("connection-scroll").OverflowYScroll().
			Child(gk.VFlex(s.WFull).MinH(gk.Relative(1)).
				Child(hero).
				Child(copyBlock)))
}

// repairLink is the way into Repair mixer, on the first screen whenever
// RC2FX isn't running: a mixer a flash left unable to keep its setup still
// shows up here, and one stuck in storage mode doesn't show up at all.
func (a *App) repairLink() gk.ButtonEl {
	return gk.Button("open-repair").Ghost().
		Label("Mixer forgetting its setup or not starting? Repair mixer").
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openRepair(cx) })
}

// copyButton is a small Copy button that says Copied for a moment after a
// click.
func (a *App) copyButton(id string, copied string, cx *gk.Cx) gk.StatefulDivEl {
	done := a.copied == id
	color, label := Muted, "Copy"
	if done {
		color, label = Accent, "Copied"
	}
	return gk.Div(s.Border1, s.CursorPointer).Px(px(10)).Py(px(4)).Rounded(px(7)).
		Bg(rgb(surface().Sunken)).BorderColor(rgb(surface().LineStrong)).FontFamily("Inter").
		TextSize(px(12)).TextColor(rgb(color)).Hover(hover().TextColor(rgb(Text))).
		Child(txt(label)).
		ID(id).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			gk.ClipboardWrite(cx, copied)
			a.copied = id
			a.notify(cx)
			gk.After(cx, 1500*time.Millisecond, func(cx *gk.Cx) {
				if a.copied == id {
					a.copied = ""
					a.notify(cx)
				}
			})
		})
}

// usbAccessScreen is Linux without the udev rule: what RC2FX needs, exactly
// what it will add (the file, shown byte for byte), and how to undo it,
// before the system's own password prompt.
func (a *App) usbAccessScreen(cx *gk.Cx) gk.Element {
	mono := gk.KitTheme(cx).MonoFontFamily(cx)
	asking := a.usbRule.Kind == USBRuleAsking
	open := a.usbRuleOpen
	point := func(t string) gk.DivEl {
		return gk.HFlex(s.ItemsStart, s.Gap3).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull).Mt(px(8)).Size(px(5)).Bg(rgb(Faint))).
			// MinW0 lets a long line shrink to the card and wrap.
			Child(gk.Div(s.Flex1, s.MinW0).Child(txt(t)))
	}
	// Exact text in a read-only box (select it or copy it), with a Copy
	// button pinned to the box's corner, outside what scrolls. Copy puts the
	// constant on the clipboard, never the field's contents.
	chevron := gk.IconChevronRight
	if open {
		chevron = gk.IconChevronDown
	}
	card := gk.Div(s.WFull, s.MxAuto, s.Border1).MaxW(px(640)).Mt(px(36)).P(px(28)).Rounded(px(20)).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line)).TextSize(px(14.5)).
		LineHeight(gk.Relative(1.55)).TextColor(rgb(Muted)).
		Child(txt("Linux only lets apps open USB devices you've allowed. RC2FX needs this once, so it can read your mixer's settings and levels and save your changes over USB.")).
		Child(gk.Div(s.FontSemibold).Mt(px(22)).Mb(px(10)).TextColor(rgb(Text)).Child(txt("What this does"))).
		Child(gk.VFlex(s.Gap2).
			Child(point("Adds one file: /etc/udev/rules.d/70-rc2fx.rules")).
			Child(point("It only covers RØDECaster mixers, and only gives access to whoever is logged in at this computer.")).
			Child(point("It doesn't cover update mode. Installing firmware still asks for permission separately.")).
			Child(point("Nothing else on your system changes, and RC2FX never sees or keeps your password."))).
		Child(gk.HFlex(s.ItemsCenter, s.Gap2, s.CursorPointer).Mt(px(18)).TextSize(px(13)).
			TextColor(rgb(Muted)).Hover(hover().TextColor(rgb(Text))).
			Child(gk.Icon(chevron).Xsmall()).
			Child(txt("Show the exact file")).
			ID("usb-rule-show").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.usbRuleOpen = !open
				a.notify(cx)
			}))
	if open {
		card = card.Child(codeBox(mono).Mt(px(10)).
			Child(gk.Textarea(a.usbRuleText).Readonly(true).Appearance(false).H(px(220)).
				AriaLabel("The udev rule RC2FX installs")).
			Child(gk.Div(s.Absolute).Top(px(8)).Right(px(8)).
				Child(a.copyButton("copy-usb-rule", installerUSBRule, cx))))
	}
	card = card.Child(gk.Div().Mt(px(18)).TextSize(px(13)).Child(txt("To undo it later:"))).
		Child(codeBox(mono).Mt(px(6)).Pr(px(76)).
			Child(gk.Input(a.usbUndoText).Readonly(true).Appearance(false)).
			Child(gk.Div(s.Absolute, s.Top0, s.Bottom0, s.Flex, s.ItemsCenter).Right(px(8)).
				Child(a.copyButton("copy-usb-undo", USBUndo, cx))))
	if a.usbRule.Kind == USBRuleFailed {
		card = card.Child(gk.Div().Mt(px(18)).TextSize(px(13.5)).TextColor(rgb(Warn)).Child(txt(a.usbRule.Error)))
	}
	prompt := "Your system will ask for your password."
	if asking {
		prompt = "Answer your system's password prompt to continue."
	}
	allow := "Allow access"
	if a.usbRule.Kind == USBRuleFailed {
		allow = "Try again"
	}
	card = card.Child(gk.HFlex(s.ItemsCenter, s.JustifyEnd, s.Gap4).Mt(px(24)).
		Child(gk.Div().TextSize(px(13)).TextColor(rgb(Faint)).Child(txt(prompt))).
		Child(gk.Button("allow-usb").Primary().Large().H(px(LargeButtonHeight)).Px(px(28)).
			Rounded(float32(26)).FontSemibold().Label(allow).Disabled(asking).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.allowUSB(cx) })))
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(gk.Div(s.Flex1, s.MinH0, s.Px8).Pt(px(24)).Pb(px(48)).ID("usb-access-scroll").OverflowYScroll().
			Child(headline("Let RC2FX talk", "to your mixer.").TextSize(px(DisplayTextSmall))).
			Child(card))
}

// allowUSB installs the udev rule through the system's password prompt, then
// connects.
func (a *App) allowUSB(cx *gk.Cx) {
	if a.usbRule.Kind == USBRuleAsking {
		return
	}
	a.usbRule = USBRule{Kind: USBRuleAsking}
	a.notify(cx)
	gk.Go(cx, installerRequestUSBRule, func(cx *gk.Cx, err error) {
		if err == nil {
			a.usbRule = USBRule{Kind: USBRuleIdle}
			a.beginConnection(cx, false)
		} else {
			a.usbRule = USBRule{Kind: USBRuleFailed, Error: err.Error()}
		}
		a.notify(cx)
	})
}
