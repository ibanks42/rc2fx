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

// Model holds the installer state machine and active run plan.
type Model struct {
	Device     firmware.Model
	AllowDuo   bool
	Privileged bool
	Status     *installer.Status
	service    *installer.Service
	// Cleared whenever RC2FX disconnects so restarts re-read the check.
	imageCheck    imageCheck
	settingsRead  SettingsRead
	Releases      []installer.Release
	NeedsApp      []installer.Release
	FirmwareCheck FirmwareCheck
	Choice        *Choice
	StockReleases []installer.Release
	StockCheck    StockCheck
	StockChoice   *Choice
	Offers        Offers
	// Bundled mixer software is only selected explicitly, never as a fallback
	// when the signed release check fails.
	Bundled           *string
	Mixer             *MixerSource
	mixerChosen       bool
	Installed         *string
	InstalledFirmware *string
	Stock             bool
	Run               *Run
	// In-flight requests, oldest first (runner replies in FIFO order).
	queue []queued
	Error string
	// Last completed run; mutually exclusive with Run.
	finished *Run
	// Unix timestamp of the last finished run's flash, carried across opens so
	// a completed flash isn't mistaken for an in-progress update.
	SettledFlash  *uint64
	lastPoll      time.Time
	inPlace       InPlace
	inPlaceAsked  time.Time
	inPlaceBefore inPlaceBaseline
	FixPolicy     FixPolicy
}

func nowUnix() uint64 {
	now := time.Now().Unix()
	if now < 0 {
		return 0
	}
	return uint64(now)
}

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

func (m *Model) DuoConnected() bool {
	return m.Status != nil && slices.ContainsFunc(m.Status.USB, func(u installer.USBMixer) bool {
		return u.Model == firmware.ModelDuo
	})
}

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

type queued struct {
	background bool
	request    installer.Request
}

// Busy reports whether a foreground (non-background) request is in flight.
func (m *Model) Busy() bool {
	return m.BusyRequest() != nil
}

func (m *Model) BusyRequest() *installer.Request {
	for i := range m.queue {
		if !m.queue[i].background {
			return &m.queue[i].request
		}
	}
	return nil
}

// ShowBusy injects a foreground request for dev-mode flow fixtures.
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

func (m *Model) quiet(request installer.Request) *installer.Request {
	m.queue = append(m.queue, queued{background: true, request: request})
	return &request
}

func (m *Model) polling() bool {
	return slices.ContainsFunc(m.queue, func(q queued) bool { return q.background })
}

func (m *Model) refresh() *installer.Request {
	if m.polling() {
		return nil
	}
	m.lastPoll = time.Now()
	return m.quiet(installer.Request{Kind: installer.RequestProbe})
}

func (m *Model) Probe() *installer.Request {
	m.lastPoll = time.Now()
	m.recheckFirmware()
	return m.send(installer.Request{Kind: installer.RequestProbe})
}

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

func (m *Model) USB() []installer.USBMixer {
	if m.Status == nil {
		return nil
	}
	return m.Status.USB
}

// IsSessionMixer reports whether sessionSerial matches the sole connected
// normal-mode mixer seen by the installer.
func (m *Model) IsSessionMixer(sessionSerial *string) bool {
	one := m.usbOne(false)
	return one != nil && one.Serial != "" && sessionSerial != nil && one.Serial == *sessionSerial
}

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

func (m *Model) InUpdateMode() bool {
	return slices.ContainsFunc(m.USB(), func(u installer.USBMixer) bool { return u.UpdateMode })
}

func (m *Model) Service() *installer.Service {
	return m.service
}

func (m *Model) SetService(service *installer.Service) {
	if service == nil || service.Kind != installer.ServiceReady {
		m.imageCheck = imageCheck{}
	}
	m.service = service
}

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

func (m *Model) ImageCheckResult() *core.ImageCheck {
	if m.imageCheck.kind != imageCheckAnswered {
		return nil
	}
	check := m.imageCheck.result
	return &check
}

func (m *Model) ShowImageCheck(check core.ImageCheck) {
	m.imageCheck = imageCheck{kind: imageCheckAnswered, result: check}
}

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

// ImageCheckRead applies the mixer's GET api/v1/image-check response (check == nil
// means the mixer software predates image checks).
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

func (m *Model) TakeSettingsRequest() bool {
	wanted := m.settingsRead == SettingsReadWanted && m.Answering()
	if wanted {
		m.settingsRead = SettingsReadAsked
	}
	return wanted
}

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

// ConnectionChanged invalidates any in-flight settings read (so it retries on
// reconnect) and fails an active in-place upload whose transport was dropped.
func (m *Model) ConnectionChanged() {
	if m.settingsRead == SettingsReadAsked {
		m.settingsRead = SettingsReadWanted
	}
	if m.inPlace.Kind == InPlaceUploading {
		m.inPlace = InPlace{Kind: InPlaceFailed, Error: inPlaceDisconnected}
	}
}

const inPlaceDisconnected = "The mixer was disconnected before it confirmed the update. " +
	"If the RC2FX version it reports did not change, nothing changed on it."

func (m *Model) Answering() bool {
	return m.Service() != nil
}

// Prepared returns the previously prepared firmware if it is still safe to reuse.
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
		// Never reuse firmware images prepared with an SSH key.
		p.KeySHA == "" &&
		p.Key == "" &&
		p.HostKey == "" &&
		model == m.Device &&
		(m.Device != firmware.ModelDuo || m.AllowDuo) {
		return p
	}
	return nil
}

func (m *Model) recentlyFlashed() bool {
	return m.Status != nil &&
		MayBeUpdating(FlashedRecently(&m.Status.Saved, m.SettledFlash, nowUnix()), m.USB(), m.Answering())
}

// MayBeUpdating reports whether a recently flashed mixer may still be updating.
// Seeing any mixer in normal mode (or RC2FX answering) overrides the time window.
func MayBeUpdating(flashed bool, usb []installer.USBMixer, answering bool) bool {
	if !flashed || answering {
		return false
	}
	return !slices.ContainsFunc(usb, func(u installer.USBMixer) bool { return !u.UpdateMode })
}

// FlashedRecently reports whether the last recorded command was a flash within
// updateWindow seconds of now and newer than settled.
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

func (m *Model) stock() bool {
	if m.Run != nil {
		return m.Run.Stock
	}
	return m.Stock
}

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

func (m *Model) Finished() bool {
	return m.finished != nil
}

func (m *Model) LastRun() *Run {
	if m.Run != nil {
		return m.Run
	}
	return m.finished
}

func (m *Model) ShowFinished(run *Run) {
	m.Run = nil
	m.finished = run
}

func (m *Model) Running() bool {
	return m.Run != nil && m.Run.status.Kind != RunFailed
}

func (m *Model) Failed() bool {
	return m.Run != nil && m.Run.status.Kind == RunFailed
}

func (m *Model) supportedChoice(choice *Choice) bool {
	return choice != nil && slices.ContainsFunc(m.Releases, func(r installer.Release) bool {
		return r.Supported && r.Channel == choice.Channel && r.Version == choice.Version
	})
}

func (m *Model) CanStart() bool {
	return m.Privileged && m.CanStartOnceGranted()
}

// CanStartOnceGranted reports whether a run could start once privileges are granted.
func (m *Model) CanStartOnceGranted() bool {
	if m.Stock {
		return m.canStartStock()
	}
	return m.canFlash() &&
		m.supportedChoice(m.Choice) &&
		m.Mixer != nil &&
		containsSource(m.MixerOptions(), *m.Mixer)
}

func (m *Model) CanStartStock() bool { return m.Privileged && m.canStartStock() }

func (m *Model) canStartStock() bool {
	return m.canFlash() &&
		m.StockChoice != nil &&
		slices.ContainsFunc(m.StockReleases, func(r installer.Release) bool {
			return r.Channel == m.StockChoice.Channel && r.Version == m.StockChoice.Version
		})
}

func (m *Model) canFlash() bool {
	return !m.Busy() &&
		!m.Running() &&
		m.Status != nil && m.Status.USBError == nil && len(m.Status.USB) <= 1 &&
		(m.Device != firmware.ModelDuo || m.AllowDuo)
}

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

// Retry replans a failed run from the mixer's current state, skipping steps
// that are already satisfied.
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
	return m.refresh()
}

func (m *Model) replan() {
	run := m.Run
	run.retry()
	m.settingsRead = SettingsReadIdle
	// Re-run the storage fix if it stopped short of an unsafe flash rather than
	// falling through to a normal flash on the undersized slot.
	if run.Fix != nil && run.Fix.Reply != nil && run.Fix.Reply.Unsafe {
		run.Fix = &RunFix{}
	}
	// Damaged files require reflashing.
	if m.damaged() {
		run.Flashed = false
		m.imageCheck = imageCheck{}
	}
	run.Committed = run.Flashed || m.InUpdateMode()
	run.Index = 0
}

func (m *Model) damaged() bool {
	return m.imageCheck.kind == imageCheckAnswered && m.imageCheck.result.Status == core.ImageStatusDamaged
}

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

func (m *Model) Stop() {
	if !m.Busy() {
		m.Run = nil
		m.settingsRead = SettingsReadIdle
		// Re-check offers on next view in case a failed selection was caused by
		// newly published releases.
		if m.Offers.Kind == OffersReady {
			m.Offers = Offers{Kind: OffersUnchecked}
		}
	}
}

func (m *Model) CanCancel() bool {
	return m.Run != nil && !m.Run.Committed && !m.Run.Flashed && m.Run.status.Kind != RunCanceling
}

func (m *Model) Canceling() bool {
	return m.Run != nil && m.Run.status.Kind == RunCanceling
}

// Cancel aborts a run before it touches the mixer. Returns true if an in-flight
// request must finish before the run clears.
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

func (m *Model) TakingLonger(now time.Time) bool {
	run := m.Run
	return run != nil && run.status.Kind == RunWaiting &&
		now.Sub(run.status.Since) >= TakingLongerAfter
}

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
