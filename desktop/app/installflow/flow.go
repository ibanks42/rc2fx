package installflow

import (
	"fmt"
	"iter"
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
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

// Choose picks the firmware release at index of Releases. A run, even a
// failed one, keeps the release it started with.
func (m *Model) Choose(index int) {
	if m.Run != nil || m.Busy() {
		return
	}
	// Both modes are restricted to this build's qualified catalog.
	if index >= 0 && index < len(m.Releases) && m.Releases[index].Supported {
		r := m.Releases[index]
		m.Choice = &Choice{Channel: r.Channel, Version: r.Version}
	}
	m.settleMixer()
}

// ChooseStock picks the stock release at index of StockReleases.
func (m *Model) ChooseStock(index int) {
	if m.Run != nil || m.Busy() {
		return
	}
	if index >= 0 && index < len(m.StockReleases) {
		r := m.StockReleases[index]
		m.StockChoice = &Choice{Channel: r.Channel, Version: r.Version}
	}
}

// Chosen is the release a run flashes: RC2FX's choice, or the stock one.
func (m *Model) Chosen() *Choice {
	if m.Stock {
		return m.StockChoice
	}
	return m.Choice
}

// ---- Stock firmware -----------------------------------------------------------

func (m *Model) checkStock() *installer.Request {
	if m.StockCheck.Kind != StockCheckUnchecked {
		return nil
	}
	m.StockCheck = StockCheck{Kind: StockCheckChecking}
	return m.quiet(installer.Request{Kind: installer.RequestStockReleases})
}

// RecheckStock checks RØDE's list again after a failed or canceled check.
func (m *Model) RecheckStock() *installer.Request {
	if m.StockCheck.Kind != StockCheckFailed || m.Busy() {
		return nil
	}
	m.StockCheck = StockCheck{Kind: StockCheckUnchecked}
	return m.checkStock()
}

// forgetStock: the mixer changed: its stock firmware is another list.
func (m *Model) forgetStock() {
	m.StockReleases = nil
	m.StockChoice = nil
	m.StockCheck = StockCheck{Kind: StockCheckUnchecked}
}

// ---- Mixer software -----------------------------------------------------------

// firmwareProfile is the catalog profile of the firmware release choice.
func (m *Model) firmwareProfile(choice *Choice) (string, bool) {
	if choice == nil {
		return "", false
	}
	for _, r := range m.Releases {
		if r.Channel == choice.Channel && r.Version == choice.Version {
			return r.Profile, r.Profile != ""
		}
	}
	return "", false
}

// MixerOptions is what can go with the chosen firmware: signed offers for
// this app, newest first, then the copy built into this app.
func (m *Model) MixerOptions() []MixerSource {
	var options []MixerSource
	if profile, ok := m.firmwareProfile(m.Choice); m.Offers.Kind == OffersReady && ok {
		for _, offer := range m.Offers.List {
			if model.OfferFits(offer, profile) {
				options = append(options, Signed(offer))
			}
		}
	}
	if m.Bundled != nil {
		options = append(options, Bundled(*m.Bundled))
	}
	return options
}

// MixerMenu is the menu: each option, its name, and whether it is the
// choice.
func (m *Model) MixerMenu() []MixerMenuItem {
	var menu []MixerMenuItem
	for _, source := range m.MixerOptions() {
		name := source.Version()
		if source.Kind == MixerSourceBundled {
			name = fmt.Sprintf("%s · included with this app", source.Version())
		}
		if m.MixerChange(source) == MixerChangeDowngrade {
			name += " · older than installed"
		}
		chosen := m.Mixer != nil && m.Mixer.Equal(source)
		menu = append(menu, MixerMenuItem{Source: source, Name: name, Chosen: chosen})
	}
	return menu
}

// MixerChange is how source relates to what the mixer runs.
func (m *Model) MixerChange(source MixerSource) MixerChange {
	return change(m.Installed, source.Version())
}

// settleMixer keeps a choice that still applies. Otherwise use the newest
// signed offer that doesn't go backwards, or nothing, which leaves the choice
// to the user: a downgrade or the bundled copy is never picked for them.
func (m *Model) settleMixer() {
	if m.Run != nil || m.Offers.Kind == OffersUnchecked || m.Offers.Kind == OffersChecking {
		return
	}
	options := m.MixerOptions()
	if m.mixerChosen && m.Mixer != nil && containsSource(options, *m.Mixer) {
		return
	}
	m.mixerChosen = false
	m.Mixer = nil
	for _, source := range options {
		if source.Kind == MixerSourceSigned && m.MixerChange(source) != MixerChangeDowngrade {
			m.Mixer = &source
			break
		}
	}
}

// InPlaceOffer is the signed release an in-place update would install, when
// one can: the mixer runs RC2FX that installs in place, the release is newer
// than what it runs and supports the RØDE firmware it already has. Firmware
// changes always need a flash, so this never changes firmware.
func (m *Model) InPlaceOffer(support InPlaceSupport) *installer.MixerOffer {
	if support.Profile == nil {
		return nil
	}
	if !support.Supported || m.Stock || m.Run != nil || !m.Answering() {
		return nil
	}
	if m.Mixer == nil || m.Mixer.Kind != MixerSourceSigned {
		return nil
	}
	offer := &m.Mixer.Offer
	if m.MixerChange(Signed(*offer)) == MixerChangeUpgrade &&
		slices.Contains(offer.Metadata.FirmwareProfiles, *support.Profile) {
		return offer
	}
	return nil
}

// StartInPlace starts an in-place update with offer: download and verify it
// here, then InPlaceReady hands it to the app to send.
func (m *Model) StartInPlace(offer installer.MixerOffer, profile string) *installer.Request {
	if m.Busy() || m.inPlace.Active() || m.Run != nil {
		return nil
	}
	m.inPlace = InPlace{Kind: InPlacePreparing, Version: offer.Metadata.Version}
	var installed *string
	if m.Installed != nil {
		v := *m.Installed
		installed = &v
	}
	return m.send(installer.Request{
		Kind:            installer.RequestInPlaceBundle,
		Offer:           &offer,
		FirmwareProfile: profile,
		Installed:       installed,
	})
}

// TakeInPlaceBundle is the verified release, once, for the app to send to
// the mixer. The app first asks for the mixer's update/last record, which
// InPlaceOutcome keeps as the one from before this update, then sends the
// release, reporting a failure to send with InPlaceSendFailed and what the
// mixer says with InPlaceProgress, InPlaceAccepted and InPlaceRefused.
func (m *Model) TakeInPlaceBundle() (installer.InPlaceBundle, bool) {
	// Asked on every poll: anything but Ready stays exactly as it is.
	if m.inPlace.Kind != InPlaceReady {
		return installer.InPlaceBundle{}, false
	}
	bundle := m.inPlace.Bundle
	m.inPlaceBefore = inPlaceBaseline{}
	m.inPlace = InPlace{
		Kind:    InPlaceUploading,
		Version: bundle.Version,
		Sent:    0,
		Total:   bundle.Size,
	}
	return bundle, true
}

// ChooseMixer picks the mixer software to install.
func (m *Model) ChooseMixer(source MixerSource) {
	if m.Run == nil && !m.Busy() && containsSource(m.MixerOptions(), source) {
		m.Mixer = &source
		m.mixerChosen = true
	}
}

// SetInstalled sets the versions this mixer reports; nil for what it did not
// report.
func (m *Model) SetInstalled(mixer, firmware *string) {
	if !opt.Equal(m.Installed, mixer) || !opt.Equal(m.InstalledFirmware, firmware) {
		m.Installed = mixer
		m.InstalledFirmware = firmware
		m.settleMixer()
	}
}

func (m *Model) checkMixers() *installer.Request {
	if m.Offers.Kind != OffersUnchecked {
		return nil
	}
	m.Offers = Offers{Kind: OffersChecking}
	return m.quiet(installer.Request{Kind: installer.RequestMixerReleases})
}

// RecheckMixers checks again after a failed or canceled check.
func (m *Model) RecheckMixers() *installer.Request {
	if m.Offers.Kind != OffersFailed {
		return nil
	}
	m.Offers = Offers{Kind: OffersUnchecked}
	return m.checkMixers()
}

// FirmwareNote is firmware that is qualified but held back until the app is
// updated.
func (m *Model) FirmwareNote() (string, bool) {
	if len(m.NeedsApp) == 0 {
		return "", false
	}
	release := m.NeedsApp[0]
	return fmt.Sprintf("RØDE %s needs RC2FX %s or newer. Update the app first.",
		release.Version, release.MinimumAppVersion), true
}

// MixerNote is why the page has no mixer software to offer, or what the user
// should know about the choices it has.
func (m *Model) MixerNote() (string, bool) {
	fallback := ""
	if m.Bundled != nil {
		fallback = fmt.Sprintf(" You can choose %s, included with this app, instead.", *m.Bundled)
	}
	switch m.Offers.Kind {
	case OffersFailed:
		if m.Offers.Error == installer.Canceled {
			return "The check for mixer software was canceled." + fallback, true
		}
		return fmt.Sprintf("Couldn't check for mixer software. %s%s", m.Offers.Error, fallback), true
	case OffersReady:
		offers := m.Offers.List
		options := m.MixerOptions()
		if len(options) == 0 {
			version := ""
			if m.Choice != nil {
				version = m.Choice.Version
			}
			if need, ok := model.NeedsNewerApp(offers, nil); ok {
				return fmt.Sprintf("Mixer software %s needs RC2FX %s or newer. Update the app first.",
					need.Version, need.MinimumApp), true
			}
			return fmt.Sprintf("No signed mixer software works with RØDE %s and this app.", version), true
		}
		if m.Mixer == nil {
			if m.Installed != nil {
				return fmt.Sprintf("Nothing newer than this mixer's %s is available. Choosing a version below replaces it with an older one.",
					*m.Installed), true
			}
			return "Choose the mixer software to install.", true
		}
		// The newest option; with incomparable versions counted equal, the
		// later of equal versions wins.
		var newest *string
		for _, source := range options {
			version := source.Version()
			if newest == nil {
				newest = &version
				continue
			}
			if c, ok := core.CompareVersions(*newest, version); !ok || c <= 0 {
				newest = &version
			}
		}
		if need, ok := model.NeedsNewerApp(offers, newest); ok {
			return fmt.Sprintf("Mixer software %s needs RC2FX %s or newer. Update the app first to get it.",
				need.Version, need.MinimumApp), true
		}
		return "", false
	default:
		return "", false
	}
}

// WarningsFor is what the confirmation must say before this replaces
// something newer, or something the mixer didn't report.
func (m *Model) WarningsFor(stock bool) []string {
	var warnings []string
	if m.Mixer != nil && !stock {
		version := m.Mixer.Version()
		switch m.MixerChange(*m.Mixer) {
		case MixerChangeDowngrade:
			if m.Installed != nil {
				warnings = append(warnings, fmt.Sprintf(
					"This mixer has RC2FX mixer software %s. This replaces it with the older %s.",
					*m.Installed, version))
			}
		case MixerChangeUnknown:
			// The newest version on offer can't be older than a released
			// one the mixer might have, so only an older choice is worth a
			// word.
			if m.newestMixer(*m.Mixer) {
				break
			}
			warnings = append(warnings, fmt.Sprintf(
				"The app can't tell which RC2FX version this mixer has, if any. If it has one, %s could be older than what it replaces.",
				version))
		}
	}
	choice := m.Choice
	if stock {
		choice = m.StockChoice
	}
	if choice != nil && m.InstalledFirmware != nil &&
		change(m.InstalledFirmware, choice.Version) == MixerChangeDowngrade {
		warnings = append(warnings, fmt.Sprintf(
			"This mixer has RØDE firmware %s. This replaces it with the older %s.",
			*m.InstalledFirmware, choice.Version))
	}
	return warnings
}

// newestMixer reports that no option is newer than source.
func (m *Model) newestMixer(source MixerSource) bool {
	for _, other := range m.MixerOptions() {
		if c, ok := core.CompareVersions(source.Version(), other.Version()); !ok || c < 0 {
			return false
		}
	}
	return true
}

// SetStock chooses between firmware with RC2FX and unchanged RØDE firmware,
// keeping the version choice valid for the mode. A run going on keeps its
// mode. A failed run is stopped first (as Stop does), so choosing a mode,
// which the Home notice's Install update does, starts over from the page.
func (m *Model) SetStock(stock bool) {
	if m.Running() || m.Busy() {
		return
	}
	if m.Failed() {
		m.Stop()
		if m.Run != nil {
			return
		}
	}
	m.Stock = stock
	m.finished = nil
	if !m.supportedChoice(m.Choice) {
		m.Choice = defaultChoice(m.Releases)
	}
	m.settleMixer()
}

// ReleasesFor is the releases a mode offers, with their index in Releases.
// Both modes offer only qualified catalog releases.
func (m *Model) ReleasesFor(stock bool) iter.Seq2[int, installer.Release] {
	return func(yield func(int, installer.Release) bool) {
		for i, r := range m.Releases {
			if r.Supported && !yield(i, r) {
				return
			}
		}
	}
}

// ChoiceFor is the release a mode would use: the current choice when it
// fits the mode, as SetStock keeps it, else the mode's default.
func (m *Model) ChoiceFor(stock bool) *Choice {
	if m.supportedChoice(m.Choice) {
		choice := *m.Choice
		return &choice
	}
	return defaultChoice(m.Releases)
}

// Select picks the mode and release together, as the firmware menu does.
func (m *Model) Select(stock bool, index int) {
	m.SetStock(stock)
	m.Choose(index)
}

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

// advance carries out the next unsatisfied step, or waits for the mixer.
func (m *Model) advance() *installer.Request {
	for {
		run := m.Run
		if run == nil {
			return nil
		}
		if run.status.Kind == RunFailed || m.Busy() {
			return nil
		}
		if m.Status != nil && (len(m.Status.USB) > 1 ||
			slices.ContainsFunc(m.Status.USB, func(u installer.USBMixer) bool {
				return u.Model != run.Device || (run.Serial != "" && u.Serial != run.Serial)
			})) {
			return m.fail("The connected mixer changed. Reconnect the original model and serial; nothing else will be flashed.")
		}
		if run.Device == firmware.ModelDuo && !m.AllowDuo {
			return m.fail("Experimental Duo flashing is disabled.")
		}
		if run.Serial == "" && m.Status != nil && len(m.Status.USB) > 0 {
			run.Serial = m.Status.USB[0].Serial
		}
		if run.Index < 0 || run.Index >= len(Steps) {
			m.finished = m.Run
			m.Run = nil
			if m.Status != nil {
				if c := m.Status.Saved.LastCommand(); c != nil && c.Command == "flash" {
					at := c.AtUnix
					m.SettledFlash = &at
				}
			}
			return nil
		}
		step := Steps[run.Index]
		if m.satisfied(step) {
			run.Index++
			run.resume()
			continue
		}
		serial := run.Serial
		switch step {
		case StepBackup:
			if !m.Answering() {
				return m.fail("RC2FX stopped answering before its settings were saved, so they were not saved and nothing was changed. " +
					"Reconnect the mixer and choose Try again, or choose Stop and start again to continue without saving them.")
			}
			if serial == "" {
				return m.wait("Connect the mixer so its settings can be matched to it.")
			}
			if m.settingsRead == SettingsReadIdle {
				m.settingsRead = SettingsReadWanted
			}
			return m.wait("Reading RC2FX's settings over USB.")
		case StepMixer:
			profile, hasProfile := m.firmwareProfile(run.Choice)
			switch {
			case run.Mixer != nil && run.Mixer.Kind == MixerSourceSigned && hasProfile:
				offer := cloneOffer(run.Mixer.Offer)
				return m.send(installer.Request{
					Kind:            installer.RequestSelectMixer,
					Offer:           &offer,
					FirmwareProfile: profile,
				})
			case run.Mixer != nil && run.Mixer.Kind == MixerSourceBundled:
				return m.send(installer.Request{Kind: installer.RequestSelectBundledMixer})
			default:
				return m.fail("Choose the mixer software to install.")
			}
		case StepPrepare:
			chosen := run.Choice
			if chosen == nil {
				return m.fail("Choose a firmware version.")
			}
			return m.send(installer.Request{
				Kind:    installer.RequestPrepare,
				Channel: chosen.Channel,
				Version: chosen.Version,
				Serial:  serial,
				Stock:   run.Stock,
			})
		case StepUSBMode:
			usb := m.USB()
			// The storage fix flashes the run's firmware itself, last; it
			// needs the mixer in normal mode, as here.
			if fix := run.Fix; fix != nil && fix.Reply == nil && fix.Error == "" &&
				len(usb) == 1 && !usb[0].UpdateMode {
				run.Committed = true
				fix.Event = nil
				return m.send(installer.Request{Kind: installer.RequestStorageFix, Final: true})
			}
			switch {
			case len(usb) == 0:
				return m.wait("Connect the mixer to this computer.")
			case len(usb) == 1 && !usb[0].UpdateMode:
				serial := usb[0].Serial
				if run.status.Kind == RunWaiting && m.usbModeSent() {
					return m.wait("Waiting for the mixer to switch to USB update mode.")
				}
				run.Committed = true
				return m.send(installer.Request{Kind: installer.RequestEnterUSBMode, Serial: serial})
			default:
				return m.fail("More than one RØDECaster is connected. Connect only one.")
			}
		case StepFlash:
			one := m.usbOne(true)
			switch {
			case one != nil && m.Status != nil && m.Status.UpdateDrive:
				serial := one.Serial
				run.Committed = true
				return m.send(installer.Request{Kind: installer.RequestFlash, Serial: serial})
			case one != nil:
				return m.wait("Waiting for the mixer's update drive to appear on this computer.")
			default:
				return m.wait("Waiting for the mixer to enter USB update mode.")
			}
		default: // StepStart
			if run.Stock {
				if len(m.USB()) == 0 {
					return m.wait("The mixer is installing the firmware and restarting. Keep it connected to see when it is back.")
				}
				return m.wait("The mixer is installing the firmware and restarting.")
			}
			service := m.Service()
			if service != nil && service.Kind == installer.ServiceStarting {
				return m.wait(fmt.Sprintf("RC2FX is starting: %s", service.Detail))
			}
			if service != nil && service.Kind == installer.ServiceReady {
				if check := &m.imageCheck.result; m.imageCheck.kind == imageCheckAnswered {
					switch check.Status {
					case core.ImageStatusDamaged:
						return m.fail(damagedMessage(check))
					case core.ImageStatusRepaired:
						return m.waitFor(repairedMessage(check), true)
					}
				}
				return m.wait("RC2FX is running and is checking the files the flash installed. This takes a minute or two.")
			}
			return m.wait("The mixer is updating and will restart a couple of times. Leave it on; this takes a few minutes.")
		}
	}
}

// fileCount is how many files a check names, with the ones it left out.
// Pages say how many, never which: file names mean nothing to the reader.
func fileCount(check *core.ImageCheck, names []string) string {
	n := len(names) + int(check.Unlisted)
	if n == 1 {
		return "1 of RØDE's files"
	}
	return fmt.Sprintf("%d of RØDE's files", n)
}

// repairedMessage: RØDE's boot already ran with the damaged copies, so only
// a restart brings the mixer up as RØDE intends.
func repairedMessage(check *core.ImageCheck) string {
	return fmt.Sprintf("The update damaged %s on the mixer, and RC2FX restored them from the mixer's previous system. "+
		"Turn the mixer off and on again to finish. If the mixer asks for its initial setup, don't redo it: the restart brings it back.",
		fileCount(check, check.Repaired))
}

func damagedMessage(check *core.ImageCheck) string {
	return fmt.Sprintf("The update damaged %s on the mixer, and there was no undamaged copy on the mixer to restore them from. "+
		"Press “Try again” to send the firmware again. Until then the mixer may forget its setup at every restart; don't redo it if it asks.",
		fileCount(check, check.Damaged))
}

// usbModeSent reports whether the USB-mode command was sent in the last two
// minutes.
func (m *Model) usbModeSent() bool {
	if m.Status == nil {
		return false
	}
	c := m.Status.Saved.LastCommand()
	return c != nil && c.Command == "usb-mode" && num.SatSub(nowUnix(), c.AtUnix) < 120
}

// Tick polls: often while a run waits for the mixer or has failed (a failed
// page says what the mixer is doing now, and Try again starts from that),
// slowly otherwise so the page follows USB and network changes.
func (m *Model) Tick(now time.Time) *installer.Request {
	if m.Busy() || m.polling() {
		return nil
	}
	var interval time.Duration
	switch {
	case m.Run != nil && (m.Run.status.Kind == RunWaiting || m.Run.status.Kind == RunFailed):
		interval = 3 * time.Second
	case m.Run != nil:
		return nil
	default:
		interval = 10 * time.Second
	}
	if !m.lastPoll.IsZero() && now.Sub(m.lastPoll) < interval {
		return nil
	}
	return m.refresh()
}

// Finish applies a finished request: its reply, or the error it failed with
// (whose text is shown). Returns the next request, if any.
func (m *Model) Finish(request installer.Request, reply installer.Reply, err error) *installer.Request {
	background := false
	if len(m.queue) > 0 {
		background = m.queue[0].background
		m.queue = m.queue[1:]
	}
	// A canceled run ends when its request returns, whatever it says.
	if !background && m.Canceling() {
		m.Run = nil
		return m.refresh()
	}
	if err != nil {
		return m.failed(request, err)
	}
	r := reply
	switch r.Kind {
	case installer.ReplyStatus:
		var status installer.Status
		if r.Status != nil {
			status = *r.Status
		}
		return m.receiveStatus(status)
	case installer.ReplyReleases:
		m.FirmwareCheck = FirmwareCheckDone
		m.Error = ""
		device := m.Device.ID()
		var items, unavailable []installer.Release
		for _, release := range r.Releases {
			if release.Model != "" && release.Model != device {
				continue
			}
			if release.Supported {
				items = append(items, release)
			} else {
				unavailable = append(unavailable, release)
			}
		}
		m.NeedsApp = nil
		for _, release := range unavailable {
			if model.AppTooOld(release.MinimumAppVersion) {
				m.NeedsApp = append(m.NeedsApp, release)
			}
		}
		if m.Choice == nil || !slices.ContainsFunc(items, func(r installer.Release) bool {
			return r.Channel == m.Choice.Channel && r.Version == m.Choice.Version
		}) {
			m.Choice = defaultChoice(items)
		}
		m.Releases = items
		m.settleMixer()
		// The catalog was read, so mixer software is looked for even when it
		// lists no firmware for this mixer.
		if next := m.checkMixers(); next != nil {
			return next
		}
		return m.nextCheck()
	case installer.ReplyMixerReleases:
		m.Offers = Offers{Kind: OffersReady, List: r.Offers}
		m.settleMixer()
		return m.nextCheck()
	case installer.ReplyStockReleases:
		device := m.Device.ID()
		var items []installer.Release
		for _, release := range r.Releases {
			if release.Model == "" || release.Model == device {
				items = append(items, release)
			}
		}
		if m.StockChoice == nil || !slices.ContainsFunc(items, func(r installer.Release) bool {
			return r.Channel == m.StockChoice.Channel && r.Version == m.StockChoice.Version
		}) {
			m.StockChoice = nil
			pick := slices.IndexFunc(items, func(r installer.Release) bool { return r.Channel == "stable" })
			if pick < 0 && len(items) > 0 {
				pick = 0
			}
			if pick >= 0 {
				m.StockChoice = &Choice{Channel: items[pick].Channel, Version: items[pick].Version}
			}
		}
		m.StockReleases = items
		m.StockCheck = StockCheck{Kind: StockCheckDone}
		return m.nextCheck()
	case installer.ReplyInPlaceBundle:
		var bundle installer.InPlaceBundle
		if r.InPlaceBundle != nil {
			bundle = *r.InPlaceBundle
		}
		m.inPlace = InPlace{Kind: InPlaceReady, Bundle: bundle}
	case installer.ReplyMixerSelected:
		release := ""
		if r.Identity != nil {
			release = r.Identity.Release
		}
		if run := m.Run; run != nil {
			if run.Mixer != nil && run.Mixer.Version() == release {
				run.Selected = true
			} else {
				wanted := ""
				if run.Mixer != nil {
					wanted = run.Mixer.Version()
				}
				m.fail(fmt.Sprintf(
					"The verified mixer software is %s rather than the %s you chose. Nothing was flashed.",
					release, wanted))
			}
		}
	case installer.ReplyStorageFix:
		if run := m.Run; run != nil && run.Fix != nil && r.StorageFix != nil {
			fix := *r.StorageFix
			run.Fix.Reply, run.Fix.Event = &fix, nil
			switch {
			case fix.FinalSent:
				// The run's firmware is going onto the fixed slot.
				run.Flashed = true
			case fix.Touched:
				// Stopped part way: what is left follows the mixer's state,
				// and trying again installs with a normal flash.
				m.fail(fmt.Sprintf("The storage fix stopped before it finished. %s", fix.Error))
			default:
				// The mixer was not touched: install with a normal flash.
				run.Committed = run.Flashed || m.InUpdateMode()
			}
		}
	case installer.ReplyBackup:
		if m.Run != nil {
			m.Run.BackedUp = true
		}
	case installer.ReplyPrepared:
		if m.Run != nil {
			m.Run.Prepared = true
		}
	case installer.ReplyDone:
		switch request.Kind {
		case installer.RequestFlash:
			if m.Run != nil {
				m.Run.Flashed = true
			}
		case installer.RequestEnterUSBMode:
			if m.Running() {
				// A new wait, not the one before the request.
				m.Run.resume()
				m.wait("Waiting for the mixer to switch to USB update mode.")
			}
		}
	}
	return m.refresh()
}

// failed applies a request that failed with err.
func (m *Model) failed(request installer.Request, err error) *installer.Request {
	e := err.Error()
	switch request.Kind {
	case installer.RequestMixerReleases:
		m.Offers = Offers{Kind: OffersFailed, Error: e}
		m.settleMixer()
		return m.nextCheck()
	case installer.RequestStockReleases:
		m.StockCheck = StockCheck{Kind: StockCheckFailed, Error: e}
		return m.nextCheck()
	case installer.RequestProbe:
		// A failed poll while waiting is not the end of the run.
		if m.Run != nil {
			return m.advance()
		}
		m.Error = e
		return nil
	case installer.RequestInPlaceBundle:
		message := fmt.Sprintf("Couldn't get the update ready. Nothing changed on the mixer. %s", e)
		if canceled(err) {
			message = "The update was canceled. Nothing changed on the mixer."
		}
		m.inPlace = InPlace{Kind: InPlaceFailed, Error: message}
		return nil
	case installer.RequestStorageFix:
		if run := m.Run; run != nil && run.Fix != nil {
			run.Fix.Error, run.Fix.Event = e, nil
		}
		m.fail(e)
		return m.refresh()
	case installer.RequestReleases:
		m.FirmwareCheck = FirmwareCheckDone
		if canceled(err) {
			m.Error = "The check for supported firmware was canceled."
		} else {
			m.Error = fmt.Sprintf("Couldn't check for supported firmware. %s", e)
		}
		// Stock firmware doesn't depend on RC2FX's catalog; mixer software
		// waits for it (see nextCheck).
		return m.nextCheck()
	default:
		m.fail(e)
		// Show what the mixer reports after the failure.
		return m.refresh()
	}
}

func (m *Model) receiveStatus(status installer.Status) *installer.Request {
	first := m.Status == nil
	if m.Run == nil && len(status.USB) == 1 && status.USB[0].Model != m.Device {
		m.Device = status.USB[0].Model
		m.Choice = nil
		m.forgetStock()
		m.Releases = nil
		m.NeedsApp = nil
		m.FirmwareCheck = FirmwareCheckUnchecked
		m.Error = ""
	}
	m.Status = &status
	if p := m.Prepared(); p != nil {
		slot := &m.Choice
		if p.Stock {
			slot = &m.StockChoice
		}
		if *slot == nil {
			*slot = &Choice{Channel: p.Channel, Version: p.Version}
		}
	}
	// Opened during an update: wait for what was flashed. Only on opening,
	// so a finished update never comes back, and only while the app sees no
	// mixer in normal mode (recentlyFlashed): one there has nothing left to
	// wait for.
	if first && m.Run == nil && m.recentlyFlashed() {
		if p := m.Prepared(); p != nil {
			m.Stock = p.Stock
			m.finished = nil
			m.Run = &Run{
				Started:  time.Now(),
				Device:   m.Device,
				Stock:    p.Stock,
				Choice:   &Choice{Channel: p.Channel, Version: p.Version},
				BackedUp: true,
				Prepared: true,
				Flashed:  true,
			}
		}
	}
	if m.Run != nil {
		// Opened during an update: RC2FX's readiness is judged against the
		// catalog, which this process may not have.
		if next := m.advance(); next != nil {
			return next
		}
		if m.flashed() {
			return m.checkFirmware()
		}
		return nil
	}
	return m.nextCheck()
}

// nextCheck is the first catalog check still to do, in order: RC2FX's
// firmware, its mixer software, then RØDE's stock firmware. The checks run one
// at a time: while one is under way, nothing else starts, and its reply calls
// nextCheck again. A check that is done or failed is passed over instead of
// ending the chain, so a list forgotten after a model change or the Duo switch
// is fetched again even when the lists before it are still current.
//
// The firmware list depends on the mixer's model, so it waits while USB
// can't be read. Mixer software is looked for once RC2FX's catalog listed
// firmware, or while USB can't be read; a failed catalog check doesn't also
// try it (a reply to the catalog check does, whatever it lists). Stock
// firmware is RØDE's and is always looked for.
func (m *Model) nextCheck() *installer.Request {
	usbFailed := m.Status != nil && m.Status.USBError != nil
	if !usbFailed {
		if next := m.checkFirmware(); next != nil {
			return next
		}
		if m.FirmwareCheck == FirmwareCheckChecking {
			return nil
		}
	}
	if len(m.Releases) > 0 || usbFailed {
		if next := m.checkMixers(); next != nil {
			return next
		}
		if m.Offers.Kind == OffersChecking {
			return nil
		}
	}
	return m.checkStock()
}

// defaultChoice is stable when RC2FX supports it, else the first supported
// release.
func defaultChoice(releases []installer.Release) *Choice {
	pick := slices.IndexFunc(releases, func(r installer.Release) bool {
		return r.Supported && r.Channel == "stable"
	})
	if pick < 0 {
		pick = slices.IndexFunc(releases, func(r installer.Release) bool { return r.Supported })
	}
	if pick < 0 {
		return nil
	}
	return &Choice{Channel: releases[pick].Channel, Version: releases[pick].Version}
}
