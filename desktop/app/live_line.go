package app

// Live level line rendering. Amplitude tracks the path's peak meter; the wave
// shape is decorative rather than an audio waveform.

import (
	"time"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/chart"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
	"github.com/ibanks42/rc2fx/desktop/app/model"
)

type LineStyle = levels.LineStyle

func levelOf(history *model.LevelHistory, now time.Time) float32 {
	if history == nil {
		return 0
	}
	return history.Display(now, level)
}

// livePlot draws the decorative level line for the given bounds, animation
// phase (seconds), and level. Always returns a plot element so layout does not
// depend on whether points are available.
func livePlot(width, height, phase, lvl float32, state levels.LineState, style LineStyle) gk.Element {
	points, ok := levels.LinePoints(width, height, phase, lvl, state, style)
	if !ok {
		return chart.Plot()
	}
	path := make([][2]float32, len(points))
	for i, p := range points {
		path[i] = [2]float32{p.X, p.Y}
	}
	var parts []gk.PlotPart
	switch state {
	case levels.LineStateLive:
		faded := func(alpha float32) gk.Fill {
			return gk.NewLinearGradient(90,
				gk.NewLinearColorStop(rgbAlpha(Accent, alpha), 0),
				gk.NewLinearColorStop(rgbAlpha(Violet, alpha), 1))
		}
		parts = append(parts, chart.NewPlotLine(path, style.Stroke, faded(1)))
	case levels.LineStateUnavailable:
		parts = append(parts, chart.NewPlotLine(path, style.Stroke, rgbAlpha(Faint, 0.5)))
	}
	return chart.Plot(parts...)
}
