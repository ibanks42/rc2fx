package app

// Installer page state and request coordination.
//
// Requests execute serially on a background goroutine that owns the installer
// engine. On Linux without root, USB update mode and flashing delegate to a
// pkexec helper started only when the user initiates or retries a run.

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"time"

	"gpuikit.dev/gk"
	"gpuikit.dev/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
	"github.com/ibanks42/rc2fx/desktop/transport"
)

// Installer holds the active guided-task model, background runner, and page UI state.
type Installer struct {
	model          *installflow.Model
	runner         *installerRunner
	kind           taskKind
	engine         *string
	progress       *installer.Progress
	controlsScroll gk.ScrollHandle
	access         installflow.Access
	firmwareSelect *picker[installflow.Choice]
	mixerSelect    *picker[installflow.MixerSource]
	versionsOpen   bool
	storageFixOpen bool
	// Pending actions to trigger automatically once admin access is granted.
	startOnGrant, retryOnGrant bool
	// Close the task once an in-flight cancelable request finishes.
	closeOnStop bool
	// Stage and wait-line timers for elapsed-time display.
	stage      installflow.Stage
	stageSince time.Time
	waitLine   string
	waitSince  time.Time
	// Non-zero while paused waiting for a manual mixer power cycle.
	pausedAt              time.Duration
	keptWaiting, waitHelp bool
	mic                   int
	shown                 int64
	// User opted for a full flash instead of an available in-place update.
	preferFlash     bool
	imageCheckAsked time.Time
	repair          *installflow.Repair
	fix             *installflow.Fix
	fixOnGrant      bool
	fixStep         int
	// Original RC2FX version before an in-place update, for rollback messaging.
	inPlaceFrom string
	inPlaceKind installflow.InPlaceKind
	// Dev-only synthetic fixture; never sends real requests.
	fixture bool
}

type taskKind int

const (
	taskInstall taskKind = iota
	taskUpdate
	taskRemove
	taskFix
)

func (i *Installer) busy() bool {
	return i.model.Busy() || (i.repair != nil && i.repair.Busy()) || (i.fix != nil && i.fix.Busy())
}

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

func (i *Installer) release(free func(...any)) {
	i.runner.close()
	i.firmwareSelect.release(free)
	i.mixerSelect.release(free)
	free(i.controlsScroll)
}

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

// Maximum time to wait for mixer/release readiness when a task was auto-started
// from a notice or the connection screen.
const installerAutostartWait = 60 * time.Second

const imageCheckEvery = 3 * time.Second

func (a *App) openTask(cx *gk.Cx, kind taskKind) {
	if a.installer != nil {
		return
	}
	// Prevent the real USB installer from acting on hardware during mock-mixer runs.
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
	i.model.FixPolicy = installflow.FixAuto
	i.model.Bundled = opt.Clone(a.bundledMixerSoftware)
	i.model.SettledFlash = a.settledFlash
	if kind == taskRemove {
		i.model.SetStock(true)
	}
	first := i.model.SetAllowDuo(a.allowDuo)
	i.send(first)
	a.installer = i
	a.notify(cx)
}

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

func (a *App) closeTask(cx *gk.Cx, overview bool) {
	if a.installer != nil && a.installer.busy() {
		return
	}
	a.discardInstaller(cx)
	if overview && a.session.Connected {
		a.nav.ShowHome()
	}
	a.notify(cx)
}

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

func (a *App) installerEvent(cx *gk.Cx, event func(m *installflow.Model) *installer.Request) {
	i := a.installer
	if i == nil {
		return
	}
	request := event(i.model)
	i.send(request)
	a.notify(cx)
}

func (a *App) installerSettingsRead(cx *gk.Cx, settings string, err error) {
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.SettingsRead(settings, err) })
}

func (a *App) installerImageCheck(cx *gk.Cx, check *core.ImageCheck, err error) {
	a.installerEvent(cx, func(m *installflow.Model) *installer.Request { return m.ImageCheckRead(check, err) })
}

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

func (a *App) stopTask(cx *gk.Cx) {
	i := a.installer
	if i == nil {
		return
	}
	if f := i.fix; f != nil && f.Busy() {
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

// installerConnectionChanged notifies the model that the transport generation
// changed so any in-flight settings read or in-place upload is invalidated.
func (a *App) installerConnectionChanged() {
	if a.installer != nil {
		a.installer.model.ConnectionChanged()
	}
}

func (a *App) pollInstaller(cx *gk.Cx) bool {
	i := a.installer
	if i == nil || i.fixture {
		return false
	}
	sameMixer := i.model.IsSessionMixer(a.connectedMixer)
	var engine *string
	if a.session.Saved != nil && a.session.Connected && a.streamConnected && sameMixer {
		if state, ok := engineState(a.session.Saved); ok {
			engine = &state
		}
	}
	var mixer, fw *string
	if sameMixer && a.deviceVersions != nil {
		mixer = opt.Clone(a.deviceVersions.Runtime)
		fw = opt.Clone(a.deviceVersions.Firmware)
	}
	// Fall back to USB descriptor firmware version when RC2FX isn't running so
	// downgrades are still caught.
	if fw == nil {
		if v, ok := i.model.USBFirmware(); ok {
			fw = &v
		}
	}
	changed := false
	if !opt.Equal(i.model.Installed, mixer) || !opt.Equal(i.model.InstalledFirmware, fw) {
		i.model.SetInstalled(mixer, fw)
		changed = true
	}
	if !opt.Equal(engine, i.engine) {
		var service *installer.Service
		if engine != nil {
			s := installer.EmbeddedService([]byte(*engine))
			service = &s
		}
		i.model.SetService(service)
		i.engine = engine
		changed = true
	}
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
				a.appUpdate.SetSigned(slices.Clone(offers))
			}
		}
		if installflow.ChangesCatalog(o.request) {
			// Force readiness re-evaluation on the next poll.
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
		if !opt.Equal(progress, i.progress) {
			i.progress = progress
			changed = true
			if progress != nil {
				if i.fix != nil {
					i.fix.Progress(*progress)
				}
				i.model.FixProgress(*progress)
			}
		}
	}
	if i.closeOnStop && i.model.Run == nil && !i.busy() {
		// Keep a standalone fix on screen if it already passed the cancel point.
		if i.fix != nil && i.fix.Kind != installflow.FixBefore {
			i.closeOnStop = false
			return true
		}
		a.closeTask(cx, false)
		return true
	}
	return a.trackStage(i) || a.driveInPlaceUpdate() || changed
}

func (a *App) trackStage(i *Installer) bool {
	now := time.Now()
	changed := false
	if f := i.fix; f != nil {
		if step := f.Step(); step != i.fixStep {
			i.fixStep, i.stageSince = step, now
			changed = true
		}
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
	// Pause the elapsed timer during a manual power-cycle prompt.
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

func (a *App) installMixerUpdate(cx *gk.Cx) {
	a.openTask(cx, taskUpdate)
	if i := a.installer; i != nil {
		i.model.SetStock(false)
		deadline := time.Now().Add(installerAutostartWait)
		a.installerAutostart = &deadline
	}
	a.notify(cx)
}

func (a *App) continueInstall(cx *gk.Cx) {
	a.openTask(cx, taskInstall)
	if a.installer != nil {
		deadline := time.Now().Add(installerAutostartWait)
		a.installerAutostart = &deadline
	}
	a.notify(cx)
}

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

// inPlaceStart marshals the POST api/v1/update request body with deterministic
// field order (version, size, sha256, metadata).
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

func (a *App) driveInPlaceUpdate() bool {
	generation := a.generation
	i := a.installer
	if i == nil {
		return false
	}
	if bundle, ok := i.model.TakeInPlaceBundle(); ok {
		// Snapshot update/last before uploading; transport commands run in order,
		// so this reply is guaranteed to predate the new install.
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

func (a *App) inPlaceOutcome(record json.RawMessage) {
	if i := a.installer; i != nil && i.model.InPlaceOutcome(record) {
		a.command(transport.CommandVersion{Generation: a.generation})
	}
}

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

// installerFlashing reports whether a run has entered USB update mode and has
// not finished flashing yet.
func (a *App) installerFlashing() bool {
	if a.installer == nil || a.installer.fixture || a.installer.model.Run == nil {
		return false
	}
	r := a.installer.model.Run
	return r.Committed && !r.Flashed
}

// installerRestartBlocker returns a reason if an app self-update restart should
// wait for the installer or a recently flashed mixer.
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

// syncInstallerSelects reconciles the version pickers with the model (called
// from poll because gk view functions cannot mutate kit state).
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
	for _, r := range m.ReleasesFor() {
		releases = append(releases, release(r))
	}
	var mixers []pickerItem[installflow.MixerSource]
	for _, item := range m.MixerMenu() {
		mixers = append(mixers, pickerItem[installflow.MixerSource]{
			title: item.Name, short: item.Source.Version(), value: item.Source})
	}
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
	i.firmwareSelect.sync(cx, releases, m.ChoiceFor())
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
