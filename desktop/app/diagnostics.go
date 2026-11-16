package app

// diagnostics.rs: the diagnostics file export, the events window's polling
// and reset, and the Telemetry section's header controls.

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

func (a *App) requestDiagnostics(cx *gk.Cx) {
	if a.diagnosticsPending || a.fileBusy || !a.session.Connected {
		return
	}
	a.diagnosticsPending = a.command(transport.CommandDiagnostics{Generation: a.generation})
	if a.diagnosticsPending {
		a.message = "Collecting mixer diagnostics…"
	}
	a.notify(cx)
}

// prettyReport is serde_json::to_string_pretty of the report the mixer sent
// (core's Value keeps the mixer's key order and serde's layout).
func prettyReport(report json.RawMessage) (string, error) {
	value, err := core.ParseJSONBytes(report, max(len(report), 1), math.MaxInt)
	if err != nil {
		return "", err
	}
	return value.Pretty(), nil
}

func (a *App) saveDiagnostics(cx *gk.Cx, report json.RawMessage, err error) {
	a.diagnosticsPending = false
	if a.fileBusy {
		a.message = "Finish the open file dialog, then export diagnostics again."
		return
	}
	var text string
	if err == nil {
		text, err = prettyReport(report)
	}
	if err != nil {
		a.message = fmt.Sprintf("Could not collect diagnostics: %s", err)
		return
	}
	directory := os.Getenv("USERPROFILE")
	if directory == "" {
		directory = os.Getenv("HOME")
	}
	if directory == "" {
		directory = "."
	}
	a.fileBusy = true
	done := func(cx *gk.Cx, path string, err error) {
		a.fileBusy = false
		switch {
		case err != nil:
			a.message = fmt.Sprintf("Could not save diagnostics: %s", err)
		case path == "":
			a.message = "Diagnostic export canceled"
		default:
			a.message = "Saved diagnostics to " + path
		}
		a.notify(cx)
	}
	gk.PromptForNewPath(cx, directory, "rc2fx-diagnostics.json", func(cx *gk.Cx, d gk.PathsDone) {
		switch {
		case d.Status == gk.DoneFailed:
			done(cx, "", errors.New(d.Error))
		case d.Status != gk.DoneAnswered || len(d.Paths) == 0:
			done(cx, "", nil)
		default:
			path := d.Paths[0]
			gk.Go(cx, func() error { return writeExport(path, text) }, func(cx *gk.Cx, err error) {
				done(cx, path, err)
			})
		}
	})
}

// openEvents opens the Diagnostic events window, or brings it forward.
func (a *App) openEvents(cx *gk.Cx) {
	reopened := a.eventsWindow != nil
	a.eventsWindow = openEventsWindow(cx, a.eventsWindow, a.eventLog, a, func(cx *gk.Cx) {
		// Closed by the user: stop reading (diagnostics.rs events_tick).
		a.eventsWindow = nil
		a.eventsPending = false
	})
	if !reopened {
		// A fresh window reads from the start of what the mixer keeps.
		a.eventLog.Update(cx, func(log *EventLog) { log.Clear() })
		a.eventsPolled = nil
	}
}

// telemetryAside is the Telemetry section's header controls: when its
// counters start, the events window and the reset.
func (a *App) telemetryAside(cx *gk.Cx) gk.DivEl {
	connected := a.session.Connected && a.streamConnected
	// Peek: the Rust read the log without observing it; the main view
	// redraws on its own changes.
	log := a.eventLog.Peek()
	setConfirm := func(value bool) func(cx *gk.Cx) {
		return func(cx *gk.Cx) {
			a.confirmReset = value
			a.notify(cx)
		}
	}
	aside := gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).
			Child(txt("Live · " + telemetry.SinceLabel(log.Since, log.Epoch))))
	if !a.confirmReset {
		aside = aside.Child(gk.Button("show-events").Outline().Small().Icon(gk.IconExternalLink).Label("Events").
			Tooltip("Open the events window").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.openEvents(cx) }))
	}
	return aside.Child(resetControl("telemetry-reset", "Reset…", a.confirmReset,
		connected && !a.resetPending, setConfirm(true), setConfirm(false), a.resetDiagnostics))
}

// resetDiagnostics asks the mixer to restart its counters and event history.
func (a *App) resetDiagnostics(cx *gk.Cx) {
	a.confirmReset = false
	a.eventLog.Update(cx, func(log *EventLog) { log.ConfirmReset = false })
	if a.session.Connected && !a.resetPending {
		a.resetPending = a.command(transport.CommandResetDiagnostics{Generation: a.generation})
		if a.resetPending {
			a.message = "Resetting mixer diagnostics…"
		}
	}
	a.notify(cx)
}

func (a *App) resetReply(cx *gk.Cx, report json.RawMessage, err error) {
	a.resetPending = false
	if err != nil {
		a.message = fmt.Sprintf("Could not reset diagnostics: %s", err)
		return
	}
	a.message = "Mixer diagnostics reset"
	now := time.Now()
	a.eventLog.Update(cx, func(log *EventLog) { log.Since = &now })
	// Read the reset entry, and whatever followed it, straight away.
	a.eventsPolled = nil
	a.receive(cx, report, false, false)
}

func (a *App) eventsReply(cx *gk.Cx, events core.DiagnosticsEvents, err error) {
	a.eventsPending = false
	received := time.Now()
	names := make(map[string]string, len(a.session.Draft))
	for i := range a.session.Draft {
		t := &a.session.Draft[i]
		names[t.ID] = a.targetName(t)
	}
	a.eventLog.Update(cx, func(log *EventLog) {
		if err != nil {
			log.Error = fmt.Sprintf("Could not read events: %s", err)
			return
		}
		log.Error = ""
		if events.More {
			// Catch up without waiting for the next interval.
			a.eventsPolled = nil
		}
		log.ReceivePage(events, received, func(id string) string {
			if name, ok := names[id]; ok {
				return name
			}
			return id
		})
	})
	switch {
	case err != nil:
		a.startup.GiveUp()
	case !events.More:
		log := a.eventLog.Peek()
		a.startup.ReadEvents(log.XrunsBetween)
	}
}

func optionalEqual(a, b *uint64) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// eventsTick is called from the main poll: it keeps the events window's
// view of the connection current and, while it is open, reads new events.
// (A closed window is noticed by openEvents' close handler, which the Rust
// checked here.)
func (a *App) eventsTick(cx *gk.Cx) {
	connected := a.session.Connected && a.streamConnected
	var periodNs *uint64
	if state := a.session.Saved; state != nil && state.Engine.SampleRate > 0 {
		p := uint64(state.Engine.BlockSize) * 1_000_000_000 / uint64(state.Engine.SampleRate)
		periodNs = &p
	}
	if !optionalEqual(a.eventLog.Peek().PeriodNs, periodNs) {
		a.eventLog.Update(cx, func(log *EventLog) { log.PeriodNs = periodNs })
	}
	var epoch *uint64
	if a.meter != nil {
		e := a.meter.DiagnosticsEpoch
		epoch = &e
	}
	log := a.eventLog.Peek()
	if log.Connected != connected || (epoch != nil && *epoch != log.Epoch) {
		a.eventLog.Update(cx, func(log *EventLog) {
			log.Connected = connected
			if epoch != nil && *epoch != log.Epoch {
				// A reset this app did not just make or read about (another
				// client's) restarts the counters at a time the app does not
				// know.
				if log.Since == nil || time.Since(*log.Since) > 5*time.Second {
					log.Since = nil
				}
				log.Epoch = *epoch
			}
		})
	}
	// Telemetry reads the log once, to place faults from before this app
	// connected.
	startup := a.telemetryOpen && a.startup.WantsEvents()
	if (a.eventsWindow == nil && !startup) || !connected || a.eventsPending ||
		(a.eventsPolled != nil && time.Since(*a.eventsPolled) < telemetry.Poll) {
		return
	}
	now := time.Now()
	a.eventsPolled = &now
	var after uint64
	if next := a.eventLog.Peek().NextSeq; next != nil {
		after = *next
	}
	a.eventsPending = a.command(transport.CommandDiagnosticsEvents{Generation: a.generation, After: after})
}

// resetEvents: a new connection or none: forget the old mixer's events and
// any request whose reply will never arrive.
func (a *App) resetEvents(cx *gk.Cx) {
	a.eventsPending = false
	a.eventsPolled = nil
	a.resetPending = false
	a.confirmReset = false
	a.eventLog.Update(cx, func(log *EventLog) {
		log.Clear()
		log.Since = nil
		log.Epoch = 0
		log.ConfirmReset = false
	})
}
