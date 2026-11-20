// Package installflow implements the UI-independent state machine for
// installing, updating, and repairing RC2FX on a mixer.
//
// Full installs follow one linear plan (Steps): save settings, verify the
// chosen mixer software, prepare the firmware image, enter USB update mode,
// flash, and wait for RC2FX to come back up. Steps already satisfied by the
// mixer's current state are skipped, and failures stop at the failing step so
// the user can retry from there.
//
// Mixers running an RC2FX version that supports in-place updates can update
// RC2FX without reflashing when staying on the same RØDE firmware; Model.InPlace
// tracks that path (see inplace.go).
//
// Model is purely a decision engine: methods return the next installer.Request
// to run (or nil). The UI runs each returned request once, in order, via
// installer.Engine.Handle on a worker and feeds the result back to Model.Finish.
package installflow

import (
	"slices"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

type InPlaceKind int

const (
	InPlaceIdle       InPlaceKind = iota
	InPlacePreparing              // downloading and verifying the signed bundle
	InPlaceReady                  // verified locally; waiting for the app to take Bundle
	InPlaceUploading              // streaming to the mixer (Sent / Total)
	InPlaceInstalling             // accepted by the mixer; restarting audio and applying (Since)
	InPlaceDone                   // mixer reported an outcome (Committed / Detail)
	InPlaceFailed                 // failed (Error)
)

// InPlace holds the state of an in-place RC2FX update. Only fields relevant to
// Kind are populated.
type InPlace struct {
	Kind        InPlaceKind
	Version     string
	Bundle      installer.InPlaceBundle
	Sent, Total uint64
	Since       time.Time
	Committed   bool
	Detail      string
	Error       string
}

// Active reports whether an in-place update is in flight.
func (p InPlace) Active() bool {
	switch p.Kind {
	case InPlacePreparing, InPlaceReady, InPlaceUploading, InPlaceInstalling:
		return true
	}
	return false
}

// InPlaceSupport describes whether the connected mixer can apply in-place
// updates and which RØDE firmware profile it is running.
type InPlaceSupport struct {
	Supported bool
	Profile   *string
}

type Step int

const (
	StepBackup Step = iota
	StepMixer
	StepPrepare
	StepUSBMode
	StepFlash
	StepStart
)

// Steps is the full install plan in execution order.
var Steps = [6]Step{
	StepBackup,
	StepMixer,
	StepPrepare,
	StepUSBMode,
	StepFlash,
	StepStart,
}

type Goal int

const (
	GoalSetup     Goal = iota // RC2FX is not answering on the mixer
	GoalReinstall             // RC2FX is answering; flashing updates or repairs it
	GoalStock                 // flashing stock RØDE firmware (removing RC2FX)
)

type MixerSourceKind int

const (
	MixerSourceSigned MixerSourceKind = iota
	MixerSourceBundled
)

// MixerSource identifies the mixer software chosen from the menu: either a
// signed release offer or the copy bundled with this app.
type MixerSource struct {
	Kind    MixerSourceKind
	Offer   installer.MixerOffer
	Bundled string
}

func Signed(offer installer.MixerOffer) MixerSource {
	return MixerSource{Kind: MixerSourceSigned, Offer: cloneOffer(offer)}
}

func Bundled(version string) MixerSource {
	return MixerSource{Kind: MixerSourceBundled, Bundled: version}
}

func (s MixerSource) Version() string {
	if s.Kind == MixerSourceSigned {
		return s.Offer.Metadata.Version
	}
	return s.Bundled
}

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

// FirmwareCheck tracks the one-shot firmware catalog lookup. Because it may
// hit the network, polling never retries it automatically.
type FirmwareCheck int

const (
	FirmwareCheckUnchecked FirmwareCheck = iota
	FirmwareCheckChecking
	FirmwareCheckDone // finished (check Model.Error on failure)
)

type StockCheckKind int

const (
	StockCheckUnchecked StockCheckKind = iota
	StockCheckChecking
	StockCheckDone
	StockCheckFailed
)

// StockCheck tracks the one-shot lookup of RØDE's stock firmware list.
type StockCheck struct {
	Kind  StockCheckKind
	Error string
}

type OffersKind int

const (
	OffersUnchecked OffersKind = iota
	OffersChecking
	OffersReady
	OffersFailed
)

type Offers struct {
	Kind  OffersKind
	List  []installer.MixerOffer
	Error string
}

// Equal compares two Offers values, treating nil and empty slices as equal.
func (o Offers) Equal(other Offers) bool {
	return o.Kind == other.Kind &&
		o.Error == other.Error &&
		slices.EqualFunc(o.List, other.List, offerEqual)
}

type MixerChange int

const (
	MixerChangeUnknown MixerChange = iota
	MixerChangeUpgrade
	MixerChangeSame
	MixerChangeDowngrade
)

type StepStateKind int

const (
	StepStatePending StepStateKind = iota
	StepStateDone
	StepStateRunning
	StepStateWaiting // Detail explains what we're waiting for
	StepStateFailed  // Detail holds the error message
)

type StepState struct {
	Kind   StepStateKind
	Detail string
}

type Choice struct {
	Channel string
	Version string
}

type RunStatusKind int

const (
	RunGoing     RunStatusKind = iota
	RunWaiting                 // waiting on the mixer (Why, Since)
	RunFailed                  // stopped at current step (Why); can retry from here
	RunCanceling               // cancel requested while a request is in flight
)

// RunStatus holds the run's status at its current step.
type RunStatus struct {
	Kind RunStatusKind
	Why  string
	// Since is when the current wait started. Preserved across reason updates
	// so waitLimit applies to the entire wait, except when entering or leaving
	// a PowerCycle wait.
	Since time.Time
	// PowerCycle is true when waiting for the user to power-cycle the mixer.
	PowerCycle bool
}

// Run represents an active or completed install plan.
type Run struct {
	Started time.Time
	Index   int
	// status is mutated only via fail, wait, resume, retry, cancel (and ShowStatus in fixtures).
	status RunStatus
	Serial string
	Device firmware.Model
	// NeedsBackup is latched at Start when RC2FX is answering so a later
	// disconnect fails the run instead of silently skipping the backup.
	NeedsBackup bool
	BackedUp    bool
	// Mixer, Stock, and Choice snapshot what the user reviewed at Start so
	// background catalog updates can't change an in-progress run.
	Mixer    *MixerSource
	Stock    bool
	Choice   *Choice
	Selected bool
	Prepared bool
	Flashed  bool
	// Committed is set once we request USB update mode or a flash; past this
	// point the mixer may have changed and the run can no longer be canceled.
	Committed bool
	// Fix tracks the pre-install storage fix when enabled at Start (see WantsStorageFix).
	Fix *RunFix
}

func (r *Run) Status() RunStatus {
	return r.status
}

// ShowStatus overrides status for UI fixtures.
func (r *Run) ShowStatus(status RunStatus) {
	r.status = status
}

func (r *Run) fail(why string) {
	r.status = RunStatus{Kind: RunFailed, Why: why}
}

// wait transitions to RunWaiting and returns the wait's start time. The start
// time carries over across reason changes unless PowerCycle flips, since a
// power-cycle prompt gets its own timeout window.
func (r *Run) wait(why string, powerCycle bool, now time.Time) time.Time {
	since := now
	if r.status.Kind == RunWaiting && r.status.PowerCycle == powerCycle {
		since = r.status.Since
	}
	r.status = RunStatus{Kind: RunWaiting, Why: why, Since: since, PowerCycle: powerCycle}
	return since
}

func (r *Run) resume() {
	if r.status.Kind == RunWaiting {
		r.status = RunStatus{}
	}
}

func (r *Run) retry() {
	if r.status.Kind != RunCanceling {
		r.status = RunStatus{}
	}
}

func (r *Run) cancel() {
	r.status = RunStatus{Kind: RunCanceling}
}

type imageCheckKind int

const (
	imageCheckUnasked imageCheckKind = iota
	imageCheckUnsupported
	imageCheckAnswered
)

type imageCheck struct {
	kind   imageCheckKind
	result core.ImageCheck
}

// RunFix tracks the storage-fix phase embedded in an install run
// (RequestStorageFix with Final=true).
type RunFix struct {
	Reply *installer.StorageFix
	Error string
	Event *installer.Progress
}

type FixPolicy int

const (
	// FixNever is the zero value so models don't run the storage fix unless
	// explicitly opted in (useful for hardware test loops).
	FixNever FixPolicy = iota
	// FixAuto runs the fix on a Pro II when RC2FX isn't running.
	FixAuto
	// FixAlways forces the fix on any normal-mode Pro II (dev/testing).
	FixAlways
)

const (
	updateWindow      = 20 * 60 // seconds after a flash during which a missing mixer may still be updating
	waitLimit         = 20 * time.Minute
	TakingLongerAfter = 6 * time.Minute // wait duration before offering a "stop" escape hatch
)

type SettingsRead int

const (
	SettingsReadIdle SettingsRead = iota
	SettingsReadWanted
	SettingsReadAsked
)

type MixerMenuItem struct {
	Source MixerSource
	Name   string
	Chosen bool
}
