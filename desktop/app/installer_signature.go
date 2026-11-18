package app

// The decorative signature line of the first screen. Its math (which
// picture, the line's shape) is in installflow/signature.go; this draws it
// as a gk.Plot.

import (
	"math"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
)

// signatureSize is the signature's slot as last laid out. A gk view cannot
// read its bounds while painting, so the slot reports its
// size after each frame (OnChildrenPrepainted) and the next frame draws at
// it. There is one signature view in the app (App.onboardingSignature), so
// one size. UI thread only.
var signatureSize gk.GpuiSize

// signatureScene is the connection screen's signature,
// ready once a mixer is found.
func signatureScene(found bool, phase float32) gk.Element {
	state := installflow.SignatureState{Kind: installflow.SignatureLooking}
	if found {
		state.Kind = installflow.SignatureReady
	}
	return signaturePaint(state, phase)
}

func signaturePaint(state installflow.SignatureState, phase float32) gk.Element {
	parts := signatureParts(state, phase, signatureSize.Width, signatureSize.Height)
	return gk.Div(s.SizeFull).
		OnChildrenPrepainted(func(cx *gk.Cx, bounds []gk.Bounds) {
			if len(bounds) == 0 || bounds[0].Size == signatureSize {
				gk.Quiet(cx)
				return
			}
			signatureSize = bounds[0].Size
		}).
		Child(gk.Plot(parts...))
}

// signatureStroke is a stroke painted with linear_gradient(90°, start at 0,
// end at 1) across the path's width, at opacity alpha.
func signatureStroke(path [][2]float32, width float32, start, end uint32, alpha float32) gk.PlotPart {
	return gk.NewPlotLine(path, width, gk.NewLinearGradient(90,
		gk.NewLinearColorStop(rgbAlpha(start, alpha), 0),
		gk.NewLinearColorStop(rgbAlpha(end, alpha), 1)))
}

// signatureParts is the signature's plot, for a slot of width ×
// height pixels: the gray line, then the filled part in the gradient.
func signatureParts(state installflow.SignatureState, phase, width, height float32) []gk.PlotPart {
	if width < 4 || height < 2 || math.IsNaN(float64(phase)) || math.IsInf(float64(phase), 0) {
		return nil
	}
	mid := height / 2
	var fill *float32
	switch state.Kind {
	case installflow.SignatureComplete:
		one := float32(1)
		fill = &one
	case installflow.SignatureWorking, installflow.SignatureFailed:
		fill = state.Fill
	}
	noise, voice := float32(1), float32(1)
	switch state.Kind {
	case installflow.SignatureLooking:
		noise, voice = 0.6, 0
	case installflow.SignatureComplete:
		noise, voice = 0, 1
	}
	failed := state.Kind == installflow.SignatureFailed
	// Peaks and the widest glow stroke must stay inside the slot, whatever
	// its height; the line is never clipped.
	headroom := max(mid-6, 0)
	amplitude := min(width*130/1584, 70, headroom*0.8)
	steps := int(max(float32(math.Ceil(float64(width/1.5))), 2))
	fillOrZero := float32(0)
	if fill != nil {
		fillOrZero = *fill
	}
	us := make([]float32, steps+1)
	points := make([][2]float32, steps+1)
	for i := 0; i <= steps; i++ {
		u := float32(i) / float32(steps)
		var y float32
		if !(failed && u > fillOrZero) {
			y = min(max(installflow.Offset(u, phase, i, noise, voice)*amplitude, -headroom), headroom)
		}
		us[i] = u
		points[i] = [2]float32{u * width, mid - y}
	}
	// stroke is the points up to and including u = end.
	stroke := func(end float32) [][2]float32 {
		n := 0
		for n < len(us) && us[n] <= end {
			n++
		}
		return points[:n]
	}
	gray := float32(0.45)
	if state.Kind == installflow.SignatureLooking {
		gray = 0.25
	}
	grayColor := uint32(0xf4efe6)
	if failed {
		grayColor = Warn
	}
	var parts []gk.PlotPart
	for _, pass := range [][2]float32{{8. / 2, gray * 0.25}, {2.5 / 2, gray}} {
		if line := stroke(1); len(line) > 0 {
			parts = append(parts, gk.NewPlotLine(line, pass[0], rgbAlpha(grayColor, pass[1])))
		}
	}
	if fill != nil && *fill > 0 {
		start, end := Accent, Violet
		if failed {
			start, end = Warn, Danger
		}
		line := stroke(*fill)
		for _, pass := range [][2]float32{{11, 0.12}, {4.5, 0.25}, {1.5, 1}} {
			parts = append(parts, signatureStroke(line, pass[0], start, end, pass[1]))
		}
	}
	return parts
}
