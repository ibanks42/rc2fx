package installflow

import (
	"errors"
	"iter"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/opt"
)

const profile = "rodecaster-pro-ii-1.7.6"

func offerFor(version, profile string) installer.MixerOffer {
	return installer.MixerOffer{
		Target: "mixer-" + version + ".payload",
		SHA256: strings.Repeat("00", 32),
		Length: 1,
		Metadata: installer.MixerTarget{
			Version:           version,
			PayloadFormat:     1,
			ProtocolVersion:   uint32(core.MixerProtocolVersion),
			MinimumAppVersion: "0.0.1",
			FirmwareProfiles:  []string{profile},
		},
	}
}

func offer(version string) installer.MixerOffer {
	return offerFor(version, profile)
}

func signed(version string) *MixerSource {
	source := Signed(offer(version))
	return &source
}

func wantRequest(t *testing.T, got *installer.Request, want installer.Request, why string) {
	t.Helper()
	if !sameRequest(got, &want) {
		t.Fatalf("%s: request %#v, want %#v", why, got, want)
	}
}

func isRequest(request *installer.Request, kind installer.RequestKind) bool {
	return request != nil && request.Kind == kind
}

func wantChoice(t *testing.T, got *Choice, channel, version, why string) {
	t.Helper()
	if !opt.Equal(got, &Choice{Channel: channel, Version: version}) {
		t.Fatalf("%s: choice %+v, want %s %s", why, got, channel, version)
	}
}

func sameSource(a, b *MixerSource) bool {
	if a == nil || b == nil {
		return a == b
	}
	return a.Equal(*b)
}

// selected: the engine verified the selection; the plan goes on to the next
// step.
func selected(t *testing.T, m *Model, request *installer.Request) *installer.Request {
	t.Helper()
	if !isRequest(request, installer.RequestSelectMixer) && !isRequest(request, installer.RequestSelectBundledMixer) {
		t.Fatalf("request %#v is no selection", request)
	}
	release := m.Run.Mixer.Version()
	identity := installer.Identity{Release: release}
	refresh := answer(t, m, request, installer.Reply{Kind: installer.ReplyMixerSelected, Identity: &identity})
	seen := Seen{status: deepCopy(*m.Status), service: m.service}
	return report(t, m, refresh, seen)
}

// Seen is what the installer sees over USB, and RC2FX as the app's
// connection sees it.
type Seen struct {
	status  installer.Status
	service *installer.Service
}

type usbSeen struct {
	serial     string
	updateMode bool
}

func status(usb []usbSeen, service *installer.Service, prepared, drive bool) Seen {
	mixers := make([]installer.USBMixer, 0, len(usb))
	for _, u := range usb {
		mixers = append(mixers, installer.USBMixer{
			Model:      firmware.ModelProIi,
			Serial:     u.serial,
			UpdateMode: u.updateMode,
			Firmware:   nil,
		})
	}
	var cached *installer.Prepared
	if prepared {
		cached = &installer.Prepared{Version: "1.7.6", Channel: "stable"}
	}
	return Seen{
		status: installer.Status{
			USB:             mixers,
			USBError:        nil,
			Saved:           installer.Saved{Prepared: cached},
			PreparedPresent: prepared,
			UpdateDrive:     drive,
		},
		service: service,
	}
}

func gv1(updateMode bool) []usbSeen {
	return []usbSeen{{"GV1", updateMode}}
}

// absent: the app is not connected to RC2FX.
func absent() *installer.Service {
	return nil
}

func ready() *installer.Service {
	return &installer.Service{Kind: installer.ServiceReady}
}

func setUp(s Seen) *Model {
	m := NewModel()
	m.Status = &s.status
	m.SetService(s.service)
	m.Releases = []installer.Release{{
		Channel:   "stable",
		Version:   "1.7.6",
		Profile:   profile,
		Supported: true,
	}}
	m.Choice = &Choice{Channel: "stable", Version: "1.7.6"}
	m.FirmwareCheck = FirmwareCheckDone
	m.StockReleases = deepCopy(m.Releases)
	m.StockChoice = &Choice{Channel: "stable", Version: "1.7.6"}
	m.StockCheck = StockCheck{Kind: StockCheckDone}
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{offer("1.0.0")}}
	m.settleMixer()
	return m
}

func answer(t *testing.T, m *Model, request *installer.Request, reply installer.Reply) *installer.Request {
	t.Helper()
	if request == nil {
		t.Fatal("no request")
	}
	return m.Finish(*request, reply, nil)
}

func report(t *testing.T, m *Model, request *installer.Request, s Seen) *installer.Request {
	t.Helper()
	m.SetService(s.service)
	return answer(t, m, request, installer.Reply{Kind: installer.ReplyStatus, Status: new(s.status)})
}

// readSettings: the app reads the settings the plan asked for.
func readSettings(t *testing.T, m *Model) *installer.Request {
	t.Helper()
	if !m.TakeSettingsRequest() {
		t.Fatal("the plan asks for the settings")
	}
	return m.SettingsRead(`{"config":{}}`, nil)
}

func wantState(t *testing.T, m *Model, step Step, kind StepStateKind) StepState {
	t.Helper()
	got := m.State(step)
	if got.Kind != kind {
		t.Fatalf("step %v: state %+v, want kind %v", step, got, kind)
	}
	return got
}

func failedRun(t *testing.T, m *Model) string {
	t.Helper()
	if m.Run == nil || m.Run.Status().Kind != RunFailed {
		t.Fatal("the run did not fail")
	}
	return m.Run.Status().Why
}

func TestDuoPreferenceGatesPreparationAndModelChangesDropCachedChoices(t *testing.T) {
	m := NewModel()
	seen := status([]usbSeen{{"DUO", false}}, nil, true, false)
	seen.status.USB[0].Model = firmware.ModelDuo
	probe := m.Probe()
	releases := m.Finish(*probe, installer.Reply{Kind: installer.ReplyStatus, Status: new(deepCopy(seen.status))}, nil)
	if releases == nil {
		t.Fatal("no request")
	}
	check := m.Finish(*releases, installer.Reply{Kind: installer.ReplyReleases, Releases: nil}, nil)
	if check == nil {
		t.Fatal("no request")
	}
	m.Finish(*check, installer.Reply{Kind: installer.ReplyMixerReleases, Offers: []installer.MixerOffer{offer("1.0.0")}}, nil)
	if m.Device != firmware.ModelDuo {
		t.Fatalf("device %v", m.Device)
	}
	if m.Prepared() != nil {
		t.Fatal("a Pro II cached archive is not a Duo choice")
	}
	m.Choice = &Choice{Channel: "stable", Version: "1.7.6"}
	if m.CanStart() {
		t.Fatal("can start")
	}
	if m.Start() != nil {
		t.Fatal("started")
	}

	enable := m.SetAllowDuo(true)
	wantRequest(t, enable, installer.Request{Kind: installer.RequestAllowExperimentalDuo, Allow: true}, "enable")
	if m.SetAllowDuo(false) != nil {
		t.Fatal("don't mutate in-flight policy")
	}
	releases = m.Finish(*enable, installer.Reply{Kind: installer.ReplyStatus, Status: new(seen.status)}, nil)
	if releases == nil {
		t.Fatal("no request")
	}
	// The mixer software list is still current, but the switch forgot RØDE's
	// stock list: it is fetched again.
	stock := m.Finish(*releases, installer.Reply{Kind: installer.ReplyReleases, Releases: []installer.Release{{
		Model:     firmware.ModelDuo.ID(),
		Channel:   "stable",
		Version:   "1.7.6",
		Profile:   profile,
		Supported: true,
	}}}, nil)
	wantRequest(t, stock, installer.Request{Kind: installer.RequestStockReleases}, "stock after the Duo switch")
	if next := answer(t, m, stock, installer.Reply{Kind: installer.ReplyStockReleases, Releases: nil}); next != nil {
		t.Fatalf("request %#v after every list is current", next)
	}
	sel := m.Start()
	request := selected(t, m, sel)
	if !isRequest(request, installer.RequestPrepare) {
		t.Fatalf("request %#v", request)
	}
	if m.Run.Device != firmware.ModelDuo {
		t.Fatalf("run device %v", m.Run.Device)
	}
	probe = m.Finish(*request, installer.Reply{Kind: installer.ReplyPrepared, Prepared: &installer.Prepared{}}, nil)
	if probe == nil {
		t.Fatal("no request")
	}
	changed := status([]usbSeen{{"DUO", false}}, nil, false, false)
	m.Finish(*probe, installer.Reply{Kind: installer.ReplyStatus, Status: new(changed.status)}, nil)
	if !strings.Contains(failedRun(t, m), "mixer changed") {
		t.Fatalf("failure %q", failedRun(t, m))
	}
}

func TestAModelChangeFetchesTheStockListAgain(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	duo := status([]usbSeen{{"DUO", false}}, absent(), false, false)
	duo.status.USB[0].Model = firmware.ModelDuo
	check := report(t, m, m.Probe(), duo)
	wantRequest(t, check, installer.Request{Kind: installer.RequestReleases}, "firmware for the new model")
	if m.StockCheck.Kind != StockCheckUnchecked || m.StockReleases != nil {
		t.Fatalf("the Pro II stock list is kept: %+v", m.StockCheck)
	}
	// The mixer software list doesn't depend on the model and is still
	// current, so the stock list is next.
	stock := answer(t, m, check, installer.Reply{Kind: installer.ReplyReleases, Releases: nil})
	wantRequest(t, stock, installer.Request{Kind: installer.RequestStockReleases}, "stock for the new model")
	next := answer(t, m, stock, installer.Reply{Kind: installer.ReplyStockReleases, Releases: []installer.Release{{
		Model:   firmware.ModelDuo.ID(),
		Channel: "stable",
		Version: "1.0.0",
	}}})
	if next != nil {
		t.Fatalf("request %#v after every list is current", next)
	}
	wantChoice(t, m.StockChoice, "stable", "1.0.0", "Duo stock choice")
	if m.polling() {
		t.Fatal("a check is left queued")
	}
}

func TestFlashingWaitsForAdministratorAccess(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if !m.CanStart() {
		t.Fatal("cannot start")
	}
	m.Privileged = false
	if m.CanStart() {
		t.Fatal("can start")
	}
	if m.Start() != nil {
		t.Fatal("started")
	}
	m.Run = &Run{
		Device: firmware.ModelProIi,
		status: RunStatus{Kind: RunFailed, Why: "Grant administrator access first."},
	}
	if m.Retry() != nil {
		t.Fatal("a failed run waits too")
	}
	m.Privileged = true
	if m.Retry() == nil {
		t.Fatal("no retry")
	}
}

func TestTheDuoOptInIsOfferedOnlyWhileADuoIsConnected(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if m.DuoConnected() {
		t.Fatal("a Pro II")
	}
	m.Status.USB[0].Model = firmware.ModelDuo
	if !m.DuoConnected() {
		t.Fatal("no Duo")
	}
	m.Status.USB = m.Status.USB[:0]
	m.Device = firmware.ModelDuo
	if m.DuoConnected() {
		t.Fatal("a Duo seen earlier but unplugged now")
	}
}

func TestTurningOffDuoHidesItsPreparationAndBlocksStart(t *testing.T) {
	s := status([]usbSeen{{"DUO", true}}, nil, true, true)
	s.status.USB[0].Model = firmware.ModelDuo
	s.status.Saved.Prepared.Model = new(firmware.ModelDuo)
	m := setUp(s)
	m.Device = firmware.ModelDuo
	m.AllowDuo = true
	if m.Prepared() == nil {
		t.Fatal("not prepared")
	}
	disable := m.SetAllowDuo(false)
	wantRequest(t, disable, installer.Request{Kind: installer.RequestAllowExperimentalDuo, Allow: false}, "disable")
	if m.Prepared() != nil {
		t.Fatal("still prepared")
	}
	if m.CanStart() {
		t.Fatal("can start")
	}
	if m.Choice != nil {
		t.Fatalf("choice %+v", m.Choice)
	}
}

func TestGoalFollowsWhetherRC2FXAnswers(t *testing.T) {
	if got := setUp(status(nil, ready(), false, false)).Goal(); got != GoalReinstall {
		t.Fatalf("goal %v", got)
	}
	if got := setUp(status(nil, absent(), false, false)).Goal(); got != GoalSetup {
		t.Fatalf("goal %v", got)
	}
	if got := setUp(status(nil, nil, false, false)).Goal(); got != GoalSetup {
		t.Fatalf("goal %v", got)
	}
}

func TestAReinstallRunsEndToEndAfterOneStart(t *testing.T) {
	m := setUp(status(gv1(false), ready(), false, false))
	if m.Start() != nil {
		t.Fatal("the app reads the settings first")
	}
	wantState(t, m, StepBackup, StepStateWaiting)
	backup := readSettings(t, m)
	wantRequest(t, backup, installer.Request{Kind: installer.RequestSaveSettings, Serial: "GV1", Settings: `{"config":{}}`}, "backup")
	refresh := answer(t, m, backup, installer.Reply{Kind: installer.ReplyBackup})
	sel := report(t, m, refresh, status(gv1(false), ready(), false, false))
	wantRequest(t, sel, installer.Request{Kind: installer.RequestSelectMixer, Offer: new(offer("1.0.0")), FirmwareProfile: profile},
		"the exact offer, before anything is built from it")
	prepare := selected(t, m, sel)
	wantRequest(t, prepare, installer.Request{
		Kind:    installer.RequestPrepare,
		Channel: "stable",
		Version: "1.7.6",
		Serial:  "GV1",
		Stock:   false,
	}, "the request names the chosen release")
	refresh = answer(t, m, prepare, installer.Reply{Kind: installer.ReplyPrepared})
	usb := report(t, m, refresh, status(gv1(false), ready(), true, false))
	wantRequest(t, usb, installer.Request{Kind: installer.RequestEnterUSBMode, Serial: "GV1"}, "usb mode")
	refresh = answer(t, m, usb, installer.Reply{Kind: installer.ReplyDone})
	// Still normal mode a moment later: wait, never resend.
	s := status(gv1(false), ready(), true, false)
	s.status.Saved.Commands = append(s.status.Saved.Commands, installer.CommandRecord{
		Command:      "usb-mode",
		AtUnix:       nowUnix(),
		Acknowledged: true,
	})
	if report(t, m, refresh, s) != nil {
		t.Fatal("resent")
	}
	wantState(t, m, StepUSBMode, StepStateWaiting)
	// Update mode, drive not yet visible: wait; then flash.
	poll := m.Probe()
	if report(t, m, poll, status(gv1(true), absent(), true, false)) != nil {
		t.Fatal("flashed without the drive")
	}
	wantState(t, m, StepFlash, StepStateWaiting)
	poll = m.Probe()
	flash := report(t, m, poll, status(gv1(true), absent(), true, true))
	wantRequest(t, flash, installer.Request{Kind: installer.RequestFlash, Serial: "GV1"}, "flash")
	refresh = answer(t, m, flash, installer.Reply{Kind: installer.ReplyDone})
	// Updating and restarting: nothing answers, then RC2FX starts.
	if report(t, m, refresh, status(nil, absent(), true, false)) != nil {
		t.Fatal("request while restarting")
	}
	wantState(t, m, StepStart, StepStateWaiting)
	poll = m.Probe()
	starting := &installer.Service{Kind: installer.ServiceStarting, Detail: "waiting for stock audio"}
	if report(t, m, poll, status(gv1(false), starting, true, false)) != nil {
		t.Fatal("request while starting")
	}
	if w := wantState(t, m, StepStart, StepStateWaiting); !strings.Contains(w.Detail, "stock audio") {
		t.Fatalf("waiting %q", w.Detail)
	}
	poll = m.Probe()
	report(t, m, poll, status(gv1(false), ready(), true, false))
	// RC2FX runs; the step waits for its check of RØDE's files.
	if !m.WantsImageCheck() {
		t.Fatal("the run asks for the image check")
	}
	if w := wantState(t, m, StepStart, StepStateWaiting); !strings.Contains(w.Detail, "checking the files") {
		t.Fatalf("waiting %q", w.Detail)
	}
	check := func(status core.ImageStatus) *core.ImageCheck {
		return &core.ImageCheck{Status: status, Repaired: []string{"usr/bin/rode-init-update-env.sh"}}
	}
	m.ImageCheckRead(check(core.ImageStatusPending), nil)
	if m.Run == nil || m.Finished() {
		t.Fatal("a pending check is waited on")
	}
	// Restored files need a restart, which clears the answer until RC2FX
	// is back and checks again.
	m.ImageCheckRead(check(core.ImageStatusRepaired), nil)
	if w := wantState(t, m, StepStart, StepStateWaiting); !strings.Contains(w.Detail, "Turn the mixer off and on again") ||
		!strings.Contains(w.Detail, "1 of RØDE's files") || strings.Contains(w.Detail, "rode-init-update-env.sh") {
		t.Fatalf("waiting %q", w.Detail)
	}
	m.SetService(nil)
	if m.imageCheck.kind != imageCheckUnasked {
		t.Fatal("the answer is cleared while RC2FX is not running")
	}
	m.SetService(ready())
	m.ImageCheckRead(nil, errors.New("timed out"))
	if !m.WantsImageCheck() {
		t.Fatal("a failed read is asked again")
	}
	m.ImageCheckRead(check(core.ImageStatusOk), nil)
	if m.Run != nil || !m.Finished() {
		t.Fatal("not finished")
	}
}

func TestFilesTheMixerCouldNotRestoreFailTheInstall(t *testing.T) {
	atStart := func() *Model {
		m := setUp(status(gv1(false), absent(), true, false))
		m.Run = &Run{Index: slices.Index(Steps[:], StepStart), Serial: "GV1", Flashed: true}
		m.SetService(ready())
		return m
	}
	m := atStart()
	m.ImageCheckRead(&core.ImageCheck{
		Status:   core.ImageStatusDamaged,
		Damaged:  []string{"usr/lib/libthing.so"},
		Unlisted: 2,
	}, nil)
	// How many files, never which: the names mean nothing to the reader.
	if e := wantState(t, m, StepStart, StepStateFailed); !strings.Contains(e.Detail, "3 of RØDE's files") ||
		strings.Contains(e.Detail, "libthing") || !strings.Contains(e.Detail, "Try again") {
		t.Fatalf("failed %q", e.Detail)
	}
	// Mixer software from before the check has nothing to wait for.
	m = atStart()
	m.ImageCheckRead(nil, nil)
	if m.Run != nil || !m.Finished() {
		t.Fatal("not finished")
	}
}

func TestStockFirmwareSavesSettingsFlashesUnchangedAndWaitsForNormalMode(t *testing.T) {
	m := setUp(status(gv1(false), ready(), false, false))
	m.Releases = []installer.Release{
		{Channel: "stable", Version: "1.7.6", Supported: true},
		{Channel: "beta", Version: "1.8.0", Supported: false},
	}
	m.Choose(1)
	wantChoice(t, m.Choice, "stable", "1.7.6", "RC2FX needs support")
	m.SetStock(true)
	if got := m.Goal(); got != GoalStock {
		t.Fatalf("goal %v", got)
	}
	m.Choose(1)
	wantChoice(t, m.Choice, "stable", "1.7.6", "stock also requires a qualified catalog release")
	if m.Start() != nil {
		t.Fatal("started without the settings")
	}
	backup := readSettings(t, m)
	if !isRequest(backup, installer.RequestSaveSettings) {
		t.Fatalf("settings are kept for later: %#v", backup)
	}
	refresh := answer(t, m, backup, installer.Reply{Kind: installer.ReplyBackup})
	prepare := report(t, m, refresh, status(gv1(false), ready(), false, false))
	if !isRequest(prepare, installer.RequestPrepare) || !prepare.Stock {
		t.Fatalf("request %#v", prepare)
	}
	// Flashed; RC2FX is gone, but normal mode is enough.
	m.Run.Flashed = true
	m.queue = nil
	poll := m.Probe()
	if report(t, m, poll, status(nil, absent(), true, false)) != nil {
		t.Fatal("request while restarting")
	}
	wantState(t, m, StepStart, StepStateWaiting)
	poll = m.Probe()
	report(t, m, poll, status(gv1(false), absent(), true, false))
	if m.Run != nil || !m.Finished() {
		t.Fatal("not finished")
	}
	// Back to RC2FX: an unsupported choice is replaced.
	m.SetStock(false)
	wantChoice(t, m.Choice, "stable", "1.7.6", "back to RC2FX")
}

func TestReadinessCountsOnlyForTheSameMixer(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if !m.IsSessionMixer(new("GV1")) {
		t.Fatal("GV1")
	}
	if m.IsSessionMixer(new("GV2")) {
		t.Fatal("GV2")
	}
	if m.IsSessionMixer(nil) {
		t.Fatal("no session")
	}
	updating := setUp(status(gv1(true), absent(), false, false))
	if updating.IsSessionMixer(new("GV1")) {
		t.Fatal("updating")
	}
	two := setUp(status([]usbSeen{{"GV1", false}, {"GV2", false}}, absent(), false, false))
	if two.IsSessionMixer(new("GV1")) {
		t.Fatal("two mixers")
	}
}

func TestKeyedImagesFromOlderInstallersAreNotReused(t *testing.T) {
	if setUp(status(gv1(false), absent(), true, false)).Prepared() == nil {
		t.Fatal("not prepared")
	}
	s := status(gv1(false), absent(), true, false)
	s.status.Saved.Prepared.KeySHA = "abc"
	if setUp(s).Prepared() != nil {
		t.Fatal("keyed image reused")
	}
	s = status(gv1(false), absent(), true, false)
	s.status.Saved.Prepared.HostKey = "abc"
	if setUp(s).Prepared() != nil {
		t.Fatal("a partial keyed cache fails closed")
	}
}

func count[K, V any](seq iter.Seq2[K, V]) int {
	n := 0
	for range seq {
		n++
	}
	return n
}

func TestEachModeCardShowsTheReleaseItWouldUse(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	m.Releases = []installer.Release{
		{Channel: "stable", Version: "1.7.6", Supported: true},
		{Channel: "beta", Version: "1.8.0", Supported: false},
	}
	if got := count(m.ReleasesFor()); got != 1 {
		t.Fatalf("%d releases", got)
	}
	m.Select(true, 1)
	wantChoice(t, m.ChoiceFor(), "stable", "1.7.6", "an unsupported release falls back")
	m.SetStock(false)
	if !opt.Equal(m.Choice, m.ChoiceFor()) {
		t.Fatalf("choice %+v", m.Choice)
	}
}

func TestCancelingWorksUntilUSBModeIsRequested(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	sel := m.Start()
	prepare := selected(t, m, sel)
	if !isRequest(prepare, installer.RequestPrepare) {
		t.Fatalf("request %#v", prepare)
	}
	if !m.CanCancel() {
		t.Fatal("cannot cancel")
	}
	if !m.Cancel() {
		t.Fatal("the download is asked to stop")
	}
	if !m.Canceling() || m.CanCancel() {
		t.Fatal("not canceling")
	}
	refresh := m.Finish(*prepare, installer.Reply{}, errors.New(installer.Canceled))
	if m.Run != nil {
		t.Fatal("canceled, not failed")
	}
	if !isRequest(refresh, installer.RequestProbe) {
		t.Fatalf("request %#v", refresh)
	}

	m = setUp(status(gv1(false), absent(), true, false))
	sel = m.Start()
	prepare = selected(t, m, sel)
	refresh = answer(t, m, prepare, installer.Reply{Kind: installer.ReplyPrepared})
	usb := report(t, m, refresh, status(gv1(false), absent(), true, false))
	if !isRequest(usb, installer.RequestEnterUSBMode) {
		t.Fatalf("request %#v", usb)
	}
	if m.CanCancel() || m.Cancel() {
		t.Fatal("past the point of no return")
	}
	if m.Run == nil {
		t.Fatal("no run")
	}
}

func TestAStockMixerSkipsSavingSettings(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	if got := m.Goal(); got != GoalSetup {
		t.Fatalf("goal %v", got)
	}
	sel := m.Start()
	if !isRequest(sel, installer.RequestSelectMixer) {
		t.Fatalf("request %#v", sel)
	}
	wantState(t, m, StepBackup, StepStateDone)
}

func TestSettingsNeedTheMixerOnUSBToBeMatched(t *testing.T) {
	m := setUp(status(nil, ready(), false, false))
	if m.Start() != nil {
		t.Fatal("started")
	}
	wantState(t, m, StepBackup, StepStateWaiting)
	poll := m.Probe()
	report(t, m, poll, status(gv1(false), ready(), false, false))
	backup := readSettings(t, m)
	if !isRequest(backup, installer.RequestSaveSettings) || backup.Serial != "GV1" {
		t.Fatalf("request %#v", backup)
	}
}

func TestRC2FXStoppingBeforeTheBackupFailsTheRunInsteadOfSkippingIt(t *testing.T) {
	m := setUp(status(gv1(false), ready(), false, false))
	if m.Start() != nil {
		t.Fatal("the app reads the settings first")
	}
	if !m.Run.NeedsBackup {
		t.Fatal("RC2FX answered at Start, so the run owes a backup")
	}
	if !m.TakeSettingsRequest() {
		t.Fatal("the plan asks for the settings")
	}
	// The connection drops before the reply; RC2FX no longer answers.
	m.ConnectionChanged()
	poll := m.Probe()
	if next := report(t, m, poll, status(gv1(false), absent(), false, false)); next != nil {
		t.Fatalf("went on without the settings: %#v", next)
	}
	if why := failedRun(t, m); !strings.Contains(why, "were not saved") {
		t.Fatalf("failure %q", why)
	}
	wantState(t, m, StepBackup, StepStateFailed)
	if m.TakeSettingsRequest() {
		t.Fatal("a failed run asks for nothing")
	}
	// Retrying while RC2FX is still away fails the same way.
	m.Privileged = true
	if next := report(t, m, m.Retry(), status(gv1(false), absent(), false, false)); next != nil {
		t.Fatalf("went on without the settings: %#v", next)
	}
	failedRun(t, m)
	// Starting again without RC2FX is the explicit way to go on unsaved.
	m.Stop()
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{offer("1.0.0")}}
	if sel := m.Start(); !isRequest(sel, installer.RequestSelectMixer) {
		t.Fatalf("request %#v", sel)
	}
	if m.Run.NeedsBackup {
		t.Fatal("nothing answered at Start, so there is nothing to save")
	}
	wantState(t, m, StepBackup, StepStateDone)
}

func TestASettingsReadLostWithItsConnectionIsAskedAgain(t *testing.T) {
	m := setUp(status(gv1(false), ready(), false, false))
	if m.Start() != nil {
		t.Fatal("the app reads the settings first")
	}
	if !m.TakeSettingsRequest() {
		t.Fatal("the plan asks for the settings")
	}
	// The reply on the old connection is dropped; while RC2FX is away the
	// read waits, and it is asked again once RC2FX answers.
	m.ConnectionChanged()
	m.SetService(absent())
	if m.TakeSettingsRequest() {
		t.Fatal("asked while RC2FX does not answer")
	}
	m.SetService(ready())
	backup := readSettings(t, m)
	wantRequest(t, backup, installer.Request{Kind: installer.RequestSaveSettings, Serial: "GV1", Settings: `{"config":{}}`}, "backup")
	refresh := answer(t, m, backup, installer.Reply{Kind: installer.ReplyBackup})
	// Once saved, RC2FX stopping no longer matters.
	sel := report(t, m, refresh, status(gv1(false), absent(), false, false))
	if !isRequest(sel, installer.RequestSelectMixer) {
		t.Fatalf("request %#v", sel)
	}
	wantState(t, m, StepBackup, StepStateDone)
}

func TestAFailureStopsAtItsStepAndRetryContinuesFromThere(t *testing.T) {
	m := setUp(status(gv1(true), absent(), true, true))
	m.Run = &Run{
		Index:     4,
		Serial:    "GV1",
		Device:    firmware.ModelProIi,
		BackedUp:  true,
		Selected:  true,
		Prepared:  true,
		Committed: true,
	}
	flash := m.advance()
	if !isRequest(flash, installer.RequestFlash) {
		t.Fatalf("request %#v", flash)
	}
	refresh := m.Finish(*flash, installer.Reply{}, errors.New("drive busy"))
	if !isRequest(refresh, installer.RequestProbe) {
		t.Fatalf("request %#v", refresh)
	}
	report(t, m, refresh, status(gv1(true), absent(), true, true))
	if got := m.State(StepFlash); got != (StepState{Kind: StepStateFailed, Detail: "drive busy"}) {
		t.Fatalf("state %+v", got)
	}
	// A failed page keeps following the mixer, so it can say what it sees.
	poll := m.Tick(time.Now().Add(60 * time.Second))
	if !isRequest(poll, installer.RequestProbe) {
		t.Fatalf("polls while failed: %#v", poll)
	}
	if report(t, m, poll, status(gv1(true), absent(), true, true)) != nil || !m.Failed() {
		t.Fatal("a poll never retries by itself")
	}
	if from, ok := m.RetryFrom(); !ok || from != StepFlash {
		t.Fatalf("retry from %v %v", from, ok)
	}
	poll = m.Retry()
	flash = report(t, m, poll, status(gv1(true), absent(), true, true))
	if !isRequest(flash, installer.RequestFlash) {
		t.Fatalf("retried from the failed step: %#v", flash)
	}
}

// A flash error's advice is to power-cycle the mixer, which brings it back
// in normal mode: Try again switches it to update mode again rather than
// waiting for update mode that never comes, and the run can be stopped
// again because the mixer is as it was.
func TestRetryFollowsTheMixerBackInNormalModeAfterAPowerCycle(t *testing.T) {
	m := setUp(status(gv1(true), absent(), true, true))
	m.Run = &Run{
		Index:     4,
		Serial:    "GV1",
		Device:    firmware.ModelProIi,
		BackedUp:  true,
		Selected:  true,
		Prepared:  true,
		Committed: true,
	}
	flash := m.advance()
	refresh := m.Finish(*flash, installer.Reply{}, errors.New("disconnect the mixer's power for 30 seconds"))
	report(t, m, refresh, status(gv1(false), absent(), true, false))
	if from, ok := m.RetryFrom(); !ok || from != StepUSBMode {
		t.Fatalf("retry from %v %v", from, ok)
	}
	poll := m.Retry()
	if !m.CanCancel() {
		t.Fatal("a mixer back as it was can be stopped again")
	}
	usb := report(t, m, poll, status(gv1(false), absent(), true, false))
	if !isRequest(usb, installer.RequestEnterUSBMode) {
		t.Fatalf("request %#v", usb)
	}
}

// Damaged files have nothing to be restored from on the mixer, so Try again
// sends the firmware again instead of failing on the same check at once.
func TestRetryAfterDamagedFilesFlashesAgain(t *testing.T) {
	m := setUp(status(gv1(false), absent(), true, false))
	m.Run = &Run{Index: slices.Index(Steps[:], StepStart), Serial: "GV1", Device: firmware.ModelProIi,
		BackedUp: true, Selected: true, Prepared: true, Flashed: true, Committed: true}
	m.SetService(ready())
	m.ImageCheckRead(&core.ImageCheck{Status: core.ImageStatusDamaged, Damaged: []string{"a"}}, nil)
	failedRun(t, m)
	if from, _ := m.RetryFrom(); from != StepUSBMode {
		t.Fatalf("retry from %v", from)
	}
	poll := m.Retry()
	if m.Run.Flashed || m.imageCheck.kind != imageCheckUnasked {
		t.Fatal("the damaged flash still counts")
	}
	usb := report(t, m, poll, status(gv1(false), ready(), true, false))
	if !isRequest(usb, installer.RequestEnterUSBMode) {
		t.Fatalf("request %#v", usb)
	}
}

// A failure before the firmware was ready starts again from getting it.
func TestRetryAfterADownloadFailureStartsAtTheDownload(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	sel := m.Start()
	prepare := selected(t, m, sel)
	refresh := m.Finish(*prepare, installer.Reply{}, errors.New("offline"))
	report(t, m, refresh, status(gv1(false), absent(), false, false))
	if from, _ := m.RetryFrom(); from != StepPrepare {
		t.Fatalf("retry from %v", from)
	}
	if m.Run.Committed {
		t.Fatal("nothing was sent to the mixer")
	}
}

// The wait for a power cycle is timed from when it was asked: the flash,
// the restarts and the file check before it don't use up its time.
func TestAPowerCycleWaitIsTimedFromItsRequest(t *testing.T) {
	m := setUp(status(gv1(false), absent(), true, false))
	m.Run = &Run{Index: slices.Index(Steps[:], StepStart), Serial: "GV1", Device: firmware.ModelProIi,
		BackedUp: true, Selected: true, Prepared: true, Flashed: true, Committed: true,
		status: RunStatus{Kind: RunWaiting, Why: "updating", Since: time.Now().Add(-19 * time.Minute)}}
	m.SetService(ready())
	m.ImageCheckRead(&core.ImageCheck{Status: core.ImageStatusRepaired, Repaired: []string{"a"}}, nil)
	w := m.Run.Status()
	if w.Kind != RunWaiting || !w.PowerCycle || time.Since(w.Since) > time.Minute {
		t.Fatalf("waiting %+v", w)
	}
	if !strings.Contains(w.Why, "1 of RØDE's files") {
		t.Fatalf("why %q", w.Why)
	}
}

// Every wait gets a way out: after TakingLongerAfter with no answer the run
// can be stopped; before that, past the point of no return, it can't.
func TestALongWaitCanBeStopped(t *testing.T) {
	m := setUp(status(nil, absent(), true, false))
	m.Run = &Run{Index: slices.Index(Steps[:], StepStart), Serial: "GV1", Device: firmware.ModelProIi,
		BackedUp: true, Selected: true, Prepared: true, Flashed: true, Committed: true,
		status: RunStatus{Kind: RunWaiting, Why: "updating", Since: time.Now()}}
	now := time.Now()
	if m.TakingLonger(now) || m.CanStop(now) {
		t.Fatal("stoppable at once")
	}
	later := now.Add(TakingLongerAfter)
	if !m.TakingLonger(later) || !m.CanStop(later) {
		t.Fatal("no way out of a long wait")
	}
	// RC2FX answering but its check not finishing is still a wait.
	m.SetService(ready())
	if !m.TakingLonger(later) {
		t.Fatal("no way out while RC2FX answers")
	}
	m.Run.resume()
	if m.TakingLonger(later) || m.CanStop(later) {
		t.Fatal("only a wait takes longer")
	}
}

func recentFlash(s Seen) Seen {
	s.status.Saved.Commands = append(s.status.Saved.Commands, installer.CommandRecord{
		Command: "flash",
		AtUnix:  nowUnix() - 60,
	})
	return s
}

func TestARecentFlashResumesByWaitingRatherThanFlashingAgain(t *testing.T) {
	s := recentFlash(status(nil, absent(), true, false))
	m := NewModel()
	poll := m.Probe()
	wantRequest(t, report(t, m, poll, s), installer.Request{Kind: installer.RequestReleases},
		"readiness needs the catalog, and only that is requested")
	if !m.Running() {
		t.Fatal("resumed on opening")
	}
	wantState(t, m, StepStart, StepStateWaiting)
	wantState(t, m, StepFlash, StepStateDone)
}

func TestAResumedRunThatCouldNotGetTheCatalogCanRetryIt(t *testing.T) {
	s := recentFlash(status(nil, absent(), true, false))
	m := NewModel()
	poll := m.Probe()
	check := report(t, m, poll, s)
	if check == nil {
		t.Fatal("no request")
	}
	stock := m.Finish(*check, installer.Reply{}, errors.New("offline"))
	wantRequest(t, stock, installer.Request{Kind: installer.RequestStockReleases}, "stock")
	answer(t, m, stock, installer.Reply{Kind: installer.ReplyStockReleases, Releases: nil})
	if !strings.Contains(m.Error, "offline") || !m.Running() {
		t.Fatalf("error %q, running %v", m.Error, m.Running())
	}
	m.Run.fail("did not get there in time")
	poll = m.Retry()
	seen := Seen{status: deepCopy(*m.Status), service: nil}
	wantRequest(t, report(t, m, poll, seen), installer.Request{Kind: installer.RequestReleases}, "retry")
}

func TestAFailedFirmwareCheckSaysWhyAndOnlyRepeatsWhenAsked(t *testing.T) {
	m := NewModel()
	poll := m.Probe()
	seen := func(m *Model) Seen {
		s := status(nil, nil, false, false).status
		if m.Status != nil {
			s = deepCopy(*m.Status)
		}
		return Seen{status: s, service: nil}
	}
	s := seen(m)
	check := report(t, m, poll, s)
	wantRequest(t, check, installer.Request{Kind: installer.RequestReleases}, "check")
	if m.FirmwareCheck != FirmwareCheckChecking {
		t.Fatalf("check %v", m.FirmwareCheck)
	}
	stock := m.Finish(*check, installer.Reply{}, errors.New("the signed catalog expired"))
	if !strings.Contains(m.Error, "the signed catalog expired") {
		t.Fatalf("error %q", m.Error)
	}
	if len(m.Releases) != 0 || m.CanStart() {
		t.Fatal("releases without a catalog")
	}
	// Stock firmware is RØDE's, so it is still looked for.
	wantRequest(t, stock, installer.Request{Kind: installer.RequestStockReleases}, "stock")
	answer(t, m, stock, installer.Reply{Kind: installer.ReplyStockReleases, Releases: []installer.Release{{
		Channel:   "stable",
		Version:   "1.7.7",
		Supported: true,
	}}})
	wantChoice(t, m.StockChoice, "stable", "1.7.7", "stock choice")

	poll = m.Tick(time.Now().Add(60 * time.Second))
	s = seen(m)
	if report(t, m, poll, s) != nil {
		t.Fatal("polling doesn't retry")
	}

	poll = m.Probe()
	s = seen(m)
	check = report(t, m, poll, s)
	wantRequest(t, check, installer.Request{Kind: installer.RequestReleases}, "Check again does")
	m.Finish(*check, installer.Reply{Kind: installer.ReplyReleases, Releases: nil}, nil)
	if m.Error != "" {
		t.Fatalf("the old failure is gone: %q", m.Error)
	}
}

func TestStockHasItsOwnReleaseAndAFailedCheckCanBeRepeated(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	m.StockReleases = []installer.Release{
		{Channel: "beta", Version: "1.7.8"},
		{Channel: "stable", Version: "1.7.6"},
	}
	m.ChooseStock(0)
	wantChoice(t, m.StockChoice, "beta", "1.7.8", "stock")
	wantChoice(t, m.Choice, "stable", "1.7.6", "RC2FX")
	if !m.CanStartStock() {
		t.Fatal("cannot start stock")
	}
	m.SetStock(true)
	wantChoice(t, m.Chosen(), "beta", "1.7.8", "stock chosen")
	m.SetStock(false)
	wantChoice(t, m.Chosen(), "stable", "1.7.6", "RC2FX chosen")

	m.StockCheck = StockCheck{Kind: StockCheckChecking}
	m.queue = append(m.queue, queued{background: true})
	if m.Finish(installer.Request{Kind: installer.RequestStockReleases}, installer.Reply{}, errors.New("offline")) != nil {
		t.Fatal("request after a failed stock check")
	}
	if m.StockCheck != (StockCheck{Kind: StockCheckFailed, Error: "offline"}) {
		t.Fatalf("stock check %+v", m.StockCheck)
	}
	wantRequest(t, m.RecheckStock(), installer.Request{Kind: installer.RequestStockReleases}, "recheck")
	if m.StockCheck != (StockCheck{Kind: StockCheckChecking}) {
		t.Fatalf("stock check %+v", m.StockCheck)
	}
}

func TestACanceledFirmwareCheckIsNotReportedAsAFailure(t *testing.T) {
	m := NewModel()
	m.queue = append(m.queue, queued{background: true})
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{}, errors.New(installer.Canceled))
	if m.Error != "The check for supported firmware was canceled." {
		t.Fatalf("error %q", m.Error)
	}
}

func TestAFinishedUpdateStaysFinishedAndTheNextPlanIsFresh(t *testing.T) {
	recent := func(service *installer.Service) Seen {
		return recentFlash(status(gv1(false), service, true, false))
	}
	m := NewModel()
	poll := m.Probe()
	// RC2FX answers, so no update is under way whatever was flashed last:
	// nothing resumes.
	if pending := report(t, m, poll, recent(ready())); pending != nil {
		report(t, m, pending, recent(ready()))
	}
	if m.Finished() || m.Run != nil {
		t.Fatal("a run resumed while RC2FX answers")
	}
	m.SetStock(true)
	poll = m.Tick(time.Now().Add(60 * time.Second))
	report(t, m, poll, recent(ready()))
	if !m.Stock {
		t.Fatal("a refresh keeps the chosen mode")
	}
	if m.Run != nil {
		t.Fatal("a run")
	}
	wantState(t, m, StepBackup, StepStatePending)
	wantState(t, m, StepFlash, StepStatePending)
}

// afterStockFlash is a status as it is just after flashing stock firmware.
func afterStockFlash(usb []usbSeen) Seen {
	s := status(usb, absent(), true, false)
	s.status.Saved.Prepared.Stock = true
	return recentFlash(s)
}

// reopen opens a new installer on s, as opening the installer does, with
// the firmware check already answered.
func reopen(t *testing.T, settledFlash *uint64, s Seen) *Model {
	t.Helper()
	m := setUp(status(nil, absent(), false, false))
	m.Status = nil
	m.SettledFlash = settledFlash
	poll := m.Probe()
	report(t, m, poll, s)
	return m
}

func TestReopeningAfterAFinishedStockRunStartsClean(t *testing.T) {
	// Opened while the mixer restarts: the run is picked up and finishes.
	m := reopen(t, nil, afterStockFlash(nil))
	if !m.Stock || m.Run == nil {
		t.Fatal("an update in progress resumes")
	}
	poll := m.Probe()
	report(t, m, poll, afterStockFlash(gv1(false)))
	if !m.Finished() || m.Run != nil {
		t.Fatal("not finished")
	}
	// Left, then opened again, even with the mixer restarting once more.
	for _, usb := range [][]usbSeen{nil, gv1(false)} {
		m := reopen(t, m.SettledFlash, afterStockFlash(usb))
		if m.Stock {
			t.Fatal("RC2FX firmware is selected again")
		}
		if m.Run != nil || m.Finished() || m.LastRun() != nil {
			t.Fatal("not clean")
		}
		wantState(t, m, StepFlash, StepStatePending)
	}
}

func TestARestartedAppDoesNotBringBackAFinishedStockRun(t *testing.T) {
	m := reopen(t, nil, afterStockFlash(gv1(false)))
	if m.Stock {
		t.Fatal("stock")
	}
	if m.Run != nil || m.Finished() || m.LastRun() != nil {
		t.Fatal("not clean")
	}
}

func TestStockNeedsNoMixerSoftware(t *testing.T) {
	m := setUp(status(gv1(false), absent(), false, false))
	m.Privileged = true
	m.Releases = []installer.Release{{Channel: "stable", Version: "1.7.6", Supported: true}}
	m.Choice = &Choice{Channel: "stable", Version: "1.7.6"}
	m.Mixer = nil
	if m.CanStart() {
		t.Fatal("can start")
	}
	if !m.CanStartStock() {
		t.Fatal("cannot start stock")
	}
}

func TestANewerFlashStillResumesAfterAnOlderOneFinished(t *testing.T) {
	s := afterStockFlash(nil)
	older := s.status.Saved.LastCommand().AtUnix - 1
	m := reopen(t, &older, s)
	if !m.Stock || m.Run == nil {
		t.Fatal("not resumed")
	}
}

func TestBackgroundRefreshesNeverDisableThePage(t *testing.T) {
	m := setUp(status(nil, ready(), false, false))
	poll := m.Tick(time.Now())
	if !isRequest(poll, installer.RequestProbe) {
		t.Fatalf("request %#v", poll)
	}
	if m.Busy() {
		t.Fatal("a refresh is not busy")
	}
	if !m.CanStart() {
		t.Fatal("cannot start")
	}
	if m.Tick(time.Now().Add(60*time.Second)) != nil {
		t.Fatal("one refresh at a time")
	}
}

func TestARunIsInOneStatusAtATime(t *testing.T) {
	var run Run
	start := time.Now()
	if since := run.wait("Connect the mixer.", false, start); !since.Equal(start) {
		t.Fatalf("since %v", since)
	}
	// Another reason is the same wait for the mixer.
	if since := run.wait("Waiting for the update drive.", false, start.Add(time.Minute)); !since.Equal(start) {
		t.Fatalf("a new reason restarted the wait: %v", since)
	}
	if s := run.Status(); s.Kind != RunWaiting || s.Why != "Waiting for the update drive." {
		t.Fatalf("status %+v", s)
	}
	run.fail("broke")
	if s := run.Status(); s != (RunStatus{Kind: RunFailed, Why: "broke"}) {
		t.Fatalf("a failure keeps nothing of the wait: %+v", s)
	}
	run.resume()
	if run.Status().Kind != RunFailed {
		t.Fatal("ending a wait cleared a failure")
	}
	run.retry()
	if run.Status() != (RunStatus{}) {
		t.Fatalf("retry %+v", run.Status())
	}
	run.cancel()
	run.resume()
	run.retry()
	if run.Status().Kind != RunCanceling {
		t.Fatal("a canceling run went on")
	}
}

func TestMixerSoftwareWithoutTheImageCheckHasNothingToWaitFor(t *testing.T) {
	m := setUp(status(gv1(false), ready(), true, false))
	if m.imageChecked() {
		t.Fatal("unasked counts as checked")
	}
	m.imageCheck = imageCheck{kind: imageCheckUnsupported}
	if !m.imageChecked() {
		t.Fatal("no check to wait for")
	}
	for status, want := range map[core.ImageStatus]bool{
		core.ImageStatusOk:       true,
		core.ImageStatusNone:     true,
		core.ImageStatusPending:  false,
		core.ImageStatusRepaired: false,
		core.ImageStatusDamaged:  false,
	} {
		m.imageCheck = imageCheck{kind: imageCheckAnswered, result: core.ImageCheck{Status: status}}
		if m.imageChecked() != want {
			t.Fatalf("%v checked %v", status, !want)
		}
	}
	m.SetService(nil)
	if m.imageCheck.kind != imageCheckUnasked {
		t.Fatal("kept while RC2FX is not running")
	}
}

func TestThePageIsBusyExactlyWhileItsOwnRequestIsOut(t *testing.T) {
	m := setUp(status(nil, ready(), false, false))
	refresh := m.Tick(time.Now())
	probe := m.Probe()
	if !isRequest(probe, installer.RequestProbe) || !m.Busy() {
		t.Fatal("the page waits on its own request")
	}
	if m.Probe() != nil {
		t.Fatal("one request the page waits on at a time")
	}
	// The runner answers in order: the refresh sent first is not the one
	// the page waits on.
	m.Finish(*refresh, installer.Reply{}, errors.New("offline"))
	if !m.Busy() {
		t.Fatal("a background answer ended the page's request")
	}
	m.Finish(*probe, installer.Reply{}, errors.New("offline"))
	if m.Busy() {
		t.Fatal("still busy after the answer")
	}
}

func TestUnknownCachedChoiceCannotStartBeforeCatalogDiscovery(t *testing.T) {
	m := setUp(status(gv1(false), absent(), true, false))
	m.Choice = &Choice{Channel: "stable", Version: "1.8.0"}
	if m.CanStart() || m.Start() != nil {
		t.Fatal("started with an unknown choice")
	}
	m.Choice = &Choice{Channel: "stable", Version: "1.7.6"}
	m.Releases = nil
	if m.CanStart() || m.Start() != nil {
		t.Fatal("started without a catalog")
	}
}

func idle() *Model {
	return setUp(status(gv1(false), absent(), false, false))
}

func twoFirmwares() *Model {
	m := idle()
	m.Releases = append(m.Releases, installer.Release{
		Channel:   "stable",
		Version:   "1.7.7",
		Profile:   "rodecaster-pro-ii-1.7.7",
		Supported: true,
	})
	return m
}

func TestTheNewestOfferForTheChosenFirmwareIsTheDefault(t *testing.T) {
	m := twoFirmwares()
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{
		offerFor("2.0.0", "rodecaster-pro-ii-1.7.7"),
		offer("1.0.0"),
	}}
	m.settleMixer()
	if !sameSource(m.Mixer, signed("1.0.0")) {
		t.Fatalf("mixer %+v", m.Mixer)
	}
	m.Choose(1)
	if m.Mixer == nil || m.Mixer.Version() != "2.0.0" {
		t.Fatalf("a choice for other firmware doesn't survive changing firmware: %+v", m.Mixer)
	}
}

func TestOffersThisAppCannotUseAreNotChoices(t *testing.T) {
	m := idle()
	newerApp := offer("3.0.0")
	newerApp.Metadata.MinimumAppVersion = "99.0.0"
	protocol := offer("2.0.0")
	protocol.Metadata.ProtocolVersion++
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{newerApp, protocol}}
	m.settleMixer()
	if len(m.MixerOptions()) != 0 || m.Mixer != nil {
		t.Fatal("an unusable offer is a choice")
	}
	if m.CanStart() {
		t.Fatal("can start")
	}
	note, ok := m.MixerNote()
	if !ok || !strings.Contains(note, "99.0.0") {
		t.Fatalf("note %q", note)
	}
}

func TestFirmwareForANewerAppIsExplainedAndCanNeverBeChosen(t *testing.T) {
	m := NewModel()
	seen := status(gv1(false), nil, false, false)
	m.Status = &seen.status
	m.queue = append(m.queue, queued{background: true})
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: []installer.Release{
		{
			Channel:           "stable",
			Version:           "1.7.8",
			Profile:           "rodecaster-pro-ii-1.7.8",
			MinimumAppVersion: "99.0.0",
		},
		{
			Channel:   "stable",
			Version:   "1.7.6",
			Profile:   profile,
			Supported: true,
		},
	}}, nil)
	wantChoice(t, m.Choice, "stable", "1.7.6", "choice")
	if got := count(m.ReleasesFor()); got != 1 {
		t.Fatalf("%d releases", got)
	}
	note, ok := m.FirmwareNote()
	if !ok || !strings.Contains(note, "1.7.8") || !strings.Contains(note, "99.0.0") {
		t.Fatalf("note %q", note)
	}
	if !strings.Contains(note, "Update the app first") {
		t.Fatalf("note %q", note)
	}
}

func TestWhenEveryFirmwareNeedsANewerAppThePageStillSaysSo(t *testing.T) {
	m := NewModel()
	seen := status(gv1(false), nil, false, false)
	m.Status = &seen.status
	m.queue = append(m.queue, queued{background: true})
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: []installer.Release{{
		Channel:           "stable",
		Version:           "1.7.8",
		MinimumAppVersion: "99.0.0",
	}}}, nil)
	if len(m.Releases) != 0 || m.CanStart() {
		t.Fatal("a release for a newer app is offered")
	}
	if _, ok := m.FirmwareNote(); !ok {
		t.Fatal("no note")
	}
}

func TestFirmwareHeldBackForAnotherReasonIsNotBlamedOnTheApp(t *testing.T) {
	m := NewModel()
	m.queue = append(m.queue, queued{background: true})
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: []installer.Release{{
		Version:           "1.7.8",
		MinimumAppVersion: "0.0.1",
	}}}, nil)
	if note, ok := m.FirmwareNote(); ok {
		t.Fatalf("note %q", note)
	}
}

func TestANewerOfferForANewerAppIsMentionedBesideWhatCanBeInstalled(t *testing.T) {
	m := idle()
	newer := offer("2.0.0")
	newer.Metadata.MinimumAppVersion = "99.0.0"
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{newer, offer("1.0.0")}}
	m.settleMixer()
	if !sameSource(m.Mixer, signed("1.0.0")) {
		t.Fatalf("mixer %+v", m.Mixer)
	}
	if !m.CanStart() {
		t.Fatal("cannot start")
	}
	note, ok := m.MixerNote()
	if !ok || !strings.Contains(note, "2.0.0") || !strings.Contains(note, "99.0.0") {
		t.Fatalf("note %q", note)
	}
}

func TestAFailedCheckNeverFallsBackToTheBundledCopy(t *testing.T) {
	m := idle()
	m.Bundled = new("0.9.0")
	m.Mixer = nil
	m.Offers = Offers{Kind: OffersChecking}
	m.queue = append(m.queue, queued{background: true})
	m.Finish(installer.Request{Kind: installer.RequestMixerReleases}, installer.Reply{}, errors.New("no configured trust root"))
	if m.Mixer != nil {
		t.Fatal("nothing is picked for the user")
	}
	if m.CanStart() {
		t.Fatal("can start")
	}
	note, ok := m.MixerNote()
	if !ok || !strings.Contains(note, "no configured trust root") || !strings.Contains(note, "0.9.0") {
		t.Fatalf("note %q", note)
	}
	m.ChooseMixer(Bundled("0.9.0"))
	if !m.CanStart() {
		t.Fatal("the user may choose it by hand")
	}
	wantRequest(t, m.Start(), installer.Request{Kind: installer.RequestSelectBundledMixer}, "start")
}

func TestTheMenuSaysWhereEachVersionComesFromAndWhatGoesBackwards(t *testing.T) {
	m := idle()
	m.Bundled = new("0.9.0")
	m.SetInstalled(new("0.9.5"), nil)
	type entry struct {
		name   string
		chosen bool
	}
	var menu []entry
	for _, item := range m.MixerMenu() {
		menu = append(menu, entry{item.Name, item.Chosen})
	}
	want := []entry{
		{"1.0.0", true},
		{"0.9.0 · included with this app · older than installed", false},
	}
	if !reflect.DeepEqual(menu, want) {
		t.Fatalf("menu %+v", menu)
	}
}

func TestABuildWithoutAPayloadOffersNoBundledCopy(t *testing.T) {
	m := idle()
	m.ChooseMixer(Bundled("0.9.0"))
	if !sameSource(m.Mixer, signed("1.0.0")) {
		t.Fatalf("mixer %+v", m.Mixer)
	}
	for _, o := range m.MixerOptions() {
		if o.Kind != MixerSourceSigned {
			t.Fatalf("option %+v", o)
		}
	}
}

func TestAFailedOrCanceledCheckCanBeRepeatedAndNeverRepeatsItself(t *testing.T) {
	m := idle()
	m.Offers = Offers{Kind: OffersFailed, Error: installer.Canceled}
	if note, ok := m.MixerNote(); !ok || !strings.Contains(note, "canceled") {
		t.Fatalf("note %q", note)
	}
	if m.checkMixers() != nil {
		t.Fatal("polling doesn't retry by itself")
	}
	wantRequest(t, m.RecheckMixers(), installer.Request{Kind: installer.RequestMixerReleases}, "recheck")
	if !m.Offers.Equal(Offers{Kind: OffersChecking}) {
		t.Fatalf("offers %+v", m.Offers)
	}
	if m.RecheckMixers() != nil {
		t.Fatal("rechecked twice")
	}
}

func TestADowngradeIsNeverTheDefaultAndIsAlwaysCalledOut(t *testing.T) {
	m := idle()
	m.SetInstalled(new("1.1.0"), nil)
	if m.Mixer != nil {
		t.Fatal("only an older offer: nothing is picked")
	}
	if m.CanStart() {
		t.Fatal("can start")
	}
	if note, ok := m.MixerNote(); !ok || !strings.Contains(note, "1.1.0") {
		t.Fatalf("note %q", note)
	}
	older := *signed("1.0.0")
	if got := m.MixerChange(older); got != MixerChangeDowngrade {
		t.Fatalf("change %v", got)
	}
	m.ChooseMixer(older)
	if !m.CanStart() {
		t.Fatal("the user may choose it by hand")
	}
	warnings := m.WarningsFor(m.Stock)
	if len(warnings) == 0 || !strings.Contains(warnings[0], "1.1.0") || !strings.Contains(warnings[0], "older 1.0.0") {
		t.Fatalf("warnings %q", warnings)
	}
	m.SetStock(true)
	if w := m.WarningsFor(m.Stock); len(w) != 0 {
		t.Fatalf("stock replaces RC2FX on purpose: %q", w)
	}
}

func TestAnUnknownInstalledVersionIsSaidOnlyForAnOlderChoice(t *testing.T) {
	m := idle()
	if w := m.WarningsFor(m.Stock); len(w) != 0 {
		t.Fatalf("the newest version can't go backwards: %q", w)
	}
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{offer("1.1.0"), offer("1.0.0")}}
	m.settleMixer()
	if w := m.WarningsFor(m.Stock); len(w) != 0 {
		t.Fatalf("newest of two: %q", w)
	}
	m.ChooseMixer(*signed("1.0.0"))
	if w := m.WarningsFor(m.Stock); len(w) == 0 || !strings.Contains(w[0], "can't tell") {
		t.Fatalf("warnings %q", w)
	}
	m.SetInstalled(new("not a version"), nil)
	if w := m.WarningsFor(m.Stock); len(w) == 0 || !strings.Contains(w[0], "can't tell") {
		t.Fatalf("warnings %q", w)
	}
	m.SetInstalled(new("0.9.0"), nil)
	if w := m.WarningsFor(m.Stock); len(w) != 0 {
		t.Fatalf("warnings %q", w)
	}
	m.SetInstalled(new("1.0.0"), nil)
	if w := m.WarningsFor(m.Stock); len(w) != 0 {
		t.Fatalf("reinstalling the same version: %q", w)
	}
	if m.Mixer == nil {
		t.Fatal("no mixer")
	}
}

func TestGoingBackOnFirmwareIsSaidToo(t *testing.T) {
	m := idle()
	m.SetInstalled(new("0.9.0"), new("1.7.7"))
	warnings := m.WarningsFor(m.Stock)
	if len(warnings) != 1 {
		t.Fatalf("warnings %q", warnings)
	}
	if !strings.Contains(warnings[0], "RØDE firmware 1.7.7") {
		t.Fatalf("warnings %q", warnings)
	}
	m.SetStock(true)
	if got := m.WarningsFor(m.Stock); !reflect.DeepEqual(got, warnings) {
		t.Fatalf("warnings %q", got)
	}
}

func TestARunInstallsWhatWasReviewedWhateverChangesOnThePage(t *testing.T) {
	m := idle()
	sel := m.Start()
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{offer("2.0.0")}}
	m.SetInstalled(new("3.0.0"), nil)
	pinned := m.Run.Mixer
	if !sameSource(pinned, signed("1.0.0")) {
		t.Fatalf("pinned %+v", pinned)
	}
	if !sameSource(m.Mixer, pinned) {
		t.Fatal("not re-chosen during a run")
	}
	if next := selected(t, m, sel); !isRequest(next, installer.RequestPrepare) {
		t.Fatalf("request %#v", next)
	}
}

func TestAFailedRunKeepsItsModeAndFirmwareUntilStopped(t *testing.T) {
	m := idle()
	m.Releases = append(m.Releases, installer.Release{
		Channel:   "beta",
		Version:   "1.8.0",
		Profile:   profile,
		Supported: true,
	})
	prepare := selected(t, m, m.Start())
	if !isRequest(prepare, installer.RequestPrepare) {
		t.Fatalf("request %#v", prepare)
	}
	m.Finish(*prepare, installer.Reply{}, errors.New("offline"))
	m.queue = nil
	if !m.Failed() {
		t.Fatal("not failed")
	}
	// The page offers no firmware choices during a run, failed or not.
	m.Choose(1)
	m.ChooseStock(0)
	if m.Run == nil || m.Stock {
		t.Fatal("a failed run was dropped or changed mode")
	}
	wantChoice(t, m.Choice, "stable", "1.7.6", "a failed run's choice")
	// What the page shows can still change (a catalog reply, say); the
	// run goes on with what was reviewed.
	m.Choice = &Choice{Channel: "beta", Version: "1.8.0"}
	poll := m.Retry()
	again := report(t, m, poll, status(gv1(false), absent(), false, false))
	wantRequest(t, again, *prepare, "retry prepares the reviewed firmware")
	m.Finish(*again, installer.Reply{}, errors.New("offline"))
	m.queue = nil
	// Choosing a mode (the notice's Install update, or the mode tabs)
	// stops a failed run and starts over from the page.
	m.SetStock(true)
	if m.Run != nil || !m.Stock {
		t.Fatal("choosing a mode stops a failed run")
	}
}

func TestAFailedSelectionStopsBeforePreparingAndRetryRepeatsTheSameOffer(t *testing.T) {
	m := idle()
	sel := m.Start()
	if sel == nil {
		t.Fatal("no request")
	}
	refresh := m.Finish(*sel, installer.Reply{}, errors.New("the signature is invalid"))
	s := status(gv1(false), absent(), false, false)
	if report(t, m, refresh, s) != nil {
		t.Fatal("went on after the failure")
	}
	if got := m.State(StepMixer); got != (StepState{Kind: StepStateFailed, Detail: "the signature is invalid"}) {
		t.Fatalf("state %+v", got)
	}
	wantState(t, m, StepPrepare, StepStatePending)
	poll := m.Retry()
	again := report(t, m, poll, status(gv1(false), absent(), false, false))
	wantRequest(t, again, *sel, "retry")
}

func runningRC2FX(installed string) *Model {
	m := setUp(status(gv1(false), ready(), false, false))
	m.Offers = Offers{Kind: OffersReady, List: []installer.MixerOffer{offer("2.0.0")}}
	m.SetInstalled(new(installed), new("1.7.6"))
	return m
}

var inPlace = InPlaceSupport{Supported: true, Profile: new(profile)}

func TestANewerRC2FXOnTheSameFirmwareInstallsInPlace(t *testing.T) {
	m := runningRC2FX("1.0.0")
	if got := m.InPlaceOffer(inPlace); got == nil || !offerEqual(*got, offer("2.0.0")) {
		t.Fatalf("offer %+v", got)
	}
	unsupported := inPlace
	unsupported.Supported = false
	if m.InPlaceOffer(unsupported) != nil {
		t.Fatal("older mixer software")
	}
	otherFirmware := inPlace
	otherFirmware.Profile = new("rodecaster-pro-ii-1.7.7")
	if m.InPlaceOffer(otherFirmware) != nil {
		t.Fatal("a release for other firmware needs a flash")
	}
	unknown := inPlace
	unknown.Profile = nil
	if m.InPlaceOffer(unknown) != nil {
		t.Fatal("unknown firmware")
	}
	if runningRC2FX("2.0.0").InPlaceOffer(inPlace) != nil {
		t.Fatal("nothing newer to install")
	}
	stock := runningRC2FX("1.0.0")
	stock.SetStock(true)
	if stock.InPlaceOffer(inPlace) != nil {
		t.Fatal("stock")
	}
}

func TestAConnectionChangeFailsAnUploadButNotAnInstall(t *testing.T) {
	m := runningRC2FX("1.0.0")
	m.inPlace = InPlace{Kind: InPlaceUploading, Version: "2.0.0", Sent: 3, Total: 7}
	m.ConnectionChanged()
	if !reflect.DeepEqual(m.InPlace(), InPlace{Kind: InPlaceFailed, Error: inPlaceDisconnected}) {
		t.Fatalf("in place %+v", m.InPlace())
	}
	if m.InPlace().Active() {
		t.Fatal("a failed upload blocks another update")
	}
	// An accepted update goes on without the app; the reconnect is how its
	// outcome is read.
	since := time.Now()
	installing := InPlace{Kind: InPlaceInstalling, Version: "2.0.0", Since: since}
	m.inPlace = installing
	m.ConnectionChanged()
	if !reflect.DeepEqual(m.InPlace(), installing) {
		t.Fatalf("in place %+v", m.InPlace())
	}
	// A release verified but not sent yet goes out on the next connection.
	ready := InPlace{Kind: InPlaceReady, Bundle: installer.InPlaceBundle{Version: "2.0.0"}}
	m.inPlace = ready
	m.ConnectionChanged()
	if !reflect.DeepEqual(m.InPlace(), ready) {
		t.Fatalf("in place %+v", m.InPlace())
	}
}

func TestAnInPlaceUpdateIsPreparedOnceAndFailuresChangeNothing(t *testing.T) {
	m := runningRC2FX("1.0.0")
	prepare := m.StartInPlace(offer("2.0.0"), profile)
	if !isRequest(prepare, installer.RequestInPlaceBundle) {
		t.Fatalf("request %#v", prepare)
	}
	if !m.inPlace.Active() {
		t.Fatal("not active")
	}
	if m.StartInPlace(offer("2.0.0"), profile) != nil {
		t.Fatal("one at a time")
	}
	bundle := installer.InPlaceBundle{
		Version: "2.0.0",
		Size:    7,
		SHA256:  strings.Repeat("00", 32),
		Path:    "payload",
		Metadata: installer.RawMetadata{
			Root:      []string{"{}"},
			Timestamp: "{}",
			Snapshot:  "{}",
			Targets:   "{}",
		},
		Base: nil,
	}
	m.Finish(*prepare, installer.Reply{Kind: installer.ReplyInPlaceBundle, InPlaceBundle: new(deepCopy(bundle))}, nil)
	if got, ok := m.TakeInPlaceBundle(); !ok || !reflect.DeepEqual(got, bundle) {
		t.Fatalf("bundle %+v %v", got, ok)
	}
	uploading := func() bool {
		return m.inPlace.Kind == InPlaceUploading && m.inPlace.Sent == 0 && m.inPlace.Total == 7
	}
	if !uploading() {
		t.Fatalf("in place %+v", m.inPlace)
	}
	if _, ok := m.TakeInPlaceBundle(); ok {
		t.Fatal("handed over once")
	}
	if !uploading() {
		t.Fatal("asking again leaves the update in progress")
	}
	m.inPlace = InPlace{Kind: InPlaceFailed, Error: "refused"}
	if _, ok := m.TakeInPlaceBundle(); ok {
		t.Fatal("handed over after a failure")
	}
	if !reflect.DeepEqual(m.inPlace, InPlace{Kind: InPlaceFailed, Error: "refused"}) {
		t.Fatalf("in place %+v", m.inPlace)
	}

	m = runningRC2FX("1.0.0")
	prepare = m.StartInPlace(offer("2.0.0"), profile)
	if prepare == nil {
		t.Fatal("no request")
	}
	m.Finish(*prepare, installer.Reply{}, errors.New("offline"))
	if m.inPlace.Kind != InPlaceFailed || !strings.Contains(m.inPlace.Error, "Nothing changed") {
		t.Fatalf("in place %+v", m.inPlace)
	}
	if m.inPlace.Active() {
		t.Fatal("active")
	}
}

func TestStoppingAfterAFailureReviewsWhatIsPublishedNow(t *testing.T) {
	m := idle()
	sel := m.Start()
	if sel == nil {
		t.Fatal("no request")
	}
	m.Finish(*sel, installer.Reply{}, errors.New("the selected mixer release changed"))
	m.queue = nil
	m.Stop()
	if !m.Offers.Equal(Offers{Kind: OffersUnchecked}) {
		t.Fatalf("offers %+v", m.Offers)
	}
	poll := m.Probe()
	check := report(t, m, poll, status(gv1(false), absent(), false, false))
	wantRequest(t, check, installer.Request{Kind: installer.RequestMixerReleases}, "check")
	m.Finish(*check, installer.Reply{Kind: installer.ReplyMixerReleases, Offers: []installer.MixerOffer{offer("1.2.0")}}, nil)
	if !sameSource(m.Mixer, signed("1.2.0")) {
		t.Fatalf("mixer %+v", m.Mixer)
	}
}

func TestASelectionCanBeCanceledAndTouchesNothing(t *testing.T) {
	m := idle()
	sel := m.Start()
	if sel == nil {
		t.Fatal("no request")
	}
	if !m.CanCancel() {
		t.Fatal("cannot cancel")
	}
	if !m.Cancel() {
		t.Fatal("the download is asked to stop")
	}
	refresh := m.Finish(*sel, installer.Reply{}, errors.New(installer.Canceled))
	if m.Run != nil {
		t.Fatal("a run")
	}
	if !isRequest(refresh, installer.RequestProbe) {
		t.Fatalf("request %#v", refresh)
	}
}

func TestVerifiedSoftwareMustBeTheVersionThatWasReviewed(t *testing.T) {
	m := idle()
	sel := m.Start()
	identity := installer.Identity{Release: "6.6.6"}
	refresh := answer(t, m, sel, installer.Reply{Kind: installer.ReplyMixerSelected, Identity: &identity})
	s := status(gv1(false), absent(), false, false)
	if report(t, m, refresh, s) != nil {
		t.Fatal("went on")
	}
	if got := wantState(t, m, StepMixer, StepStateFailed); !strings.Contains(got.Detail, "6.6.6") {
		t.Fatalf("failure %q", got.Detail)
	}
	wantState(t, m, StepPrepare, StepStatePending)
}

func TestStockFirmwareNeedsNoMixerSoftwareAtAll(t *testing.T) {
	m := idle()
	m.Offers = Offers{Kind: OffersFailed, Error: "offline"}
	m.settleMixer()
	m.SetStock(true)
	if !m.CanStart() {
		t.Fatal("cannot start")
	}
	if p := m.Start(); !isRequest(p, installer.RequestPrepare) || !p.Stock {
		t.Fatal("not a stock preparation")
	}
	if m.Run.Mixer != nil {
		t.Fatalf("mixer %+v", m.Run.Mixer)
	}
}

func TestSignedDiscoveryFollowsTheFirmwareListOnce(t *testing.T) {
	m := NewModel()
	seen := status(nil, nil, false, false)
	m.Status = &seen.status
	m.queue = append(m.queue, queued{background: true})
	check := m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: nil}, nil)
	wantRequest(t, check, installer.Request{Kind: installer.RequestMixerReleases}, "check")
	if m.Tick(time.Now().Add(60*time.Second)) != nil {
		t.Fatal("polling waits for the check")
	}
	m.Finish(*check, installer.Reply{Kind: installer.ReplyMixerReleases, Offers: []installer.MixerOffer{}}, nil)
	if !m.Offers.Equal(Offers{Kind: OffersReady, List: []installer.MixerOffer{}}) {
		t.Fatalf("offers %+v", m.Offers)
	}
	if m.checkMixers() != nil {
		t.Fatal("checked twice")
	}
}

func TestReleasesDefaultToStableAndPreparedFirmwareWins(t *testing.T) {
	m := NewModel()
	seen := status(nil, nil, false, false)
	m.Status = &seen.status
	releases := []installer.Release{
		{Channel: "stable", Version: "1.7.6", Supported: true},
		{Channel: "beta", Version: "1.7.7", Supported: true},
	}
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: deepCopy(releases)}, nil)
	wantChoice(t, m.Choice, "stable", "1.7.6", "default")
	m = NewModel()
	s := status(nil, nil, true, false)
	s.status.Saved.Prepared.Channel = "beta"
	s.status.Saved.Prepared.Version = "1.7.7"
	next := m.Finish(installer.Request{Kind: installer.RequestProbe}, installer.Reply{Kind: installer.ReplyStatus, Status: new(s.status)}, nil)
	wantRequest(t, next, installer.Request{Kind: installer.RequestReleases}, "releases")
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: releases}, nil)
	wantChoice(t, m.Choice, "beta", "1.7.7", "prepared")

	// A cached beta choice must disappear after stable promotion; an unknown
	// or unsupported release must never remain in the menu.
	m.Finish(installer.Request{Kind: installer.RequestReleases}, installer.Reply{Kind: installer.ReplyReleases, Releases: []installer.Release{
		{Channel: "stable", Version: "1.7.7", Supported: true},
		{Channel: "stable", Version: "1.8.0", Supported: false},
	}}, nil)
	wantChoice(t, m.Choice, "stable", "1.7.7", "promoted")
	if len(m.Releases) != 1 {
		t.Fatalf("%d releases", len(m.Releases))
	}
}

// flashedAgo is s with a flash sent secs seconds ago as the last command.
func flashedAgo(s Seen, secs uint64) Seen {
	s.status.Saved.Commands = append(s.status.Saved.Commands, installer.CommandRecord{
		Command: "flash",
		AtUnix:  nowUnix() - secs,
	})
	return s
}

// What the mixer shows wins over the recent-flash window: a mixer seen in
// normal mode, answering as RØDE's firmware or as RC2FX, is not updating;
// only a mixer the app can't see, or one in update mode, may still be.
func TestWhatTheMixerShowsWinsOverARecentFlash(t *testing.T) {
	for _, c := range []struct {
		why      string
		seen     Seen
		updating bool
	}{
		{"stock mixer in normal mode 2 minutes after a flash", flashedAgo(status(gv1(false), absent(), true, false), 120), false},
		{"RC2FX answering 2 minutes after a flash", flashedAgo(status(gv1(false), ready(), true, false), 120), false},
		{"mixer absent 2 minutes after a flash", flashedAgo(status(nil, absent(), true, false), 120), true},
		{"mixer in update mode 2 minutes after a flash", flashedAgo(status(gv1(true), absent(), true, false), 120), true},
		{"mixer absent, no flash", status(nil, absent(), true, false), false},
	} {
		m := setUp(c.seen)
		if got := m.RecentlyFlashed(); got != c.updating {
			t.Errorf("%s: updating %v, want %v", c.why, got, c.updating)
		}
		if u := m.Unplug(); (u.Kind == UnplugNo) != c.updating {
			t.Errorf("%s: unplug %+v", c.why, u)
		}
		// Opening the installer resumes waiting only for a mixer that may
		// still be updating.
		r := reopen(t, nil, Seen{status: deepCopy(c.seen.status), service: c.seen.service})
		if got := r.Run != nil; got != c.updating {
			t.Errorf("%s: opening resumed a run: %v, want %v", c.why, got, c.updating)
		}
	}
}

func TestMayBeUpdating(t *testing.T) {
	normal := []installer.USBMixer{{Serial: "GV1"}}
	update := []installer.USBMixer{{Serial: "GV1", UpdateMode: true}}
	if MayBeUpdating(false, nil, false) {
		t.Error("no flash")
	}
	if !MayBeUpdating(true, nil, false) || !MayBeUpdating(true, update, false) {
		t.Error("a recent flash with no mixer, or one in update mode")
	}
	if MayBeUpdating(true, normal, false) || MayBeUpdating(true, nil, true) {
		t.Error("a mixer in normal mode, or RC2FX answering")
	}
}
