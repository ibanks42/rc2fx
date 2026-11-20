package app

import (
	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
)

// levelBarFill renders the filled portion of an overview meter bar.
// level is in [0, 1], or nil when no fresh reading is available.
func levelBarFill(bar levels.Bar, level *float32) gk.Element {
	fill := gk.Div().Rounded(px(bar.Radius()))
	switch {
	case level == nil:
		fill = fill.Bg(rgb(Dim))
	case bar == levels.BarVertical:
		// 0° points upward.
		fill = fill.Bg(gk.NewLinearGradient(0, gk.NewLinearColorStop(rgb(Accent), 0), gk.NewLinearColorStop(rgb(Violet), 1)))
	default:
		fill = fill.Bg(gradient())
	}
	length := px(levels.Stub)
	if level != nil {
		length = gk.Relative(levels.FillFraction(*level))
	}
	fill = fill.Style(s.Absolute, s.Left0)
	if bar == levels.BarVertical {
		fill = fill.Style(s.Bottom0, s.WFull).H(length)
	} else {
		fill = fill.Style(s.Top0, s.HFull).W(length)
	}
	return gk.Div(s.Relative, s.SizeFull).Child(fill)
}
