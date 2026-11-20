package app

// Interactive EQ frequency response graph (axis math and curve caching live in edit/graph.go).

import (
	"fmt"
	"math"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
)

type EqCurve = edit.EQCurve

func (a *App) eqCurveDB() []float64 {
	return edit.Curve(&a.eqCurve, a.target().EQ, a.channel)
}

func (a *App) moveBand(cx *gk.Cx, position [2]float32) {
	frequency, gain, ok := a.graphPoint(position)
	if !ok {
		gk.Quiet(cx)
		return
	}
	bands := a.target().EQ.ChannelMut(a.channel)
	if a.band >= 0 && a.band < len(*bands) {
		band := &(*bands)[a.band]
		band.Frequency = frequency
		if edit.HasGain(band.BandType) {
			band.Gain = gain
		}
	}
	a.dragMoved = true
	a.syncEditorValues(cx)
	a.notify(cx)
}

func (a *App) graphPoint(position [2]float32) (frequency, gain float64, ok bool) {
	b := a.graphBounds
	return edit.GraphPoint(b.Origin.X, b.Origin.Y, b.Size.Width, b.Size.Height, position[0], position[1])
}

// eqPlot renders the grid, filled curve area, and stroke using the bounds
// captured on the previous prepaint pass.
func (a *App) eqPlot(curve []float64, width, tall float32) gk.Element {
	if width <= 0 || tall <= 0 {
		return gk.Plot()
	}
	at := func(x, y float64) [2]float32 { return [2]float32{float32(x) * width, float32(y) * tall} }
	var parts []gk.PlotPart
	rule := func(from, to [2]float32, color uint32) {
		parts = append(parts, gk.NewPlotLine([][2]float32{from, to}, 1, rgb(color)))
	}
	for _, frequency := range edit.GridHz {
		x := edit.FrequencyX(frequency)
		rule(at(x, 0), at(x, 1), 0x1c2133)
	}
	for _, db := range edit.GridDB {
		rule(at(0, edit.Height(db)), at(1, edit.Height(db)), 0x181d2c)
	}
	rule(at(0, 0.5), at(1, 0.5), surface().LineStrong)
	last := float64(max(len(curve), 2) - 1)
	points := make([][2]float32, len(curve))
	for i, db := range curve {
		points[i] = at(float64(i)/last, edit.Height(db))
	}
	// Close the filled area along the 0 dB centerline.
	area := make([][2]float32, 0, len(points)+2)
	area = append(area, at(0, 0.5))
	area = append(area, points...)
	area = append(area, at(1, 0.5))
	parts = append(parts,
		gk.NewPlotArea(area, rgba(0x7aa8f01f)),
		gk.NewPlotLine(points, 2.5, gradient()))
	return gk.Plot(parts...)
}

func (a *App) eqGraph(cx *gk.Cx) gk.Element {
	curve := a.eqCurveDB()
	label := func(text string) gk.DivEl {
		return gk.Div(s.Absolute).TextSize(px(10.5)).TextColor(rgb(Dim)).Child(txt(text))
	}
	area := gk.Div(s.Relative, s.SizeFull, s.Border1, s.CursorCrosshair).Rounded(px(14)).
		BorderColor(rgb(surface().Line)).Bg(gk.HSLA(0.62, 0.4, 0.03, 0.35)).
		// Capture the canvas's painted bounds (first child) for hit-testing and next frame's plot.
		OnChildrenPrepainted(func(cx *gk.Cx, children []gk.Bounds) {
			if len(children) == 0 || children[0] == a.graphBounds {
				gk.Quiet(cx)
				return
			}
			a.graphBounds = children[0]
		}).
		Child(gk.Div(s.Absolute, s.Inset0).
			Child(a.eqPlot(curve, a.graphBounds.Size.Width, a.graphBounds.Size.Height)))
	for _, frequency := range edit.GridHz {
		l := label(edit.GridLabel(frequency)).Bottom(px(8))
		if frequency >= 20000 {
			l = l.Right(px(6))
		} else {
			l = l.Left(gk.Relative(float32(edit.FrequencyX(frequency)))).Ml(px(5))
		}
		area = area.Child(l)
	}
	for _, db := range edit.GridDB {
		area = area.Child(label(edit.DBLabel(db)).Left(px(6)).Top(gk.Relative(float32(edit.Height(db)))).Mt(px(-15)))
	}
	graph := area.ID("eq-plot").
		OnMouseDown(gk.MouseButtonLeft, func(cx *gk.Cx, e gk.MouseEvent) {
			// Band points stop propagation, so events reaching here hit empty canvas.
			if e.ClickCount == 2 && len(a.target().EQ.Channel(a.channel)) < core.MaxBands {
				if frequency, gain, ok := a.graphPoint(e.Position); ok {
					a.addBand(cx, &[2]float64{frequency, gain})
					return
				}
			}
			gk.Quiet(cx)
		}).
		OnMouseMove(func(cx *gk.Cx, e gk.MouseEvent) {
			switch {
			case a.draggingBand && e.Button == gk.ButtonLeft:
				a.moveBand(cx, e.Position)
			case a.draggingBand:
				a.draggingBand = false
				a.notify(cx)
			default:
				gk.Quiet(cx)
			}
		}).
		OnMouseUp(gk.MouseButtonLeft, func(cx *gk.Cx, _ gk.MouseEvent) {
			// Treat a press-and-release without movement as a click to open the popover.
			if a.draggingBand && !a.dragMoved {
				a.draggingBand = false
				a.openBandPopover(cx, a.band)
				return
			}
			a.draggingBand = false
			a.notify(cx)
		})
	for index, band := range a.target().EQ.Channel(a.channel) {
		selected := index == a.band
		color := bandColor(index)
		x := gk.Relative(float32(edit.FrequencyX(band.Frequency)))
		gain := 0.
		if edit.HasGain(band.BandType) {
			gain = band.Gain
		}
		y := gk.Relative(float32(edit.Height(gain)))
		radius := float32(9)
		if selected {
			radius = 11
			graph = graph.Child(gk.Div(s.Absolute, s.RoundedFull).Left(x).Top(y).Ml(px(-17)).Mt(px(-17)).
				Size(px(34)).Bg(rgba(color<<8 | 0x24)))
		}
		fill, text := surface().Sunken, color
		if selected {
			fill, text = color, 0x0b0d14
		}
		point := gk.HFlex(s.Absolute, s.RoundedFull, s.Border2, s.ItemsCenter, s.JustifyCenter, s.FontBold,
			s.CursorGrab).Left(x).Top(y).Ml(px(-radius)).Mt(px(-radius)).Size(px(radius * 2)).
			BorderColor(rgb(color)).Bg(rgb(fill)).TextSize(px(11)).TextColor(rgb(text))
		if !band.Enabled {
			point = point.Opacity(0.4)
		}
		graph = graph.Child(point.Child(txt(fmt.Sprint(index+1))).ID(gk.Key("eq-point", index)).
			OnMouseDown(gk.MouseButtonLeft, func(cx *gk.Cx, _ gk.MouseEvent) {
				gk.StopPropagation(cx)
				if a.band != index {
					a.band = index
					a.rebuildEditors(cx)
				}
				a.draggingBand = true
				a.dragMoved = false
				a.notify(cx)
			}).
			OnScroll(func(cx *gk.Cx, e gk.ScrollEvent) {
				var delta float32
				switch d := e.Delta.(type) {
				case gk.ScrollDeltaPixels:
					delta = d.Y
				case gk.ScrollDeltaLines:
					delta = d.Y * 16
				}
				if delta == 0 {
					gk.Quiet(cx)
					return
				}
				gk.StopPropagation(cx)
				if a.band != index {
					a.band = index
					a.rebuildEditors(cx)
				}
				bands := a.target().EQ.ChannelMut(a.channel)
				if index < len(*bands) {
					b := &(*bands)[index]
					factor := 1 / 1.08
					if delta < 0 {
						factor = 1.08
					}
					b.Q = math.Round(math.Min(math.Max(b.Q*factor, 0.1), 100)*100) / 100
				}
				a.syncEditorValues(cx)
				a.notify(cx)
			}))
		if selected && a.draggingBand {
			readout := edit.ParameterFrequency.Display(band.Frequency)
			if edit.HasGain(band.BandType) {
				readout = readout + " · " + edit.ParameterBandGain.Display(band.Gain)
			}
			graph = graph.Child(gk.HFlex(s.Absolute, s.JustifyCenter).Left(x).Top(y).Ml(px(-90)).Mt(px(-46)).W(px(180)).
				Child(gk.Div(s.Border1, s.WhitespaceNowrap).Px(px(8)).Py(px(4)).Rounded(px(8)).Bg(rgb(0x0b0e16)).
					BorderColor(rgb(surface().LineStrong)).TextSize(px(12)).
					Child(txt(fmt.Sprintf("%s · Q %s", readout, edit.ParameterQ.Display(band.Q))))))
		}
	}
	// Mount the popover as a sibling of graph so clicks inside it don't bubble to the canvas.
	wrap := gk.Div(s.Relative, s.SizeFull).Child(graph)
	if index, ok := a.popoverIndex(); ok {
		band := a.target().EQ.Channel(a.channel)[index]
		gain := 0.
		if edit.HasGain(band.BandType) {
			gain = band.Gain
		}
		x := float32(edit.FrequencyX(band.Frequency))
		// Place the popover on whichever side/corner has more room away from the band point.
		panel := gk.Div(s.Absolute).Left(gk.Relative(x))
		if x < 0.55 {
			panel = panel.Ml(px(26))
		} else {
			panel = panel.Ml(px(-26 - PopoverWidth))
		}
		if edit.Height(gain) < 0.5 {
			panel = panel.Top(px(12))
		} else {
			panel = panel.Bottom(px(12))
		}
		wrap = wrap.Child(panel.Child(a.bandPopoverPanel(band, cx)))
	}
	return wrap
}
