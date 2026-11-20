package app

// The level indicator, the design's live line. Its height
// follows a path's reported output level (peak-meter ballistics,
// LevelHistory.Display); its shape is decorative, not an audio waveform, and
// the UI says so. This file only draws: when it moves is indicators.go's,
// the geometry is app/levels (LinePoints).

import (
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
	"github.com/ibanks42/rc2fx/desktop/app/model"
)

// LineStyle is the sizes of one indicator (levels.LineStyle).
type LineStyle = levels.LineStyle

// levelOf is level_of: the displayed level of history at now, 0 to 1 on the
// meter scale.
func levelOf(history *model.LevelHistory, now time.Time) float32 {
	if history == nil {
		return 0
	}
	return history.Display(now, level)
}

// livePlot is paint_line: the indicator drawn in an element of width ×
// height (its bounds' size). phase (seconds) moves the decorative shape;
// level scales its height. The plot fills its parent whether or not it
// draws, so the layout never depends on it.
func livePlot(width, height, phase, lvl float32, state levels.LineState, style LineStyle) gk.Element {
	points, ok := levels.LinePoints(width, height, phase, lvl, state, style)
	if !ok {
		return gk.Plot()
	}
	path := make([][2]float32, len(points))
	for i, p := range points {
		path[i] = [2]float32{p.X, p.Y}
	}
	var parts []gk.PlotPart
	switch state {
	case levels.LineStateLive:
		// The RC2FX gradient at alpha.
		faded := func(alpha float32) gk.Fill {
			return gk.NewLinearGradient(90,
				gk.NewLinearColorStop(rgbAlpha(Accent, alpha), 0),
				gk.NewLinearColorStop(rgbAlpha(Violet, alpha), 1))
		}
		parts = append(parts, gk.NewPlotLine(path, style.Stroke, faded(1)))
	case levels.LineStateUnavailable:
		parts = append(parts, gk.NewPlotLine(path, style.Stroke, rgbAlpha(Faint, 0.5)))
	}
	return gk.Plot(parts...)
}
