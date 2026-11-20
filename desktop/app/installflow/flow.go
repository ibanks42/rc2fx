package installflow

import (
	"fmt"
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// Model is the installer page's state and the plan it carries out.
type Model struct {
	Device   firmware.Model
	AllowDuo bool
	// Privileged: whether this computer may switch the mixer to update mode
	// and flash it: false on Linux until the user grants administrator
	// access.
	Privileged bool
	Status     *installer.Status
	// service is what RC2FX reports over the app's USB connection; nil while
	// the app is not connected to it.
	service *installer.Service
	// imageCheck is the mixer's check of the files the flash unpacked.
	// Cleared whenever RC2FX is not running, so a restart reads it afresh.
	imageCheck   imageCheck
	settingsRead SettingsRead
	Releases     []installer.Release
	// NeedsApp is qualified firmware only a newer app may install, newest
	// first.
	NeedsApp      []installer.Release
	FirmwareCheck FirmwareCheck
	Choice        *Choice
	// StockReleases is stock firmware RØDE lists for this mixer, newest
	// first.
	StockReleases []installer.Release
	StockCheck    StockCheck
	// StockChoice is the stock release to flash, chosen apart from RC2FX's.
	StockChoice *Choice
	Offers      Offers
	// Bundled is the mixer software built into this app, if any. Chosen only
	// by hand: it never stands in for a signed release that could not be
	// reached.
	Bundled *string
	Mixer   *MixerSource
	// mixerChosen: the user picked Mixer, rather than it being the default.
	mixerChosen bool
	// Installed and InstalledFirmware are the versions this mixer reports
	// over the app's USB connection.
	Installed         *string
	InstalledFirmware *string
	// Stock: flash unchanged RØDE firmware rather than firmware with RC2FX.
	Stock bool
	Run   *Run
	// queue holds the requests in flight, oldest first. The runner answers
	// in order.
	queue []queued
	Error string
	// finished is the last run, once it finished, for showing what it did;
	// nil while a run goes on and after the page starts over. Set only when
	// Run becomes nil, so a finished run and a run in progress never
	// coexist.
	finished *Run
	// SettledFlash is when the flash of the last finished run was sent. The
	// app carries it to the next installer, so a run that finished is not
	// picked up as an update in progress.
	SettledFlash *uint64
	// lastPoll is zero before the first poll.
	lastPoll time.Time
	// inPlace is the in-place update, if any (see InPlace).
	inPlace InPlace
	// inPlaceAsked is when the mixer was last asked how the in-place update
	// ended; zero for never.
	inPlaceAsked time.Time
	// inPlaceBefore is the mixer's update/last record from just before the
	// in-place update in progress was sent.
	inPlaceBefore inPlaceBaseline
	// FixPolicy is whether a run fixes the mixer's storage first.
	FixPolicy FixPolicy
}

func nowUnix() uint64 {
	now := time.Now().Unix()
	if now < 0 {
		return 0
	}
	return uint64(now)
}

// NewModel is an installer page that has not heard from the mixer yet.
func NewModel() *Model {
	return &Model{
		Device:        firmware.ModelProIi,
		AllowDuo:      false,
		Privileged:    true,
		settingsRead:  SettingsReadIdle,
		FirmwareCheck: FirmwareCheckUnchecked,
		StockCheck:    StockCheck{Kind: StockCheckUnchecked},
		Offers:        Offers{Kind: OffersUnchecked},
		inPlace:       InPlace{Kind: InPlaceIdle},
	}
}

func change(installed *string, target string) MixerChange {
	if installed == nil {
		return MixerChangeUnknown
	}
	c, ok := core.CompareVersions(*installed, target)
	switch {
	case !ok:
		return MixerChangeUnknown
	case c < 0:
		return MixerChangeUpgrade
	case c == 0:
		return MixerChangeSame
	default:
		return MixerChangeDowngrade
	}
}

func canceled(err error) bool {
	return err.Error() == installer.Canceled
}

// DuoConnected reports whether a Duo is on USB now: the experimental Duo
// opt-in is only offered for one.
func (m *Model) DuoConnected() bool {
	return m.Status != nil && slices.ContainsFunc(m.Status.USB, func(u installer.USBMixer) bool {
		return u.Model == firmware.ModelDuo
	})
}

// SetAllowDuo sets the experimental Duo opt-in.
func (m *Model) SetAllowDuo(allow bool) *installer.Request {
	if m.Busy() || m.Run != nil {
		return nil
	}
	m.AllowDuo = allow
	m.Choice = nil
	m.forgetStock()
	m.Releases = nil
	m.NeedsApp = nil
	m.FirmwareCheck = FirmwareCheckUnchecked
	m.Error = ""
	m.finished = nil
	return m.send(installer.Request{Kind: installer.RequestAllowExperimentalDuo, Allow: allow})
}

// queued is one request in flight: a background refresh, which never
// disables the page, or the one request the page waits on (see Busy).
type queued struct {
	background bool
	request    installer.Request
}

// Busy reports a request the page waits on: one sent, not yet finished.
func (m *Model) Busy() bool {
	return m.BusyRequest() != nil
}

// BusyRequest is the request the page waits on; nil when there is none.
func (m *Model) BusyRequest() *installer.Request {
	for i := range m.queue {
		if !m.queue[i].background {
			return &m.queue[i].request
		}
	}
	return nil
}

// ShowBusy puts request in flight as the one the page waits on, for pages
// made from made-up state (the app's flow fixtures). Nothing answers it.
func (m *Model) ShowBusy(request installer.Request) {
	m.queue = append(m.queue, queued{request: request})
}

func (m *Model) send(request installer.Request) *installer.Request {
	if m.Busy() {
		return nil
	}
	m.queue = append(m.queue, queued{request: request})
	return &request
}

// quiet is a request the user does not wait on.
func (m *Model) quiet(request installer.Request) *installer.Request {
	m.queue = append(m.queue, queued{background: true, request: request})
	return &request
}

func (m *Model) polling() bool {
	return slices.ContainsFunc(m.queue, func(q queued) bool { return q.background })
}

// refresh what the mixer reports, in the background.
func (m *Model) refresh() *installer.Request {
	if m.polling() {
		return nil
	}
	m.lastPoll = time.Now()
	return m.quiet(installer.Request{Kind: installer.RequestProbe})
}

// Probe asks what the mixer reports, and waits for it.
func (m *Model) Probe() *installer.Request {
	m.lastPoll = time.Now()
	m.recheckFirmware()
	return m.send(installer.Request{Kind: installer.RequestProbe})
}

// recheckFirmware: the next status looks for firmware again if the last
// check found none.
func (m *Model) recheckFirmware() {
	if len(m.Releases) == 0 && m.FirmwareCheck == FirmwareCheckDone {
		m.FirmwareCheck = FirmwareCheckUnchecked
	}
}

func (m *Model) checkFirmware() *installer.Request {
	if m.FirmwareCheck != FirmwareCheckUnchecked {
		return nil
	}
	m.FirmwareCheck = FirmwareCheckChecking
	return m.quiet(installer.Request{Kind: installer.RequestReleases})
}

// ---- What the mixer reports ------------------------------------------------

// USB is the mixers the installer sees on USB.
func (m *Model) USB() []installer.USBMixer {
	if m.Status == nil {
		return nil
	}
	return m.Status.USB
}

// IsSessionMixer reports whether the app's USB 1 session is with the mixer
// the installer sees: the only one connected, in normal mode, with the same
// serial. RC2FX's readiness from any other mixer must not count for this
// one.
func (m *Model) IsSessionMixer(sessionSerial *string) bool {
	one := m.usbOne(false)
	return one != nil && one.Serial != "" && sessionSerial != nil && one.Serial == *sessionSerial
}

// USBFirmware is the RØDE firmware the one connected mixer reports in its
// USB device information: known even when RC2FX isn't running on it.
func (m *Model) USBFirmware() (string, bool) {
	one := m.usbOne(false)
	if one == nil || one.Firmware == nil {
		return "", false
	}
	return *one.Firmware, true
}

func (m *Model) usbOne(updateMode bool) *installer.USBMixer {
	usb := m.USB()
	if len(usb) == 1 && usb[0].UpdateMode == updateMode {
		return &usb[0]
	}
	return nil
}

// InUpdateMode reports a mixer in USB update mode.
func (m *Model) InUpdateMode() bool {
	return slices.ContainsFunc(m.USB(), func(u installer.USBMixer) bool { return u.UpdateMode })
}

// Service is what the app's USB connection says about RC2FX; nil while it is
// not connected.
func (m *Model) Service() *installer.Service {
	return m.service
}

// SetService sets what the app's USB connection says about RC2FX; nil while
// it is not connected.
func (m *Model) SetService(service *installer.Service) {
	if service == nil || service.Kind != installer.ServiceReady {
		m.imageCheck = imageCheck{}
	}
	m.service = service
}

// WantsImageCheck reports whether the run is waiting on the mixer's image
// check, so the app should ask for it.
func (m *Model) WantsImageCheck() bool {
	run := m.Run
	atStart := run != nil && run.status.Kind != RunFailed && run.Index >= 0 && run.Index < len(Steps) && Steps[run.Index] == StepStart
	service := m.Service()
	return atStart &&
		!run.Stock &&
		m.flashed() &&
		service != nil && service.Kind == installer.ServiceReady &&
		!m.imageChecked()
}

// ImageCheckResult is the mixer's answer to the image check, once it gave
// one; nil before that and for mixer software that predates the check.
func (m *Model) ImageCheckResult() *core.ImageCheck {
	if m.imageCheck.kind != imageCheckAnswered {
		return nil
	}
	check := m.imageCheck.result
	return &check
}

// ShowImageCheck sets the mixer's answer to the image check, for pages made
// from made-up state (the app's flow fixtures).
func (m *Model) ShowImageCheck(check core.ImageCheck) {
	m.imageCheck = imageCheck{kind: imageCheckAnswered, result: check}
}

// imageChecked: the check passed, or there is nothing to check.
func (m *Model) imageChecked() bool {
	switch m.imageCheck.kind {
	case imageCheckUnasked:
		return false
	case imageCheckUnsupported:
		return true
	}
	status := m.imageCheck.result.Status
	return status == core.ImageStatusOk || status == core.ImageStatusNone
}

// ImageCheckRead hands over the mixer's answer to `GET api/v1/image-check`
// (check nil: mixer software that predates it). A failed read is asked
// again on the next poll.
func (m *Model) ImageCheckRead(check *core.ImageCheck, readErr error) *installer.Request {
	if readErr != nil {
		return nil
	}
	if !m.WantsImageCheck() {
		return nil
	}
	if check == nil {
		m.imageCheck = imageCheck{kind: imageCheckUnsupported}
	} else {
		m.imageCheck = imageCheck{kind: imageCheckAnswered, result: *check}
	}
	return m.advance()
}

// TakeSettingsRequest reports that the app should read RC2FX's settings now;
// true once per request.
func (m *Model) TakeSettingsRequest() bool {
	// Only RC2FX can answer; while it does not, the run decides (advance).
	wanted := m.settingsRead == SettingsReadWanted && m.Answering()
	if wanted {
		m.settingsRead = SettingsReadAsked
	}
	return wanted
}

// SettingsRead hands over RC2FX's settings as the app read them, or why it
// could not: keep them, or stop the run.
func (m *Model) SettingsRead(settings string, readErr error) *installer.Request {
	if m.settingsRead != SettingsReadAsked {
		return nil
	}
	m.settingsRead = SettingsReadIdle
	run := m.Run
	if run == nil {
		return nil
	}
	run.resume()
	serial := run.Serial
	if readErr != nil {
		return m.fail(fmt.Sprintf("Couldn't read RC2FX's settings: %s", readErr.Error()))
	}
	return m.send(installer.Request{Kind: installer.RequestSaveSettings, Serial: serial, Settings: settings})
}

// ConnectionChanged reports that the app's USB connection to RC2FX was
// dropped or replaced. A settings read in flight on the old connection is
// never answered, so it is asked again once RC2FX answers; if RC2FX does
// not come back, the run fails (advance) rather than going on unsaved. An
// in-place update still uploading ends with the connection (the transport
// cancels it), and nothing reports that, so it fails here.
func (m *Model) ConnectionChanged() {
	if m.settingsRead == SettingsReadAsked {
		m.settingsRead = SettingsReadWanted
	}
	if m.inPlace.Kind == InPlaceUploading {
		m.inPlace = InPlace{Kind: InPlaceFailed, Error: inPlaceDisconnected}
	}
}

// inPlaceDisconnected is why an in-place update failed when the connection
// ended mid-upload. The mixer may already have accepted it, just before.
const inPlaceDisconnected = "The mixer was disconnected before it confirmed the update. " +
	"If the RC2FX version it reports did not change, nothing changed on it."

// Answering reports that RC2FX answers over the app's USB connection.
func (m *Model) Answering() bool {
	// Ready and Starting are the only kinds of service, so any answer counts.
	return m.Service() != nil
}

// Prepared is the firmware prepared for this mixer, if it may be reused.
func (m *Model) Prepared() *installer.Prepared {
	s := m.Status
	if s == nil || s.Saved.Prepared == nil {
		return nil
	}
	p := s.Saved.Prepared
	model := firmware.ModelProIi
	if p.Model != nil {
		model = *p.Model
	}
	if s.PreparedPresent &&
		// Firmware prepared with an SSH key is never reused.
		p.KeySHA == "" &&
		p.Key == "" &&
		p.HostKey == "" &&
		model == m.Device &&
		(m.Device != firmware.ModelDuo || m.AllowDuo) {
		return p
	}
	return nil
}

// recentlyFlashed: the last USB command was a flash in the last few
// minutes and the app sees nothing that says the update is over, so the
// mixer may still be updating (MayBeUpdating).
func (m *Model) recentlyFlashed() bool {
	return m.Status != nil &&
		MayBeUpdating(FlashedRecently(&m.Status.Saved, m.SettledFlash, nowUnix()), m.USB(), m.Answering())
}

// MayBeUpdating reports whether a mixer may still be updating, given that a
// flash was sent recently (FlashedRecently), the mixers on USB, and whether
// RC2FX answers. What the mixer shows wins over the window: one in normal
// mode, whether it answers as RØDE's firmware or as RC2FX, is not updating.
// The window counts only while the app sees no mixer, or one in update
// mode.
func MayBeUpdating(flashed bool, usb []installer.USBMixer, answering bool) bool {
	if !flashed || answering {
		return false
	}
	return !slices.ContainsFunc(usb, func(u installer.USBMixer) bool { return !u.UpdateMode })
}

// FlashedRecently reports that the last USB command in saved was a flash
// sent in the last few minutes, now being Unix seconds, and newer than the
// flash this app saw finish (settled, nil for none): the mixer may still be
// updating.
func FlashedRecently(saved *installer.Saved, settled *uint64, now uint64) bool {
	if saved == nil {
		return false
	}
	c := saved.LastCommand()
	return c != nil &&
		c.Command == "flash" &&
		num.SatSub(now, c.AtUnix) < updateWindow &&
		(settled == nil || c.AtUnix > *settled)
}

func (m *Model) flashed() bool {
	return m.Run != nil && m.Run.Flashed
}

// stock: the run, or else the page, is for unchanged RØDE firmware.
func (m *Model) stock() bool {
	if m.Run != nil {
		return m.Run.Stock
	}
	return m.Stock
}

// Goal is what starting achieves.
func (m *Model) Goal() Goal {
	switch {
	case m.stock():
		return GoalStock
	case m.Answering() && !m.flashed():
		return GoalReinstall
	default:
		return GoalSetup
	}
}

func (m *Model) satisfied(step Step) bool {
	run := m.Run
	flashed := m.flashed()
	switch step {
	case StepBackup:
		switch {
		case flashed:
			return true
		case run == nil:
			// Before a run: the plan shows whether there is anything to save.
			return !m.Answering()
		default:
			return run.BackedUp || !run.NeedsBackup
		}
	case StepMixer:
		// Stock firmware carries no mixer software.
		return flashed || m.stock() || (run != nil && run.Selected)
	case StepPrepare:
		return flashed || (run != nil && run.Prepared)
	case StepUSBMode:
		return flashed || m.InUpdateMode()
	case StepFlash:
		return flashed
	default:
		if m.stock() {
			// Stock firmware is done once the mixer is back in normal mode.
			return flashed && m.usbOne(false) != nil
		}
		service := m.Service()
		ready := service != nil && service.Kind == installer.ServiceReady
		return flashed && !m.InUpdateMode() && ready && m.imageChecked()
	}
}

// State is how far step is.
func (m *Model) State(step Step) StepState {
	if run := m.Run; run != nil {
		position := slices.Index(Steps[:], step)
		if position >= 0 && position == run.Index {
			switch status := run.status; status.Kind {
			case RunFailed:
				return StepState{Kind: StepStateFailed, Detail: status.Why}
			case RunWaiting:
				return StepState{Kind: StepStateWaiting, Detail: status.Why}
			}
			// Between an action and the refresh that follows it.
			if m.Busy() || m.polling() {
				return StepState{Kind: StepStateRunning}
			}
		}
		if position >= 0 && position < run.Index {
			return StepState{Kind: StepStateDone}
		}
	}
	if m.satisfied(step) {
		return StepState{Kind: StepStateDone}
	}
	return StepState{Kind: StepStatePending}
}

// Finished reports that the last run finished and nothing has started
// since: the page shows what it did.
func (m *Model) Finished() bool {
	return m.finished != nil
}

// LastRun is the run in progress, or else the one that finished; nil for
// neither.
func (m *Model) LastRun() *Run {
	if m.Run != nil {
		return m.Run
	}
	return m.finished
}

// ShowFinished makes run the finished run, with none in progress, for pages
// made from made-up state (the app's flow fixtures).
func (m *Model) ShowFinished(run *Run) {
	m.Run = nil
	m.finished = run
}

// Running reports a run that has not failed.
func (m *Model) Running() bool {
	return m.Run != nil && m.Run.status.Kind != RunFailed
}

// Failed reports a run that stopped at a failed step: it can be retried
// from there, or stopped.
func (m *Model) Failed() bool {
	return m.Run != nil && m.Run.status.Kind == RunFailed
}

func (m *Model) supportedChoice(choice *Choice) bool {
	return choice != nil && slices.ContainsFunc(m.Releases, func(r installer.Release) bool {
		return r.Supported && r.Channel == choice.Channel && r.Version == choice.Version
	})
}

// CanStart reports whether a run can start now.
func (m *Model) CanStart() bool {
	return m.Privileged && m.CanStartOnceGranted()
}

// CanStartOnceGranted reports whether a run could start now if it had
// administrator access: the start button asks for access first where it
// is missing, then starts.
func (m *Model) CanStartOnceGranted() bool {
	if m.Stock {
		return m.canStartStock()
	}
	return m.canFlash() &&
		m.supportedChoice(m.Choice) &&
		m.Mixer != nil &&
		containsSource(m.MixerOptions(), *m.Mixer)
}

// CanStartStock reports whether stock firmware could be flashed now. It
// needs no mixer software or RC2FX support, so it stays available when
// neither can be offered.
func (m *Model) CanStartStock() bool { return m.Privileged && m.canStartStock() }

func (m *Model) canStartStock() bool {
	return m.canFlash() &&
		m.StockChoice != nil &&
		slices.ContainsFunc(m.StockReleases, func(r installer.Release) bool {
			return r.Channel == m.StockChoice.Channel && r.Version == m.StockChoice.Version
		})
}

// canFlash is what any flash needs besides access: an idle page and at
// most one mixer.
func (m *Model) canFlash() bool {
	return !m.Busy() &&
		!m.Running() &&
		m.Status != nil && m.Status.USBError == nil && len(m.Status.USB) <= 1 &&
		(m.Device != firmware.ModelDuo || m.AllowDuo)
}

// ---- Events -----------------------------------------------------------------

// Start starts a run.
func (m *Model) Start() *installer.Request {
	if !m.CanStart() {
		return nil
	}
	m.Error = ""
	m.finished = nil
	m.settingsRead = SettingsReadIdle
	serial := ""
	if usb := m.USB(); len(usb) > 0 {
		serial = usb[0].Serial
	}
	var mixer *MixerSource
	if m.Mixer != nil && !m.Stock {
		source := *m.Mixer
		mixer = &source
	}
	// CanStart checked that there is a choice for the mode.
	choice := *m.Chosen()
	m.Run = &Run{
		Started:     time.Now(),
		Device:      m.Device,
		Serial:      serial,
		Mixer:       mixer,
		Stock:       m.Stock,
		Choice:      &choice,
		NeedsBackup: m.Answering(),
	}
	if m.WantsStorageFix() {
		m.Run.Fix = &RunFix{}
	}
	return m.advance()
}

// Retry starts a failed run again from the step the mixer's state calls for
// now, not from the step that failed: following a failure's advice (a power
// cycle, say) changes what is left to do. Steps already done (settings
// saved, firmware ready) are not repeated.
func (m *Model) Retry() *installer.Request {
	if !m.Privileged {
		return nil
	}
	run := m.Run
	if run == nil || run.status.Kind != RunFailed || m.Busy() {
		return nil
	}
	m.replan()
	m.recheckFirmware()
	// The status this asks for carries the run on from the first step the
	// mixer's state leaves undone.
	return m.refresh()
}

// replan clears a failure and goes back to the first step, which advance
// skips through to the first one the mixer's state leaves undone.
func (m *Model) replan() {
	run := m.Run
	run.retry()
	m.settingsRead = SettingsReadIdle
	// A fix that stopped short of an unsafe flash is run again, never
	// replaced by a normal flash onto that slot.
	if run.Fix != nil && run.Fix.Reply != nil && run.Fix.Reply.Unsafe {
		run.Fix = &RunFix{}
	}
	// Damaged files have no copy to restore them from on the mixer: only
	// sending the firmware again fixes them.
	if m.damaged() {
		run.Flashed = false
		m.imageCheck = imageCheck{}
	}
	// A mixer back in normal mode before the flash is as it was, so the run
	// can be stopped again.
	run.Committed = run.Flashed || m.InUpdateMode()
	run.Index = 0
}

func (m *Model) damaged() bool {
	return m.imageCheck.kind == imageCheckAnswered && m.imageCheck.result.Status == core.ImageStatusDamaged
}

// RetryFrom is the step Retry would start from, judged from what the mixer
// reports now; ok false without a failed run.
func (m *Model) RetryFrom() (step Step, ok bool) {
	if !m.Failed() {
		return 0, false
	}
	plan := *m
	run := *m.Run
	plan.Run = &run
	plan.replan()
	for _, step := range Steps {
		if !plan.satisfied(step) {
			return step, true
		}
	}
	return StepStart, true
}

// Stop ends the run.
func (m *Model) Stop() {
	if !m.Busy() {
		m.Run = nil
		m.settingsRead = SettingsReadIdle
		// A failed selection may mean the offers changed: look again, so
		// the user reviews what is published now.
		if m.Offers.Kind == OffersReady {
			m.Offers = Offers{Kind: OffersUnchecked}
		}
	}
}

// CanCancel reports that the run has not touched the mixer's mode or
// firmware yet.
func (m *Model) CanCancel() bool {
	return m.Run != nil && !m.Run.Committed && !m.Run.Flashed && m.Run.status.Kind != RunCanceling
}

// Canceling reports a run canceled while its request is in flight.
func (m *Model) Canceling() bool {
	return m.Run != nil && m.Run.status.Kind == RunCanceling
}

// Cancel cancels before the point of no return. A request in flight (saving
// settings or preparing firmware) is asked to stop and the run ends when it
// returns. Returns whether a request is in flight.
func (m *Model) Cancel() bool {
	if !m.CanCancel() {
		return false
	}
	if m.Busy() {
		m.Run.cancel()
		return true
	}
	m.Run = nil
	m.settingsRead = SettingsReadIdle
	return false
}

func (m *Model) fail(message string) *installer.Request {
	m.settingsRead = SettingsReadIdle
	if m.Run != nil {
		m.Run.fail(message)
	}
	return nil
}

func (m *Model) wait(why string) *installer.Request { return m.waitFor(why, false) }

// waitFor waits for the mixer, failing after waitLimit. A wait for a power
// cycle is timed from when it was asked, not from the step's start.
func (m *Model) waitFor(why string, powerCycle bool) *installer.Request {
	run := m.Run
	if run == nil {
		return nil
	}
	now := time.Now()
	if since := run.wait(why, powerCycle, now); now.Sub(since) > waitLimit {
		return m.fail(fmt.Sprintf("%s The mixer didn't get there in time. Check it, then press “Try again”.", why))
	}
	return nil
}

// TakingLonger reports a wait the mixer hasn't ended in TakingLongerAfter:
// the page then says so and lets the run be stopped. Every wait gets this
// way out, whether or not RC2FX answers meanwhile.
func (m *Model) TakingLonger(now time.Time) bool {
	run := m.Run
	return run != nil && run.status.Kind == RunWaiting &&
		now.Sub(run.status.Since) >= TakingLongerAfter
}

// CanStop reports that the run can be stopped now: before it touches the
// mixer, after it failed, or once a wait is taking longer than usual.
func (m *Model) CanStop(now time.Time) bool {
	switch run := m.Run; {
	case run == nil:
		return false
	case run.status.Kind == RunFailed:
		return !m.Busy()
	case m.CanCancel():
		return true
	}
	return !m.Busy() && m.TakingLonger(now)
}
