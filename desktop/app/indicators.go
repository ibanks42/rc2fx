package app

// Animated level indicators (editor live lines and overview bars).
//
// Each indicator owns its own gk.View so gk.Notify only re-renders that view
// instead of dirtying the whole app tree every frame. A single shared gk.Every
// timer drives redraws at 30 Hz (10 Hz unfocused; minimized windows can't be
// detected from Go so they also tick at 10 Hz), skipping indicators whose
// rendered output hasn't changed. Static indicators (stale audio, reduced
// motion, or animation disabled) are updated on the 50 ms app poll instead.

import (
	"slices"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
)

type lineSource struct{ path string }

func linePath(id string) lineSource { return lineSource{path: id} }

type indicatorKey struct {
	id    string
	shape levels.Shape
}

type indicatorView struct {
	key  indicatorKey
	view gk.View
	// App.renderGeneration that last embedded this view.
	embedded uint64
	drawn    bool
	fresh    bool
	animate  bool
	level    *float32
	size     gk.GpuiSize
	measured bool
}

// LineSlots holds indicator views (retained for the app's lifetime, at most
// one per path and shape), pending creation requests, and the shared ticker.
type LineSlots struct {
	views map[indicatorKey]*indicatorView
	// Keys drawn inline because their view hasn't been created yet.
	wanted  []indicatorKey
	epoch   time.Time
	ticker  gk.Timer
	ticking bool
	period  time.Duration
	// renderGeneration+1 of the last render that drew a bar inline (0 = none).
	inlineBars uint64
	ambient    *ambientState
}

func (a *App) canAnimate() bool {
	return levels.Enabled() && a.animatedPage() && !a.reducedMotion
}

// embed returns the indicator's child view for this render, or false if the
// view doesn't exist yet (queued for creation) or was already embedded once
// in this render pass.
func (a *App) embed(key indicatorKey) (gk.Element, bool) {
	slots := &a.lineSlots
	v := slots.views[key]
	if v == nil {
		if !slices.Contains(slots.wanted, key) {
			slots.wanted = append(slots.wanted, key)
		}
		return nil, false
	}
	if v.embedded == a.renderGeneration {
		return nil, false
	}
	v.embedded = a.renderGeneration
	return gk.Div(s.SizeFull).ChildView(v.view), true
}

func (a *App) indicatorNow(key indicatorKey, now time.Time) (level *float32, fresh, animate bool) {
	if key.shape.IsBar {
		level = a.overviewLevel(key.id)
		fresh = level != nil
	} else {
		meter, _ := a.freshMeterFor(key.id)
		fresh = meter != nil
		var l float32
		if fresh {
			l = levelOf(a.levels.Get(key.id), now)
		}
		level = &l
	}
	return level, fresh, fresh && a.canAnimate()
}

func (a *App) newIndicatorView(key indicatorKey) *indicatorView {
	v := &indicatorView{key: key}
	v.view = gk.NewView(nil, func(cx *gk.Cx) gk.Element { return a.drawIndicator(v) })
	return v
}

func (a *App) drawIndicator(v *indicatorView) gk.Element {
	now := time.Now()
	level, fresh, animate := a.indicatorNow(v.key, now)
	v.drawn, v.fresh, v.animate, v.level = true, fresh, animate, level
	if v.key.shape.IsBar {
		return levelBarFill(v.key.shape.Bar, level)
	}
	var phase float32
	if animate {
		phase = float32(max(now.Sub(a.lineSlots.epoch), 0).Seconds())
	}
	state := levels.LineStateUnavailable
	if fresh {
		state = levels.LineStateLive
	}
	return gk.Div(s.SizeFull).
		OnChildrenPrepainted(func(cx *gk.Cx, bounds []gk.Bounds) {
			// Only notify when bounds actually change to avoid a redraw loop.
			if len(bounds) == 0 || v.measured && bounds[0].Size == v.size {
				gk.Quiet(cx)
				return
			}
			v.size, v.measured = bounds[0].Size, true
		}).
		Child(livePlot(v.size.Width, v.size.Height, phase, *level, state, v.key.shape.Line))
}

func (a *App) indicatorChanged(v *indicatorView, now time.Time) bool {
	if !v.drawn {
		return false
	}
	level, fresh, animate := a.indicatorNow(v.key, now)
	if fresh != v.fresh || animate != v.animate {
		return true
	}
	claim := levels.Claim{Shape: v.key.shape, Height: v.size.Height, Measured: v.measured}
	if animate {
		slots := levels.Slots{Claims: []levels.Claim{claim}, Drawn: []*float32{v.level}}
		return !slots.Unchanged([]*float32{level})
	}
	return !levels.StillUnchanged(claim, v.level, level)
}

func (a *App) onScreen(v *indicatorView) bool {
	return v.embedded != 0 && v.embedded == a.renderGeneration
}

func (a *App) redrawIndicators(cx *gk.Cx, now time.Time) {
	for _, v := range a.lineSlots.views {
		if a.onScreen(v) && a.indicatorChanged(v, now) {
			gk.Notify(cx, v.view)
		}
	}
}

// pollIndicators creates newly requested views, manages the animation ticker,
// and updates static indicators on the 50 ms app poll.
func (a *App) pollIndicators(cx *gk.Cx) {
	slots := &a.lineSlots
	now := time.Now()
	if slots.epoch.IsZero() {
		slots.epoch = now
	}
	if len(slots.wanted) > 0 {
		if slots.views == nil {
			slots.views = map[indicatorKey]*indicatorView{}
		}
		for _, key := range slots.wanted {
			if slots.views[key] == nil {
				slots.views[key] = a.newIndicatorView(key)
			}
		}
		slots.wanted = slots.wanted[:0]
		// Re-render so the new views replace the inline fallbacks.
		a.notify(cx)
	}
	animating := 0
	for _, v := range slots.views {
		if !a.onScreen(v) {
			continue
		}
		if _, _, animate := a.indicatorNow(v.key, now); animate {
			animating++
		}
	}
	active := false
	if animating > 0 {
		active = gk.GetWindowInfo(cx, a.window).Active
	}
	period, run := levels.Tick(animating, a.reducedMotion, active)
	period = period.Round(time.Millisecond)
	if slots.ticking && (!run || period != slots.period) {
		gk.Release(cx, slots.ticker)
		slots.ticking = false
	}
	if run && !slots.ticking {
		slots.ticker = gk.Every(cx, period, func(cx *gk.Cx) {
			gk.Quiet(cx)
			a.redrawIndicators(cx, time.Now())
		})
		slots.ticking, slots.period = true, period
	}
	if !slots.ticking {
		a.redrawIndicators(cx, now)
	}
	a.pollAmbient(cx)
}

// barsAnimate reports whether all overview bars are backed by their own views
// (meaning meter updates don't need to trigger a full app re-render).
func (a *App) barsAnimate() bool {
	return a.lineSlots.inlineBars != a.renderGeneration+1
}
