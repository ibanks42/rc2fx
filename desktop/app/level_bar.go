package app

// The overview's level bars. A bar is a div inside its track, filled to
// levels.FillFraction: a fader's from the bottom, an output's from the
// left, the levels.Stub pixels without a fresh reading. When it is redrawn is
// line_overlay.go's.

import (
	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
)

// levelBarFill is the bar in its track, which it fills
// (the track must be relative, as both meters are). level is 0 to 1, or nil
// without a fresh reading.
func levelBarFill(bar levels.Bar, level *float32) gk.Element {
	fill := gk.Div().Rounded(px(bar.Radius()))
	switch {
	case level == nil:
		fill = fill.Bg(rgb(Dim))
	case bar == levels.BarVertical:
		// From the bottom up (0° points up).
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
