// Package app is the RC2FX desktop app: the port of crates/rc2fx-app (lib.rs
// and its modules) onto gpuikit.dev/gk. One Go file per Rust module; the
// non-drawing parts live in the subpackages without gk (model, installflow,
// licenses, ...). stubs.go says how names inside the package are made.
package app

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime"
	"slices"
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

// Backend is the USB link to the mixer, or why there isn't one.
type Backend struct {
	live        *transport.Transport
	unavailable string
}

func newBackend() (Backend, string) {
	t, err := transport.New()
	if err != nil {
		return Backend{unavailable: err.Error()}, err.Error()
	}
	return Backend{live: t}, "Connect to load your mixer’s channels and settings"
}

func (b *Backend) send(command transport.Command) error {
	if b.live == nil {
		return errors.New(b.unavailable)
	}
	return b.live.Send(command)
}

func (b *Backend) tryRecv() (transport.Event, bool) {
	if b.live == nil {
		return transport.Event{}, false
	}
	return b.live.TryRecv()
}

// HomeKey is what Home last drew that can change with time alone
// (home.rs home_key's (String, String, Vec<bool>)).
type HomeKey struct {
	V0, V1 string
	V2     []bool
}

func homeKeyEqual(a, b *HomeKey) bool {
	if a == nil || b == nil {
		return a == b
	}
	return a.V0 == b.V0 && a.V1 == b.V1 && slices.Equal(a.V2, b.V2)
}

// App is Rc2FxApp: every field of it, including the ones other work
// packages use, then the Go-only handles at the end.
type App struct {
	session   core.Session
	transport Backend
	// generation tags commands; events of an older generation are dropped.
	generation uint64
	// connectedMixer is the USB serial of the mixer the session belongs to.
	connectedMixer *string
	// deviceVersions is what the connected mixer reported; nil is unknown.
	deviceVersions *core.DeviceVersions
	appUpdate      AppUpdate
	selfUpdate     SelfUpdate
	// developerMode shows developer controls in Settings; grants nothing by
	// itself.
	developerMode bool
	// devPage is the page RC2FX_DEV_PAGE asked for, until it has been opened.
	devPage *DevPage
	ssh     SSH
	// sshAddress is the address typed for the login command. Kept for this
	// run of the app, across reconnects; the pinned host key rejects a wrong
	// mixer.
	sshAddress gk.InputState
	// bundledMixerSoftware is the RC2FX mixer software this app installs,
	// read at start.
	bundledMixerSoftware *string
	selected             int
	inputs               ShowInputs
	effect               core.Effect
	band                 int
	channel              core.Channel
	editorEpoch          uint64
	// importDraft is lib.rs `import` (a Go keyword).
	importDraft *ImportDraft
	importError *string
	fileBusy    bool
	editors     []ParameterEditor
	// editingValue is the control whose value is being typed, if any.
	editingValue *Parameter
	// resave: a save was refused for an older revision; it is sent again once
	// the mixer's current settings arrive (settleConflict).
	resave bool
	// awaitingRefresh: waiting for the refresh that follows a refused save.
	awaitingRefresh bool
	// nav is the page in front: Home or a channel, and Settings, its
	// Diagnostics page or the installer over it (navigation.go).
	nav navigation
	// contentScroll is the main column's scroll position, shared by the
	// pages shown in it.
	contentScroll gk.ScrollHandle
	// usbRule is the Linux USB permission screen's request (connection.go).
	usbRule USBRule
	// usbRuleOpen is its "Show the exact file" disclosure.
	usbRuleOpen bool
	// copied is which Copy button just copied, briefly; "" for none.
	copied string
	// usbRuleText and usbUndoText are its read-only boxes: the rule's exact
	// text, and the undo command.
	usbRuleText   gk.TextareaState
	usbUndoText   gk.InputState
	subscriptions []gk.Subscription
	// retired and retiring hold kit state waiting to be released (editor.go
	// retire): retired this poll, retiring the poll before.
	retired, retiring []any
	pending           *core.PendingApply
	routingPending    *core.PendingRouting
	restartPending    *core.PendingRestart
	selectionPending  *core.PendingSelection
	// selectionAsked: this app asked for the current path change, so its
	// failure is ours to report. A failure the mixer reports on its own
	// (while shutting down for an update, say) is not.
	selectionAsked bool
	// installerAutostart is set by a notice's Install update or the first
	// screen's Continue the install: start the task as soon as it can, until
	// this deadline.
	installerAutostart *time.Time
	// settledFlash is the last flash this run of the app saw finish
	// (Model::settled_flash).
	settledFlash *uint64
	// allowDuo is the remembered opt-in to install on an (untested) Duo.
	allowDuo bool
	// firstHintTarget is the channel the finished install opened, whose
	// suppression page shows the first-channel hint; "" for none.
	// firstHintDone: dismissed or completed, on this computer.
	firstHintTarget string
	firstHintDone   bool
	// firstHintSaved: the hint was recorded as done on this computer.
	firstHintSaved bool
	// look is what the first screen last saw on USB (nil before it looked),
	// looking whether a look is under way, and lookedAt when the last ended.
	look     *firstLook
	looking  bool
	lookedAt time.Time
	// flowFixture: RC2FX_DEV_PAGE opened a guided-flow page from made-up
	// state (development builds): nothing connects, looks or downloads.
	flowFixture   bool
	telemetryOpen bool
	// telemetryDetails: Telemetry shows every figure, not just the summary.
	telemetryDetails bool
	// confirmReset: the Telemetry section's reset is waiting for confirmation.
	confirmReset bool
	// eventLog is the mixer's recent events, shown by the events window
	// (a gk.Model: both windows read it).
	eventLog     *gk.Model[EventLog]
	eventsWindow *gk.Window
	// eventsPolled is when the open events window last asked for events, and
	// eventsPending whether that request is still out.
	eventsPolled       *time.Time
	eventsPending      bool
	resetPending       bool
	diagnosticsPending bool
	presets            PresetLibrary
	// link is what the app is doing about the connection: off, connecting,
	// on, retrying at a time, or paused for a flash (link.go).
	link            linkIntent
	streamConnected bool
	streamError     *string
	usb             model.USBObservation
	message         string
	meter           *core.Meter
	meterProgress   map[string]*telemetryProgress
	// levels is recent output peaks per processed path, for the live line.
	levels model.LevelHistories
	// startup is what the mixer counted while starting, kept out of telemetry.
	startup telemetryStartupWindow
	// recentFaults is audible faults in the last minute, and when each last
	// happened.
	recentFaults telemetryRecentFaults
	// renderGeneration counts this view's renders; Home's line slot is
	// claimed per render (line_overlay.rs; WP3 redesigns it, audit 2.4).
	renderGeneration    uint64
	lineSlots           LineSlots
	onboardingSignature gk.View
	// layout is the window's size and scale at this view's last render.
	layout LineLayout
	// reducedMotion is the cached system policy, from GPUI/Base's platform
	// motion integration. This is not an RC2FX preference; old persisted
	// values are ignored.
	reducedMotion       bool
	systemMotionChecked time.Time
	engineSeen          *time.Time
	graphBounds         gk.Bounds
	eqCurve             *EqCurve
	draggingBand        bool
	// dragMoved is whether the point being pressed has moved: a press
	// that never moves is a click, which opens the band's popover.
	dragMoved bool
	// bandPopover is the band whose editor is open over the graph, nil for
	// none. It names the channel and band, so it closes by itself when
	// either changes.
	bandPopover *popoverBand
	// live is live editing: edits on the running audio, saving once edits
	// stop, undo and checkpoints (live.go).
	live model.Live
	// pageWidth is the page's content width at the last prepaint (0 before
	// the first); narrow() reads it.
	pageWidth float32
	wasFresh  bool
	// lastHomeKey is lib.rs `home_key` (the field; Go cannot have a field
	// and the homeKey method of one name).
	lastHomeKey *HomeKey
	installer   *Installer
	// focus keeps keyboard focus in the window so Esc returns to the overview.
	focus gk.FocusHandle
	// toasted is the last message considered for a toast (toastMessage).
	toasted string

	// Go only.
	root          gk.View
	window        gk.Window
	confirmDialog gk.Built[gk.DialogEl]
	confirmOpen   *confirmState
	pollTimer     gk.Timer
	// stagePreviews are the flow lane's drag previews (DraggedStage), one
	// view per effect, made once.
	stagePreviews map[core.Effect]gk.View
	// bandPreviews are the band chips' drag previews (DraggedBand), one view
	// per possible band, made by the first rebuildEditors (eq_controls.go).
	bandPreviews []gk.View
}

// newApp is the half of Rc2FxApp::new that needs no window; init is the
// rest.
func newApp() *App {
	backend, message := newBackend()
	log := gk.NewModel(newEventLog())
	a := &App{
		session:             core.DefaultSession(),
		transport:           backend,
		selfUpdate:          newSelfUpdate(),
		developerMode:       rememberedDeveloper(),
		allowDuo:            rememberedDuo(),
		firstHintDone:       rememberedFirstChannelHint(),
		devPage:             DevPageFromEnv(),
		effect:              core.EffectSuppression,
		channel:             core.ChannelLeft,
		telemetryOpen:       rememberedTelemetry(),
		eventLog:            &log,
		message:             message,
		meterProgress:       map[string]*telemetryProgress{},
		systemMotionChecked: time.Now(),
	}
	if v, ok := model.BundledMixerSoftware(); ok {
		a.bundledMixerSoftware = &v
	}
	return a
}

// init is the rest of Rc2FxApp::new: kit state, focus, the first
// connection, the background checks and the 50 ms poll. It runs when the
// window first shows the root view.
func (a *App) init(cx *gk.Cx) {
	// The theme registry needs a window, so the themes load here, before
	// the first frame, rather than in gk.Run's start function.
	gk.ThemeChange(cx, gk.ThemeModeDark)
	loadThemes(cx)
	setAppearance(cx, rememberedAppearance())
	makeHalo(cx)
	a.sshAddress = addressInput(cx)
	a.usbRuleText = gk.NewTextareaState(cx).DefaultValue(installerUSBRule).Build()
	a.usbUndoText = gk.NewInputState(cx).DefaultValue(USBUndo).Build()
	a.presets = newPresetLibrary(cx)
	a.contentScroll = gk.NewScrollHandle(cx)
	a.focus = gk.NewFocusHandle(cx)
	a.reducedMotion = gk.ReduceMotion(cx)
	a.onboardingSignature = newSignature(a)
	a.confirmDialog = gk.NewBuilt(cx, a.confirmView)
	a.stagePreviews = map[core.Effect]gk.View{}
	for _, effect := range core.EffectAll {
		a.stagePreviews[effect] = a.stagePreview(effect.Label())
	}
	gk.Focus(cx, a.focus)
	if a.devPage != nil && a.devPage.Kind == DevPageFlow {
		// A made-up page to look at: no mixer, no helper, no network.
		a.openDevPage(cx)
	} else {
		a.connect(cx)
		// Notification only, in the background; a failure here stays quiet.
		a.checkAppUpdate(cx, false)
		a.startSelfUpdate(cx)
		a.openDevPage(cx)
	}
	// The menu bar's Quit (Cmd+Q) asks the same as the close button.
	// Deferred: a menu or key action arrives while GPUI has the key window
	// checked out, so updating it from inside the handler fails and the quit
	// would be dropped.
	if runtime.GOOS == "darwin" {
		quit := gk.Shortcut(cx, "cmd-q", func(cx *gk.Cx) {
			gk.Post(a.quitFromMain)
		})
		// macOS shows this under the app's name in the menu bar.
		gk.SetMenus(cx, gk.Menu{Name: "RC2FX", Items: []gk.MenuItem{gk.MenuAction{Name: "Quit", Action: quit}}})
	}
	// A timer made in init redraws the root view after each tick; poll opts
	// out (gk.Quiet) and redraws only where the Rust called cx.notify().
	a.pollTimer = gk.Every(cx, 50*time.Millisecond, a.poll)
}

// notify is cx.notify() on the app's view.
func (a *App) notify(cx *gk.Cx) { gk.Notify(cx, a.root) }

// confirmClose is whether the main window may close: no file dialog open,
// and no edits or writes in flight unless the user discards them.
func (a *App) confirmClose(cx *gk.Cx) bool {
	if a.fileBusy {
		a.message = "Finish or cancel the file dialog before closing."
		a.notify(cx)
		return false
	}
	if verdict, ok := a.confirmCloseInstaller(cx); ok {
		return verdict
	}
	if !a.session.AnyDirty() && a.pending == nil && !a.presets.writing() &&
		a.routingPending == nil && !a.transitionBusy() {
		return true
	}
	if a.confirmOpen != nil {
		// "Close without saving?" is already asked, or another question
		// is open: answer that first.
		return false
	}
	detail := "Your local edits have not been saved to the mixer."
	if a.pending != nil || a.presets.writing() || a.routingPending != nil || a.transitionBusy() {
		detail = "A change already sent to the mixer may still finish. Any remaining local edits will be discarded."
	}
	discarded := false
	a.confirm(cx, confirmKindClose, Confirm{
		Title:  "Close without saving your edits?",
		Detail: detail,
		Cancel: "Keep editing",
		Action: "Discard and close",
	}, func(cx *gk.Cx) {
		discarded = true
		// Quit with the window still open: see requestQuit.
		gk.Quit(cx)
	}, func(cx *gk.Cx) {
		// Only when cancelled: "Discard and close" sets discarded before
		// this runs, and its quit must still apply a requested update.
		if !discarded {
			a.selfUpdate.cancelRestart()
		}
		a.notify(cx)
	})
	return false
}

// target is the selected channel's draft.
func (a *App) target() *core.Target {
	return &a.session.Draft[max(0, min(a.selected, len(a.session.Draft)-1))]
}

// targetMut is target_mut; the same pointer as target in Go.
func (a *App) targetMut() *core.Target { return a.target() }

func (a *App) command(command transport.Command) bool {
	if err := a.transport.send(command); err != nil {
		a.message = err.Error()
		return false
	}
	return true
}

// connect connects to the mixer on USB 1. Until connected, or until
// Disconnect, the app keeps trying every few seconds.
func (a *App) connect(cx *gk.Cx) { a.beginConnection(cx, false) }

func (a *App) beginConnection(cx *gk.Cx, backgroundRetry bool) {
	// Only the scheduled retry path may retain a previous observation.
	// An explicit connection starts a new lineage, even with a timer pending.
	a.nav.homeBehind()
	a.generation++
	a.installerConnectionChanged()
	a.diagnosticsPending = false
	a.resetEvents(cx)
	a.session.Disconnect()
	a.deviceVersions = nil
	a.ssh.clear()
	a.usb.Clear()
	a.inputs = ShowInputs{}
	a.pending = nil
	a.resave = false
	a.awaitingRefresh = false
	a.routingPending = nil
	a.restartPending = nil
	a.selectionPending = nil
	a.presets.reset()
	a.meter = nil
	clear(a.meterProgress)
	a.levels.Clear()
	a.startup = telemetryStartupWindow{}
	a.recentFaults = telemetryRecentFaults{}
	a.engineSeen = nil
	a.streamConnected = false
	a.streamError = nil
	sent := a.command(transport.CommandConnect{Generation: a.generation})
	a.link.begin(sent)
	if sent && !backgroundRetry {
		a.message = ""
	}
	a.notify(cx)
}

func (a *App) disconnect(cx *gk.Cx) {
	a.diagnosticsPending = false
	a.resetEvents(cx)
	a.link.stop()
	a.generation++
	a.installerConnectionChanged()
	a.command(transport.CommandDisconnect{Generation: a.generation})
	a.session.Disconnect()
	a.deviceVersions = nil
	a.ssh.clear()
	a.usb.Clear()
	a.inputs = ShowInputs{}
	a.pending = nil
	a.resave = false
	a.awaitingRefresh = false
	a.routingPending = nil
	a.restartPending = nil
	a.selectionPending = nil
	a.presets.reset()
	a.streamConnected = false
	a.streamError = nil
	a.meter = nil
	clear(a.meterProgress)
	a.levels.Clear()
	a.startup = telemetryStartupWindow{}
	a.recentFaults = telemetryRecentFaults{}
	a.engineSeen = nil
	a.message = "Disconnected • mixer processing is independent of this app"
	a.notify(cx)
}

func (a *App) apply(cx *gk.Cx) {
	if a.pending != nil || a.presets.busy() || a.fileBusy || !a.streamConnected || a.transitionBusy() {
		return
	}
	pending, err := a.session.BeginApply()
	if err != nil {
		saveLogf("apply not begun: %s", err)
		a.saveFailed(err.Error())
	} else if a.command(transport.CommandApply{Generation: a.generation, Update: pending.Request}) {
		a.pending = &pending
		saveLogf("apply sent: expected revision %d", pending.Request.ExpectedRevision)
		saveLogTargets("  apply targets", pending.Request.Targets)
		// Not over an outcome the user has not seen yet, nor over a failed
		// save tried again: that stays the message, and is not toasted
		// again at every retry.
		if a.live.SaveError() == "" && (a.message == a.toasted || progressMessage(a.message)) {
			a.message = "Saving settings to mixer…"
		}
	}
	a.notify(cx)
}

// saveFailed notes a save that failed for reason: it is not tried again
// until its backoff passes (Live.SaveFailed), and the reason is shown.
func (a *App) saveFailed(reason string) {
	a.live.SaveFailed(reason, time.Now())
	a.message = saveFailedMessage(reason)
}

// savedGeneration is the saved state's engine generation, ok false without
// one.
func savedGeneration(saved *core.DeviceState) (uint64, bool) {
	if saved == nil {
		return 0, false
	}
	return saved.Engine.Generation, true
}

func (a *App) receive(cx *gk.Cx, value json.RawMessage, acknowledged, snapshot bool) {
	previousDraft := core.CloneTargets(a.session.Draft)
	prevGen, prevOK := savedGeneration(a.session.Saved)
	state, err := core.DeviceStateFromJSON(string(value))
	if err == nil {
		a.logReceive(state, acknowledged, snapshot)
	}
	if err != nil {
		a.message = fmt.Sprintf("Device state rejected: %s. This app requires a matching controller with schema %d and explicit processing-path selection.",
			err, core.SchemaVersion)
		if acknowledged {
			a.pending = nil
			a.session.Conflict = true
			a.awaitingRefresh = a.command(transport.CommandRefresh{Generation: a.generation}) || a.awaitingRefresh
		}
		return
	}
	runtimeObserved := false
	if acknowledged {
		if a.pending != nil {
			pending := *a.pending
			a.pending = nil
			if err := a.session.Acknowledge(pending, state); err != nil {
				saveLogf("  acknowledge failed: %s", err)
				a.message = err.Error()
				a.session.Conflict = true
				a.awaitingRefresh = a.command(transport.CommandRefresh{Generation: a.generation}) || a.awaitingRefresh
			} else {
				a.message = "Saved to mixer • waiting for applied telemetry"
			}
		}
	} else {
		preserveSnapshotRuntime := snapshot && a.streamConnected &&
			a.engineSeen != nil && time.Since(*a.engineSeen) < 30*time.Second
		received, err := model.Receive(&a.session, state, a.pending, a.presets.loadingRevision(),
			snapshot, preserveSnapshotRuntime)
		if err != nil {
			a.message = err.Error()
			return
		}
		saveLogf("  receive -> %s (dirty %v, conflict %v)", received.Update, a.session.Dirty(), a.session.Conflict)
		runtimeObserved = received.RuntimeObserved
		switch received.Update {
		case core.RemoteUpdateStale:
			return
		case core.RemoteUpdateConflict:
			// settleConflict puts the edits on top of the new settings and
			// the next autosave saves them; edits that no longer fit show
			// the conflict card instead.
			a.message = mixerChangedElsewhere
		case core.RemoteUpdateLoaded:
			a.message = "Mixer settings loaded"
		}
	}
	newGen, newOK := savedGeneration(a.session.Saved)
	generationChanged := prevOK != newOK || prevGen != newGen
	if runtimeObserved {
		now := time.Now()
		a.engineSeen = &now
	} else if generationChanged {
		// A settings ACK is not an observation of live audio. A different
		// engine lifetime cannot inherit the prior generation's timestamp.
		a.engineSeen = nil
	}
	if generationChanged || (a.session.Saved != nil && a.session.Saved.Engine.Status != core.EnginePhaseRunning) {
		a.meter = nil
		clear(a.meterProgress)
		a.levels.Clear()
	}
	a.selected = max(0, min(a.selected, len(a.session.Draft)-1))
	if !core.TargetsEqual(a.session.Draft, previousDraft) || len(a.editors) == 0 {
		a.rebuildEditors(cx)
	}
}

// meterTargets is the meter's (path id, output peak) pairs, for
// LevelHistories.Receive.
func meterTargets(meter *core.Meter) func(yield func(string, float64) bool) {
	return func(yield func(string, float64) bool) {
		for _, t := range meter.Targets {
			if !yield(t.ID, t.OutputPeak) {
				return
			}
		}
	}
}

func rangeEqual(a, b *[2]time.Time) bool {
	if a == nil || b == nil {
		return a == b
	}
	return a[0].Equal(b[0]) && a[1].Equal(b[1])
}

func (a *App) poll(cx *gk.Cx) {
	a.releaseRetired(cx)
	gk.Quiet(cx)
	// Base follows Linux portal changes itself. Its native macOS/Windows
	// readers need reapplying to notice settings changed while we're open.
	// On Linux this reuses the existing subscription; it starts no new one.
	if time.Since(a.systemMotionChecked) >= time.Second {
		gk.BaseApplySystemReduceMotion(cx)
		a.systemMotionChecked = time.Now()
	}
	motion := gk.ReduceMotion(cx)
	changed := a.reducedMotion != motion
	a.reducedMotion = motion
	meterArrived := false
	for range 64 {
		event, ok := a.transport.tryRecv()
		if !ok {
			break
		}
		if event.Generation != a.generation {
			continue
		}
		// A reading alone redraws only what shows it (see below).
		if _, isMeter := event.Kind.(transport.EventKindMeter); !isMeter {
			changed = true
		}
		switch k := event.Kind.(type) {
		case transport.EventKindSelection:
			a.selectionReply(k.State, k.Err)
		case transport.EventKindRestartEngine:
			a.restartReply(k.State, k.Err)
		case transport.EventKindInputs:
			a.receiveInputs(cx, k.Inventory, k.Err)
		case transport.EventKindDiagnostics:
			a.saveDiagnostics(cx, k.Report, k.Err)
		case transport.EventKindSettings:
			a.installerSettingsRead(cx, k.Settings, k.Err)
		case transport.EventKindRouting:
			a.routingReply(k.State, k.Err)
		case transport.EventKindVersion:
			// A failed read leaves the versions unknown; it is not an error
			// worth interrupting the user for.
			if k.Err != nil {
				a.deviceVersions = nil
			} else {
				a.deviceVersions = k.Versions
			}
		case transport.EventKindSSH:
			a.receiveSSH(cx, k.ID, k.Status, k.Err)
		case transport.EventKindMixerUpdate:
			a.inPlaceEvent(k.Event)
		case transport.EventKindImageCheck:
			a.installerImageCheck(cx, k.Check, k.Err)
		case transport.EventKindMixerUpdateLast:
			// A failed read is asked again on the next poll.
			if k.Err == nil {
				a.inPlaceOutcome(k.Last)
			}
		case transport.EventKindConnected:
			a.link.connected()
			a.message = ""
			// Edits carry over to the same mixer, not to another one.
			if a.connectedMixer == nil || *a.connectedMixer != k.Mixer {
				var draft []core.Target
				keepDraft := a.session.Dirty()
				if keepDraft {
					draft = a.session.Draft
					a.session.Draft = nil
				}
				var selectionDraft []string
				keepSelection := a.session.SelectionDirty()
				if keepSelection {
					selectionDraft = slices.Clone(a.session.SelectionDraft)
				}
				a.session = core.DefaultSession()
				if keepDraft {
					a.session.Draft = draft
				}
				if keepSelection {
					a.session.SelectionDraft = selectionDraft
				}
			}
			mixer := k.Mixer
			a.connectedMixer = &mixer
			a.receive(cx, k.State, false, false)
			a.openDevPage(cx)
			// Versions change only across a restart: read once per connection.
			a.command(transport.CommandVersion{Generation: a.generation})
			if a.nav.settings() {
				a.listPresets()
			}
		case transport.EventKindPreset:
			a.presetReply(cx, k.Reply, k.Err)
		case transport.EventKindApplied:
			a.live.SaveSucceeded()
			a.receive(cx, k.State, true, false)
		case transport.EventKindState:
			a.receive(cx, k.State, false, false)
		case transport.EventKindRefreshed:
			a.receive(cx, k.State, false, true)
			a.awaitingRefresh = false
		case transport.EventKindEngine:
			var engine core.EngineStatus
			if core.DecodeStrict(k.Engine, &engine) == nil && engine.Validate() == nil {
				reset := a.session.Saved == nil || a.session.Saved.Engine.Generation != engine.Generation ||
					engine.Status != core.EnginePhaseRunning
				if a.session.ReceiveEngine(engine) {
					if reset {
						a.meter = nil
						clear(a.meterProgress)
						a.levels.Clear()
					}
					now := time.Now()
					a.engineSeen = &now
				}
			} else {
				a.message = "Invalid engine status received"
				a.engineSeen = nil
				a.meter = nil
			}
		case transport.EventKindMeter:
			var meter core.Meter
			if core.DecodeStrict(k.Meter, &meter) == nil && a.session.Saved != nil &&
				telemetryReceiveMeter(a.session.Saved, &meter, a.meterProgress, &a.engineSeen, time.Now()) {
				a.levels.Receive(time.Now(), meterTargets(&meter))
				var rate uint32
				if a.session.Saved != nil {
					rate = a.session.Saved.Engine.SampleRate
				}
				a.startup.Observe(&meter, rate, time.Now())
				a.recentFaults.Observe(&meter, &a.startup, time.Now(), time.Now())
				// The events window leaves startup out unless asked.
				startup := a.startup.Range()
				if !rangeEqual(a.eventLog.Peek().Startup, startup) {
					a.eventLog.Update(cx, func(log *EventLog) { log.Startup = startup })
				}
				if a.startup.Open() {
					for _, p := range a.meterProgress {
						p.ForgetStartupDeadline()
					}
				}
				meterArrived = true
				a.meter = &meter
			}
		case transport.EventKindUSB:
			a.usb.Receive(k.USB, time.Now())
		case transport.EventKindResetDiagnostics:
			a.resetReply(cx, k.Report, k.Err)
		case transport.EventKindDiagnosticsEvents:
			a.eventsReply(cx, k.Events, k.Err)
		case transport.EventKindStreamStatus:
			a.streamConnected = k.Connected
			if k.Connected {
				a.streamError = nil
				// The mixer may have been flashed or restarted meanwhile.
				a.command(transport.CommandVersion{Generation: a.generation})
				a.ssh.clear()
				a.readSSH()
			} else {
				message := k.Message
				a.streamError = &message
				// Never show a pre-restart observation as current.
				a.deviceVersions = nil
				a.ssh.clear()
				// A refresh asked for before the loss won't arrive.
				a.awaitingRefresh = false
				a.usb.Clear()
				// Durable device generations remain ordered across
				// reconnect. Preserve the observed high-water mark; only
				// the new stream can reconcile uncertain writes.
				a.routingPending = nil
				a.restartPending = nil
				a.selectionPending = nil
				a.meter = nil
				clear(a.meterProgress)
				a.levels.Clear()
				a.engineSeen = nil
			}
		// Only this app writes the mixer's settings, so a refused save
		// means our revision was behind our own last change: read the
		// current settings, keep the edits, save again.
		case transport.EventKindLiveApplied:
			a.liveReply(cx, k)
		case transport.EventKindConflict:
			saveLogf("save refused as a conflict; refreshing")
			a.pending = nil
			a.session.Conflict = true
			a.resave = true
			a.awaitingRefresh = a.command(transport.CommandRefresh{Generation: a.generation})
		case transport.EventKindFailure:
			saving := a.pending != nil
			a.pending = nil
			if saving {
				a.saveFailed(k.Message)
			} else {
				a.message = k.Message
			}
			a.link.failed(a.session.Connected, time.Now())
		}
	}
	if a.session.Conflict && !a.awaitingRefresh {
		changed = true
		a.settleConflict(cx)
	}
	a.syncLive(cx)
	a.noteFirstChannelHint()
	a.autoFlashPoll(cx)
	fresh, _ := a.freshMeter()
	changed = a.usb.Expire(time.Now()) || changed
	changed = a.pollInstaller(cx) || changed
	changed = a.pollRepair() || changed
	a.pollFirstLook(cx)
	a.autostartInstaller(cx)
	// lib.rs called this at the top of every render; a gk view function may
	// not write kit state, so it runs here, on the same 50 ms tick.
	a.syncInstallerSelects(cx)
	if a.session.Saved != nil && a.session.Saved.Selection.Status == core.SelectionPhaseIdle {
		a.selectionAsked = false
	}
	// While the mixer is being flashed its controls and their errors mean
	// nothing: hand the window to the installer. Once flashed, look for the
	// mixer again; reconnecting is how the installer sees RC2FX back.
	if a.installerFlashing() {
		if a.session.Connected || a.link.auto() {
			a.disconnect(cx)
			a.message = "The mixer is updating."
			a.link.pauseForFlash()
		}
	} else if a.link.flashDone(time.Now()) {
		// The pre-flash connection result belongs to an earlier device
		// state. Start a new observation after the expected USB transition.
		a.message = ""
	}
	a.refreshInputs()
	a.eventsTick(cx)
	// Freshness and warnings expire with time, not events: when meters stop
	// while the connection stays up, nothing else would redraw Home.
	// Compared on this existing 50 ms tick; no extra frames are scheduled,
	// and a redraw happens only when what Home shows changes.
	var homeKey *HomeKey
	if a.session.Connected && a.nav.home() {
		k := a.homeKey()
		homeKey = &k
	}
	// A reading redraws the app unless the overview is in front and the
	// overlay is already drawing its bars every frame. Each such redraw is a
	// whole extra frame, on top of the overlay's.
	overview := homeKey != nil && a.nav.basePageInFront() && a.installer == nil
	isFresh := fresh != nil
	if changed || isFresh != a.wasFresh || !homeKeyEqual(homeKey, a.lastHomeKey) ||
		(meterArrived && !(overview && a.barsAnimate(cx))) {
		a.notify(cx)
	}
	a.toastMessage(cx)
	a.wasFresh = isFresh
	a.lastHomeKey = homeKey
	// The live indicators' views and ticker, and the signature's
	// (line_overlay.go: in Go each indicator is its own view, audit 2.4).
	a.pollIndicators(cx)
	if a.link.due(a.session.Connected, time.Now()) {
		a.beginConnection(cx, true)
	}
}

// freshMeter is the selected path's fresh reading and whether it is
// synthetic; nil when there is none.
func (a *App) freshMeter() (*core.TargetMeter, bool) { return a.freshMeterFor(a.target().ID) }

func (a *App) freshMeterFor(targetID string) (*core.TargetMeter, bool) {
	if !a.session.Connected || !a.streamConnected || a.session.Saved == nil || a.meter == nil {
		return nil, false
	}
	return telemetryFreshMeter(a.session.Saved, a.meter, a.meterProgress, targetID, a.engineSeen, time.Now())
}

// reportedLookahead is what the mixer reports for the selected path: the
// latency it reports there, and the lookahead it runs there. Both nil while
// offline or before anything is applied, so the editor states relative costs
// only.
func (a *App) reportedLookahead() Reported {
	id := a.target().ID
	var reported Reported
	if a.session.Saved != nil {
		for _, t := range a.session.Saved.Config.Targets {
			if t.ID == id {
				lookahead := t.Suppression.Lookahead
				reported.liveLookahead = &lookahead
				break
			}
		}
	}
	if meter, _ := a.freshMeterFor(id); meter != nil && meter.LatencySamples > 0 {
		samples := meter.LatencySamples
		reported.latencySamples = &samples
	}
	return reported
}

// systemStatus is the connection, path-change, restart and engine states
// that outrank everything else, shared by the status line and Home.
func (a *App) systemStatus() (string, uint32, bool) {
	switch {
	case a.link.connecting():
		return "Connecting", Warn, true
	case !a.session.Connected:
		return "Offline draft", Muted, true
	case !a.streamConnected:
		return "Connection interrupted", Warn, true
	case a.selectionBusy():
		return "Changing processing paths", Warn, true
	case a.restartBusy():
		return a.processingTransitionLabel(), Warn, true
	case a.selectionFailed():
		return "Processing path change failed", Warn, true
	}
	if a.session.Saved != nil {
		if _, ok := telemetryEngineError(a.session.Saved); ok {
			return telemetryEngineErrorNoticeTitle, Warn, true
		}
	}
	return "", 0, false
}

// routingStatus is audio routing that isn't active, where the engine has
// processing controls. Shared by the status line and Home.
func (a *App) routingStatus() (string, uint32, bool) {
	state := a.session.Saved
	if state == nil || !state.Engine.Mode.HasProcessingControls() {
		return "", 0, false
	}
	switch state.Routing.Status {
	case core.RoutingPhaseUnmanaged:
		return "Processing not routed", Muted, true
	case core.RoutingPhaseStarting:
		return "Connecting audio", Warn, true
	case core.RoutingPhaseBypassing:
		return "Restoring stock audio", Warn, true
	case core.RoutingPhaseBypassed:
		return "RC2FX audio off · settings saved", Warn, true
	case core.RoutingPhaseError:
		return "Audio routing needs attention", Warn, true
	}
	return "", 0, false
}

func (a *App) status() (string, uint32) {
	if label, color, ok := a.systemStatus(); ok {
		return label, color
	}
	saved := a.session.Saved
	switch {
	case a.session.Conflict:
		return "Reload needed", Warn
	// Edits save themselves (live.go), so saving is routine, not a warning;
	// the rail shows it.
	case a.pending != nil || a.presets.writing():
		return "Saving", Muted
	case a.session.AnyDirty():
		return "Saving", Muted
	case saved != nil && saved.StockSelected():
		return "Stock audio · no processing paths", Muted
	case saved != nil && saved.CanEnableProcessing():
		return "Processing disabled · settings saved", Muted
	case a.unprocessedSource() != nil:
		return "Selected input has no RC2FX processing", Muted
	case saved != nil && !slices.Contains(saved.Selection.ActiveTargetIDs, a.target().ID):
		return "Selected path is inactive · effects saved", Muted
	}
	if label, color, ok := a.routingStatus(); ok {
		return label, color
	}
	if warning, ok := a.audioWarning(); ok {
		return warning.Label(), Warn
	}
	if a.target().Bypass {
		return "Selected channel effects bypassed", Muted
	}
	if fresh, _ := a.freshMeter(); a.session.IsApplied() && fresh != nil {
		if a.meter != nil && a.meter.Synthetic {
			return "Applied · simulation", Accent
		}
		return "Applied · DSP advancing", Accent
	}
	return "Saved · audio unconfirmed", Muted
}

func (a *App) audioWarning() (telemetryWarning, bool) { return a.audioWarningFor(a.target().ID) }

func (a *App) audioWarningFor(targetID string) (telemetryWarning, bool) {
	meter, synthetic := a.freshMeterFor(targetID)
	state := a.session.Saved
	if meter == nil || state == nil || synthetic || state.Routing.Status != core.RoutingPhaseActive {
		return 0, false
	}
	suppressionActive := state.Config.Revision == state.Engine.AppliedRevision &&
		slices.Contains(state.Selection.ActiveTargetIDs, meter.ID) &&
		slices.ContainsFunc(state.Config.Targets, func(t core.Target) bool {
			return t.ID == meter.ID && !t.Bypass && t.Suppression.Enabled && t.Suppression.Wet > 0
		})
	progress := a.meterProgress[meter.ID]
	if progress == nil {
		return 0, false
	}
	return progress.WarningFor(time.Now(), suppressionActive)
}

// settleConflict puts local edits back on top of the mixer's current
// settings when the revision moved under them. Only this app writes the
// mixer's settings (over USB 1), so that move is always one of our own
// changes and never a choice for the user. A refused save is sent again
// once.
func (a *App) settleConflict(cx *gk.Cx) {
	if a.pending != nil || a.presets.writing() {
		return
	}
	if err := a.session.RebaseEdits(); err != nil {
		// The edits no longer fit the mixer's channels: the notice below
		// offers the one way on.
		saveLogf("rebase failed: %s", err)
		a.resave = false
		a.message = err.Error()
		return
	}
	saveLogf("rebased edits (resave %v)", a.resave)
	if a.resave {
		a.resave = false
		a.apply(cx)
	}
}

// toastMessage shows a new outcome in message (an error, a saved file, a
// loaded preset) as a toast. The footer no longer carries it; progress steps
// ("Saving…", "… waiting for …") aren't worth interrupting for. Before a
// connection the connection screen shows message itself.
func (a *App) toastMessage(cx *gk.Cx) {
	if a.message == a.toasted {
		return
	}
	a.toasted = a.message
	if a.session.Connected && a.message != "" && !progressMessage(a.message) {
		gk.PushNotification(cx, a.message)
	}
}

// openOverview is the overview of every channel, from the sidebar or Esc.
func (a *App) openOverview(cx *gk.Cx) {
	a.nav.showHome()
	a.notify(cx)
}

// quitFromMain is lib.rs quit_from: requestQuit on the main window; quits
// outright if it is already gone.
func (a *App) quitFromMain(cx *gk.Cx) {
	if slices.Contains(gk.Windows(cx), a.window) {
		a.requestQuit(cx)
		return
	}
	gk.Quit(cx)
}

// requestQuit quits once the main window's close checks pass (busy file
// dialog, unsaved edits); when they ask first, their prompt quits on its own.
//
// It quits through gk.Quit and leaves the window to the shutdown instead of
// closing it here. Removing it first released the NSWindow a run-loop turn
// before `terminate:` ran, and AppKit's Touch Bar bookkeeping then threw on
// the released window during the display flush in between ("RC2FX quit
// unexpectedly").
func (a *App) requestQuit(cx *gk.Cx) {
	if a.confirmClose(cx) {
		gk.Quit(cx)
	}
}

// windowTitle is the main window's title.
func windowTitle() string {
	if mockMixer {
		return "RC2FX (mock mixer)"
	}
	return "RC2FX"
}

// Run is the native application's entry point (lib.rs run): the helper modes
// the installer starts this executable in, then the window.
func Run() {
	args := os.Args
	if len(args) > 1 && args[1] == installerUSBRuleFlag {
		// Started through pkexec by the USB permission screen: write the
		// embedded rule, reload udev, exit. No window, no other input.
		if err := installerInstallUSBRule(); err != nil {
			fmt.Fprintf(os.Stderr, "rc2fx: %s\n", err)
			os.Exit(1)
		}
		return
	}
	if len(args) > 1 && args[1] == installerHelperFlag {
		// The privileged installer process: no window, JSON lines on stdio.
		if len(args) < 4 {
			os.Exit(2)
		}
		if err := installerServe(args[2], args[3]); err != nil {
			fmt.Fprintf(os.Stderr, "rc2fx installer helper: %s\n", err)
			os.Exit(1)
		}
		return
	}
	if previewMode(args) {
		return
	}
	a := newApp()
	gk.Run(func(cx *gk.Cx) {
		gk.QuitOnLastWindow(cx, true)
		a.root = gk.NewView(a.init, a.view)
		// Home's animated indicators wrapped the app in a shell in the Rust
		// (line_overlay.rs, RC2FX_ANIMATED_LINE); in Go each indicator is its
		// own view (audit 2.4, WP3), so the window holds the app directly.
		a.window = gk.OpenWindowWith(cx, windowIdentity(gk.WindowConfig{
			Width:     Window[0],
			Height:    Window[1],
			MinWidth:  WindowMin[0],
			MinHeight: WindowMin[1],
			Title:     windowTitle(),
		}), a.root)
		// A confirmed "Restart to update" hands over to the updater as RC2FX
		// quits, whichever prompt ends up quitting.
		gk.OnAppQuit(cx, func(*gk.Cx) { a.selfUpdate.launchOnQuit() })
		// The close button (and Alt-F4, Cmd-W) asks as Quit does; the quit
		// itself comes from requestQuit or the prompt it opens.
		gk.OnWindowShouldClose(cx, a.window, func(cx *gk.Cx) bool {
			a.requestQuit(cx)
			return false
		})
		gk.ActivateWindow(cx, a.window)
	})
}
