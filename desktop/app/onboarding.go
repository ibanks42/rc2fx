package app

// The opening screens' background, halo, headline and
// animated signature. Onboarding has its own animation lifetime: it does not
// use Home's indicators. Its decorative motion follows the system motion
// policy.

import (
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/home"
	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

// background is the opening screens' wash, top to bottom.
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

// haloImage is the halo, made once when the app starts (makeHalo) and kept:
// gpui uploads an image to the GPU once per image, so one made on every
// render would upload, and keep, a new copy each time. As a live blurred
// shadow it covered the whole window with per-pixel blur maths on every
// frame: more than half the GPU time of these screens on an integrated GPU,
// for a picture that never changes. An image costs one texture read a pixel.
var haloImage *gk.RenderImage

// makeHalo makes haloImage (in the app's init; images can't be made while a
// view is described). home.HaloPixels gives bytes in RenderImage's own
// order, BGRA; gk.PixelImage takes RGBA and swaps
// red and blue on its way to the same RenderImage, so they are swapped here
// first.
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

// backdrop is the halo behind the opening screens (Dusk only). The halo
// belongs to the window, never to a clipped line-sized canvas.
func backdrop() gk.Element {
	d := gk.Div(s.Absolute, s.Inset0, s.SizeFull)
	if appearance() == AppearanceDusk && haloImage != nil {
		// The halo is meant to stretch (ObjectFit::Fill); gk's ImgEl
		// has no ObjectFit yet, so gpui's default (Contain) applies: at a
		// window shape other than the halo's own (1320 × 860) the glow is
		// narrower or shorter than the window.
		d = d.Child(gk.Img(gk.ImageSourceRender{Value: *haloImage}, s.SizeFull))
	}
	return d
}

// headline is the opening screens' two-line headline, the second line
// faint.
func headline(first, second string) gk.DivEl {
	return gk.VFlex(s.ItemsCenter, s.TextCenter, s.FontExtrabold).TextSize(px(DisplayText)).
		LineHeight(gk.Relative(0.98)).
		Child(txt(first)).
		Child(gk.Div().TextColor(rgb(Faint)).Child(txt(second)))
}

// signatureFrame is 30 Hz. The line's noise steps 30 times a second (see
// installflow.Offset), so faster frames only repaint the same picture.
const signatureFrame = 33 * time.Millisecond

// signature is the connection screen's and the
// installer's animated line, a view of its own redrawn by its own ticker.
type signature struct {
	view    gk.View
	epoch   time.Time
	ticker  gk.Timer
	ticking bool
}

// newSignature makes the signature's view (call in init). Its ticker is
// started and stopped by the poll (pollSignature).
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

// signatureActive is whether the first screen's line moves. It stands
// still while two mixers wait for one to be unplugged. (It keeps moving
// while the window is hidden: gk does not report that.)
func (a *App) signatureActive() bool {
	return !a.reducedMotion && !a.session.Connected && a.installer == nil &&
		a.firstState() != installflow.FirstTwo
}

// pollSignature starts the signature's ticker when it becomes active and
// stops it when it no longer is; the ticker also stops itself.
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
