package app

// Landing/onboarding screen chrome: background gradient, cached halo texture,
// headline, and the shared paced ambient ticker for the signature waveform and
// loading spinners.

import (
	"math"
	"strings"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/home"
	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

func background() gk.LinearGradient {
	var top, bottom uint32
	switch appearance() {
	case AppearanceGraphite:
		top, bottom = 0x171a20, 0x0c0d11
	case AppearanceBlack:
		top, bottom = 0x08090c, 0x030405
	default:
		top, bottom = 0x131622, 0x0b0d14
	}
	return gk.NewLinearGradient(180, gk.NewLinearColorStop(rgb(top), 0), gk.NewLinearColorStop(rgb(bottom), 1))
}

// Pre-rendered halo texture uploaded once at startup. Using a live blurred shadow
// across the full window cost >50% of GPU frame time on integrated GPUs.
var haloImage *gk.RenderImage

// makeHalo builds haloImage during init (images can't be created during render).
// home.HaloPixels returns BGRA, so swap R and B before passing RGBA to gk.
func makeHalo(cx *gk.Cx) {
	if haloImage != nil {
		return
	}
	width, height := home.HaloSize()
	pixels := home.HaloPixels(width, height)
	for i := 0; i+3 < len(pixels); i += 4 {
		pixels[i], pixels[i+2] = pixels[i+2], pixels[i]
	}
	image := gk.NewRenderImage(cx, gk.PixelImage{Width: uint32(width), Height: uint32(height), RGBA: pixels})
	haloImage = &image
}

func backdrop() gk.Element {
	d := gk.Div(s.Absolute, s.Inset0, s.SizeFull)
	if appearance() == AppearanceDusk && haloImage != nil {
		// gk's ImgEl doesn't expose ObjectFit::Fill yet, so gpui defaults to Contain
		// and letterboxes the 1320x860 glow at non-matching aspect ratios.
		d = d.Child(gk.Img(gk.ImageSourceRender{Value: *haloImage}, s.SizeFull))
	}
	return d
}

func headline(first, second string) gk.DivEl {
	return gk.VFlex(s.ItemsCenter, s.TextCenter, s.FontExtrabold).TextSize(px(DisplayText)).
		LineHeight(gk.Relative(0.98)).
		Child(txt(first)).
		Child(gk.Div().TextColor(rgb(Faint)).Child(txt(second)))
}

const (
	// Ambient animation cadence (~30 Hz) shared by the onboarding signature and
	// loading spinners when focused: matches installflow.Offset's 30 Hz noise
	// step rate and keeps the spinner smooth without vsync-rate redraws.
	ambientFPS   = 30
	ambientFrame = 33 * time.Millisecond
	// Unfocused ellipsis cadence (2 Hz) so waiting states stay visibly alive in
	// the background at <1% GPU while the waveform and spinner rest.
	unfocusedEllipsisFrame = 500 * time.Millisecond
	// Frames per full rotation of the 8-spoke loader icon (1 rev/s, 12°/frame,
	// well below the 22.5° aliasing limit for 45°-spaced spokes).
	spinnerFramesPerTurn = ambientFPS
	// Maximum simultaneous waiting/spinner views retained by App.
	spinnerPoolSize = 2
)

type spinnerSlot struct {
	view          gk.View
	embedded      uint64
	text          string
	centered      bool
	drawnText     string
	drawnCentered bool
}

type ambientState struct {
	tick        uint64
	ellipsis    uint64
	active      bool
	sigView     gk.View
	sigEmbedded uint64
	sigFound    bool
	spinners    []*spinnerSlot
	ticker      gk.Timer
	ticking     bool
	period      time.Duration
}

func spinnerAngle(tick uint64) float32 {
	return float32(tick%spinnerFramesPerTurn) * (2 * math.Pi / spinnerFramesPerTurn)
}

func ambientTick(sigAnimate, spinAnimate, ellipsisAnimate, reducedMotion, active bool) (time.Duration, bool) {
	if reducedMotion {
		return 0, false
	}
	if active && (sigAnimate || spinAnimate) {
		return ambientFrame, true
	}
	if !active && ellipsisAnimate {
		return unfocusedEllipsisFrame, true
	}
	return 0, false
}

func ellipsisDots(step uint64) int {
	return int(step%3) + 1
}

func renderWaitRow(text string, centered, active, reducedMotion bool, tick, ellipsis uint64) gk.Element {
	icon := gk.Icon(gk.IconLoader).Size(px(16)).TextColor(rgb(Accent))
	if active && !reducedMotion {
		icon = icon.Rotate(gk.Radians{V0: spinnerAngle(tick)})
	}
	base, hasEllipsis := strings.CutSuffix(text, "…")
	dots := 3
	if !active && !reducedMotion && hasEllipsis {
		dots = ellipsisDots(ellipsis)
	}
	var label gk.Element
	if centered {
		if hasEllipsis {
			row := gk.HFlex(s.ItemsCenter).Child(txt(base))
			for i := 1; i <= 3; i++ {
				d := gk.Div().Child(txt("."))
				if i > dots {
					d = d.Opacity(0)
				}
				row = row.Child(d)
			}
			label = row
		} else {
			label = txt(text)
		}
		return gk.HFlex(s.ItemsCenter, s.Gap3, s.TextSm).TextColor(rgb(Muted)).
			Child(icon).
			Child(label)
	}
	display := text
	if hasEllipsis {
		display = base + strings.Repeat(".", dots)
	}
	return gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(icon).
		Child(prose(display, Muted))
}

func (a *App) initAmbient() {
	amb := &ambientState{
		active:   true,
		spinners: make([]*spinnerSlot, spinnerPoolSize),
	}
	amb.sigView = gk.NewView(nil, func(cx *gk.Cx) gk.Element {
		// Center each tick in its 1/30 s bucket so float32 rounding never repeats floor(phase*30).
		phase := (float32(amb.tick) + 0.5) / ambientFPS
		return signatureScene(a.signatureFound(), phase)
	})
	for i := range amb.spinners {
		slot := &spinnerSlot{}
		slot.view = gk.NewView(nil, func(cx *gk.Cx) gk.Element {
			slot.drawnText, slot.drawnCentered = slot.text, slot.centered
			return renderWaitRow(slot.text, slot.centered, amb.active, a.reducedMotion, amb.tick, amb.ellipsis)
		})
		amb.spinners[i] = slot
	}
	a.lineSlots.ambient = amb
}

func (a *App) signatureFound() bool {
	first := a.firstState()
	return first == installflow.FirstFound || first == installflow.FirstDuo
}

func (a *App) embedSignature() gk.Element {
	amb := a.lineSlots.ambient
	if amb == nil {
		return gk.Div(s.SizeFull)
	}
	amb.sigEmbedded = a.renderGeneration
	return gk.Div(s.SizeFull).ChildView(amb.sigView)
}

// waitLine embeds a paced spinner/ellipsis row driven by the shared ambient
// ticker (30 Hz rotating icon when focused; 2 Hz animated ellipsis when
// unfocused), avoiding gk.Spinner's uncapped vsync animation loop.
func (a *App) waitLine(text string, centered bool) gk.DivEl {
	if a.reducedMotion {
		return gk.Div().Child(renderWaitRow(text, centered, false, true, 0, 0))
	}
	if amb := a.lineSlots.ambient; amb != nil {
		for _, slot := range amb.spinners {
			if slot.embedded != a.renderGeneration {
				slot.embedded = a.renderGeneration
				slot.text = text
				slot.centered = centered
				return gk.Div().ChildView(slot.view)
			}
		}
	}
	return gk.Div().Child(renderWaitRow(text, centered, false, true, 0, 0))
}

func (a *App) pollAmbient(cx *gk.Cx) {
	amb := a.lineSlots.ambient
	if amb == nil {
		return
	}
	sigShown := amb.sigEmbedded != 0 && amb.sigEmbedded == a.renderGeneration
	sigAnimate := sigShown && !a.reducedMotion && a.firstState() != installflow.FirstTwo
	spinAnimate, ellipsisAnimate := false, false
	if !a.reducedMotion {
		for _, slot := range amb.spinners {
			if slot.embedded != 0 && slot.embedded == a.renderGeneration {
				spinAnimate = true
				if strings.HasSuffix(slot.text, "…") {
					ellipsisAnimate = true
				}
				if slot.text != slot.drawnText || slot.centered != slot.drawnCentered {
					gk.Notify(cx, slot.view)
				}
			}
		}
	}
	if sigShown {
		found := a.signatureFound()
		if found != amb.sigFound {
			amb.sigFound = found
			gk.Notify(cx, amb.sigView)
		}
	}
	active := amb.active
	if sigAnimate || spinAnimate {
		active = gk.GetWindowInfo(cx, a.window).Active
	}
	focusChanged := active != amb.active
	amb.active = active
	if focusChanged && spinAnimate {
		if !active {
			amb.ellipsis = 0
		}
		for _, slot := range amb.spinners {
			if slot.embedded != 0 && slot.embedded == a.renderGeneration {
				gk.Notify(cx, slot.view)
			}
		}
	}
	period, run := ambientTick(sigAnimate, spinAnimate, ellipsisAnimate, a.reducedMotion, active)
	if amb.ticking && (!run || period != amb.period) {
		gk.Release(cx, amb.ticker)
		amb.ticking = false
	}
	if run && !amb.ticking {
		amb.ticker = gk.Every(cx, period, func(cx *gk.Cx) {
			gk.Quiet(cx)
			a.tickAmbient(cx)
		})
		amb.ticking, amb.period = true, period
	}
}

func (a *App) tickAmbient(cx *gk.Cx) {
	amb := a.lineSlots.ambient
	if amb == nil {
		return
	}
	any := false
	if amb.active {
		amb.tick++
		if !a.reducedMotion && amb.sigEmbedded != 0 && amb.sigEmbedded == a.renderGeneration &&
			a.firstState() != installflow.FirstTwo {
			amb.sigFound = a.signatureFound()
			gk.Notify(cx, amb.sigView)
			any = true
		}
	} else {
		amb.ellipsis++
	}
	if !a.reducedMotion {
		for _, slot := range amb.spinners {
			if slot.embedded != 0 && slot.embedded == a.renderGeneration {
				gk.Notify(cx, slot.view)
				any = true
			}
		}
	}
	if !any && amb.ticking {
		gk.Release(cx, amb.ticker)
		amb.ticking = false
	}
}
