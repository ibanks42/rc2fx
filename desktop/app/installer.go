package app

// installer/mod.rs: install or update RC2FX on a RØDECaster Pro II.
//
// Requests run one at a time on a goroutine that owns the installer engine.
// On Linux without root, switching to USB update mode and flashing run in a
// helper started through pkexec, because raw USB, mounting and ejection need
// it. It starts only when the person presses the button that starts the run
// (or tries it again), which then asks for the password.
//
// The plan itself is app/installflow (installer/flow.rs); the page's own
// decisions (access, what the helper runs and holds) are in
// installflow/page.go, and the signature's maths in installflow/signature.go.

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

// installerOutcome is a finished request and its result (mod.rs Outcome).
type installerOutcome struct {
	request installer.Request
	reply   installer.Reply
	err     error
}

// helperSlot is the privileged helper, once access is granted, shared by
// the access goroutine (which fills it) and the request goroutine.
type helperSlot struct {
	mu     sync.Mutex
	helper *helper.Helper
}

// cancellerSlot stops a download in the helper, from any goroutine.
type cancellerSlot struct {
	mu        sync.Mutex
	canceller *helper.Canceller
}

func (c *cancellerSlot) set(canceller *helper.Canceller) {
	c.mu.Lock()
	c.canceller = canceller
	c.mu.Unlock()
}

// helperLink is the request goroutine's side of the privileged helper (mod.rs
// HelperLink).
type helperLink struct {
	slot      *helperSlot
	canceller *cancellerSlot
	// lost reports a helper that ended.
	lost func(err error)
	// allowDuo is what the helper should be told about Duo flashing, and
	// helperDuo what it was last told (nil: nothing yet).
	allowDuo  bool
	helperDuo *bool
	selection installflow.Selection
}

// run brings the helper up to date, then has it handle request, if any (nil
// for none; the reply is then the zero Reply).
func (l *helperLink) run(request *installer.Request, report func(installer.Progress)) (installer.Reply, error) {
	l.slot.mu.Lock()
	defer l.slot.mu.Unlock()
	h := l.slot.helper
	if h == nil {
		return installer.Reply{}, errors.New("Grant administrator access first.")
	}
	var reply installer.Reply
	err := l.sync(h, report)
	if err == nil && request != nil {
		reply, err = h.Handle(*request, report)
	}
	if err != nil && !h.Alive() {
		l.slot.helper = nil
		h.Close()
		l.canceller.set(nil)
		l.helperDuo = nil
		l.selection.HelperCleared()
		l.lost(errors.New("Administrator access ended. Grant it again to continue."))
	}
	return reply, err
}

func (l *helperLink) sync(h *helper.Helper, report func(installer.Progress)) error {
	if l.helperDuo == nil || *l.helperDuo != l.allowDuo {
		if _, err := h.Handle(installflow.AllowDuoRequest(l.allowDuo), func(installer.Progress) {}); err != nil {
			return err
		}
		allow := l.allowDuo
		l.helperDuo = &allow
	}
	if request := l.selection.Pending(); request != nil {
		l.selection.HelperCleared()
		if _, err := h.Handle(*request, report); err != nil {
			return err
		}
		l.selection.HelperAnswered()
	}
	return nil
}

// installerRunner runs the engine on its own goroutine (mod.rs Runner). Its
// results reach the UI thread through gk.Post into done and access, which
// only the UI thread touches; poll drains them.
type installerRunner struct {
	mu     sync.Mutex
	jobs   []installer.Request
	closed bool
	wake   chan struct{}

	progressMu sync.Mutex
	progress   *installer.Progress
	// cancel stops a download in progress, in this process or in the helper.
	cancel *atomic.Bool
	helper *cancellerSlot
	// privileged is the helper, once access is granted.
	privileged *helperSlot
	work       string

	// UI thread only.
	done []installerOutcome
	// access is the outcome of each grant attempt, and a lost helper (nil:
	// granted).
	access []error
}

// startInstallerRunner is Runner::start.
func startInstallerRunner(work string) *installerRunner {
	r := &installerRunner{
		wake:       make(chan struct{}, 1),
		cancel:     new(atomic.Bool),
		helper:     &cancellerSlot{},
		privileged: &helperSlot{},
		work:       work,
	}
	go r.serve()
	return r
}

func (r *installerRunner) serve() {
	report := func(p installer.Progress) {
		r.progressMu.Lock()
		r.progress = &p
		r.progressMu.Unlock()
	}
	engine := installerEngine(r.work)
	engine.Progress = report
	engine.Cancel = r.cancel
	separate := helper.Needed()
	link := &helperLink{
		slot:      r.privileged,
		canceller: r.helper,
		lost:      r.reportAccess,
	}
	defer r.closeHelper()
	for {
		request, ok := r.next()
		if !ok {
			return
		}
		r.cancel.Store(false)
		report(installer.Progress{})
		if allow, ok := installflow.AllowDuoOf(request); ok {
			link.allowDuo = allow
		}
		if installflow.IsStockPrepare(request) {
			link.selection.Clear()
		}
		var reply installer.Reply
		var err error
		if separate && installflow.Privileged(request) {
			reply, err = link.run(&request, report)
		} else {
			reply, err = engine.Handle(request)
			if installflow.IsSelection(request) {
				link.selection.EngineAnswered(request, err == nil)
			}
			// The helper fetches and verifies its own copy now, in the same
			// cancellable step, before the mixer is touched.
			if separate && installflow.IsSelection(request) && err == nil {
				if r.cancel.Load() {
					reply, err = installer.Reply{}, installer.Error(installer.Cancelled)
				} else if _, lerr := link.run(nil, report); lerr != nil {
					reply, err = installer.Reply{}, lerr
				}
			}
		}
		if !r.deliver(installerOutcome{request: request, reply: reply, err: err}) {
			return
		}
	}
}

// next waits for the next request; false once the runner is closed.
func (r *installerRunner) next() (installer.Request, bool) {
	for {
		r.mu.Lock()
		if r.closed {
			r.mu.Unlock()
			return installer.Request{}, false
		}
		if len(r.jobs) > 0 {
			request := r.jobs[0]
			r.jobs = r.jobs[1:]
			r.mu.Unlock()
			return request, true
		}
		r.mu.Unlock()
		<-r.wake
	}
}

// deliver hands an outcome to the UI thread; false once the page is closed,
// which ends the goroutine (the Rust thread stopped when its receiver went).
func (r *installerRunner) deliver(o installerOutcome) bool {
	r.mu.Lock()
	closed := r.closed
	r.mu.Unlock()
	if closed {
		return false
	}
	gk.Post(func(cx *gk.Cx) { r.done = append(r.done, o) })
	return true
}

// reportAccess hands a grant outcome or a lost helper to the UI thread.
func (r *installerRunner) reportAccess(err error) {
	gk.Post(func(cx *gk.Cx) { r.access = append(r.access, err) })
}

// closeHelper ends a helper still held when the goroutine stops (the Rust
// helper was dropped with the thread).
func (r *installerRunner) closeHelper() {
	r.privileged.mu.Lock()
	h := r.privileged.helper
	r.privileged.helper = nil
	r.privileged.mu.Unlock()
	r.helper.set(nil)
	if h != nil {
		h.Close()
	}
}

// send queues request; false when the goroutine is gone.
func (r *installerRunner) send(request installer.Request) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return false
	}
	r.jobs = append(r.jobs, request)
	select {
	case r.wake <- struct{}{}:
	default:
	}
	return true
}

// stopDownload is Runner::cancel: stops a download in progress, here or in
// the helper.
func (r *installerRunner) stopDownload() {
	r.cancel.Store(true)
	r.helper.mu.Lock()
	c := r.helper.canceller
	r.helper.mu.Unlock()
	if c != nil {
		c.Cancel()
	}
}

// close is Runner's Drop: closing the page ends a release check rather than
// leaving it, and any helper, running until the network gives up.
func (r *installerRunner) close() {
	r.stopDownload()
	r.mu.Lock()
	r.closed = true
	r.jobs = nil
	r.mu.Unlock()
	select {
	case r.wake <- struct{}{}:
	default:
	}
}

// grant starts the helper, which opens the system's password prompt, and
// checks that it answers before handing it to the request goroutine.
func (r *installerRunner) grant() {
	go func() {
		h, err := startInstallerHelper(r.work)
		if err == nil {
			if _, err = h.Handle(installflow.ProbeRequest(), func(installer.Progress) {}); err != nil {
				h.Close()
			}
		}
		if err == nil {
			r.mu.Lock()
			closed := r.closed
			r.mu.Unlock()
			if closed {
				// The page went away while the prompt was open.
				h.Close()
				return
			}
			c := h.Canceller()
			r.helper.set(&c)
			r.privileged.mu.Lock()
			r.privileged.helper = h
			r.privileged.mu.Unlock()
		}
		r.reportAccess(err)
	}()
}

func (r *installerRunner) takeAccess() []error {
	out := r.access
	r.access = nil
	return out
}

func (r *installerRunner) takeOutcomes() []installerOutcome {
	out := r.done
	r.done = nil
	return out
}

// currentProgress is the last progress reported, nil before any.
func (r *installerRunner) currentProgress() *installer.Progress {
	r.progressMu.Lock()
	defer r.progressMu.Unlock()
	if r.progress == nil || r.progress.Phase == "" {
		return nil
	}
	p := *r.progress
	return &p
}

// Installer is installer/mod.rs Installer: the guided task's model and the
// runner that carries out its requests, with what the page itself keeps.
type Installer struct {
	model  *installflow.Model
	runner *installerRunner
	// kind is the task it was opened for.
	kind taskKind
	// engine is the engine state readiness was last judged from.
	engine   *string
	progress *installer.Progress
	// controlsScroll is the task body's scroll position.
	controlsScroll gk.ScrollHandle
	access         installflow.Access
	// Before you start's version menus, made on first sync and kept in step
	// with the model there.
	firmwareSelect *picker[installflow.Choice]
	mixerSelect    *picker[installflow.MixerSource]
	// versionsOpen: Before you start's "Choose versions" is open.
	versionsOpen bool
	// startOnGrant and retryOnGrant: the button that asked for
	// administrator access starts the run, or tries again, once it is
	// granted.
	startOnGrant, retryOnGrant bool
	// closeOnStop: Stop asked a request in flight to stop; the task closes
	// once it has.
	closeOnStop bool
	// stage is the stage on screen and stageSince when it began, for the
	// time so far.
	stage      installflow.Stage
	stageSince time.Time
	// waitLine is the storage fix's waiting line on screen (fixWaitLine)
	// and waitSince when it began, for that wait's own time so far.
	waitLine  string
	waitSince time.Time
	// pausedAt is the time so far when the mixer was asked to be switched
	// off and on; zero otherwise.
	pausedAt time.Duration
	// keptWaiting and waitHelp are the taking-longer panel's Keep waiting
	// and What can I do?, for the current stage.
	keptWaiting, waitHelp bool
	// mic is the input picked on the finished page.
	mic int
	// shown is the second the time so far was last drawn at.
	shown int64
	// preferFlash: the user chose the full flash over an available in-place
	// update.
	preferFlash bool
	// imageCheckAsked is when the mixer's image check was last asked for;
	// zero for never.
	imageCheckAsked time.Time
	// repair is the Repair mixer page, when it is the one open.
	repair *installflow.Repair
	// fix is the storage fix on its own, when it is the task open;
	// fixOnGrant starts it once administrator access is granted, and
	// fixStep is its step on screen, for the time so far.
	fix        *installflow.Fix
	fixOnGrant bool
	fixStep    int
	// inPlaceFrom is the RC2FX version the mixer ran when an in-place
	// update started, for the page that says it went back to it;
	// inPlaceKind is its stage on screen, for the time so far.
	inPlaceFrom string
	inPlaceKind installflow.InPlaceKind
	// fixture: a development page drawn from made-up state
	// (flow_fixtures_on.go). It never sends a request.
	fixture bool
}

// taskKind is the guided task the installer was opened for.
type taskKind int

const (
	taskInstall taskKind = iota
	taskUpdate
	taskRemove
	// taskFix is the storage fix on its own (Installer.fix).
	taskFix
)

func (i *Installer) busy() bool {
	return i.model.Busy() || (i.repair != nil && i.repair.Busy()) || (i.fix != nil && i.fix.Busy())
}

// send runs request, if any (nil for none).
func (i *Installer) send(request *installer.Request) {
	if request == nil || i.fixture {
		return
	}
	i.progress = nil
	r := *request
	if installflow.IsStorageFix(r) {
		r.FixDebug = fixDebug()
	}
	if !i.runner.send(r) {
		i.model.Finish(*request, installer.Reply{}, errors.New("The app's installer stopped unexpectedly. Close this page and open it again."))
	}
}

// release stops the page's runner and hands its kit state to free (App.retire:
// released once no view shows it any more).
func (i *Installer) release(free func(...any)) {
	i.runner.close()
	i.firmwareSelect.release(free)
	i.mixerSelect.release(free)
	free(i.controlsScroll)
}

// workDirectory is the installer's folder for downloads and backups.
func workDirectory() (string, bool) {
	if path, ok := os.LookupEnv("RC2FX_INSTALLER_WORK"); ok {
		if filepath.IsAbs(path) {
			return path, true
		}
		cwd, err := os.Getwd()
		if err != nil {
			return "", false
		}
		return filepath.Join(cwd, path), true
	}
	return installer.DefaultWork()
}

// installerAutostartWait is how long a task started from elsewhere (Home's
// Install update, the first screen's Continue the install) waits for the
// installer to find the mixer and be ready, before leaving the page to the
// person.
const installerAutostartWait = 60 * time.Second

// imageCheckEvery is how often to ask for the mixer's image check while it
// runs.
const imageCheckEvery = 3 * time.Second

// openTask opens the guided task for kind in front of everything else, where
// it stays until Done, Stop, Not now or Close. A task already open stays as
// it is.
func (a *App) openTask(cx *gk.Cx, kind taskKind) {
	if a.installer != nil {
		return
	}
	// The installer finds mixers on USB by itself, so it would act on a
	// real one while the rest of this build shows the simulated one.
	if mockMixer {
		a.message = "The installer is off while using the mock mixer."
		a.notify(cx)
		return
	}
	work, ok := workDirectory()
	if !ok {
		a.message = "The installer has no folder for its downloads and backups. Set RC2FX_INSTALLER_WORK to one."
		a.notify(cx)
		return
	}
	// The key the engine builds in, as it chooses it.
	access := installflow.Access{Kind: installflow.AccessNotNeeded}
	if helper.Needed() {
		access = installflow.Access{Kind: installflow.AccessNeeded}
	}
	i := &Installer{
		model:          installflow.NewModel(),
		runner:         startInstallerRunner(work),
		kind:           kind,
		controlsScroll: gk.NewScrollHandle(cx),
		access:         access,
		stageSince:     time.Now(),
	}
	i.model.Privileged = i.access.AllowsFlashing()
	// This app runs the storage fix's request, so its installs may fix the
	// mixer's storage first.
	i.model.FixPolicy = installflow.FixAuto
	i.model.Bundled = cloneString(a.bundledMixerSoftware)
	i.model.SettledFlash = a.settledFlash
	if kind == taskRemove {
		i.model.SetStock(true)
	}
	first := i.model.SetAllowDuo(a.allowDuo)
	i.send(first)
	a.installer = i
	a.notify(cx)
}

func cloneString(v *string) *string {
	if v == nil {
		return nil
	}
	c := *v
	return &c
}

func sameString(a, b *string) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// grantInstallerAccess asks the system for administrator access: this is
// what opens its password prompt, and only when the person presses a button
// that needs it.
func (a *App) grantInstallerAccess(cx *gk.Cx) {
	i := a.installer
	if i == nil {
		return
	}
	if i.access.Kind == installflow.AccessNeeded || i.access.Kind == installflow.AccessRefused {
		i.access = installflow.Access{Kind: installflow.AccessAsking}
		i.runner.grant()
		a.notify(cx)
	}
}

// closeTask closes the guided task, unless a request is in flight. After a
// finished task with the mixer connected the overview comes next.
func (a *App) closeTask(cx *gk.Cx, overview bool) {
	if a.installer != nil && a.installer.busy() {
		return
	}
	a.discardInstaller(cx)
	if overview && a.session.Connected {
		a.nav.showHome()
	}
	a.notify(cx)
}

// discardInstaller drops the installer, keeping only which flash has
// finished. (Rust's had no cx: dropping the entity freed its menus.)
func (a *App) discardInstaller(cx *gk.Cx) {
	if i := a.installer; i != nil {
		a.installer = nil
		a.installerAutostart = nil
		if !i.fixture {
			a.settledFlash = i.model.SettledFlash
		}
		i.release(a.retire)
	}
}

// installerEvent runs a model event, then sends any request it makes.
func (a *App) installerEvent(cx *gk.Cx, event func(m *installflow.Model) *installer.Request) {
	i := a.installer
	if i == nil {
		return
	}
	request := event(i.model)
	i.send(request)
	a.notify(cx)
}

// installerSettingsRead is lib.rs's EventKind::Settings:
// `self.installer_event(cx, |m| m.settings_read(result))`.
func (a *App) installerSettingsRead(cx *gk.Cx, settings string, err error) {
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.SettingsRead(settings, err) })
}

// installerImageCheck hands the mixer's image check to the model. A failed
// read is asked again on the next poll.
func (a *App) installerImageCheck(cx *gk.Cx, check *core.ImageCheck, err error) {
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.ImageCheckRead(check, err) })
}

// startTask starts the run. Where administrator access is missing the
// button asks for it first (the system's password prompt), and the run
// starts once it is granted.
func (a *App) startTask(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.busy() {
		return
	}
	if !i.access.AllowsFlashing() {
		if i.model.CanStartOnceGranted() {
			i.startOnGrant = true
			a.grantInstallerAccess(cx)
		}
		return
	}
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.Start() })
}

// retryTask tries a failed run again from the step the mixer's state calls
// for now, asking for administrator access first where it ended.
func (a *App) retryTask(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.busy() {
		return
	}
	if !i.access.AllowsFlashing() {
		i.retryOnGrant = true
		a.grantInstallerAccess(cx)
		return
	}
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.Retry() })
}

// stopTask is the footer's Stop: it cancels a run that hasn't touched the
// mixer, ends a failed one or one whose wait is taking longer than usual,
// and closes the task.
func (a *App) stopTask(cx *gk.Cx) {
	i := a.installer
	if i == nil {
		return
	}
	if f := i.fix; f != nil && f.Busy() {
		// Only before the fix touches the mixer; it goes back to Before you
		// start when it has stopped, and the task closes.
		if f.Cancel() {
			i.runner.stopDownload()
			i.closeOnStop = true
			a.notify(cx)
		}
		return
	}
	m := i.model
	switch run := m.Run; {
	case run == nil:
	case run.Status().Kind == installflow.RunFailed:
		m.Stop()
	case m.CanCancel():
		if m.Cancel() {
			// A request is in flight: the run ends when it returns.
			i.runner.stopDownload()
			i.closeOnStop = true
			a.notify(cx)
			return
		}
	case m.CanStop(time.Now()):
		m.Stop()
	default:
		return
	}
	a.closeTask(cx, false)
}

// engineState is what poll judges RC2FX's readiness from:
// `{"engine": s.engine, "config": {"revision": s.config.revision}}`.
func engineState(saved *core.DeviceState) (string, bool) {
	type config struct {
		Revision uint64 `json:"revision"`
	}
	body, err := json.Marshal(struct {
		Engine core.EngineStatus `json:"engine"`
		Config config            `json:"config"`
	}{saved.Engine, config{saved.Config.Revision}})
	if err != nil {
		return "", false
	}
	return string(body), true
}

// installerConnectionChanged: the connection's generation changed, so a
// settings read sent on the old one is never answered; ask it again. An
// in-place upload on the old connection was canceled with it and fails.
func (a *App) installerConnectionChanged() {
	if a.installer != nil {
		a.installer.model.ConnectionChanged()
	}
}

// accessOf is the access a grant outcome (or a lost helper) leaves.
func accessOf(err error) installflow.Access {
	switch {
	case err == nil:
		return installflow.Access{Kind: installflow.AccessGranted}
	case errors.Is(err, helper.ErrNotGranted):
		return installflow.Access{Kind: installflow.AccessRefused,
			Why: "The password prompt was closed, or the password wasn't accepted."}
	}
	return installflow.Access{Kind: installflow.AccessRefused, Why: err.Error()}
}

// pollInstaller collects finished requests, polls and progress. Returns
// whether to redraw.
func (a *App) pollInstaller(cx *gk.Cx) bool {
	i := a.installer
	if i == nil || i.fixture {
		return false
	}
	// RC2FX's readiness, as this app's USB connection sees it.
	sameMixer := i.model.IsSessionMixer(a.connectedMixer)
	var engine *string
	if a.session.Saved != nil && a.session.Connected && a.streamConnected && sameMixer {
		if state, ok := engineState(a.session.Saved); ok {
			engine = &state
		}
	}
	var mixer, fw *string
	if sameMixer && a.deviceVersions != nil {
		mixer = cloneString(a.deviceVersions.Runtime)
		fw = cloneString(a.deviceVersions.Firmware)
	}
	// RC2FX names the exact firmware profile it runs on; without RC2FX, the
	// mixer's USB device information still says which RØDE firmware it has,
	// so replacing newer firmware is never silent.
	if fw == nil {
		if v, ok := i.model.USBFirmware(); ok {
			fw = &v
		}
	}
	changed := false
	if !sameString(i.model.Installed, mixer) || !sameString(i.model.InstalledFirmware, fw) {
		i.model.SetInstalled(mixer, fw)
		changed = true
	}
	if !sameString(engine, i.engine) {
		var service *installer.Service
		if engine != nil {
			s := installer.EmbeddedService([]byte(*engine))
			service = &s
		}
		i.model.SetService(service)
		i.engine = engine
		changed = true
	}
	// After a flash, RC2FX checks RØDE's files in the background; ask how it
	// went until it has an answer worth acting on.
	if i.model.WantsImageCheck() &&
		(i.imageCheckAsked.IsZero() || time.Since(i.imageCheckAsked) >= imageCheckEvery) {
		i.imageCheckAsked = time.Now()
		a.command(transport.CommandImageCheck{Generation: a.generation})
		return changed
	}
	if i.model.TakeSettingsRequest() {
		if !a.command(transport.CommandSettings{Generation: a.generation}) {
			why := a.message
			a.message = ""
			a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
				return m.SettingsRead("", errors.New(why))
			})
		}
		return true
	}
	for _, err := range i.runner.takeAccess() {
		changed = true
		i.access = accessOf(err)
		i.model.Privileged = i.access.AllowsFlashing()
		start, retry, fix := i.startOnGrant, i.retryOnGrant, i.fixOnGrant
		i.startOnGrant, i.retryOnGrant, i.fixOnGrant = false, false, false
		if i.access.AllowsFlashing() {
			switch {
			case fix:
				a.startFix(cx)
			case start:
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.Start() })
			case retry:
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.Retry() })
			}
		}
	}
	for _, o := range i.runner.takeOutcomes() {
		changed = true
		if installflow.IsRepair(o.request) {
			repairOutcome(i, o.request, o.reply, o.err)
			continue
		}
		if installflow.IsStorageFix(o.request) && i.fix != nil {
			fixOutcome(i, o.reply, o.err)
			continue
		}
		if o.err == nil {
			if offers, ok := installflow.MixerReleaseOffers(o.reply); ok {
				a.appUpdate.setSigned(slices.Clone(offers))
			}
		}
		if installflow.ChangesCatalog(o.request) {
			// Judge readiness again on the next poll.
			i.engine = nil
		}
		next := i.model.Finish(o.request, o.reply, o.err)
		i.send(next)
	}
	if poll := i.model.Tick(time.Now()); poll != nil {
		changed = true
		i.send(poll)
	}
	if i.busy() {
		progress := i.runner.currentProgress()
		if !sameProgress(progress, i.progress) {
			i.progress = progress
			changed = true
			// The storage fix's steps come as progress.
			if progress != nil {
				if i.fix != nil {
					i.fix.Progress(*progress)
				}
				i.model.FixProgress(*progress)
			}
		}
	}
	if i.closeOnStop && i.model.Run == nil && !i.busy() {
		// A fix that got past the point where it stops stays on screen.
		if i.fix != nil && i.fix.Kind != installflow.FixBefore {
			i.closeOnStop = false
			return true
		}
		a.closeTask(cx, false)
		return true
	}
	return a.trackStage(i) || a.driveInPlaceUpdate() || changed
}

// trackStage follows the stage on screen, for the time so far and the
// taking-longer panel, and redraws once a second while a run goes on.
func (a *App) trackStage(i *Installer) bool {
	now := time.Now()
	changed := false
	if f := i.fix; f != nil {
		if step := f.Step(); step != i.fixStep {
			i.fixStep, i.stageSince = step, now
			changed = true
		}
		// The page's time so far counts from when its current line began.
		if line := fixDetailLine(f.Event); line != i.waitLine {
			i.waitLine, i.waitSince = line, now
		}
		if f.Busy() && now.Unix() != i.shown {
			i.shown = now.Unix()
			changed = true
		}
		return changed
	}
	if kind := i.model.InPlace().Kind; kind != i.inPlaceKind {
		i.inPlaceKind, i.stageSince = kind, now
		changed = true
	}
	if i.model.InPlace().Active() && now.Unix() != i.shown {
		i.shown = now.Unix()
		changed = true
	}
	// The time so far stops where it was when the mixer has to be switched
	// off and on, and starts again once it is back.
	power := i.model.PowerCycle()
	switch {
	case power && i.pausedAt == 0:
		i.pausedAt = max(now.Sub(i.stageSince), time.Second)
	case !power && i.pausedAt != 0:
		i.pausedAt = 0
		i.stageSince = now
	}
	if stage := i.model.Stage(); stage != i.stage {
		i.stage = stage
		if !power {
			i.stageSince = now
		}
		i.keptWaiting, i.waitHelp = false, false
		changed = true
	}
	if line := fixWaitLine(i.model); line != i.waitLine {
		i.waitLine, i.waitSince = line, now
		changed = true
	}
	if i.model.Running() && now.Unix() != i.shown {
		i.shown = now.Unix()
		changed = true
	}
	return changed
}

func sameProgress(a, b *installer.Progress) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// installMixerUpdate is a notice's Install update: open Update RC2FX on
// RC2FX (not stock) firmware and start it as soon as it can: in place when
// the mixer can install the update itself, otherwise the flash.
func (a *App) installMixerUpdate(cx *gk.Cx) {
	a.openTask(cx, taskUpdate)
	if i := a.installer; i != nil {
		i.model.SetStock(false)
		deadline := time.Now().Add(installerAutostartWait)
		a.installerAutostart = &deadline
	}
	a.notify(cx)
}

// continueInstall is the first screen's Continue the install, for a mixer
// waiting in update mode: open Install RC2FX and start as soon as it can.
// Before you start stays on screen until then, and for the person to press
// if it can't start by itself.
func (a *App) continueInstall(cx *gk.Cx) {
	a.openTask(cx, taskInstall)
	if a.installer != nil {
		deadline := time.Now().Add(installerAutostartWait)
		a.installerAutostart = &deadline
	}
	a.notify(cx)
}

// mixerUpdateNotice is shown on Home when the mixer reports that part of its
// RC2FX software is older than the rest. The fix is installing the current
// mixer software, so the notice starts that.
func (a *App) mixerUpdateNotice(cx *gk.Cx) (gk.DivEl, bool) {
	if a.deviceVersions == nil || a.deviceVersions.NativeProtocol == nil {
		return gk.DivEl{}, false
	}
	if a.installer != nil {
		return gk.DivEl{}, false
	}
	text := "Part of the RC2FX software on this mixer is older than the rest, so some " +
		"telemetry is missing. Installing the current version flashes the mixer, " +
		"which then restarts; audio stops until it is back. Your drafts are kept."
	if n := a.deviceVersions.InPlaceUpdates; n != nil && *n > 0 {
		text = "Part of the RC2FX software on this mixer is older than the rest, so some " +
			"telemetry is missing. Installing the current version needs no flash: " +
			"the mixer's audio stops for a few seconds while RC2FX restarts. Your " +
			"drafts are kept."
	}
	return card().BorderColor(rgb(Warn)).
		Child(gk.Div(s.FontSemibold).TextColor(rgb(Warn)).Child(txt("Update the RC2FX mixer software"))).
		Child(gk.Div(s.TextSm).TextColor(rgb(Muted)).Child(txt(text))).
		Child(gk.HFlex(s.Gap2).Child(gk.Button("mixer-update-install").Primary().Label("Install update").
			Disabled(a.installerFlashing()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.installMixerUpdate(cx) }))), true
}

// inPlaceSupport is what the connected mixer says about in-place updates,
// when the installer and the app's connection see the same mixer.
func (a *App) inPlaceSupport() installflow.InPlaceSupport {
	sameMixer := a.installer != nil && a.installer.model.IsSessionMixer(a.connectedMixer)
	if !sameMixer || a.deviceVersions == nil {
		return installflow.InPlaceSupport{}
	}
	v := a.deviceVersions
	return installflow.InPlaceSupport{
		Supported: v.InPlaceUpdates != nil && *v.InPlaceUpdates > 0,
		Profile:   v.FirmwareProfile,
	}
}

// inPlaceOffer is the signed release an in-place update would install now,
// if any.
func (a *App) inPlaceOffer() *installer.MixerOffer {
	if a.installer == nil {
		return nil
	}
	offer := a.installer.model.InPlaceOffer(a.inPlaceSupport())
	if offer == nil {
		return nil
	}
	c := *offer
	return &c
}

// startInPlaceUpdate installs the newer signed RC2FX release without a
// flash: download and verify it here, then send it to the mixer, which
// verifies it again.
func (a *App) startInPlaceUpdate(cx *gk.Cx) {
	offer := a.inPlaceOffer()
	if offer == nil {
		return
	}
	profile := a.inPlaceSupport().Profile
	if profile == nil {
		return
	}
	p := *profile
	if i := a.installer; i != nil {
		i.inPlaceFrom = ""
		if i.model.Installed != nil {
			i.inPlaceFrom = *i.model.Installed
		}
	}
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.StartInPlace(*offer, p) })
}

// inPlaceStart is the body of the mixer's `POST api/v1/update`, in the
// Rust's key order.
func inPlaceStart(bundle installer.InPlaceBundle) (json.RawMessage, error) {
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	err := enc.Encode(struct {
		Version  string                `json:"version"`
		Size     uint64                `json:"size"`
		SHA256   string                `json:"sha256"`
		Metadata installer.RawMetadata `json:"metadata"`
	}{bundle.Version, bundle.Size, bundle.SHA256, bundle.Metadata})
	return json.RawMessage(bytes.TrimSuffix(b.Bytes(), []byte("\n"))), err
}

// driveInPlaceUpdate moves an in-place update along: sends a verified
// release to the mixer, and while it installs asks how it ended. True when
// it changed. What the mixer says is the model's to judge.
func (a *App) driveInPlaceUpdate() bool {
	generation := a.generation
	i := a.installer
	if i == nil {
		return false
	}
	if bundle, ok := i.model.TakeInPlaceBundle(); ok {
		// What update/last says before this update: the transport runs
		// commands in order, so the reply predates the install.
		if !a.command(transport.CommandMixerUpdateLast{Generation: generation}) {
			a.message = ""
		}
		start, err := inPlaceStart(bundle)
		sent := err == nil && a.command(transport.CommandMixerUpdate{
			Generation: generation,
			Start:      start,
			Payload:    bundle.Path,
			Base:       bundle.Base,
		})
		if !sent {
			why := a.message
			if err != nil {
				why = err.Error()
			}
			a.message = ""
			if a.installer != nil {
				a.installer.model.InPlaceSendFailed(why)
			}
		}
		return true
	}
	ask, changed := i.model.InPlaceTick(time.Now(), a.session.Connected)
	if ask {
		a.command(transport.CommandMixerUpdateLast{Generation: generation})
	}
	return changed
}

// inPlaceEvent is the transport's report on an in-place update.
func (a *App) inPlaceEvent(event transport.MixerUpdateEvent) {
	i := a.installer
	if i == nil {
		return
	}
	switch e := event.(type) {
	case transport.MixerUpdateEventUploading:
		i.model.InPlaceProgress(e.Sent, e.Total)
	case transport.MixerUpdateEventInstalling:
		i.model.InPlaceAccepted(time.Now())
	case transport.MixerUpdateEventFailed:
		i.model.InPlaceRefused(e.Message)
	}
}

// inPlaceOutcome is the mixer's record of its last in-place update (JSON
// null is None); once it ends the update, read the versions the mixer now
// runs.
func (a *App) inPlaceOutcome(record json.RawMessage) {
	if i := a.installer; i != nil && i.model.InPlaceOutcome(record) {
		a.command(transport.CommandVersion{Generation: a.generation})
	}
}

// autostartInstaller starts a task opened by Install update or Continue
// the install once it has found the mixer and a release; gives up quietly
// at the deadline, leaving Before you start to say what is missing.
func (a *App) autostartInstaller(cx *gk.Cx) {
	if a.installerAutostart == nil {
		return
	}
	deadline := *a.installerAutostart
	i := a.installer
	if i == nil {
		a.installerAutostart = nil
		return
	}
	inPlace := a.inPlaceOffer()
	switch {
	case i.model.Run != nil || i.model.InPlace().Active() || !time.Now().Before(deadline):
		a.installerAutostart = nil
	case !i.busy() && inPlace != nil:
		a.installerAutostart = nil
		a.startInPlaceUpdate(cx)
	case !i.busy() && i.model.CanStart():
		a.installerAutostart = nil
		a.startTask(cx)
	}
}

// installerFlashing reports a run that has switched the mixer to USB update
// mode and not yet finished flashing it: the mixer cannot run RC2FX
// meanwhile.
func (a *App) installerFlashing() bool {
	if a.installer == nil || a.installer.fixture || a.installer.model.Run == nil {
		return false
	}
	r := a.installer.model.Run
	return r.Committed && !r.Flashed
}

// installerRestartBlocker is why RC2FX should not restart to apply an app
// update: the installer has a run going, even between steps, or the mixer
// may still be updating after a flash. ok false for None.
func (a *App) installerRestartBlocker() (string, bool) {
	i := a.installer
	if i == nil {
		return "", false
	}
	if i.busy() || i.model.Run != nil {
		return "Finish or stop the install first.", true
	}
	if i.model.RecentlyFlashed() {
		return "Wait for the mixer to finish updating.", true
	}
	return "", false
}

// confirmCloseInstaller is the installer's part of the window-close
// decision. Closing is only held back while a step is running.
func (a *App) confirmCloseInstaller(cx *gk.Cx) (verdict, ok bool) {
	if a.installer == nil || !a.installer.busy() {
		return false, false
	}
	a.confirm(cx, confirmKindOther, Confirm{
		Title:  "The app is working",
		Detail: "Wait for the current step to finish before closing the app. Keep the mixer connected and switched on.",
		Action: "OK",
	}, func(cx *gk.Cx) {}, func(cx *gk.Cx) {})
	return false, true
}

// inPlaceShown reports that the page is about an in-place update rather
// than a flash.
func (a *App) inPlaceShown(i *Installer) bool {
	return i.model.InPlace().Kind != installflow.InPlaceIdle ||
		(!i.preferFlash && a.inPlaceOffer() != nil)
}

func choiceKey(c installflow.Choice) string { return c.Channel + "\x00" + c.Version }

func mixerSourceKey(source installflow.MixerSource) string {
	if source.Kind == installflow.MixerSourceBundled {
		return "bundled\x00" + source.Bundled
	}
	o := source.Offer
	return fmt.Sprintf("signed\x00%s\x00%s\x00%d\x00%s", o.Target, o.SHA256, o.Length, o.Metadata.Version)
}

// syncInstallerSelects brings Before you start's version menus in step with
// the model: their items, what is chosen, and the menus themselves on first
// use. (lib.rs ran it before each render; a gk view may not write kit
// state, so poll runs it.)
func (a *App) syncInstallerSelects(cx *gk.Cx) {
	i := a.installer
	if i == nil {
		return
	}
	m := i.model
	release := func(r installer.Release) pickerItem[installflow.Choice] {
		name := fmt.Sprintf("%s %s", r.Version, r.Channel)
		return pickerItem[installflow.Choice]{title: name, short: name,
			value: installflow.Choice{Channel: r.Channel, Version: r.Version}}
	}
	var releases []pickerItem[installflow.Choice]
	for _, r := range m.ReleasesFor(false) {
		releases = append(releases, release(r))
	}
	var mixers []pickerItem[installflow.MixerSource]
	for _, item := range m.MixerMenu() {
		mixers = append(mixers, pickerItem[installflow.MixerSource]{
			title: item.Name, short: item.Source.Version(), value: item.Source})
	}
	// The page may have drawn before its menus existed: draw it again.
	created := i.firmwareSelect == nil || i.mixerSelect == nil
	if i.firmwareSelect == nil {
		i.firmwareSelect = newPicker(a, cx, choiceKey, func(m *installflow.Model, c installflow.Choice) {
			if index := slices.IndexFunc(m.Releases, func(r installer.Release) bool {
				return r.Channel == c.Channel && r.Version == c.Version
			}); index >= 0 {
				m.Select(false, index)
			}
		})
	}
	i.firmwareSelect.sync(cx, releases, m.ChoiceFor(false))
	if i.mixerSelect == nil {
		i.mixerSelect = newPicker(a, cx, mixerSourceKey, func(m *installflow.Model, source installflow.MixerSource) {
			m.ChooseMixer(source)
		})
	}
	i.mixerSelect.sync(cx, mixers, m.Mixer)
	if created {
		a.notify(cx)
	}
}

// pickerItem is one entry of a version menu (mod.rs Choice): its line
// in the menu, the shorter text the closed menu shows, and what choosing it
// picks.
type pickerItem[V any] struct {
	title, short string
	value        V
}

// picker is a select menu whose items and choice come from the installer's
// model (mod.rs Picker). The model stays the source of truth: choosing sends
// the pick to it, and each sync brings the menu back in step with it. The
// kit's items carry key(value) as their value.
type picker[V any] struct {
	state  gk.SelectState
	source gk.RowSource
	sub    gk.Subscription
	key    func(V) string
	items  []pickerItem[V]
	// keys is what the menu was last given (title and key per item), and
	// selected the key last chosen (nil: none).
	keys     []string
	selected *string
}

func newPicker[V any](a *App, cx *gk.Cx, key func(V) string, pick func(m *installflow.Model, v V)) *picker[V] {
	p := &picker[V]{key: key}
	// A menu line with room above and below, so entries don't crowd.
	p.source = gk.NewSearchSource(cx, nil, gk.SearchRow(func(cx *gk.Cx, at gk.RowAt) gk.Element {
		if at.Row < 0 || at.Row >= len(p.items) {
			return gk.Div()
		}
		return gk.Div().Py(px(4)).Child(txt(p.items[at.Row].title))
	}))
	p.state = gk.NewSelectState(cx, p.source, gk.NoRow).Build()
	p.sub = p.state.OnSelectEvent(cx, func(cx *gk.Cx, e gk.SelectEvent) {
		confirm, ok := e.(gk.SelectEventConfirm)
		if !ok {
			return
		}
		value, ok := confirm.Value.Get()
		if !ok {
			return
		}
		for _, item := range p.items {
			if p.key(item.value) == value {
				v := item.value
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
					pick(m, v)
					return nil
				})
				return
			}
		}
	})
	return p
}

func (p *picker[V]) sync(cx *gk.Cx, items []pickerItem[V], selected *V) {
	keys := make([]string, len(items))
	for n, item := range items {
		keys[n] = item.title + "\x00" + p.key(item.value)
	}
	changed := !slices.Equal(keys, p.keys)
	p.items = items
	if changed {
		p.keys = keys
		search := make([]gk.SearchItem, len(items))
		for n, item := range items {
			search[n] = gk.SearchItem{Value: p.key(item.value), Title: item.title}
			if item.short != item.title {
				search[n].Display = gk.Some(item.short)
			}
		}
		gk.SetSearchItems(cx, p.source, search)
		p.state.SetItems(cx, p.source)
	}
	var key *string
	if selected != nil {
		k := p.key(*selected)
		key = &k
	}
	if changed || !sameString(key, p.selected) {
		if key != nil {
			p.state.SetSelectedValue(cx, *key)
		} else {
			p.state.SetSelectedIndex(cx, gk.NoRow)
		}
		p.selected = key
	}
}

func (p *picker[V]) release(free func(...any)) {
	if p == nil {
		return
	}
	free(p.sub, p.state, p.source)
}

// pickerSelect is a version menu, or nothing before its first sync. A
// select fills its parent and draws nothing in a parent without a definite
// width, so it sits in a box of its own width; its menu matches it.
// versionSelectWidth fits the longest version name with its channel.
const versionSelectWidth = 260

func pickerSelect[V any](p *picker[V], id string, disabled bool) gk.Element {
	slot := gk.Div(s.FlexNone).W(px(versionSelectWidth))
	if p != nil {
		slot = slot.Child(gk.Select(p.state).ID(id).Placeholder("None available").Disabled(disabled))
	}
	return slot
}
