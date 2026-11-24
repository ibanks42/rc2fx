// Package app implements the RC2FX desktop UI on github.com/ibanks42/goui-kit/gk.
// Non-UI logic lives in cgo-free subpackages (model, installflow, state, theme, ...).
package app

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime"
	"slices"
	"strings"
	"time"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/editor"
	"github.com/ibanks42/goui-kit/gk/overlay"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
	"github.com/ibanks42/rc2fx/desktop/transport"
	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
)

// Backend wraps the USB transport to the mixer, or records why it is unavailable.
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

// HomeKey snapshots the time-dependent Home state (headline, detail, and meter
// freshness) so the poll loop can detect when meters go stale.
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

// usbRuleUI groups the Linux USB permission screen's transient UI state.
type usbRuleUI struct {
	usbRule     USBRule
	usbRuleOpen bool
	copied      string
	usbRuleText editor.TextareaState
	usbUndoText editor.InputState
}

// firstHintUI groups the post-install first-channel onboarding hint state.
type firstHintUI struct {
	firstHintTarget string
	firstHintDone   bool
	firstHintSaved  bool
}

// lookUI groups the first-screen periodic USB sighting state.
type lookUI struct {
	look     *firstLook
	looking  bool
	lookedAt time.Time
}

// telemetryUI groups Settings' Diagnostics / Telemetry / Events window state.
type telemetryUI struct {
	telemetryOpen      bool
	telemetryDetails   bool
	confirmReset       bool
	eventLog           *gk.Model[EventLog]
	eventsWindow       *gk.Window
	eventsPolled       *time.Time
	eventsPending      bool
	resetPending       bool
	diagnosticsPending bool
}

// eqGraphUI groups interactive EQ graph drag & popover state.
type eqGraphUI struct {
	graphBounds  gk.Bounds
	eqCurve      *EqCurve
	draggingBand bool
	dragMoved    bool
	bandPopover  *popoverBand
}

// confirmUI groups the modal confirm dialog and drag-preview caches.
type confirmUI struct {
	confirmDialog gk.Built[overlay.DialogEl]
	confirmOpen   *confirmState
	stagePreviews map[core.Effect]gk.View
	bandPreviews  []gk.View
}

// App holds the desktop UI state: session, navigation, live editing, installer
// coordination, and window handles.
type App struct {
	// resumedSession is true when this process received a USB session from the
	// app it replaced during an update restart.
	resumedSession bool
	// restartWhenReady records that the user started an app update, so RC2FX
	// restarts into it once verified and unblocked (see autoRestart).
	restartWhenReady bool
	// quietUntil holds back the connection screen while the first connect
	// runs, so a launch doesn't flash "Connecting" (see launchQuiet).
	launched, quietUntil time.Time
	wasQuiet             bool
	session              core.Session
	transport            Backend
	generation           uint64
	connectedMixer       *string
	deviceVersions       *core.DeviceVersions
	appUpdate            AppUpdate
	selfUpdate           selfupdate.SelfUpdate
	developerMode        bool
	devPage              *DevPage
	ssh                  SSH
	sshAddress           editor.InputState
	bundledMixerSoftware *string

	// Channel editor selection and import state.
	selected        int
	inputs          ShowInputs
	effect          core.Effect
	band            int
	channel         core.Channel
	editorEpoch     uint64
	importDraft     *ImportDraft
	importError     *string
	fileBusy        bool
	editors         []ParameterEditor
	editingValue    *Parameter
	resave          bool
	awaitingRefresh bool

	// Navigation and scrolling.
	nav           navigation
	contentScroll gk.ScrollHandle

	// Pending mixer mutations and installer coordination.
	subscriptions      []gk.Subscription
	retired, retiring  []any
	pending            *core.PendingApply
	routingPending     *core.PendingRouting
	selectionPending   *core.PendingSelection
	selectionAsked     bool
	installerAutostart *time.Time
	settledFlash       *uint64
	allowDuo           bool
	flowFixture        bool
	presets            PresetLibrary
	installer          *Installer

	// Connection lifecycle, metering, and live editing.
	link                linkIntent
	streamConnected     bool
	streamError         *string
	usb                 model.USBObservation
	message             string
	meter               *core.Meter
	meterProgress       map[string]*telemetry.Progress
	levels              model.LevelHistories
	startup             telemetry.StartupWindow
	recentFaults        telemetry.RecentFaults
	renderGeneration    uint64
	lineSlots           LineSlots
	reducedMotion       bool
	systemMotionChecked time.Time
	engineSeen          *time.Time
	live                model.Live
	pageWidth           float32
	wasFresh            bool
	lastHomeKey         *HomeKey
	focus               gk.FocusHandle
	toasted             string

	// Window handles and domain-grouped UI sub-states.
	root   gk.View
	window gk.Window
	usbRuleUI
	firstHintUI
	lookUI
	telemetryUI
	eqGraphUI
	confirmUI
}

// newApp initializes the window-independent portion of App; init finishes setup
// once the root view mounts.
func newApp() *App {
	backend, message := newBackend()
	log := gk.NewModel(newEventLog())
	a := &App{
		session:             core.DefaultSession(),
		transport:           backend,
		selfUpdate:          newSelfUpdate(),
		developerMode:       rememberedDeveloper(),
		allowDuo:            rememberedDuo(),
		devPage:             DevPageFromEnv(),
		effect:              core.EffectSuppression,
		channel:             core.ChannelLeft,
		message:             message,
		meterProgress:       map[string]*telemetry.Progress{},
		systemMotionChecked: time.Now(),
		firstHintUI:         firstHintUI{firstHintDone: rememberedFirstChannelHint()},
		telemetryUI:         telemetryUI{telemetryOpen: rememberedTelemetry(), eventLog: &log},
	}
	if v, ok := model.BundledMixerSoftware(); ok {
		a.bundledMixerSoftware = &v
	}
	return a
}

// init sets up kit handles, focus, initial connection, update checks, and the
// 50 ms poll loop when the window first mounts the root view.
func (a *App) init(cx *gk.Cx) {
	// The theme registry requires an open window, so load themes here rather
	// than in gk.Run's startup callback.
	gk.ThemeChange(cx, gk.ThemeModeDark)
	loadThemes(cx)
	setAppearance(cx, rememberedAppearance())
	makeHalo(cx)
	a.sshAddress = addressInput(cx)
	a.usbRuleText = editor.NewTextareaState(cx).DefaultValue(helper.USBRule).Build()
	a.usbUndoText = editor.NewInputState(cx).DefaultValue(USBUndo).Build()
	a.presets = newPresetLibrary(cx)
	a.contentScroll = gk.NewScrollHandle(cx)
	a.focus = gk.NewFocusHandle(cx)
	a.reducedMotion = gk.ReduceMotion(cx)
	a.initAmbient()
	a.confirmDialog = gk.NewBuilt(cx, a.confirmView)
	a.stagePreviews = map[core.Effect]gk.View{}
	for _, effect := range core.EffectAll {
		a.stagePreviews[effect] = a.stagePreview(effect.Label())
	}
	gk.Focus(cx, a.focus)
	if a.devPage != nil && a.devPage.Kind == DevPageFlow {
		// Fixture page: skip mixer, helper, and network startup.
		a.openDevPage(cx)
	} else {
		a.launched = time.Now()
		a.quietUntil = a.launched.Add(launchQuiet)
		if a.resumedSession {
			a.quietUntil = a.launched.Add(usbLockout)
		}
		a.connect(cx)
		a.checkAppUpdate(cx, false)
		a.startSelfUpdate(cx)
		a.openDevPage(cx)
	}
	// Defer Cmd+Q via gk.Post: menu actions fire while GPUI has the key window
	// checked out, so updating it inline would fail and drop the quit.
	if runtime.GOOS == "darwin" {
		quit := gk.Shortcut(cx, "cmd-q", func(cx *gk.Cx) {
			gk.Post(a.quitFromMain)
		})
		gk.SetMenus(cx, gk.Menu{Name: "RC2FX", Items: []gk.MenuItem{gk.MenuAction{Name: "Quit", Action: quit}}})
	}
	// Timers created in init trigger a root redraw by default; poll calls
	// gk.Quiet and explicitly notifies only when state changes.
	gk.Every(cx, 50*time.Millisecond, a.poll)
}

func (a *App) notify(cx *gk.Cx) { gk.Notify(cx, a.root) }

// confirmClose checks whether the main window can close immediately, or opens
// a confirmation dialog if there are unsaved edits or writes in flight.
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
		// Quit with the window still open; see requestQuit.
		gk.Quit(cx)
	}, func(cx *gk.Cx) {
		// Only cancel a pending update restart if the user chose "Keep editing".
		if !discarded {
			a.selfUpdate.CancelRestart()
		}
		a.notify(cx)
	})
	return false
}

func (a *App) target() *core.Target {
	return &a.session.Draft[max(0, min(a.selected, len(a.session.Draft)-1))]
}

func (a *App) command(command transport.Command) bool {
	if err := a.transport.send(command); err != nil {
		a.message = err.Error()
		return false
	}
	return true
}

func (a *App) connect(cx *gk.Cx) { a.beginConnection(cx, false) }

func (a *App) beginConnection(cx *gk.Cx, backgroundRetry bool) {
	// Only the scheduled retry path may retain a previous observation.
	// An explicit connection starts a new lineage, even with a timer pending.
	a.nav.HomeBehind()
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
	a.selectionPending = nil
	a.presets.reset()
	a.meter = nil
	clear(a.meterProgress)
	a.levels.Clear()
	a.startup = telemetry.StartupWindow{}
	a.recentFaults = telemetry.RecentFaults{}
	a.engineSeen = nil
	a.streamConnected = false
	a.streamError = nil
	sent := a.command(transport.CommandConnect{Generation: a.generation})
	a.link.Begin(sent)
	if sent && !backgroundRetry {
		a.message = ""
	}
	a.notify(cx)
}

func (a *App) disconnect(cx *gk.Cx) {
	a.diagnosticsPending = false
	a.resetEvents(cx)
	a.link.Stop()
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
	a.selectionPending = nil
	a.presets.reset()
	a.streamConnected = false
	a.streamError = nil
	a.meter = nil
	clear(a.meterProgress)
	a.levels.Clear()
	a.startup = telemetry.StartupWindow{}
	a.recentFaults = telemetry.RecentFaults{}
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
		// Avoid overwriting an unseen outcome message or a retrying save error.
		if a.live.SaveError() == "" && (a.message == a.toasted || progressMessage(a.message)) {
			a.message = "Saving settings to mixer…"
		}
	}
	a.notify(cx)
}

func (a *App) saveFailed(reason string) {
	a.live.SaveFailed(reason, time.Now())
	a.message = saveFailedMessage(reason)
}

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
			// settleConflict rebases edits onto the new state (or shows the
			// conflict card if they no longer fit).
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
		// Meter updates only redraw the views that display them (handled below).
		if _, isMeter := event.Kind.(transport.EventKindMeter); !isMeter {
			changed = true
		}
		switch k := event.Kind.(type) {
		case transport.EventKindSelection:
			a.selectionReply(k.State, k.Err)
		case transport.EventKindInputs:
			a.receiveInputs(cx, k.Inventory, k.Err)
		case transport.EventKindDiagnostics:
			a.saveDiagnostics(cx, k.Report, k.Err)
		case transport.EventKindSettings:
			a.installerSettingsRead(cx, k.Settings, k.Err)
		case transport.EventKindRouting:
			a.routingReply(k.State, k.Err)
		case transport.EventKindVersion:
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
			if k.Err == nil {
				a.inPlaceOutcome(k.Last)
			}
		case transport.EventKindConnected:
			a.link.Connected()
			a.message = ""
			// Only preserve unsaved drafts when reconnecting to the same mixer.
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
			// Versions only change across a mixer restart, so read once per connection.
			a.command(transport.CommandVersion{Generation: a.generation})
			if a.nav.Settings() {
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
		case transport.EventKindMeter:
			var meter core.Meter
			if core.DecodeStrict(k.Meter, &meter) == nil && a.session.Saved != nil &&
				telemetry.ReceiveMeter(a.session.Saved, &meter, a.meterProgress, &a.engineSeen, time.Now()) {
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
				a.selectionPending = nil
				a.meter = nil
				clear(a.meterProgress)
				a.levels.Clear()
				a.engineSeen = nil
			}
		case transport.EventKindLiveApplied:
			a.liveReply(cx, k)
		case transport.EventKindConflict:
			// This app is the sole writer over USB 1, so a revision conflict means
			// we lagged behind our own write; refresh, rebase, and resave.
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
			now := time.Now()
			a.link.Failed(a.session.Connected, now)
			a.launchFailure(k.Message, now)
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
	// Sync installer selects here because gk view functions cannot mutate kit state.
	a.syncInstallerSelects(cx)
	if a.session.Saved != nil && a.session.Saved.Selection.Status == core.SelectionPhaseIdle {
		a.selectionAsked = false
	}
	// Disconnect while flashing so mixer errors don't interfere; resume
	// auto-reconnect once flashing finishes so the installer detects RC2FX returning.
	if a.installerFlashing() {
		if a.session.Connected || a.link.Auto() {
			a.disconnect(cx)
			a.message = "The mixer is updating."
			a.link.PauseForFlash()
		}
	} else if a.link.FlashDone(time.Now()) {
		// Clear pre-flash status before observing the post-flash USB transition.
		a.message = ""
	}
	a.refreshInputs()
	a.eventsTick(cx)
	// Poll Home's time-dependent state so stale meters trigger a redraw even
	// when no new transport events arrive.
	var homeKey *HomeKey
	if a.session.Connected && a.nav.Home() {
		k := a.homeKey()
		homeKey = &k
	}
	// Skip full-app redraws for meter packets when Home is visible and all
	// overview bars are already updating in their own views.
	overview := homeKey != nil && a.nav.BasePageInFront() && a.installer == nil
	isFresh := fresh != nil
	if changed || isFresh != a.wasFresh || !homeKeyEqual(homeKey, a.lastHomeKey) ||
		(meterArrived && !(overview && a.barsAnimate())) {
		a.notify(cx)
	}
	a.toastMessage(cx)
	a.wasFresh = isFresh
	a.lastHomeKey = homeKey
	a.pollIndicators(cx)
	a.autoRestart(cx)
	if quiet := a.launchQuiet(); quiet != a.wasQuiet {
		a.wasQuiet = quiet
		a.notify(cx)
	}
	if a.link.Due(a.session.Connected, time.Now()) {
		a.beginConnection(cx, true)
	}
}

func (a *App) freshMeter() (*core.TargetMeter, bool) { return a.freshMeterFor(a.target().ID) }

func (a *App) freshMeterFor(targetID string) (*core.TargetMeter, bool) {
	if !a.session.Connected || !a.streamConnected || a.session.Saved == nil || a.meter == nil {
		return nil, false
	}
	return telemetry.FreshMeter(a.session.Saved, a.meter, a.meterProgress, targetID, a.engineSeen, time.Now())
}

// reportedLookahead returns the mixer's reported latency and live lookahead
// for the selected path (nil fields while offline or before apply).
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

// systemStatus returns high-priority connection, transition, or engine states
// shared by the status bar and Home headline.
func (a *App) systemStatus() (string, uint32, bool) {
	switch {
	case a.link.Connecting():
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
		if _, ok := telemetry.EngineError(a.session.Saved); ok {
			return telemetry.EngineErrorNoticeTitle, Warn, true
		}
	}
	return "", 0, false
}

// routingStatus returns non-active audio routing states for engines with
// processing controls, shared by the status bar and Home.
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
	// Autosave is routine, so keep it muted rather than warning.
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

func (a *App) audioWarning() (telemetry.Warning, bool) { return a.audioWarningFor(a.target().ID) }

func (a *App) audioWarningFor(targetID string) (telemetry.Warning, bool) {
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

// settleConflict rebases local edits onto the latest mixer state after a
// revision conflict and retries the refused save once.
func (a *App) settleConflict(cx *gk.Cx) {
	if a.pending != nil || a.presets.writing() {
		return
	}
	if err := a.session.RebaseEdits(); err != nil {
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

// toastMessage posts non-progress status messages as notifications once connected.
func (a *App) toastMessage(cx *gk.Cx) {
	if a.message == a.toasted {
		return
	}
	a.toasted = a.message
	if a.session.Connected && a.message != "" && !progressMessage(a.message) {
		overlay.PushNotification(cx, a.message)
	}
}

func (a *App) openOverview(cx *gk.Cx) {
	a.nav.ShowHome()
	a.notify(cx)
}

func (a *App) quitFromMain(cx *gk.Cx) {
	if slices.Contains(gk.Windows(cx), a.window) {
		a.requestQuit(cx)
		return
	}
	gk.Quit(cx)
}

// requestQuit runs close checks and quits via gk.Quit, leaving window teardown
// to shutdown. Closing the window first releases the NSWindow one run-loop turn
// before terminate:, causing AppKit Touch Bar bookkeeping to crash during the
// intermediate display flush.
func (a *App) requestQuit(cx *gk.Cx) {
	if a.confirmClose(cx) {
		gk.Quit(cx)
	}
}

func windowTitle() string {
	if mockMixer {
		return "RC2FX (mock mixer)"
	}
	return "RC2FX"
}

// Run is the native GUI entry point. Headless privileged helper modes
// (--install-usb-rule, --installer-helper) are dispatched in cmd/rc2fx first.
func Run() {
	if previewMode(os.Args) {
		return
	}
	resumed := resumeUSBSession(os.Args[1:])
	a := newApp()
	a.resumedSession = resumed
	gk.Run(func(cx *gk.Cx) {
		gk.QuitOnLastWindow(cx, true)
		a.root = gk.NewView(a.init, a.view)
		a.window = gk.OpenWindowWith(cx, windowIdentity(gk.WindowConfig{
			Width:     Window[0],
			Height:    Window[1],
			MinWidth:  WindowMin[0],
			MinHeight: WindowMin[1],
			Title:     windowTitle(),
		}), a.root)
		// Hand off any pending update restart to the updater on exit, passing
		// this process's USB session so the restarted app connects at once.
		gk.OnAppQuit(cx, func(*gk.Cx) {
			var args []string
			if token, ok := usbcontrol.HandoffToken(); ok {
				args = []string{usbSessionFlag + token}
			}
			a.selfUpdate.LaunchOnQuit(args)
		})
		// Intercept window close (close button, Alt+F4, Cmd+W) so close checks run.
		gk.OnWindowShouldClose(cx, a.window, func(cx *gk.Cx) bool {
			a.requestQuit(cx)
			return false
		})
		gk.ActivateWindow(cx, a.window)
	})
}

// usbSessionFlag carries the USB session from an app that restarted to update.
const usbSessionFlag = "--usb-session="

func resumeUSBSession(args []string) bool {
	for _, arg := range args {
		if token, ok := strings.CutPrefix(arg, usbSessionFlag); ok {
			return usbcontrol.ResumeFrom(token)
		}
	}
	return false
}

const (
	// launchQuiet hides the connection screen while the initial connect runs;
	// a normal connect finishes in a fraction of it.
	launchQuiet = 1500 * time.Millisecond
	// usbLockout covers the mixer's 5 s USB session lockout plus retry margin.
	usbLockout = 6 * time.Second
	// lockoutRetry is how often launch retries while the mixer reports USB is in use.
	lockoutRetry = 400 * time.Millisecond
)

// launchQuiet reports whether the connection screen is still held back.
func (a *App) launchQuiet() bool {
	return a.link.Auto() && !a.session.Connected && time.Now().Before(a.quietUntil)
}

// launchFailure keeps the connection screen hidden through the mixer's USB
// lockout and schedules a quick retry, or reveals it immediately on any other
// failure.
func (a *App) launchFailure(message string, now time.Time) {
	if !now.Before(a.quietUntil) {
		return
	}
	if strings.HasPrefix(message, "USB is in use") && now.Before(a.launched.Add(usbLockout)) {
		a.quietUntil = a.launched.Add(usbLockout)
		a.link.RetrySoon(now, lockoutRetry)
		return
	}
	a.quietUntil = time.Time{}
}
