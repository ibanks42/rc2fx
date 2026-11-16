package app

// line_overlay.rs, redesigned: Home's and the editor's animated level
// indicators, the glowing lines and the overview's bars.
//
// Why the Rust had an overlay: GPUI renders the window root on every frame,
// and notifying a view also dirties its ancestors, so animating inside the
// app would have re-rendered the whole app on every frame. In the binding a
// view marked with gk.Notify is described alone, its parent kept: so each
// indicator is its own gk.View, embedded where the Rust claimed a slot, and
// one shared gk.Every redraws only the indicators whose picture would
// change (levels.Slots.Unchanged). The slots, claims, latches, content masks
// and geometry checks are gone; the decisions the Rust tests cover stay, in
// app/levels.
//
// Who draws an indicator (never both, never neither): Home, the sidebar and
// the editor ask for one with indicator or levelBar; it is that indicator's
// view, or, until the poll has made the view (and for a second request of
// the same indicator in one render), the same drawing made in place, still.
//
// When: the ticker runs at 60 Hz, 30 Hz while the window is not focused,
// only while some indicator on screen can animate (levels.Tick: a fresh
// reading, on Home or an editor, without reduced motion, animation on).
// Window visibility is not readable in Go, so a minimised window keeps
// ticking at 30 Hz. Indicators that do not animate (no fresh reading,
// reduced motion, Settings, RC2FX_ANIMATED_LINE=0) are drawn still, and the
// app's 50 ms poll redraws one when its level or freshness changes, as each
// app render redrew them in the Rust.

import (
	"slices"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/levels"
)

// lineSource is line_overlay.rs Source: what an indicator shows. Source::Path
// (one path's reported output peak) is its only kind.
type lineSource struct{ path string }

// linePath is Source::Path.
func linePath(id string) lineSource { return lineSource{path: id} }

// indicatorKey is one indicator: what it shows and how.
type indicatorKey struct {
	id    string
	shape levels.Shape
}

// indicatorView is one indicator's view and what it last drew.
type indicatorView struct {
	key  indicatorKey
	view gk.View
	// embedded is the app render (App.renderGeneration) that last placed it:
	// it is on screen while that is the latest render.
	embedded uint64
	// What the view drew last; drawn is false until it has drawn.
	drawn   bool
	fresh   bool
	animate bool
	level   *float32
	// size is a line's bounds as last laid out (measured false until then).
	size     gk.GpuiSize
	measured bool
}

// LineSlots is the indicators' state in App (the field kept its Rust name,
// line_slots): every indicator's view, the ones asked for before their
// view existed, the animation's clock and the shared ticker. Views are
// kept for the app's life: at most one per path and shape (a line in two
// styles, a bar in two directions).
type LineSlots struct {
	views map[indicatorKey]*indicatorView
	// wanted were drawn in place for want of a view; the poll makes theirs.
	wanted []indicatorKey
	// epoch is the animation's clock: shapes move with time since it.
	epoch   time.Time
	ticker  gk.Timer
	ticking bool
	period  time.Duration
	// inlineBars is the app render, plus one, that drew a bar in place (0
	// for none yet): barsAnimate.
	inlineBars uint64
	signature  *signature
}

// LineLayout was line_overlay.rs Layout (the window's size and scale, which
// released refused geometry). The redesign refuses no geometry, so it holds
// nothing; App.layout can go.
type LineLayout struct{}

// canAnimate is home.rs can_animate without the latch: whether an indicator
// with a fresh reading moves now.
func (a *App) canAnimate() bool {
	return levels.Enabled() && a.animatedPage() && !a.reducedMotion
}

// embed is the indicator's view for this render, or ok false when it has
// none yet (the poll makes it) or was placed already in this render.
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

// indicatorNow is what an indicator shows now: its level (a line's is 0
// without a fresh reading, a bar's nil), whether it has a fresh reading, and
// whether it moves.
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

// newIndicatorView makes an indicator's view (in a handler: the poll).
func (a *App) newIndicatorView(key indicatorKey) *indicatorView {
	v := &indicatorView{key: key}
	v.view = gk.NewView(nil, func(cx *gk.Cx) gk.Element { return a.drawIndicator(v) })
	return v
}

// drawIndicator is an indicator view's function: the overlay's paint for
// one claim, or Home's still drawing, from the app's state now.
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
			// The plot fills this div: its bounds are the line's. Stored only
			// when they change, or every frame would redraw the line.
			if len(bounds) == 0 || v.measured && bounds[0].Size == v.size {
				gk.Quiet(cx)
				return
			}
			v.size, v.measured = bounds[0].Size, true
		}).
		Child(livePlot(v.size.Width, v.size.Height, phase, *level, state, v.key.shape.Line))
}

// indicatorChanged is whether redrawing the indicator would change its picture.
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

// onScreen is whether the latest app render placed the indicator.
func (a *App) onScreen(v *indicatorView) bool {
	return v.embedded != 0 && v.embedded == a.renderGeneration
}

// redrawIndicators marks every indicator on screen whose picture would
// change.
func (a *App) redrawIndicators(cx *gk.Cx, now time.Time) {
	for _, v := range a.lineSlots.views {
		if a.onScreen(v) && a.indicatorChanged(v, now) {
			gk.Notify(cx, v.view)
		}
	}
}

// pollIndicators runs on the app's 50 ms poll: it makes the views asked
// for, starts, retimes or stops the shared ticker, redraws still
// indicators that changed, and runs the opening screens' signature.
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
		// Render again so the views replace the indicators drawn in place.
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
	// The timer counts whole milliseconds.
	period = period.Round(time.Millisecond)
	if slots.ticking && (!run || period != slots.period) {
		gk.Release(cx, slots.ticker)
		slots.ticking = false
	}
	if run && !slots.ticking {
		// One frame per tick (a target, not a measured frame rate); a late
		// tick is not made up.
		slots.ticker = gk.Every(cx, period, func(cx *gk.Cx) {
			gk.Quiet(cx)
			a.redrawIndicators(cx, time.Now())
		})
		slots.ticking, slots.period = true, period
	}
	if !slots.ticking {
		a.redrawIndicators(cx, now)
	}
	a.pollSignature(cx)
}

// barsAnimate is home.rs bars_animate: whether the overview's bars redraw
// themselves, so a reading needs no redraw of the app. In Go every bar with
// a view does, moving or still; only a bar drawn in place (before the poll
// made its view) needs the app redrawn. (The Rust asked the overlay, which
// stopped under reduced motion and while hidden.)
func (a *App) barsAnimate(cx *gk.Cx) bool {
	_ = cx
	return a.lineSlots.inlineBars != a.renderGeneration+1
}
