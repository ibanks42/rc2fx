package app

// Live editing loop: pushes draft changes to the mixer immediately and saves
// once edits settle, prompting first when a change requires an engine restart.

import (
	"fmt"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func (a *App) syncLive(cx *gk.Cx) {
	if !a.session.Connected || a.session.Saved == nil {
		a.live.Reset()
		return
	}
	now := time.Now()
	draft := a.session.Draft
	saved := a.session.Saved.Config.Targets
	a.live.Observe(draft, now)
	a.live.Saved(saved)
	if !a.streamConnected || a.session.Conflict || a.pending != nil || a.presets.busy() || a.fileBusy ||
		a.transitionBusy() || a.restartAsked() {
		a.logHeld(fmt.Sprintf("held: stream %v conflict %v pending %v presets %v file %v transition %v restartAsked %v",
			a.streamConnected, a.session.Conflict, a.pending != nil, a.presets.busy(), a.fileBusy, a.transitionBusy(), a.restartAsked()))
		return
	}
	a.logHeld("")
	switch a.live.NextLive(draft, saved, now) {
	case model.LiveSend:
		pending, err := a.session.BeginApply()
		saveLogf("live edit sent (err %v)", err)
		if err != nil {
			a.message = err.Error()
		}
		if err != nil || !a.command(transport.CommandLive{Generation: a.generation, Update: pending.Request}) {
			a.live.LiveAnswered(false, false, true, false, now)
		}
		return
	case model.LiveRestart:
		if a.live.Quiet(now) >= restartAskDelay {
			a.askRestart(cx)
		}
		return
	}
	if a.live.SaveDue(draft, saved, a.session.Dirty(), now) {
		if a.live.Support == model.LiveNo && core.LiveNeedsSave(saved, draft) {
			a.askRestart(cx)
			return
		}
		a.apply(cx)
	}
}

// Debounce restart prompts briefly so quick toggle-flips don't pop a dialog.
const restartAskDelay = 400 * time.Millisecond

// askRestart confirms changes that require an audio engine restart; canceling
// reverts the draft back to what is currently running.
func (a *App) askRestart(cx *gk.Cx) {
	if a.confirmOpen != nil {
		return
	}
	a.confirm(cx, confirmKindRestart, Confirm{
		Eyebrow: "Restarts the audio",
		Title:   "Apply this change now?",
		Detail: "Switching noise suppression or its lookahead, or reordering effects while it is on, restarts " +
			"the mixer's audio engine. Processed channels go quiet for a moment.",
		Cancel: "Undo the change",
		Action: "Apply",
	}, func(cx *gk.Cx) {
		a.apply(cx)
	}, func(cx *gk.Cx) {
		if a.pending == nil && a.session.Saved != nil {
			running := a.live.Running(a.session.Saved.Config.Targets)
			if core.LiveNeedsSave(running, a.session.Draft) {
				a.session.Draft = core.CloneTargets(running)
				a.rebuildEditors(cx)
			}
		}
		a.notify(cx)
	})
}

func (a *App) liveReply(cx *gk.Cx, k transport.EventKindLiveApplied) {
	saveLogf("live edit answered: %v %s", k.Result, k.Message)
	needsSave := a.live.LiveAnswered(k.Result == transport.LiveAccepted,
		k.Result == transport.LiveUnsupported, k.Result == transport.LiveBusy,
		k.Result == transport.LiveNeedsSave, time.Now())
	switch k.Result {
	case transport.LiveConflict:
		a.session.Conflict = true
		a.awaitingRefresh = a.command(transport.CommandRefresh{Generation: a.generation})
	case transport.LiveFailed:
		a.message = k.Message
	}
	if needsSave && a.confirmOpen == nil {
		a.askRestart(cx)
	}
}

func (a *App) undo(cx *gk.Cx) {
	if previous, ok := a.live.Undo(a.session.Draft, time.Now()); ok {
		a.setDraft(cx, previous)
	}
}

func (a *App) redo(cx *gk.Cx) {
	if next, ok := a.live.Redo(a.session.Draft, time.Now()); ok {
		a.setDraft(cx, next)
	}
}

func (a *App) revertChannel(cx *gk.Cx) {
	target := a.target()
	checkpoint, ok := a.live.CheckpointOf(target.ID)
	if !ok {
		gk.Quiet(cx)
		return
	}
	draft := core.CloneTargets(a.session.Draft)
	for i := range draft {
		if draft[i].ID == checkpoint.ID {
			draft[i] = checkpoint
		}
	}
	a.session.Draft = draft
	a.rebuildEditors(cx)
	a.notify(cx)
}

func (a *App) setDraft(cx *gk.Cx, draft []core.Target) {
	if len(draft) != len(a.session.Draft) {
		gk.Quiet(cx)
		return
	}
	a.session.Draft = draft
	a.rebuildEditors(cx)
	a.notify(cx)
}

func (a *App) liveBar(target *core.Target) gk.Element {
	icon := func(id string, name gk.IconName, enabled bool, tip string, run func(cx *gk.Cx)) gk.Element {
		d := gk.HFlex(s.ItemsCenter, s.JustifyCenter, s.FlexShrink0).Size(px(26)).Rounded(px(6))
		color := Dim
		if enabled {
			color = Muted
			d = d.Style(s.CursorPointer).Hover(hover().Bg(white(0.06)))
		}
		return d.Child(gk.Icon(name).Size(px(14)).TextColor(rgb(color))).ID(id).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if !enabled {
					gk.Quiet(cx)
					return
				}
				run(cx)
			}).
			Tooltip(gk.NewKitTooltip(tip))
	}
	status := gk.Div(s.Flex1, s.MinW0, s.Truncate).TextSize(px(12)).TextColor(rgb(Faint))
	var statusEl gk.Element
	if reason := a.live.SaveError(); reason != "" && (a.pending != nil || a.session.Dirty()) {
		message := saveFailedMessage(reason)
		statusEl = status.TextColor(rgb(Danger)).Child(txt(message)).ID("save-error").
			Tooltip(gk.NewKitTooltip(message + " RC2FX tries again shortly."))
	} else if since, changed := a.live.Changed(*target); changed {
		statusEl = status.Child(txt("Changed " + ago(time.Since(since))))
	} else if a.pending != nil || a.session.Dirty() {
		statusEl = status.Child(txt("Saving…"))
	} else {
		statusEl = status.Child(txt("Saved"))
	}
	bar := gk.HFlex(s.ItemsCenter, s.Gap1).Pl(px(8)).Pr(px(4)).Mt(px(10)).
		Child(icon("undo", gk.IconUndo2, a.live.CanUndo(), "Undo (Ctrl+Z)", a.undo)).
		Child(icon("redo", gk.IconRedo2, a.live.CanRedo(), "Redo (Ctrl+Shift+Z)", a.redo)).
		Child(gk.Div().W(px(6))).
		Child(statusEl)
	if _, changed := a.live.Changed(*target); changed {
		bar = bar.Child(gk.Div(s.FlexShrink0, s.CursorPointer).Px(px(6)).Rounded(px(5)).TextSize(px(12)).
			TextColor(rgb(Accent)).Hover(hover().Bg(white(0.06))).
			Child(txt("Revert")).ID("revert-channel").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.revertChannel(cx) }).
			Tooltip(gk.NewKitTooltip("Put this channel back as it was before this round of edits. Undo brings the edits back.")))
	}
	return bar
}

func ago(d time.Duration) string {
	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%d min ago", int(d/time.Minute))
	}
	return fmt.Sprintf("%d h ago", int(d/time.Hour))
}
