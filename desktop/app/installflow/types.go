// Package installflow is the installer as one plan, independent of the UI.
//
// RC2FX is installed by flashing firmware that contains it, so every install
// or update is the same plan: save the mixer's settings, verify the chosen
// mixer software, build the firmware, switch to USB update mode, flash, and
// wait for RC2FX to answer. Steps the mixer already satisfies are done. One
// start runs the rest in order, waiting for the mixer where it has to and
// stopping at the first error, which can be retried from that step.
//
// A mixer whose RC2FX installs signed releases in place needs no flash for an
// RC2FX update that keeps its RØDE firmware: Model.InPlace tracks that
// shorter path. The app only carries it to and from the mixer over its USB
// connection; what each answer means is decided here (inplace.go).
//
// The model only decides. It returns the installer.Request to run next (nil
// for none); the UI runs it with the installer engine on a worker, one at a time and in the
// order returned, and hands each result back to Model.Finish (see Runner).
package installflow

import (
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Runner is what the UI provides to carry out the requests a Model returns:
// the installer engine (installer.Engine's Handle), on a worker thread. Every
// request a Model method returns must be run exactly once, in the order
// returned, and its result passed to Model.Finish with the same request.
type Runner interface {
	Handle(request installer.Request) (installer.Reply, error)
}

// InPlaceKind is the stage of an RC2FX update installed in place, without a
// flash.
type InPlaceKind int

const (
	InPlaceIdle InPlaceKind = iota
	// InPlacePreparing: downloading and verifying the signed release.
	InPlacePreparing
	// InPlaceReady: verified here; the app sends Bundle to the mixer next.
	InPlaceReady
	// InPlaceUploading: Sent of Total bytes are with the mixer.
	InPlaceUploading
	// InPlaceInstalling: the mixer accepted it, is installing and restarts
	// its audio, Since then.
	InPlaceInstalling
	// InPlaceDone: the mixer reported how it ended.
	InPlaceDone
	// InPlaceFailed: Error says why.
	InPlaceFailed
)

// InPlace is an RC2FX update installed in place, without a flash. Only the
// fields of its Kind are set.
type InPlace struct {
	Kind InPlaceKind
	// Version, for every kind but Idle, Ready and Failed.
	Version string
	// Bundle, for Ready.
	Bundle installer.InPlaceBundle
	// Sent and Total, for Uploading.
	Sent, Total uint64
	// Since, for Installing.
	Since time.Time
	// Committed and Detail, for Done.
	Committed bool
	Detail    string
	// Error, for Failed.
	Error string
}

// Active reports something in flight: nothing else may start meanwhile.
func (p InPlace) Active() bool {
	switch p.Kind {
	case InPlacePreparing, InPlaceReady, InPlaceUploading, InPlaceInstalling:
		return true
	}
	return false
}

// InPlaceSupport is what the mixer says it can do and runs, for
// Model.InPlaceOffer.
type InPlaceSupport struct {
	// Supported: the mixer's RC2FX installs signed releases in place.
	Supported bool
	// Profile is the RØDE firmware profile RC2FX runs on; nil when unknown.
	Profile *string
}

// Step is one step of the plan.
type Step int

const (
	StepBackup Step = iota
	StepMixer
	StepPrepare
	StepUSBMode
	StepFlash
	StepStart
)

// Steps is the plan in order.
var Steps = [6]Step{
	StepBackup,
	StepMixer,
	StepPrepare,
	StepUSBMode,
	StepFlash,
	StepStart,
}

// Goal is what a run achieves.
type Goal int

const (
	// GoalSetup: no RC2FX answers at the mixer's address.
	GoalSetup Goal = iota
	// GoalReinstall: RC2FX answers; flashing again updates or repairs it.
	GoalReinstall
	// GoalStock: flash unchanged RØDE firmware, removing RC2FX.
	GoalStock
)

// MixerSourceKind is where a run's mixer software comes from.
type MixerSourceKind int

const (
	// MixerSourceSigned: a signed offer, Offer.
	MixerSourceSigned MixerSourceKind = iota
	// MixerSourceBundled: the copy built into this app, version Bundled.
	MixerSourceBundled
)

// MixerSource is the mixer software a run installs. Only something the user
// can see in the menu: a signed offer, or the copy built into this app.
type MixerSource struct {
	Kind MixerSourceKind
	// Offer, for MixerSourceSigned.
	Offer installer.MixerOffer
	// Bundled is the version, for MixerSourceBundled.
	Bundled string
}

// Signed is the signed offer as a source.
func Signed(offer installer.MixerOffer) MixerSource {
	return MixerSource{Kind: MixerSourceSigned, Offer: cloneOffer(offer)}
}

// Bundled is the copy built into this app, version, as a source.
func Bundled(version string) MixerSource {
	return MixerSource{Kind: MixerSourceBundled, Bundled: version}
}

// Version is the mixer software version the source installs.
func (s MixerSource) Version() string {
	if s.Kind == MixerSourceSigned {
		return s.Offer.Metadata.Version
	}
	return s.Bundled
}

// Equal compares two sources.
func (s MixerSource) Equal(other MixerSource) bool {
	if s.Kind != other.Kind {
		return false
	}
	if s.Kind == MixerSourceBundled {
		return s.Bundled == other.Bundled
	}
	return offerEqual(s.Offer, other.Offer)
}

func offerEqual(a, b installer.MixerOffer) bool {
	return a.Target == b.Target &&
		a.SHA256 == b.SHA256 &&
		a.Length == b.Length &&
		a.Metadata.Version == b.Metadata.Version &&
		a.Metadata.PayloadFormat == b.Metadata.PayloadFormat &&
		a.Metadata.ProtocolVersion == b.Metadata.ProtocolVersion &&
		a.Metadata.MinimumAppVersion == b.Metadata.MinimumAppVersion &&
		slices.Equal(a.Metadata.FirmwareProfiles, b.Metadata.FirmwareProfiles)
}

func cloneOffer(offer installer.MixerOffer) installer.MixerOffer {
	offer.Metadata.FirmwareProfiles = slices.Clone(offer.Metadata.FirmwareProfiles)
	return offer
}

func containsSource(sources []MixerSource, source MixerSource) bool {
	return slices.ContainsFunc(sources, source.Equal)
}

// FirmwareCheck is the check for supported firmware. It can need the
// network, so it runs once and is repeated only when asked, never by the
// page's polling.
type FirmwareCheck int

const (
	FirmwareCheckUnchecked FirmwareCheck = iota
	FirmwareCheckChecking
	// FirmwareCheckDone: it answered, or failed and said why in Model.Error.
	FirmwareCheckDone
)

// StockCheckKind is the state of the check of RØDE's firmware list.
type StockCheckKind int

const (
	StockCheckUnchecked StockCheckKind = iota
	StockCheckChecking
	StockCheckDone
	// StockCheckFailed: Error says why.
	StockCheckFailed
)

// StockCheck is the check of RØDE's firmware list, for stock firmware. It
// needs the network, so it runs once and is repeated only when asked.
type StockCheck struct {
	Kind  StockCheckKind
	Error string
}

// OffersKind is the state of the signed release check.
type OffersKind int

const (
	OffersUnchecked OffersKind = iota
	OffersChecking
	// OffersReady: List is what it found.
	OffersReady
	// OffersFailed: Error says why.
	OffersFailed
)

// Offers is what the signed release check has found.
type Offers struct {
	Kind  OffersKind
	List  []installer.MixerOffer
	Error string
}

// Equal compares two check states; a nil and an empty list are the same.
func (o Offers) Equal(other Offers) bool {
	return o.Kind == other.Kind &&
		o.Error == other.Error &&
		slices.EqualFunc(o.List, other.List, offerEqual)
}

// MixerChange is how a mixer software version relates to what the mixer
// runs.
type MixerChange int

const (
	// MixerChangeUnknown: the mixer did not report a comparable version.
	MixerChangeUnknown MixerChange = iota
	MixerChangeUpgrade
	MixerChangeSame
	MixerChangeDowngrade
)

// StepStateKind is how far a step is.
type StepStateKind int

const (
	StepStatePending StepStateKind = iota
	StepStateDone
	StepStateRunning
	// StepStateWaiting: Detail says what for.
	StepStateWaiting
	// StepStateFailed: Detail says why.
	StepStateFailed
)

// StepState is how far a step is.
type StepState struct {
	Kind   StepStateKind
	Detail string
}

// Choice is a firmware release, by channel and version.
type Choice struct {
	Channel string
	Version string
}

// RunStatusKind is where a run stands at its current step.
type RunStatusKind int

const (
	// RunGoing: carrying out its steps.
	RunGoing RunStatusKind = iota
	// RunWaiting: waiting for the mixer; Why says what for, since Since.
	RunWaiting
	// RunFailed: stopped at its step; Why says why. Retry goes on from it.
	RunFailed
	// RunCanceling: canceled while a request was in flight; the run ends
	// when it returns.
	RunCanceling
)

// RunStatus is where a run stands at its current step. Only the fields of
// its Kind are set.
type RunStatus struct {
	Kind RunStatusKind
	// Why, for RunWaiting and RunFailed.
	Why string
	// Since, for RunWaiting: when the wait began. A wait that changes its
	// reason keeps it, so the limit covers the whole wait for the mixer.
	Since time.Time
	// PowerCycle: the mixer has to be switched off and on by hand. Its wait
	// is timed on its own, from when that was asked.
	PowerCycle bool
}

// Run is a plan being carried out.
type Run struct {
	// Started is when the run started (or, for one picked up after the app
	// restarted, when it was picked up).
	Started time.Time
	Index   int
	// status is where the run stands; written only by fail, wait, resume,
	// retry and cancel (and ShowStatus, for made-up pages).
	status RunStatus
	// Serial is the mixer this run is for, from USB.
	Serial string
	Device firmware.Model
	// NeedsBackup: RC2FX answered when the run started, so its settings
	// must be saved before anything changes. Decided once, so RC2FX
	// dropping off later never skips the backup silently.
	NeedsBackup bool
	BackedUp    bool
	// Mixer is the mixer software the user reviewed; nil for stock
	// firmware. Held here so nothing that changes on the page can alter a
	// run.
	Mixer *MixerSource
	// Stock and Choice are the mode and the firmware release the user
	// reviewed, held for the same reason; Choice is nil when not known.
	Stock  bool
	Choice *Choice
	// Selected: Mixer was verified and selected for this run.
	Selected bool
	Prepared bool
	Flashed  bool
	// Committed: USB update mode or the flash was requested: past this the
	// mixer is not left as it was, so the run can no longer be canceled.
	Committed bool
	// Fix is the storage fix this run does before installing, nil for a
	// run without it (decided at Start, see WantsStorageFix).
	Fix *RunFix
}

// Status is where the run stands at its current step.
func (r *Run) Status() RunStatus {
	return r.status
}

// ShowStatus sets where the run stands, for pages made from made-up state
// (the app's flow fixtures).
func (r *Run) ShowStatus(status RunStatus) {
	r.status = status
}

// fail stops the run at its step, whatever it was doing.
func (r *Run) fail(why string) {
	r.status = RunStatus{Kind: RunFailed, Why: why}
}

// wait records waiting for the mixer, for why, and returns when the wait
// began. A wait keeps when an earlier one began across a change of reason,
// unless it starts or ends waiting for a power cycle, which is timed on its
// own from when it was asked.
func (r *Run) wait(why string, powerCycle bool, now time.Time) time.Time {
	since := now
	if r.status.Kind == RunWaiting && r.status.PowerCycle == powerCycle {
		since = r.status.Since
	}
	r.status = RunStatus{Kind: RunWaiting, Why: why, Since: since, PowerCycle: powerCycle}
	return since
}

// resume ends a wait; a failed or canceling run stays as it is.
func (r *Run) resume() {
	if r.status.Kind == RunWaiting {
		r.status = RunStatus{}
	}
}

// retry goes on with a failed or waiting run; a canceling run stays as it
// is.
func (r *Run) retry() {
	if r.status.Kind != RunCanceling {
		r.status = RunStatus{}
	}
}

// cancel marks a run canceled while its request is in flight.
func (r *Run) cancel() {
	r.status = RunStatus{Kind: RunCanceling}
}

// imageCheckKind is how far the mixer's check of the files the flash
// unpacked is.
type imageCheckKind int

const (
	// imageCheckUnasked: no answer since RC2FX last started.
	imageCheckUnasked imageCheckKind = iota
	// imageCheckUnsupported: the mixer software predates the check, so
	// there is nothing to wait for.
	imageCheckUnsupported
	// imageCheckAnswered: result is the mixer's answer.
	imageCheckAnswered
)

// imageCheck is the mixer's check of the files the flash unpacked.
type imageCheck struct {
	kind imageCheckKind
	// result, for imageCheckAnswered.
	result core.ImageCheck
}

// RunFix is the storage fix inside an install: RØDE's archive with the
// fix's script, then the run's own firmware on the fixed slot, both in one
// request (installer.RequestStorageFix with Final).
type RunFix struct {
	// Reply is what the fix did, once it ran.
	Reply *installer.StorageFix
	// Error is why the request failed, when it did: the install then goes
	// on with a normal flash.
	Error string
	// Event is the fix's newest progress while it runs.
	Event *installer.Progress
}

// FixPolicy is whether an install fixes the mixer's storage first.
type FixPolicy int

const (
	// FixNever: never. The zero value: a model runs the fix only when the
	// app that runs its requests says so (development: hardware tests that
	// flash repeatedly keep it off).
	FixNever FixPolicy = iota
	// FixAuto: on a Pro II the app can't ask for its partition table, which
	// is one without RC2FX running.
	FixAuto
	// FixAlways: on any Pro II in normal mode (development).
	FixAlways
)

const (
	// updateWindow is how long after a flash the mixer counts as still
	// updating, in seconds.
	updateWindow = 20 * 60
	// waitLimit is how long to wait for the mixer before reporting it.
	waitLimit = 20 * time.Minute
	// TakingLongerAfter is how long a wait with no answer from the mixer
	// lasts before the page says it is taking longer than usual and offers
	// a way out.
	TakingLongerAfter = 6 * time.Minute
)

// SettingsRead is reading RC2FX's settings over the app's USB connection,
// for Save settings.
type SettingsRead int

const (
	SettingsReadIdle SettingsRead = iota
	// SettingsReadWanted: the plan needs them; the app has not asked yet.
	SettingsReadWanted
	// SettingsReadAsked: the app asked; waiting for the reply.
	SettingsReadAsked
)

// MixerMenuItem is one entry of the mixer software menu.
type MixerMenuItem struct {
	Source MixerSource
	Name   string
	Chosen bool
}
