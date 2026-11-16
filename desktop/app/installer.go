package app

// installer/mod.rs: install or update RC2FX on a RØDECaster Pro II.
//
// Requests run one at a time on a goroutine that owns the installer engine.
// On Linux without root, switching to USB update mode and flashing run in a
// helper started through pkexec, because raw USB, mounting and ejection need
// it. It starts only when the user grants access on the page.
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
	"reflect"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
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

// Installer is installer/mod.rs Installer: the page's model and the runner
// that carries out its requests.
type Installer struct {
	model  *installflow.Model
	runner *installerRunner
	// engine is the engine state readiness was last judged from.
	engine     *string
	progress   *installer.Progress
	confirming bool
	// controlsScroll is the page's scroll position.
	controlsScroll gk.ScrollHandle
	access         installflow.Access
	// The firmware card's menus, made on first sync and kept in step with
	// the model there.
	firmwareSelect *picker[installflow.Choice]
	mixerSelect    *picker[installflow.MixerSource]
	stockSelect    *picker[installflow.Choice]
	// preferFlash: the user chose the full flash over an available in-place
	// update.
	preferFlash bool
	// imageCheckAsked is when the mixer's image check was last asked for;
	// zero for never.
	imageCheckAsked time.Time
	// repair is the Repair mixer page, when it is the one open.
	repair *installflow.Repair
}

// needsAccess is Installer::needs_access (onboarding.rs reads it).
func (i *Installer) needsAccess() bool { return installflow.NeedsAccess(i.model, i.access) }

func (i *Installer) busy() bool {
	return i.model.Busy != nil || (i.repair != nil && i.repair.Busy())
}

// send runs request, if any (nil for none).
func (i *Installer) send(request *installer.Request) {
	if request == nil {
		return
	}
	i.progress = nil
	if !i.runner.send(*request) {
		i.model.Finish(*request, installer.Reply{}, errors.New("The installer stopped unexpectedly. Close and reopen it."))
	}
}

// release stops the page's runner and hands its kit state to free (App.retire:
// released once no view shows it any more).
func (i *Installer) release(free func(...any)) {
	i.runner.close()
	i.firmwareSelect.release(free)
	i.mixerSelect.release(free)
	i.stockSelect.release(free)
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

func paragraph(text string, color uint32) gk.DivEl {
	return gk.Div(s.TextSm).TextColor(rgb(color)).Child(txt(text))
}

func stepTitle(step installflow.Step) string {
	switch step {
	case installflow.StepBackup:
		return "Save settings"
	case installflow.StepMixer:
		return "Get mixer software"
	case installflow.StepPrepare:
		return "Prepare firmware"
	case installflow.StepUSBMode:
		return "Switch to USB update mode"
	case installflow.StepFlash:
		return "Flash firmware"
	}
	return "Wait for RC2FX"
}

func progressText(p *installer.Progress) string {
	megabytes := func(what string) string {
		if p.Total > 0 {
			return fmt.Sprintf("%s · %.0f of %.0f MB", what, float64(p.Completed)/1e6, float64(p.Total)/1e6)
		}
		return what
	}
	switch p.Phase {
	case "download":
		if p.Total == 0 {
			return fmt.Sprintf("Downloading firmware · %.0f MB", float64(p.Completed)/1e6)
		}
		return megabytes("Downloading firmware")
	case "verification":
		return fmt.Sprintf("Checking the %s checksum", p.Detail)
	case "stage":
		return megabytes(fmt.Sprintf("Copying %s to the mixer", p.Detail))
	case "verify":
		return megabytes(p.Detail)
	case "repair-backup":
		return megabytes(p.Detail)
	case "repair-check":
		if p.Total > 0 {
			return fmt.Sprintf("%s · %d of %d files", p.Detail, p.Completed, p.Total)
		}
	}
	return p.Detail
}

// installerAutostartWait is how long Install update waits for the installer
// to find the mixer and be ready, before leaving the page for the user.
const installerAutostartWait = 60 * time.Second

// imageCheckEvery is how often to ask for the mixer's image check while it
// runs.
const imageCheckEvery = 3 * time.Second

// openInstaller is open_installer.
func (a *App) openInstaller(cx *gk.Cx) {
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
		controlsScroll: gk.NewScrollHandle(cx),
		access:         access,
	}
	i.model.Privileged = i.access.AllowsFlashing()
	i.model.Bundled = cloneString(a.bundledMixerSoftware)
	i.model.SettledFlash = a.settledFlash
	first := i.model.SetAllowDuo(rememberedDuo())
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
// what opens its password prompt, and only when the user chooses to.
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

func (a *App) closeInstaller(cx *gk.Cx) {
	if a.installer != nil && a.installer.busy() {
		return
	}
	a.discardInstaller(cx)
	a.installerPage = false
	a.notify(cx)
}

// discardInstaller drops the installer, keeping only which flash has
// finished. (Rust's had no cx: dropping the entity freed its menus.)
func (a *App) discardInstaller(cx *gk.Cx) {
	if i := a.installer; i != nil {
		a.installer = nil
		a.settledFlash = i.model.SettledFlash
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

// startOrRetry is `if retry { m.retry() } else { m.start() }`.
func startOrRetry(retry bool) func(m *installflow.Model) *installer.Request {
	return func(m *installflow.Model) *installer.Request {
		if retry {
			return m.Retry()
		}
		return m.Start()
	}
}

// startInstaller starts the plan. Changing firmware asks once, in a dialog.
func (a *App) startInstaller(cx *gk.Cx) {
	i := a.installer
	if i == nil {
		return
	}
	retry := i.model.Run != nil && i.model.Run.Failed != nil
	if !i.model.NeedsConfirmation() || (retry && i.model.Device != firmware.ModelDuo) {
		a.installerEvent(cx, startOrRetry(retry))
		return
	}
	a.confirmInstaller(cx, retry, i.model.Stock)
}

func cloneChoice(c *installflow.Choice) *installflow.Choice {
	if c == nil {
		return nil
	}
	v := *c
	return &v
}

func sameChoice(a, b *installflow.Choice) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

func sameSource(a, b *installflow.MixerSource) bool {
	if a == nil || b == nil {
		return a == b
	}
	return a.Equal(*b)
}

func (a *App) confirmInstaller(cx *gk.Cx, retry, stock bool) {
	i := a.installer
	if i == nil || i.confirming {
		return
	}
	i.confirming = true
	m := i.model
	choice := m.Choice
	if stock {
		choice = m.StockChoice
	}
	version := "the firmware"
	if choice != nil {
		version = "RØDE " + choice.Version
	}
	warnings := m.WarningsFor(stock)
	var mixer *installflow.MixerSource
	if m.Mixer != nil {
		c := *m.Mixer
		mixer = &c
	}
	downgrade := !stock && mixer != nil && m.MixerChange(*mixer) == installflow.MixerChangeDowngrade
	withRC2FX := "RC2FX"
	if mixer != nil {
		withRC2FX = "RC2FX " + mixer.Version()
	}
	var title, detail, action string
	if stock {
		title = fmt.Sprintf("Flash stock %s?", version)
		detail = "This removes RC2FX. The mixer's audio stops while it updates, which takes a few minutes. Keep it powered on and connected until it's done. Your RC2FX settings are saved on this computer first."
		action = "Flash stock firmware"
	} else {
		if downgrade {
			title = fmt.Sprintf("Downgrade to %s on %s?", withRC2FX, version)
			action = "Downgrade and flash"
		} else {
			title = fmt.Sprintf("Flash %s with %s?", version, withRC2FX)
			action = "Flash and install"
		}
		detail = "The mixer's audio stops while it updates, which takes a few minutes. Keep it powered on and connected until it's done. Your settings are saved first and carried over."
	}
	device := m.Device
	reviewedChoice := cloneChoice(m.Choice)
	reviewedStock := cloneChoice(m.StockChoice)
	mode := m.Stock
	usb := slices.Clone(m.USB())
	// The device sits above the question, so the title stays short and
	// clear of the dialog's close button.
	eyebrow := device.Label()
	if device == firmware.ModelDuo {
		detail = installer.DuoWarning + "\n\n" + detail
	}
	detail = strings.Join(append(slices.Clone(warnings), detail), "\n\n")
	if device == firmware.ModelDuo {
		action = "Flash Duo at my own risk"
	}
	a.confirm(cx, Confirm{
		Eyebrow: eyebrow,
		Title:   title,
		Detail:  detail,
		Cancel:  "Cancel",
		Action:  action,
	}, func(cx *gk.Cx) {
		if a.installer != nil {
			a.installer.confirming = false
		}
		// A dialog is not permission to flash a different device, release
		// or option set than the one the user reviewed.
		if i := a.installer; i != nil &&
			i.model.Device == device &&
			sameChoice(i.model.Choice, reviewedChoice) &&
			sameChoice(i.model.StockChoice, reviewedStock) &&
			i.model.Stock == mode &&
			sameSource(i.model.Mixer, mixer) &&
			slices.Equal(i.model.WarningsFor(stock), warnings) &&
			reflect.DeepEqual(i.model.USB(), usb) &&
			i.access.AllowsFlashing() &&
			!i.busy() {
			a.installerEvent(cx, startOrRetry(retry))
		}
		a.notify(cx)
	}, func(cx *gk.Cx) {
		if a.installer != nil {
			a.installer.confirming = false
		}
		a.notify(cx)
	})
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

// pollInstaller collects finished requests, polls and progress. Returns
// whether to redraw.
func (a *App) pollInstaller(cx *gk.Cx) bool {
	i := a.installer
	if i == nil {
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
		switch {
		case err == nil:
			i.access = installflow.Access{Kind: installflow.AccessGranted}
		case strings.Contains(err.Error(), "not granted"):
			i.access = installflow.Access{Kind: installflow.AccessRefused,
				Why: "The password prompt was dismissed or the password wasn't accepted."}
		default:
			i.access = installflow.Access{Kind: installflow.AccessRefused, Why: err.Error()}
		}
		i.model.Privileged = i.access.AllowsFlashing()
	}
	for _, o := range i.runner.takeOutcomes() {
		changed = true
		if installflow.IsRepair(o.request) {
			repairOutcome(i, o.request, o.reply, o.err)
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
		}
	}
	return a.driveInPlaceUpdate() || changed
}

func sameProgress(a, b *installer.Progress) bool {
	if a == nil || b == nil {
		return a == b
	}
	return *a == *b
}

// openMixerControls closes the installer and connects the mixer controls to
// the mixer.
func (a *App) openMixerControls(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.busy() {
		return
	}
	a.closeInstaller(cx)
	if !a.session.Connected {
		a.connect(cx)
	}
}

// installMixerUpdate is a notice's Install update: open the installer on
// RC2FX (not stock) firmware and start it as soon as it can: in place when
// the mixer can install the update itself, otherwise the flash, which still
// asks first.
func (a *App) installMixerUpdate(cx *gk.Cx) {
	if !a.installerPage {
		a.toggleInstallerPage(cx)
	}
	if i := a.installer; i != nil {
		i.model.SetStock(false)
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
	if a.installerPage {
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
// release to the mixer, and after the mixer restarts asks how it ended. True
// when it changed.
func (a *App) driveInPlaceUpdate() bool {
	const askEvery = 3 * time.Second
	const giveUp = 6 * time.Minute
	generation := a.generation
	i := a.installer
	if i == nil {
		return false
	}
	if bundle, ok := i.model.TakeInPlaceBundle(); ok {
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
				a.installer.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceFailed,
					Error: fmt.Sprintf("Couldn't reach the mixer. Nothing changed on it. %s", why)}
			}
		}
		return true
	}
	if i.model.InPlace.Kind != installflow.InPlaceInstalling {
		return false
	}
	since := i.model.InPlace.Since
	if time.Since(since) >= giveUp {
		i.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceFailed,
			Error: "The mixer didn't say how the update ended. Check the RC2FX version it reports; " +
				"if it didn't change, the previous version is still running."}
		return true
	}
	if a.session.Connected && (a.inPlaceAsked == nil || time.Since(*a.inPlaceAsked) >= askEvery) {
		now := time.Now()
		a.inPlaceAsked = &now
		a.command(transport.CommandMixerUpdateLast{Generation: generation})
	}
	return false
}

// inPlaceEvent is the transport's report on an in-place update.
func (a *App) inPlaceEvent(event transport.MixerUpdateEvent) {
	i := a.installer
	if i == nil || i.model.InPlace.Kind != installflow.InPlaceUploading {
		return
	}
	version := i.model.InPlace.Version
	switch e := event.(type) {
	case transport.MixerUpdateEventUploading:
		i.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceUploading,
			Version: version, Sent: e.Sent, Total: e.Total}
	case transport.MixerUpdateEventInstalling:
		now := time.Now()
		a.inPlaceAsked = &now
		i.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceInstalling,
			Version: version, Since: time.Now()}
	case transport.MixerUpdateEventFailed:
		i.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceFailed,
			Error: fmt.Sprintf("The mixer didn't accept the update. Nothing changed on it. %s", e.Message)}
	}
}

// inPlaceOutcome is the mixer's record of its last in-place update (JSON
// null is None). Only the outcome of the update in progress counts;
// anything else is an older record.
func (a *App) inPlaceOutcome(record json.RawMessage) {
	i := a.installer
	if i == nil || i.model.InPlace.Kind != installflow.InPlaceInstalling {
		return
	}
	version := i.model.InPlace.Version
	var fields map[string]any
	if json.Unmarshal(record, &fields) != nil || fields == nil {
		return
	}
	str := func(key string) (string, bool) {
		v, ok := fields[key].(string)
		return v, ok
	}
	if to, ok := str("to"); !ok || to != version {
		return
	}
	outcome, _ := str("outcome")
	committed := outcome == "committed"
	reason, _ := str("reason")
	reboot, _ := fields["guiShimNeedsReboot"].(bool)
	var detail string
	switch {
	case committed && reboot:
		detail = "Part of the update applies the next time the mixer restarts."
	case committed:
		detail = ""
	case reason != "":
		detail = fmt.Sprintf("The previous version was put back: %s", reason)
	default:
		detail = "The previous version was put back."
	}
	i.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceDone,
		Version: version, Committed: committed, Detail: detail}
	a.command(transport.CommandVersion{Generation: a.generation})
}

// autostartInstaller starts the installer for Install update once it has
// found the mixer and a release; gives up quietly at the deadline, leaving
// the page to say what is missing.
func (a *App) autostartInstaller(cx *gk.Cx) {
	if a.installerAutostart == nil {
		return
	}
	deadline := *a.installerAutostart
	i := a.installer
	if i == nil || !a.installerPage {
		a.installerAutostart = nil
		return
	}
	inPlace := a.inPlaceOffer()
	switch {
	case i.model.Run != nil || i.model.InPlace.Active() || i.confirming || !time.Now().Before(deadline):
		a.installerAutostart = nil
	case !i.busy() && inPlace != nil:
		a.installerAutostart = nil
		a.startInPlaceUpdate(cx)
	case !i.busy() && i.model.CanStart():
		a.installerAutostart = nil
		a.startInstaller(cx)
	}
}

// toggleInstallerPage is the header's installer button: bring the installer
// to the front, opening it if needed, or put it away. Put away while idle it
// closes; a run keeps going behind other pages.
func (a *App) toggleInstallerPage(cx *gk.Cx) {
	if a.installerPage {
		a.installerPage = false
		if i := a.installer; i != nil && !i.busy() && i.model.Run == nil {
			a.discardInstaller(cx)
		}
	} else {
		a.setSettingsOpen(false)
		a.openInstaller(cx)
		a.installerPage = a.installer != nil
	}
	a.notify(cx)
}

// installerFlashing reports a run that has switched the mixer to USB update
// mode and not yet finished flashing it: the mixer cannot run RC2FX
// meanwhile.
func (a *App) installerFlashing() bool {
	if a.installer == nil || a.installer.model.Run == nil {
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
		return "Finish or cancel the installer's run first.", true
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
	a.confirm(cx, Confirm{
		Title:  "The installer is working",
		Detail: "Wait for the current step to finish before closing RC2FX. Keep the mixer connected and powered on.",
		Action: "OK",
	}, func(cx *gk.Cx) {}, func(cx *gk.Cx) {})
	return false, true
}

// installerBody is the installer itself: the mixer on the left, what to do
// on the right. Shown on its own screen without a connection, and inside
// Settings with one.
func (a *App) installerBody(i *Installer, cx *gk.Cx) gk.DivEl {
	return gk.VFlex(s.Gap8).
		Child(a.installerContent(i, cx)).
		Child(a.installerActions(i, cx))
}

// inPlaceShown reports that the page is about an in-place update rather
// than a flash.
func (a *App) inPlaceShown(i *Installer) bool {
	return i.model.InPlace.Kind != installflow.InPlaceIdle ||
		(!i.preferFlash && a.inPlaceOffer() != nil)
}

func (a *App) installerContent(i *Installer, cx *gk.Cx) gk.DivEl {
	m := i.model
	// An in-place update needs no flash plan and no administrator access:
	// its footer says everything, and the flash stays one click away.
	if a.inPlaceShown(i) {
		d := gk.VFlex(s.Gap3, s.Pt8, s.WFull)
		if m.Error != "" {
			d = d.Child(paragraph(m.Error, Danger))
		}
		return d
	}
	// Permission is its own step, not a competing card below unusable
	// firmware choices. Once granted this step disappears completely.
	if i.needsAccess() {
		page := gk.VFlex(s.Gap6, s.Pt8).Child(a.accessCard(i,
			"Administrator access lets RC2FX switch the mixer into update mode and install firmware.", cx))
		if m.Run != nil {
			page = page.Child(a.timeline(i))
		}
		return page
	}
	idle := m.Run == nil && !m.Finished
	content := gk.VFlex(s.Gap5, s.WFull)
	if m.Error != "" {
		content = content.Child(paragraph(m.Error, Danger))
	}
	usbError := m.Status != nil && m.Status.USBError != nil
	if usbError {
		content = content.Child(paragraph(fmt.Sprintf("USB check failed: %s", *m.Status.USBError), Danger))
	}
	if len(m.USB()) > 1 {
		content = content.Child(paragraph(
			"More than one mixer is connected. Connect only the mixer you want to update.", Warn))
	}
	if m.Error != "" || usbError || len(m.USB()) > 1 {
		content = content.Child(gk.Div().Child(gk.Button("installer-check").Ghost().Small().
			Label("Check again").Disabled(i.busy() || m.Running()).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.Probe() })
			})))
	}
	content = content.Child(a.actionCard(i, cx))
	if idle {
		if m.DuoConnected() {
			content = content.Child(a.duoOption(i, cx))
		}
	} else if m.Device == firmware.ModelDuo {
		content = content.Child(paragraph(installer.DuoWarning, Warn))
	}
	return content
}

// installerScreen is the installer on its own screen, without a connection.
func (a *App) installerScreen(cx *gk.Cx) gk.Element {
	i := a.installer
	if i == nil {
		return gk.Div()
	}
	if i.repair != nil {
		return a.repairScreen(i, cx)
	}
	return gk.VFlex(s.SizeFull, s.Relative).FontFamily("Inter").Bg(background()).
		Child(backdrop()).
		TextColor(rgb(Text)).
		Child(gk.Div(s.Flex1, s.MinH0, s.Px8, s.Py4).ID("installer-scroll").
			TrackScroll(i.controlsScroll).OverflowYScroll().
			Child(gk.Div(s.WFull, s.MxAuto).MaxW(px(760)).Child(a.installerContent(i, cx)))).
		Child(gk.Div(s.FlexShrink0, s.Px8, s.Pb6).
			Child(gk.Div(s.WFull, s.MxAuto).MaxW(px(760)).Child(a.installerActions(i, cx))))
}

// leadText is the large muted sentence under the action card's title.
func leadText(text string) gk.DivEl {
	return gk.Div().MaxW(px(540)).TextSize(px(LeadText)).LineHeight(gk.Relative(1.55)).
		TextColor(rgb(Muted)).Child(txt(text))
}

// actionCard is what to do: the choice of firmware before a run, the run's
// progress during it, and the result after.
func (a *App) actionCard(i *Installer, cx *gk.Cx) gk.DivEl {
	m := i.model
	idle := m.Run == nil && !m.Finished
	var title, detail string
	switch {
	case m.Finished && m.Stock:
		title, detail = "Stock RØDE firmware is installed",
			"RC2FX is removed. The mixer may restart once more by itself."
	case m.Finished:
		title, detail = "RC2FX is installed", "It starts by itself whenever the mixer turns on."
	case m.WaitingAfterFlash():
		title, detail = "Waiting for your mixer",
			"Keep it powered on and connected while it finishes updating."
	case !idle:
		title = "Installing RC2FX"
		if m.Stock {
			title = "Flashing stock firmware"
		}
	case m.Status == nil:
		title = "Checking your mixer…"
	default:
		switch m.Goal() {
		case installflow.GoalReinstall:
			title, detail = "Update RC2FX", "Any RC2FX settings are saved first and carried over."
		case installflow.GoalSetup:
			title, detail = "Install RC2FX",
				"RC2FX is built into official RØDE firmware and starts with the mixer."
		case installflow.GoalStock:
			title, detail = "Restore stock firmware", "Removes RC2FX. Your settings stay on this computer."
		}
	}
	found := idle && m.Goal() == installflow.GoalSetup && len(m.USB()) == 1
	ready := m.Finished && !m.Stock && m.Run == nil
	card := gk.VFlex(s.Gap4)
	if !a.reducedMotion {
		height := float32(120)
		if idle {
			height = 96
		} else if ready {
			height = 150
		}
		card = card.Child(gk.Div(s.WFull).H(px(height)).ChildView(a.onboardingSignature))
	}
	var heading gk.Element
	switch {
	case found:
		heading = gk.Div(s.Text3xl, s.FontBold).Child(txt("Install RC2FX"))
	case ready:
		heading = headline("Clean signal.", "You're ready.")
	default:
		size := float32(40)
		if idle {
			size = 32
		}
		heading = gk.Div(s.FontBold).TextSize(px(size)).Child(txt(title))
	}
	head := gk.VFlex(s.ItemsCenter, s.TextCenter, s.Gap4).Child(heading)
	if found {
		head = head.Child(leadText(fmt.Sprintf(
			"Noise suppression, EQ and compression, running on your %s. Choose your firmware below.",
			m.Device.Label())))
	}
	if !found && detail != "" {
		head = head.Child(leadText(detail))
	}
	card = card.Child(head)
	if !idle {
		return card.Child(a.timeline(i))
	}
	if note, ok := m.FirmwareNote(); ok {
		card = card.Child(paragraph(note, Warn))
	}
	// Stock firmware comes from RØDE, so it stays on offer when RC2FX's own
	// catalog has nothing.
	if len(m.Releases) > 0 || len(m.StockReleases) > 0 || m.StockCheck.Kind == installflow.StockCheckFailed {
		card = card.Child(a.firmwareCard(i, cx))
	} else if m.FirmwareCheck == installflow.FirmwareCheckChecking {
		card = card.Child(gk.HFlex(s.ItemsCenter, s.JustifyCenter, s.Gap2).
			Child(gk.Spinner().Color(rgb(Accent))).
			Child(paragraph("Checking supported firmware…", Muted)).
			Child(gk.Button("installer-firmware-cancel").Ghost().Small().Label("Cancel").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if a.installer != nil {
						a.installer.runner.stopDownload()
					}
					a.notify(cx)
				})))
	}
	return card
}

// installerRowHeight is every firmware card row, so the tabs line up row for
// row.
const installerRowHeight float32 = 64

// installerSelectWidth is wide enough for a release such as "1.7.7 stable".
const installerSelectWidth float32 = 160

// firmwareCard is what to put on the mixer: RC2FX, its RØDE firmware and
// mixer software, or stock RØDE firmware, which removes RC2FX. Both tabs
// share one layout; the page's main button flashes the one shown.
func (a *App) firmwareCard(i *Installer, cx *gk.Cx) gk.DivEl {
	m := i.model
	onMixer := func(v *string) string {
		if v == nil {
			return "On the mixer: unknown"
		}
		return "On the mixer: " + *v
	}
	busy := m.Busy != nil
	tab := func(id, label string, stock bool) gk.StatefulDivEl {
		return pill(id, label, m.Stock == stock).OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
				if m.Stock != stock {
					m.SetStock(stock)
				}
				return nil
			})
		})
	}
	tabs := pills().
		Child(tab("installer-mode-rc2fx", "RC2FX", false)).
		Child(tab("installer-mode-stock", "Stock firmware", true))
	panel := gk.VFlex(s.WFull, s.RoundedLg, s.Border1).MaxW(px(440)).
		BorderColor(rgb(surface().Line)).Bg(rgb(surface().Raised))
	// Stock has one row and RC2FX two. The panel sits at the top of room
	// kept for two, so switching tabs doesn't move the page.
	if m.Stock {
		row := cardRow(true, "RØDE firmware", onMixer(m.InstalledFirmware), Muted,
			pickerSelect(i.stockSelect, "installer-stock-release", busy || len(m.StockReleases) == 0))
		if status, ok := a.stockStatus(m, cx); ok {
			row = row.Child(status)
		}
		panel = panel.Child(row)
	} else {
		noReleases := true
		for range m.ReleasesFor(false) {
			noReleases = false
			break
		}
		mixerRow := cardRow(false, "Mixer software", onMixer(m.Installed), Muted,
			pickerSelect(i.mixerSelect, "installer-mixer", busy || len(m.MixerMenu()) == 0))
		if status, ok := a.mixerStatus(m, cx); ok {
			mixerRow = mixerRow.Child(status)
		}
		panel = panel.
			Child(cardRow(true, "RØDE firmware", onMixer(m.InstalledFirmware), Muted,
				pickerSelect(i.firmwareSelect, "installer-release", busy || noReleases))).
			Child(mixerRow)
	}
	return gk.VFlex(s.ItemsCenter, s.Gap4, s.WFull).
		Child(tabs).
		Child(gk.Div(s.WFull).MaxW(px(440)).MinH(px(2*installerRowHeight + 3)).Child(panel))
}

func choiceKey(c installflow.Choice) string { return c.Channel + "\x00" + c.Version }

func mixerSourceKey(source installflow.MixerSource) string {
	if source.Kind == installflow.MixerSourceBundled {
		return "bundled\x00" + source.Bundled
	}
	o := source.Offer
	return fmt.Sprintf("signed\x00%s\x00%s\x00%d\x00%s", o.Target, o.SHA256, o.Length, o.Metadata.Version)
}

// syncInstallerSelects brings the firmware card's menus in step with the
// model: their items, what is chosen, and the menus themselves on first
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
	var stock []pickerItem[installflow.Choice]
	for _, r := range m.StockReleases {
		stock = append(stock, release(r))
	}
	var mixers []pickerItem[installflow.MixerSource]
	for _, item := range m.MixerMenu() {
		mixers = append(mixers, pickerItem[installflow.MixerSource]{
			title: item.Name, short: item.Source.Version(), value: item.Source})
	}
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
	if i.stockSelect == nil {
		i.stockSelect = newPicker(a, cx, choiceKey, func(m *installflow.Model, c installflow.Choice) {
			if index := slices.IndexFunc(m.StockReleases, func(r installer.Release) bool {
				return r.Channel == c.Channel && r.Version == c.Version
			}); index >= 0 {
				m.ChooseStock(index)
			}
		})
	}
	i.stockSelect.sync(cx, stock, m.StockChoice)
}

// stockStatus is the check of RØDE's firmware list while it runs, or why it
// has nothing to offer. Nothing once it has answered with releases.
func (a *App) stockStatus(m *installflow.Model, cx *gk.Cx) (gk.DivEl, bool) {
	status := gk.HFlex(s.FlexWrap, s.ItemsCenter, s.Gap2, s.Px4, s.Pb3)
	switch m.StockCheck.Kind {
	case installflow.StockCheckUnchecked, installflow.StockCheckChecking:
		return status.
			Child(gk.Spinner().Color(rgb(Accent))).
			Child(paragraph("Checking RØDE's firmware list…", Muted)), true
	case installflow.StockCheckFailed:
		text, color := fmt.Sprintf("Couldn't read RØDE's firmware list. %s", m.StockCheck.Error), Danger
		if m.StockCheck.Error == installer.Cancelled {
			text, color = "The check of RØDE's firmware list was cancelled.", Warn
		}
		return status.Child(paragraph(text, color)).
			Child(gk.Button("installer-stock-check").Ghost().Small().Label("Check again").
				Disabled(m.Busy != nil).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.RecheckStock() })
				})), true
	}
	if len(m.StockReleases) == 0 {
		return status.Child(paragraph("RØDE lists no firmware for this mixer.", Muted)), true
	}
	return gk.DivEl{}, false
}

// mixerStatus is why there may be no mixer software to choose, and the
// check that finds it while it runs. Nothing once the check has answered
// cleanly.
func (a *App) mixerStatus(m *installflow.Model, cx *gk.Cx) (gk.DivEl, bool) {
	note, hasNote := m.MixerNote()
	if m.Offers.Kind == installflow.OffersReady && !hasNote {
		return gk.DivEl{}, false
	}
	controls := gk.HFlex(s.FlexWrap, s.ItemsCenter, s.Gap2)
	switch m.Offers.Kind {
	case installflow.OffersUnchecked, installflow.OffersChecking:
		controls = controls.
			Child(gk.Spinner().Color(rgb(Accent))).
			Child(paragraph("Checking signed releases…", Muted)).
			Child(gk.Button("installer-mixer-cancel").Ghost().Small().Label("Cancel").
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if a.installer != nil {
						a.installer.runner.stopDownload()
					}
					a.notify(cx)
				}))
	case installflow.OffersFailed:
		controls = controls.Child(gk.Button("installer-mixer-check").Ghost().Small().Label("Check again").
			Disabled(m.Busy != nil).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.RecheckMixers() })
			}))
	}
	noteColor := Warn
	if m.Offers.Kind == installflow.OffersFailed {
		noteColor = Danger
	}
	d := gk.VFlex(s.Gap1, s.Px4, s.Pb3).Child(controls)
	if hasNote {
		d = d.Child(paragraph(note, noteColor))
	}
	return d, true
}

// timeline is the run's steps: finished ones as one line, the current one
// in full, later ones dimmed.
func (a *App) timeline(i *Installer) gk.DivEl {
	m := i.model
	list := gk.VFlex(s.Gap1)
	for _, step := range installflow.Steps {
		title := stepTitle(step)
		if step == installflow.StepStart && m.Stock {
			title = "Wait for the mixer"
		}
		// A finished run did every step.
		state := m.State(step)
		if m.Finished && m.Run == nil {
			state = installflow.StepState{Kind: installflow.StepStateDone}
		}
		// The current operation is the focus, not a second dashboard of
		// completed and future steps. Keep its actual message, errors and
		// measured progress. A finished run retains the final receipt.
		if state.Kind == installflow.StepStatePending ||
			(state.Kind == installflow.StepStateDone &&
				!(m.Finished && m.Run == nil && step == installflow.StepStart)) {
			continue
		}
		iconCell := func(child gk.Element) gk.DivEl {
			return gk.Div(s.FlexShrink0, s.Flex, s.ItemsCenter, s.JustifyCenter).W(px(18)).H(px(20)).Child(child)
		}
		var row gk.DivEl
		if state.Kind == installflow.StepStateDone {
			row = gk.HFlex(s.ItemsStart, s.Gap3, s.Py1, s.TextSm).TextColor(rgb(Muted)).
				Child(iconCell(gk.Icon(gk.IconCheck).TextColor(rgb(Accent)))).
				Child(gk.Div(s.Flex1, s.MinW0).Child(txt(a.doneText(m, step))))
		} else {
			var line string
			var color uint32
			var marker gk.Element
			switch state.Kind {
			case installflow.StepStateFailed:
				line, color, marker = state.Detail, Danger, gk.Icon(gk.IconCircleX).TextColor(rgb(Danger))
			case installflow.StepStateWaiting:
				line, color, marker = state.Detail, Muted, gk.Spinner().Color(rgb(Accent))
			default:
				line = "Working…"
				if i.progress != nil {
					if t := progressText(i.progress); t != "" {
						line = t
					}
				}
				color, marker = Muted, gk.Spinner().Color(rgb(Accent))
			}
			body := gk.VFlex(s.Gap2, s.Flex1, s.MinW0).
				Child(gk.Div(s.TextSm, s.FontSemibold).Child(txt(title))).
				Child(paragraph(line, color))
			if state.Kind == installflow.StepStateRunning && i.progress != nil && i.progress.Total > 0 {
				p := i.progress
				body = body.Child(gk.Progress("installer-progress").
					Value(min(max(float32(p.Completed)/float32(p.Total)*100, 0), 100)))
			}
			row = gk.HFlex(s.ItemsStart, s.Gap3, s.My2, s.Py3).
				Child(iconCell(marker)).
				Child(body)
		}
		list = list.Child(row)
	}
	return list
}

func (a *App) doneText(m *installflow.Model, step installflow.Step) string {
	runOrLast := m.Run
	if runOrLast == nil {
		runOrLast = m.Last
	}
	switch step {
	case installflow.StepMixer:
		if runOrLast != nil && runOrLast.Mixer != nil {
			return fmt.Sprintf("Mixer software %s verified", runOrLast.Mixer.Version())
		}
		return "No mixer software needed"
	case installflow.StepPrepare:
		p := m.Prepared()
		if p == nil {
			return "Firmware ready"
		}
		if p.Stock {
			return fmt.Sprintf("Firmware ready: stock RØDE %s %s", p.Version, p.Channel)
		}
		parts := []string{"RC2FX"}
		if p.Settings != "" {
			parts = append(parts, "your settings")
		}
		if p.Key != "" {
			parts = append(parts, "SSH access")
		}
		with := parts[len(parts)-1]
		if rest := parts[:len(parts)-1]; len(rest) > 0 {
			with = fmt.Sprintf("%s and %s", strings.Join(rest, ", "), with)
		}
		return fmt.Sprintf("Firmware ready: RØDE %s %s with %s", p.Version, p.Channel, with)
	case installflow.StepBackup:
		if runOrLast != nil && runOrLast.BackedUp {
			return "Settings saved"
		}
		return "No RC2FX settings to save"
	case installflow.StepUSBMode:
		return "In USB update mode"
	case installflow.StepFlash:
		return "Firmware sent to the mixer"
	}
	if m.Stock {
		return "Back in normal mode"
	}
	return "RC2FX running"
}

// largeButton is the page's main button style.
func largeButton(b gk.ButtonEl) gk.ButtonEl {
	return b.Large().H(px(LargeButtonHeight)).Px(px(28)).FontSemibold().Rounded(float32(26))
}

// accessCard is administrator access on systems that need it for flashing:
// what it is for, and the one button that opens the system's password
// prompt.
func (a *App) accessCard(i *Installer, purpose string, cx *gk.Cx) gk.DivEl {
	asking := i.access.Kind == installflow.AccessAsking
	detail, color := fmt.Sprintf("%s Your password is handled by the system, not stored by RC2FX.", purpose), Muted
	label := "Grant access"
	switch i.access.Kind {
	case installflow.AccessAsking:
		detail, color = "Answer your system's password prompt to continue.", Muted
		label = "Waiting for access…"
	case installflow.AccessRefused:
		detail, color = fmt.Sprintf("Access wasn't granted. %s", i.access.Why), Warn
		label = "Try again"
	}
	return gk.VFlex(s.Gap4, s.P6, s.Mt4, s.RoundedXl, s.Border1).
		Bg(rgb(surface().Panel)).BorderColor(rgb(surface().Line)).
		Child(gk.Div(s.TextLg, s.FontSemibold).Child(txt("Administrator access"))).
		Child(gk.Div(s.TextSm).LineHeight(gk.Relative(1.5)).TextColor(rgb(color)).Child(txt(detail))).
		Child(gk.HFlex(s.JustifyEnd).Child(gk.Button("installer-grant-access").Primary().Large().
			H(px(LargeButtonHeight)).Px(px(28)).Rounded(float32(26)).
			Label(label).Disabled(asking).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.grantInstallerAccess(cx) })))
}

func (a *App) duoOption(i *Installer, cx *gk.Cx) gk.DivEl {
	return card().Style(s.WFull, s.Gap3).
		Child(gk.HFlex(s.ItemsCenter, s.Gap3).
			Child(gk.Div(s.Flex1, s.TextSm, s.FontSemibold).Child(txt("Allow experimental Duo flashing"))).
			Child(gk.Switch("allow-experimental-duo").
				Checked(i.model.AllowDuo).
				Disabled(i.busy() || i.model.Run != nil || i.confirming).
				OnClick(func(cx *gk.Cx, value bool) {
					if err := rememberDuo(value); err != nil {
						a.message = fmt.Sprintf("Could not save Duo preference: %s", err)
						a.notify(cx)
						return
					}
					a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.SetAllowDuo(value) })
				}))).
		Child(paragraph(installer.DuoWarning, Warn)).
		Child(paragraph("Saved on this computer. Every Duo flash still requires risk confirmation. Switching this off does not uninstall an existing Duo runtime.", Muted))
}

// inPlaceActions is the footer while an in-place update is offered or under
// way, in place of the flash plan's. ok false when the flash plan applies.
func (a *App) inPlaceActions(i *Installer, cx *gk.Cx) (gk.DivEl, bool) {
	m := i.model
	var offer *installer.MixerOffer
	if !i.preferFlash {
		offer = a.inPlaceOffer()
	}
	if m.InPlace.Kind == installflow.InPlaceIdle && offer == nil {
		return gk.DivEl{}, false
	}
	closeButton := func() gk.ButtonEl {
		return gk.Button("in-place-close").Ghost().Label("Close").
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				if a.installer != nil {
					a.installer.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceIdle}
				}
				a.closeInstaller(cx)
			})
	}
	working := func(text string) gk.DivEl {
		return gk.HFlex(s.ItemsCenter, s.Gap3).
			Child(gk.Spinner().Color(rgb(Accent))).
			Child(paragraph(text, Muted))
	}
	var status, buttons gk.DivEl
	switch p := m.InPlace; p.Kind {
	case installflow.InPlaceIdle:
		version := offer.Metadata.Version
		firmware := ""
		if m.InstalledFirmware != nil {
			firmware = *m.InstalledFirmware
		}
		status = gk.VFlex(s.Gap1).
			Child(paragraph(fmt.Sprintf("RC2FX %s can be installed without a flash.", version), Text)).
			Child(paragraph(fmt.Sprintf("RØDE firmware %s stays as it is. The mixer's audio "+
				"stops for a few seconds while RC2FX restarts; your settings "+
				"are kept. To change the RØDE firmware, use the full flash.", firmware), Faint))
		buttons = gk.HFlex(s.FlexWrap, s.JustifyEnd, s.Gap2).
			Child(gk.Button("in-place-flash").Ghost().Label("Full flash instead…").
				Disabled(i.busy()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if a.installer != nil {
						a.installer.preferFlash = true
					}
					a.notify(cx)
				})).
			Child(largeButton(gk.Button("in-place-start").Primary()).
				Label("Update RC2FX").
				Disabled(i.busy()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.startInPlaceUpdate(cx) }))
	case installflow.InPlacePreparing:
		status, buttons = working(fmt.Sprintf("Downloading and checking RC2FX %s…", p.Version)), gk.Div()
	case installflow.InPlaceReady:
		status, buttons = working(fmt.Sprintf("Sending RC2FX %s to the mixer…", p.Bundle.Version)), gk.Div()
	case installflow.InPlaceUploading:
		share := float32(0)
		if p.Total != 0 {
			share = min(max(float32(p.Sent)/float32(p.Total)*100, 0), 100)
		}
		status = gk.VFlex(s.Gap2).
			Child(paragraph(fmt.Sprintf("Sending RC2FX %s to the mixer… %.0f %%", p.Version, share), Muted)).
			Child(gk.Progress("in-place-progress").Value(share)).
			Child(paragraph("Keep the mixer on and connected.", Faint))
		buttons = gk.Div()
	case installflow.InPlaceInstalling:
		status = gk.VFlex(s.Gap1).
			Child(working(fmt.Sprintf("The mixer is checking and installing RC2FX %s…", p.Version))).
			Child(paragraph("Its audio stops for a few seconds while RC2FX restarts. Keep it on "+
				"and connected.", Faint))
		buttons = gk.Div()
	case installflow.InPlaceDone:
		text, color := fmt.Sprintf("RC2FX %s is installed.", p.Version), Text
		if !p.Committed {
			text, color = fmt.Sprintf("RC2FX %s didn't start, so the mixer went back to the previous version.", p.Version), Warn
		}
		status = gk.VFlex(s.Gap1).Child(paragraph(text, color))
		if p.Detail != "" {
			status = status.Child(paragraph(p.Detail, Faint))
		}
		buttons = gk.HFlex(s.JustifyEnd).Child(closeButton())
	default: // InPlaceFailed
		status = paragraph(p.Error, Warn)
		buttons = gk.HFlex(s.FlexWrap, s.JustifyEnd, s.Gap2).Child(closeButton())
		if offer != nil {
			buttons = buttons.Child(largeButton(gk.Button("in-place-retry").Primary()).
				Label("Try again").
				Disabled(i.busy()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if a.installer != nil {
						a.installer.model.InPlace = installflow.InPlace{Kind: installflow.InPlaceIdle}
					}
					a.startInPlaceUpdate(cx)
				}))
		}
	}
	return gk.VFlex(s.Gap3, s.Pt4, s.BorderT1).BorderColor(rgb(surface().Line)).
		Child(status).
		Child(buttons), true
}

// The installer-primary button's actions (mod.rs installer_actions' Action).
const (
	installerActionClose = iota
	installerActionControls
	installerActionRetry
	installerActionStart
)

// installerActions is the action card's footer: leaving or cancelling, and
// the next step.
func (a *App) installerActions(i *Installer, cx *gk.Cx) gk.DivEl {
	if footer, ok := a.inPlaceActions(i, cx); ok {
		return footer
	}
	leave := "Exit installer"
	if a.session.Connected {
		leave = "Close"
	}
	if i.needsAccess() {
		return gk.HFlex(s.JustifyEnd, s.Pt4, s.BorderT1).BorderColor(rgb(surface().Line)).
			Child(gk.Button("close-installer").Ghost().Label(leave).
				Disabled(i.busy() || i.access.Kind == installflow.AccessAsking).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeInstaller(cx) }))
	}
	m := i.model
	busy := i.busy()
	failed := m.Run != nil && m.Run.Failed != nil
	finished := m.Finished && m.Run == nil
	idle := m.Run == nil && !finished
	note := ""
	switch {
	case m.Running():
		note = "Keep the mixer on and connected."
	case idle && !m.Privileged:
		note = "Grant administrator access to continue."
	case idle:
		note = "Takes a few minutes. Audio stops while the mixer updates."
	}
	footer := func(buttons gk.DivEl) gk.DivEl {
		f := gk.VFlex(s.Gap3, s.Pt4, s.BorderT1).BorderColor(rgb(surface().Line))
		if note != "" {
			f = f.Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt(note)))
		}
		return f.Child(buttons)
	}
	bar := gk.HFlex(s.FlexWrap, s.JustifyEnd, s.Gap2)
	if m.Running() {
		if m.CanCancel() || m.Cancelling() {
			label := "Cancel"
			if m.Cancelling() {
				label = "Cancelling…"
			}
			bar = bar.Child(gk.Button("installer-cancel").Label(label).
				Disabled(m.Cancelling()).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
					if i := a.installer; i != nil && i.model.Cancel() {
						i.runner.stopDownload()
					}
					a.notify(cx)
				}))
		}
		return footer(bar)
	}
	switch {
	case failed:
		bar = bar.Child(gk.Button("installer-stop").Ghost().Label("Stop").Disabled(busy).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
					m.Stop()
					return nil
				})
			}))
	case finished && (m.Stock || a.session.Connected):
		bar = bar.Child(gk.Button("installer-again").Ghost().Label("Install again").Disabled(busy).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
				a.installerEvent(cx, func(m *installflow.Model) *installer.Request {
					m.StartOver()
					return nil
				})
			}))
	default:
		// The way into Repair mixer while the mixer is connected; the first
		// screen has it while RC2FX isn't running.
		if idle {
			bar = bar.Child(gk.Button("installer-repair").Ghost().Label("Repair mixer").Disabled(busy).
				OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.showRepair(cx) }))
		}
		label := leave
		if finished {
			label = "Done"
		}
		bar = bar.Child(gk.Button("close-installer").Ghost().Label(label).Disabled(busy).
			OnClick(func(cx *gk.Cx, _ gk.ClickEvent) { a.closeInstaller(cx) }))
	}
	// Inside Settings the mixer controls are already open.
	var label string
	var action int
	switch {
	case finished && (m.Stock || a.session.Connected):
		label, action = "Done", installerActionClose
	case finished:
		label, action = "Open mixer controls", installerActionControls
	case failed:
		label, action = "Retry", installerActionRetry
	default:
		action = installerActionStart
		switch {
		case !m.NeedsConfirmation():
			label = "Continue"
		case m.Goal() == installflow.GoalReinstall:
			label = "Update…"
		case m.Goal() == installflow.GoalSetup:
			label = "Install…"
		default:
			label = "Flash stock firmware…"
		}
	}
	enabled := !busy
	if action == installerActionStart {
		enabled = m.CanStart()
	}
	return footer(bar.Child(largeButton(gk.Button("installer-primary").Primary()).
		Label(label).
		Disabled(!enabled).
		OnClick(func(cx *gk.Cx, _ gk.ClickEvent) {
			switch action {
			case installerActionClose:
				a.closeInstaller(cx)
			case installerActionControls:
				a.openMixerControls(cx)
			default:
				a.startInstaller(cx)
			}
		})))
}

// cardRow is one row of the firmware card: what it is, a line about it, and
// its controls on the right.
func cardRow(first bool, title, detail string, color uint32, controls gk.Element) gk.DivEl {
	row := gk.VFlex()
	if !first {
		row = row.Style(s.BorderT1).BorderColor(rgb(surface().LineStrong))
	}
	return row.Child(gk.HFlex(s.ItemsCenter, s.Gap4, s.Px4).MinH(px(installerRowHeight)).
		Child(gk.VFlex(s.Flex1, s.MinW0).Gap(px(2)).
			Child(gk.Div(s.TextSm, s.FontSemibold).Child(txt(title))).
			Child(gk.Div(s.TextXs).TextColor(rgb(color)).Child(txt(detail)))).
		Child(gk.Div(s.FlexNone).Child(controls)))
}

// pickerItem is one entry of a firmware card menu (mod.rs Choice): its line
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

// pickerSelect is a firmware card menu, or nothing before its first sync. A
// select fills its parent, so it sits in a box of its own width; its menu
// matches it.
func pickerSelect[V any](p *picker[V], id string, disabled bool) gk.Element {
	slot := gk.Div(s.FlexNone).W(px(installerSelectWidth))
	if p != nil {
		slot = slot.Child(gk.Select(p.state).ID(id).Placeholder("None available").Disabled(disabled))
	}
	return slot
}
