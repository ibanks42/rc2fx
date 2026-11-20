// Package telemetry handles meter freshness, audio progress tracking, startup
// fault exclusion, the diagnostic event log, and the Telemetry view model.
// Kept free of gk imports so it tests with plain `go test`.
package telemetry

import (
	"fmt"
	"math"
	"slices"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// StartupAudio is the grace period after an engine generation starts while
// JACK, RØDE's mixer, and path bindings settle. Fault bursts during this
// window are excluded from telemetry headlines and hidden by default in the
// events window.
const StartupAudio = 60 * time.Second

func sinceSat(now, at time.Time) time.Duration { return max(now.Sub(at), 0) }

func nanos(ns uint64) time.Duration {
	if ns > math.MaxInt64 {
		return time.Duration(math.MaxInt64)
	}
	return time.Duration(ns)
}

// StartupFaults captures the fault counters accumulated during StartupAudio so
// they can be subtracted from displayed telemetry without mutating mixer state.
type StartupFaults struct {
	Xruns        uint64
	overruns     uint64
	clockErrors  uint64
	helperLate   uint64
	helperStalls uint64
	paths        map[string]pathFaults
}

type pathFaults struct {
	clipped           uint64
	missedDeadlines   uint64
	staleResults      uint64
	inputDrops        uint64
	outputDrops       uint64
	workerOverruns    uint64
	workerClockErrors uint64
}

func pathFaultsOf(t *core.TargetMeter) pathFaults {
	f := pathFaults{
		clipped:         t.Clipped,
		missedDeadlines: t.MissedDeadlines,
		staleResults:    t.StaleResults,
		inputDrops:      t.InputDrops,
		outputDrops:     t.OutputDrops,
	}
	if p := t.Performance; p != nil {
		f.workerOverruns = p.WorkerWall.Overruns
		f.workerClockErrors = p.WorkerWall.ClockErrors + p.WorkerCPU.ClockErrors
	}
	return f
}

// sharedPerformance returns the first available Performance block (callback,
// xrun, and helper counters are shared across all paths).
func sharedPerformance(meter *core.Meter) *core.Performance {
	for i := range meter.Targets {
		if p := meter.Targets[i].Performance; p != nil {
			return p
		}
	}
	return nil
}

func startupFaultsOf(meter *core.Meter) StartupFaults {
	var f StartupFaults
	if p := sharedPerformance(meter); p != nil {
		f.Xruns = p.Xruns
		f.overruns = p.Callback.Overruns
		f.clockErrors = p.Callback.ClockErrors
		if p.HelperLate != nil {
			f.helperLate = *p.HelperLate
		}
		if p.HelperStalls != nil {
			f.helperStalls = *p.HelperStalls
		}
	}
	f.paths = make(map[string]pathFaults, len(meter.Targets))
	for i := range meter.Targets {
		t := &meter.Targets[i]
		f.paths[t.ID] = pathFaultsOf(t)
	}
	return f
}

type startupPhase int

const (
	// Startup faults are finalized or indistinguishable.
	phaseClosed startupPhase = iota
	// Engine is still within StartupAudio; incoming faults count as startup.
	phaseOpen
	// Connected after StartupAudio elapsed; waiting on the event log to classify xruns.
	phaseUnread
)

type generationEpoch struct{ generation, epoch uint64 }

// StartupWindow tracks an engine generation's startup grace period and records
// the faults that occurred inside it so post-startup telemetry stays clean.
type StartupWindow struct {
	seen *generationEpoch
	// Estimated engine start time on the host clock; nil after a diagnostics reset.
	started *time.Time
	phase   startupPhase
	Faults  StartupFaults
}

// Observe updates the startup window from a meter snapshot.
func (w *StartupWindow) Observe(meter *core.Meter, sampleRate uint32, now time.Time) {
	// Prefer the mixer's own running time; falling back to processed frames lags
	// slightly because frames only start counting once paths bind.
	var age time.Duration
	if meter.RunningForNs != nil {
		age = nanos(*meter.RunningForNs)
	} else {
		if len(meter.Targets) == 0 || sampleRate == 0 {
			return
		}
		var frames uint64
		for _, t := range meter.Targets {
			frames = max(frames, t.ProcessedFrames)
		}
		age = time.Duration(float64(frames) / float64(sampleRate) * float64(time.Second))
	}
	key := generationEpoch{meter.Generation, meter.DiagnosticsEpoch}
	if w.seen == nil || *w.seen != key {
		reset := w.seen != nil && w.seen.generation == key.generation
		*w = StartupWindow{seen: &key}
		// A diagnostics reset clears counters without restarting the engine, and an
		// earlier reset before connect obscures the real engine start time.
		if !reset && meter.DiagnosticsEpoch == 0 {
			started := now.Add(-age)
			w.started = &started
			if age < StartupAudio {
				w.phase = phaseOpen
			} else {
				w.phase = phaseUnread
			}
		}
	}
	if w.phase == phaseOpen {
		if age < StartupAudio {
			w.Faults = startupFaultsOf(meter)
		} else {
			w.phase = phaseClosed
		}
	}
}

// Range returns the startup window bounds on the host clock, or nil if unknown.
func (w *StartupWindow) Range() *[2]time.Time {
	if w.started == nil {
		return nil
	}
	return &[2]time.Time{*w.started, w.started.Add(StartupAudio)}
}

// Open reports whether the engine is still within the startup grace period.
func (w *StartupWindow) Open() bool { return w.phase == phaseOpen }

// WantsEvents reports whether the event log should be fetched to classify startup xruns.
func (w *StartupWindow) WantsEvents() bool { return w.phase == phaseUnread }

// ReadEvents finalizes startup xrun counts using the polled event log.
func (w *StartupWindow) ReadEvents(xruns func(from, until time.Time) uint64) {
	if w.phase == phaseUnread {
		started := *w.started
		w.Faults.Xruns = xruns(started, started.Add(StartupAudio))
		w.phase = phaseClosed
	}
}

// GiveUp closes the unread phase when the event log cannot be fetched, leaving
// all faults counted in the headline.
func (w *StartupWindow) GiveUp() {
	if w.WantsEvents() {
		w.phase = phaseClosed
	}
}

// Settle subtracts recorded startup faults from meter in place.
func (w *StartupWindow) Settle(meter *core.TargetMeter) {
	faults := &w.Faults
	if path, ok := faults.paths[meter.ID]; ok {
		meter.Clipped = num.SatSub(meter.Clipped, path.clipped)
		meter.MissedDeadlines = num.SatSub(meter.MissedDeadlines, path.missedDeadlines)
		meter.StaleResults = num.SatSub(meter.StaleResults, path.staleResults)
		meter.InputDrops = num.SatSub(meter.InputDrops, path.inputDrops)
		meter.OutputDrops = num.SatSub(meter.OutputDrops, path.outputDrops)
		if p := meter.Performance; p != nil {
			p.WorkerWall.Overruns = num.SatSub(p.WorkerWall.Overruns, path.workerOverruns)
			// Subtract from wall clock errors first, then spill into CPU clock errors.
			wall := min(p.WorkerWall.ClockErrors, path.workerClockErrors)
			p.WorkerWall.ClockErrors -= wall
			p.WorkerCPU.ClockErrors = num.SatSub(p.WorkerCPU.ClockErrors, path.workerClockErrors-wall)
		}
	}
	p := meter.Performance
	if p == nil {
		return
	}
	p.Xruns = num.SatSub(p.Xruns, faults.Xruns)
	p.Callback.Overruns = num.SatSub(p.Callback.Overruns, faults.overruns)
	p.Callback.ClockErrors = num.SatSub(p.Callback.ClockErrors, faults.clockErrors)
	if p.HelperLate != nil {
		n := num.SatSub(*p.HelperLate, faults.helperLate)
		p.HelperLate = &n
	}
	if p.HelperStalls != nil {
		n := num.SatSub(*p.HelperStalls, faults.helperStalls)
		p.HelperStalls = &n
	}
}

// SettledXruns returns post-startup xruns from meter, or false if no
// Performance block is present.
func (w *StartupWindow) SettledXruns(meter *core.Meter) (uint64, bool) {
	p := sharedPerformance(meter)
	if p == nil {
		return 0, false
	}
	return num.SatSub(p.Xruns, w.Faults.Xruns), true
}

// CloneTargetMeter deep-copies t's Spectrum slice and Performance pointers so
// Settle can mutate the clone safely.
func CloneTargetMeter(t *core.TargetMeter) core.TargetMeter {
	c := *t
	c.Spectrum = slices.Clone(t.Spectrum)
	if t.Performance != nil {
		p := *t.Performance
		if p.HelperLate != nil {
			v := *p.HelperLate
			p.HelperLate = &v
		}
		if p.HelperStalls != nil {
			v := *p.HelperStalls
			p.HelperStalls = &v
		}
		if p.CallbackRecentMaxNs != nil {
			v := *p.CallbackRecentMaxNs
			p.CallbackRecentMaxNs = &v
		}
		if p.WorkerRecentMaxNs != nil {
			v := *p.WorkerRecentMaxNs
			p.WorkerRecentMaxNs = &v
		}
		c.Performance = &p
	}
	return c
}

// RecentWindow is the sliding window over which RecentCount tracks counter rises.
const RecentWindow = 60 * time.Second

// Recent holds a counter's increase over RecentWindow and the last wall-clock
// time it rose (nil if never observed rising).
type Recent struct {
	Count uint64
	Last  *time.Time
}

type reading struct {
	at    time.Time
	count uint64
}

// RecentCount tracks how much a cumulative counter increased within the last
// RecentWindow. Resets on a new generation or diagnostics epoch; the first
// sample only establishes a baseline.
type RecentCount struct {
	key      *generationEpoch
	readings []reading // arrival order; oldest entry may predate the window as a baseline
	lastRise *time.Time
}

// Observe records a sample using monotonic time now and wall-clock time wall.
func (r *RecentCount) Observe(generation, epoch, count uint64, now, wall time.Time) {
	key := generationEpoch{generation, epoch}
	if r.key == nil || *r.key != key {
		*r = RecentCount{key: &key}
	}
	if n := len(r.readings); n > 0 && count > r.readings[n-1].count {
		w := wall
		r.lastRise = &w
	}
	r.readings = append(r.readings, reading{now, count})
	// Retain one reading at or before the window boundary as the baseline.
	drop := 0
	for len(r.readings)-drop > 1 && sinceSat(now, r.readings[drop+1].at) >= RecentWindow {
		drop++
	}
	if drop > 0 {
		r.readings = slices.Delete(r.readings, 0, drop)
	}
}

// Within returns the delta over RecentWindow, or false before any observation.
func (r *RecentCount) Within() (uint64, bool) {
	if len(r.readings) == 0 {
		return 0, false
	}
	return num.SatSub(r.readings[len(r.readings)-1].count, r.readings[0].count), true
}

// Recent returns the current sliding-window count and last rise timestamp.
func (r *RecentCount) Recent() Recent {
	count, _ := r.Within()
	return Recent{Count: count, Last: r.lastRise}
}

// RecentFaults tracks sliding-window audible fault counters (engine dropouts
// plus per-path late, dropped, and clipped frames) with startup faults removed.
type RecentFaults struct {
	counts map[string]*RecentCount
}

// PathRecent holds one path's recent audible fault counters.
type PathRecent struct {
	Late    Recent
	Dropped Recent
	Clipped Recent
}

// Observe updates all recent fault counters from a meter snapshot.
func (f *RecentFaults) Observe(meter *core.Meter, startup *StartupWindow, now, wall time.Time) {
	if f.counts == nil {
		f.counts = map[string]*RecentCount{}
	}
	feed := func(name string, count uint64) {
		c := f.counts[name]
		if c == nil {
			c = &RecentCount{}
			f.counts[name] = c
		}
		c.Observe(meter.Generation, meter.DiagnosticsEpoch, count, now, wall)
	}
	if xruns, ok := startup.SettledXruns(meter); ok {
		feed("xruns", xruns)
	}
	for i := range meter.Targets {
		shown := CloneTargetMeter(&meter.Targets[i])
		startup.Settle(&shown)
		feed("late/"+shown.ID, shown.MissedDeadlines)
		feed("dropped/"+shown.ID, shown.InputDrops+shown.OutputDrops)
		feed("clipped/"+shown.ID, shown.Clipped)
	}
}

func (f *RecentFaults) get(name string) Recent {
	if c := f.counts[name]; c != nil {
		return c.Recent()
	}
	return Recent{}
}

// Xruns returns recent engine dropouts.
func (f *RecentFaults) Xruns() Recent { return f.get("xruns") }

// Path returns recent audible faults for target id.
func (f *RecentFaults) Path(id string) PathRecent {
	return PathRecent{
		Late:    f.get("late/" + id),
		Dropped: f.get("dropped/" + id),
		Clipped: f.get("clipped/" + id),
	}
}

// MeterMatchesEngine reports whether meter belongs to the currently running
// engine generation, applied revision, and expected target set.
func MeterMatchesEngine(state *core.DeviceState, meter *core.Meter) bool {
	expected := state.Selection.ActiveTargetIDs
	if state.Engine.Mode == core.EngineModeNative {
		expected = state.Selection.QualifiedTargetIDs
	}
	if state.Engine.Status != core.EnginePhaseRunning ||
		state.Selection.Status != core.SelectionPhaseIdle ||
		meter.Generation != state.Engine.Generation ||
		meter.Revision != state.Engine.AppliedRevision ||
		len(meter.Targets) != len(expected) {
		return false
	}
	for i := range meter.Targets {
		if meter.Targets[i].ID != expected[i] {
			return false
		}
	}
	return true
}

// ReceiveMeter validates meter against state and updates per-path progress.
// Invalid, stale-generation, or mismatched-revision meters are rejected without
// touching progress or engineSeen. engineSeen is renewed only when active paths
// actually advance.
func ReceiveMeter(state *core.DeviceState, meter *core.Meter, progress map[string]*Progress,
	engineSeen **time.Time, now time.Time) bool {
	for i := range meter.Targets {
		if meter.Targets[i].Validate() != nil {
			return false
		}
	}
	if !MeterMatchesEngine(state, meter) {
		return false
	}
	if observeMeterProgress(progress, meter, state.Selection.ActiveTargetIDs, now) {
		// State events only fire on change; advancing meters also confirm the engine is alive.
		seen := now
		*engineSeen = &seen
	}
	return true
}

// FreshMeter returns targetID's meter reading and synthetic flag if the engine
// observation is current (< 30 s), audio is advancing, and targetID itself is fresh.
func FreshMeter(state *core.DeviceState, meter *core.Meter, progress map[string]*Progress,
	targetID string, engineSeen *time.Time, now time.Time) (*core.TargetMeter, bool) {
	visible := state.Selection.ActiveTargetIDs
	if state.Engine.Mode == core.EngineModeNative {
		visible = state.Selection.QualifiedTargetIDs
	}
	if engineSeen == nil || sinceSat(now, *engineSeen) >= 30*time.Second ||
		state.Engine.Status != core.EnginePhaseRunning ||
		!slices.Contains(visible, targetID) ||
		!state.Engine.AudioAdvancing ||
		meter.Revision != state.Engine.AppliedRevision ||
		meter.Generation != state.Engine.Generation {
		return nil, false
	}
	var target *core.TargetMeter
	for i := range meter.Targets {
		if meter.Targets[i].ID == targetID {
			target = &meter.Targets[i]
			break
		}
	}
	if target == nil {
		return nil, false
	}
	p := progress[targetID]
	if p == nil || !p.Fresh(now) {
		return nil, false
	}
	return target, meter.Synthetic
}

// observeMeterProgress advances per-path progress for a validated meter.
// Returns true only when at least one path advanced and every active path
// advanced; idle native passthrough paths track their own freshness without
// blocking engine renewal.
func observeMeterProgress(progress map[string]*Progress, meter *core.Meter, active []string, now time.Time) bool {
	for id := range progress {
		if !slices.ContainsFunc(meter.Targets, func(t core.TargetMeter) bool { return t.ID == id }) {
			delete(progress, id)
		}
	}
	anyAdvanced := false
	activeAdvanced := true
	for i := range meter.Targets {
		target := &meter.Targets[i]
		p := progress[target.ID]
		if p == nil {
			p = NewProgress(target.ProcessedFrames)
			progress[target.ID] = p
		}
		advanced := p.ObserveMeter(meter.Revision, meter.DiagnosticsEpoch, target, now)
		anyAdvanced = anyAdvanced || advanced
		if slices.Contains(active, target.ID) {
			activeAdvanced = activeAdvanced && advanced
		}
	}
	return anyAdvanced && activeAdvanced
}

// EngineErrorNotice holds the message and recovery context for a terminal engine error.
type EngineErrorNotice struct {
	Message        string
	RecoveryDetail string
}

// EngineErrorNoticeTitle is the heading used for EngineErrorNotice banners.
const EngineErrorNoticeTitle = "Audio processing error"

// EngineError returns a notice when the saved engine state is in EnginePhaseError.
// Terminal errors persist regardless of meter freshness.
func EngineError(state *core.DeviceState) (EngineErrorNotice, bool) {
	if state.Engine.Status != core.EnginePhaseError {
		return EngineErrorNotice{}, false
	}
	var recovery string
	switch state.Engine.Mode {
	case core.EngineModeOffline:
		recovery = "No live audio routing is confirmed in offline mode."
	case core.EngineModeNative:
		recovery = "Native channel processing reported an error. Mixer audio delivery is not confirmed by this status."
	default:
		recovery = "Original audio route recovery is not confirmed."
	}
	message := state.Engine.Message
	if strings.TrimSpace(message) == "" {
		message = "The audio engine reported an error without further details."
	}
	return EngineErrorNotice{Message: message, RecoveryDetail: recovery}, true
}

// Warning identifies a recent fault condition surfaced in the status bar.
type Warning int

const (
	WarningWorkerFailed Warning = iota
	WarningAudioDeadline
	WarningSuppressionFallback
	WarningClockFailure
)

// Label returns the user-facing status text for w.
func (w Warning) Label() string {
	switch w {
	case WarningWorkerFailed:
		return "Noise worker failed · stopping processing"
	case WarningAudioDeadline:
		return "Recent audio deadline missed"
	case WarningSuppressionFallback:
		return "Recent suppression fallback"
	case WarningClockFailure:
		return "Audio timing measurement failed"
	}
	return fmt.Sprintf("Warning(%d)", int(w))
}

type faultCounters struct {
	callbacks         uint64
	suppressionFrames uint64
	suppression       [4]uint64
	hasPerformance    bool
	audio             [2]uint64 // callback overruns, xruns
	clock             [3]uint64 // callback, worker wall, worker CPU clock errors
}

func faultCountersFrom(m *core.TargetMeter) faultCounters {
	c := faultCounters{
		callbacks:         m.Callbacks,
		suppressionFrames: m.SuppressionFrames,
		suppression:       [4]uint64{m.MissedDeadlines, m.StaleResults, m.InputDrops, m.OutputDrops},
	}
	if p := m.Performance; p != nil {
		c.hasPerformance = true
		c.audio = [2]uint64{p.Callback.Overruns, p.Xruns}
		c.clock = [3]uint64{p.Callback.ClockErrors, p.WorkerWall.ClockErrors, p.WorkerCPU.ClockErrors}
	}
	return c
}

func (c faultCounters) suppressionResetFrom(old faultCounters) bool {
	if c.suppressionFrames < old.suppressionFrames {
		return true
	}
	for i := range c.suppression {
		if c.suppression[i] < old.suppression[i] {
			return true
		}
	}
	return false
}

// increased reports whether at least one counter rose and none decreased
// (a decrease indicates a counter reset/new baseline, not a fault).
func increased(now, before []uint64) bool {
	rose := false
	for i := range now {
		if now[i] < before[i] {
			return false
		}
		if now[i] > before[i] {
			rose = true
		}
	}
	return rose
}

func (c faultCounters) changedFrom(old faultCounters) (Warning, bool) {
	if c.hasPerformance && old.hasPerformance {
		if increased(c.audio[:], old.audio[:]) {
			return WarningAudioDeadline, true
		}
		if increased(c.clock[:], old.clock[:]) {
			return WarningClockFailure, true
		}
	}
	if c.suppressionFrames >= old.suppressionFrames && increased(c.suppression[:], old.suppression[:]) {
		return WarningSuppressionFallback, true
	}
	return 0, false
}

type timedWarning struct {
	at      time.Time
	warning Warning
}

// Progress tracks whether a single path's audio frames are advancing and detects
// new faults against a stable (revision, epoch) baseline. The first sample and
// any counter reset only set the baseline; a subsequent increase is required
// before the path is considered fresh.
type Progress struct {
	frames            uint64
	advancedAt        *time.Time
	observedAt        *time.Time
	baseline          *generationEpoch
	counters          *faultCounters
	recentWarning     *timedWarning
	recentSuppression *time.Time
	workerFailed      bool
}

// NewProgress initializes Progress at frames without marking it fresh.
func NewProgress(frames uint64) *Progress { return &Progress{frames: frames} }

// Observe updates the frame counter and marks the path fresh when frames increases.
func (p *Progress) Observe(frames uint64, now time.Time) {
	if frames > p.frames {
		at := now
		p.advancedAt = &at
	} else if frames < p.frames {
		p.advancedAt = nil
	}
	p.frames = frames
}

// ObserveMeter processes one path's meter sample and reports whether it advanced.
func (p *Progress) ObserveMeter(revision, epoch uint64, meter *core.TargetMeter, now time.Time) bool {
	baseline := generationEpoch{revision, epoch}
	counters := faultCountersFrom(meter)
	receiptGap := p.observedAt != nil && sinceSat(now, *p.observedAt) >= time.Second
	callbackReset := p.counters != nil && counters.callbacks < p.counters.callbacks
	reset := p.baseline == nil || *p.baseline != baseline ||
		meter.ProcessedFrames < p.frames || callbackReset || receiptGap
	advanced := !reset && meter.ProcessedFrames > p.frames &&
		p.counters != nil && counters.callbacks > p.counters.callbacks &&
		!meter.WorkerFailed
	if reset {
		p.recentWarning = nil
		p.recentSuppression = nil
		p.advancedAt = nil
		p.frames = meter.ProcessedFrames
	} else if p.counters != nil {
		old := *p.counters
		if counters.suppressionResetFrom(old) {
			p.recentSuppression = nil
		}
		if warning, ok := counters.changedFrom(old); ok {
			at := now
			if warning == WarningSuppressionFallback {
				p.recentSuppression = &at
			} else {
				p.recentWarning = &timedWarning{at, warning}
			}
		}
	}
	p.Observe(meter.ProcessedFrames, now)
	observed := now
	p.observedAt = &observed
	p.baseline = &baseline
	p.counters = &counters
	p.workerFailed = meter.WorkerFailed
	return advanced
}

// ForgetStartupDeadline clears a missed audio deadline warning observed during startup.
func (p *Progress) ForgetStartupDeadline() {
	if p.recentWarning != nil && p.recentWarning.warning == WarningAudioDeadline {
		p.recentWarning = nil
	}
}

// WarningFor returns the highest-priority active warning for the path at now.
func (p *Progress) WarningFor(now time.Time, suppressionActive bool) (Warning, bool) {
	if suppressionActive && p.workerFailed {
		return WarningWorkerFailed, true
	}
	if w := p.recentWarning; w != nil && sinceSat(now, w.at) < 5*time.Second {
		return w.warning, true
	}
	if suppressionActive && p.recentSuppression != nil && sinceSat(now, *p.recentSuppression) < 5*time.Second {
		return WarningSuppressionFallback, true
	}
	return 0, false
}

// Fresh reports whether frames advanced within the last second.
func (p *Progress) Fresh(now time.Time) bool {
	return p.advancedAt != nil && sinceSat(now, *p.advancedAt) < time.Second
}
