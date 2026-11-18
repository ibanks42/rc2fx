package app

// The first screen, shown until the app is connected to RC2FX: it names the
// state the mixer is in, from what the app can see on USB without RC2FX
// (installflow.FirstOf decides), and offers the one thing to do there. Also
// the Linux USB permission screen and its Copy buttons. The preferences file
// and the connection messages' meanings are in prefs.go.

import (
	"fmt"
	"reflect"
	"runtime"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
	"github.com/ibanks42/rc2fx/desktop/installer/platform"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
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

// firstLook is what the app last saw of the mixer before connecting.
// usbOK is false when this system could not list USB devices, so only the
// connection's own messages say anything.
type firstLook struct {
	seen  installflow.Sighting
	usbOK bool
}

func sameLook(a, b firstLook) bool {
	return a.usbOK == b.usbOK && a.seen.RC2FX == b.seen.RC2FX && a.seen.Storage == b.seen.Storage &&
		a.seen.Flashed == b.seen.Flashed && reflect.DeepEqual(a.seen.USB, b.seen.USB)
}

// firstLookEvery is how often the first screen looks at USB.
const firstLookEvery = 2 * time.Second

// lookAtMixer is what the app can see of the mixer without RC2FX: the
// mixers on USB and their mode, whether RC2FX's control reports are live on
// the one in normal mode, whether a mixer's storage shows as a disk, and
// whether this app sent a flash in the last few minutes. It only reads.
func lookAtMixer(work string, settled *uint64) firstLook {
	var look firstLook
	host := platform.Host()
	if items, err := host.Inventory(); err == nil {
		look.usbOK = true
		for _, m := range flash.Mixers(items) {
			look.seen.USB = append(look.seen.USB, installer.USBMixer{
				Model: firmware.FromHardware(m.Model), Serial: m.Serial, UpdateMode: m.UpdateMode, Firmware: m.Firmware})
		}
		if len(look.seen.USB) == 1 && !look.seen.USB[0].UpdateMode {
			for _, h := range items {
				if h.Serial == look.seen.USB[0].Serial && h.Control {
					look.seen.RC2FX = true
				}
			}
		}
	}
	if disk, err := platform.RecoveryDiskOf(host); err == nil && disk != nil {
		look.seen.Storage = true
	}
	if work != "" {
		if saved, err := installer.LoadSaved(work); err == nil {
			look.seen.Flashed = installflow.FlashedRecently(saved, settled, uint64(max(time.Now().Unix(), 0)))
		}
	}
	return look
}

// pollFirstLook looks at USB every few seconds while the first screen is
// up, on a worker so the window never waits on it.
func (a *App) pollFirstLook(cx *gk.Cx) {
	// A look from before a connection would be stale by the time the first
	// screen comes back.
	if a.session.Connected && !a.flowFixture {
		a.look = nil
	}
	if a.flowFixture || a.looking || a.session.Connected || a.installer != nil || !a.link.auto() ||
		time.Since(a.lookedAt) < firstLookEvery {
		return
	}
	a.looking = true
	work, _ := workDirectory()
	settled := cloneUint64(a.settledFlash)
	gk.Go(cx, func() firstLook { return lookAtMixer(work, settled) }, func(cx *gk.Cx, look firstLook) {
		a.looking = false
		a.lookedAt = time.Now()
		if a.look == nil || !sameLook(*a.look, look) {
			a.look = &look
			a.notify(cx)
		}
	})
}

func cloneUint64(v *uint64) *uint64 {
	if v == nil {
		return nil
	}
	c := *v
	return &c
}

// firstSighting is what the first screen goes by: the last look at USB,
// or, where this system can't list USB devices, the connection's own
// messages.
func (a *App) firstSighting() installflow.Sighting {
	if a.look != nil && a.look.usbOK {
		return a.look.seen
	}
	var seen installflow.Sighting
	if a.look != nil {
		seen = a.look.seen
	}
	switch {
	case mixerFound(a.message):
		seen.USB = []installer.USBMixer{{Model: firmware.ModelProIi}}
	case strings.HasPrefix(a.message, "More than one"):
		seen.USB = []installer.USBMixer{{Model: firmware.ModelProIi}, {Model: firmware.ModelProIi}}
	}
	return seen
}

// firstState is the state the first screen names now.
func (a *App) firstState() installflow.First {
	return installflow.FirstOf(a.firstSighting())
}

// hero is one state of the first screen: the headline in two lines, a
// sentence under it, what can be done, and a status line in the corner.
type hero struct {
	first, second string
	lead          string
	act           gk.Element
	dot           uint32
	status        string
}

// Hero sizes, from the approved mock-up. The lead and the actions keep
// their height whatever they hold, so nothing moves between states.
const (
	heroSignatureHeight float32 = 200
	heroLeadHeight      float32 = 70
	heroActHeight       float32 = 128
)

func (a *App) connectionScreen(cx *gk.Cx) gk.Element {
	if a.link.auto() && runtime.GOOS == "linux" && usbPermission(a.message) {
		return a.usbAccessScreen(cx)
	}
	h := a.firstHero()
	signature := gk.Div(s.FlexShrink0).H(px(heroSignatureHeight)).W(gk.Relative(0.62)).Mt(px(22))
	if !a.reducedMotion {
		signature = signature.ChildView(a.onboardingSignature)
	}
	column := gk.VFlex(s.WFull, s.ItemsCenter).Pb(px(64)).
		Child(signature).
		Child(headline(h.first, h.second)).
		Child(gk.Div(s.TextCenter).Mt(px(22)).MaxW(px(560)).MinH(px(heroLeadHeight)).Px(px(16)).
			TextSize(px(LeadText)).LineHeight(gk.Relative(1.55)).TextColor(rgb(Muted)).Child(txt(h.lead))).
		Child(gk.VFlex(s.ItemsCenter, s.FlexShrink0).Gap(px(12)).Mt(px(18)).H(px(heroActHeight)).Child(h.act))
	status := gk.HFlex(s.Absolute, s.ItemsCenter).Left(px(28)).Bottom(px(20)).Gap(px(8)).
		TextSize(px(12.5)).TextColor(rgb(Faint)).
		Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(7)).Bg(rgb(h.dot))).
		Child(txt(h.status))
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(gk.Div(s.Flex1, s.MinH0).ID("connection-scroll").OverflowYScroll().Child(column)).
		Child(status)
}

// heroButton is the first screen's main button.
func heroButton(id gk.ID, label string) gk.ButtonEl {
	return gk.Button(id).Primary().Large().H(px(LargeButtonHeight)).Px(px(28)).FontSemibold().
		Rounded(float32(26)).Label(label)
}

// heroFacts is the three short facts under Install RC2FX.
func heroFacts() gk.DivEl {
	fact := func(text string) gk.DivEl {
		return gk.HFlex(s.ItemsCenter).Gap(px(8)).
			Child(gk.Div(s.FlexShrink0, s.RoundedFull).Size(px(5)).Bg(rgb(Accent))).
			Child(txt(text))
	}
	return gk.HFlex(s.ItemsCenter, s.TextSm).Gap(px(28)).TextColor(rgb(Faint)).
		Child(fact("About 10 minutes the first time")).
		Child(fact("Your settings are kept")).
		Child(fact("Can be removed any time"))
}

func heroSpinner(text string) gk.DivEl {
	return gk.HFlex(s.ItemsCenter, s.Gap3, s.TextSm).TextColor(rgb(Muted)).
		Child(gk.Spinner().Color(rgb(Accent))).
		Child(txt(text))
}

// firstHero is the first screen for the state the app sees now.
func (a *App) firstHero() hero {
	if !a.link.auto() {
		return hero{
			first: "Disconnected from", second: "your RØDECaster.",
			lead: "The mixer keeps processing on its own while the app is disconnected.",
			act: heroButton("connect", "Connect").Disabled(a.link.connecting()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.connect(cx) }),
			dot: Faint, status: "Disconnected",
		}
	}
	seen := a.firstSighting()
	var mixer installer.USBMixer
	if len(seen.USB) > 0 {
		mixer = seen.USB[0]
	}
	label := mixer.Model.Label()
	switch installflow.FirstOf(seen) {
	case installflow.FirstFound:
		status := label + " · RC2FX not running"
		if mixer.Firmware != nil {
			status = fmt.Sprintf("%s · RØDE firmware %s · RC2FX not running", label, *mixer.Firmware)
		}
		return hero{
			first: "Found your", second: label + ".",
			lead: "RC2FX adds noise suppression, EQ and compression to your mixer’s channels. They run on the mixer itself, so they work for recording, streaming and calls, even with this app closed.",
			act: gk.VFlex(s.ItemsCenter).Gap(px(12)).
				Child(heroButton("open-installer", "Install RC2FX").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openTask(cx, taskInstall) })).
				Child(heroFacts()).
				// The storage fix alone is for the Pro II only (autofix.ProOnly),
				// which is what this state is.
				Child(gk.Button("open-fix").Ghost().Label("Not installing RC2FX? Fix the update bug only").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openFix(cx) })),
			dot: Accent, status: status,
		}
	case installflow.FirstDuo:
		return hero{
			first: "Found your", second: label + ".",
			lead: "RC2FX is made and tested on the Pro II. It has not been tested on a Duo at all. The Duo runs the same RØDE software, so it should work, but nobody has tried it yet.",
			act: gk.VFlex(s.ItemsCenter).Gap(px(12)).
				Child(a.duoOptIn("first-allow-duo")).
				Child(heroButton("open-installer", "Install RC2FX").Disabled(!a.allowDuo).
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openTask(cx, taskInstall) })).
				Child(heroFacts()),
			dot: Accent, status: label + " · RC2FX not running",
		}
	case installflow.FirstFinishing:
		// The app sees no mixer, a few minutes after it sent a flash: the
		// mixer is restarting. What was flashed may be RØDE's firmware, so
		// nothing here promises RC2FX.
		return hero{
			first: "Your mixer is", second: "finishing an update.",
			lead: "It restarts while it installs the update, which can take a few minutes. Keep it switched on and connected.",
			act:  heroSpinner("Waiting for the mixer to come back…"),
			dot:  Violet, status: "Mixer restarting · updating",
		}
	case installflow.FirstStarting:
		status := label + " · RC2FX starting"
		if a.message != "" {
			status = "Last try: " + a.message
		}
		return hero{
			first: "RC2FX is starting", second: "on your mixer.",
			lead: "RC2FX is on your mixer. The app opens your channels as soon as it answers.",
			act:  heroSpinner("Connecting to RC2FX…"),
			dot:  Violet, status: status,
		}
	case installflow.FirstUpdateMode:
		return hero{
			first: "Your mixer is waiting", second: "for an update.",
			lead: "An update was started and hasn’t finished. The mixer’s audio is off until it does.",
			act: gk.VFlex(s.ItemsCenter).Gap(px(12)).
				Child(heroButton("continue-install", "Continue the install").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.continueInstall(cx) })).
				Child(gk.Button("leave-update-mode").Ghost().Label("Leave update mode…").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
						a.confirm(cx, confirmKindOther, Confirm{
							Title: "Leave update mode without installing",
							Detail: "Disconnect the mixer’s power cable for 30 seconds, then plug it back in and turn the mixer on.\n\n" +
								"It starts with the firmware it had before, and its audio comes back. You can install RC2FX later from the first screen.",
							Action: "Close",
						}, func(*gk.Cx) {}, func(*gk.Cx) {})
					})),
			dot: Warn, status: label + " · in update mode",
		}
	case installflow.FirstStorage:
		leave := gk.Button("leave-storage-mode").Ghost().Label("How to leave storage mode").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.confirm(cx, confirmKindOther, Confirm{
					Title:  "How to leave storage mode",
					Detail: "Unplug USB 1 and plug it back in. The mixer starts normally.\n\nLeaving storage mode changes nothing on the mixer.",
					Action: "Close",
				}, func(*gk.Cx) {}, func(*gk.Cx) {})
			})
		// Repair mixer only where this system can do it.
		if !installer.RepairSupported {
			return hero{
				first: "Your mixer is in", second: "storage mode.",
				lead: "Its screen stays dark, and this computer sees it as a disk. To leave storage mode, unplug USB 1 and plug it back in.",
				act:  gk.VFlex(s.ItemsCenter).Gap(px(12)).Child(leave),
				dot:  Warn, status: "A mixer in storage mode",
			}
		}
		return hero{
			first: "Your mixer is in", second: "storage mode.",
			lead: "Its screen stays dark, and this computer sees it as a disk. The app can check the mixer for files damaged by an update and repair them.",
			act: gk.VFlex(s.ItemsCenter).Gap(px(12)).
				Child(heroButton("open-repair", "Repair mixer").
					OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openRepair(cx) })).
				Child(leave),
			dot: Warn, status: "A mixer in storage mode",
		}
	case installflow.FirstTwo:
		return hero{
			first: "Connect only", second: "one mixer.",
			lead: "Two RØDECasters are connected to this computer. Unplug the one you don’t want to use with RC2FX. The app continues by itself.",
			act:  heroSpinner("Waiting for one mixer to be unplugged…"),
			dot:  Warn, status: fmt.Sprintf("%d mixers found on USB", max(len(seen.USB), 2)),
		}
	}
	return hero{
		first: "Connect your", second: "RØDECaster.",
		lead: "Use a USB cable from the mixer’s USB 1 port (on the back) to this computer. The app finds it by itself.",
		act:  heroSpinner("Looking for a mixer…"),
		dot:  Faint, status: "No mixer found on USB",
	}
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
				Child(a.copyButton("copy-usb-rule", helper.USBRule, cx))))
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
	gk.Go(cx, helper.RequestUSBRule, func(cx *gk.Cx, err error) {
		if err == nil {
			a.usbRule = USBRule{Kind: USBRuleIdle}
			a.beginConnection(cx, false)
		} else {
			a.usbRule = USBRule{Kind: USBRuleFailed, Error: err.Error()}
		}
		a.notify(cx)
	})
}
