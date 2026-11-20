package app

// Landing/onboarding screen chrome: background gradient, cached halo texture,
// headline, and the 30 Hz animated signature view.

import (
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

// Match installflow.Offset's 30 Hz noise step rate; faster ticks just redraw the same frame.
const signatureFrame = 33 * time.Millisecond

type signature struct {
	view    gk.View
	epoch   time.Time
	ticker  gk.Timer
	ticking bool
}

func newSignature(a *App) gk.View {
	sig := &signature{epoch: time.Now()}
	sig.view = gk.NewView(nil, func(cx *gk.Cx) gk.Element {
		phase := float32(time.Since(sig.epoch).Seconds())
		first := a.firstState()
		return signatureScene(first == installflow.FirstFound || first == installflow.FirstDuo, phase)
	})
	a.lineSlots.signature = sig
	return sig.view
}

func (a *App) signatureActive() bool {
	return !a.reducedMotion && !a.session.Connected && a.installer == nil &&
		a.firstState() != installflow.FirstTwo
}

func (a *App) pollSignature(cx *gk.Cx) {
	sig := a.lineSlots.signature
	if sig == nil {
		return
	}
	active := a.signatureActive()
	if !active && sig.ticking {
		gk.Release(cx, sig.ticker)
		sig.ticking = false
	}
	if active && !sig.ticking {
		sig.ticker = gk.Every(cx, signatureFrame, func(cx *gk.Cx) {
			gk.Quiet(cx)
			if !a.signatureActive() {
				gk.Release(cx, sig.ticker)
				sig.ticking = false
				return
			}
			gk.Notify(cx, sig.view)
		})
		sig.ticking = true
	}
}
