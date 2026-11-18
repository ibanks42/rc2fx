// Package telemetry is the desktop's reading of the mixer's meters and
// diagnostics, without drawing: meter freshness and audio progress, plus
// the pure halves of the events window (EventLog, the event rows and their
// wording) and of the Telemetry section (its statuses, checks and figures).
// It does not import gk, so it tests with plain `go test`.
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

// StartupAudio is how long after an engine generation starts running counts
// as startup: JACK coming up, RØDE's mixer registering, paths binding,
// threads settling. Bursts inside it are expected and left out of telemetry
// and the events window; a fault after it counts normally.
const StartupAudio = 60 * time.Second

// sinceSat is now - at, or 0 when at is later.
func sinceSat(now, at time.Time) time.Duration { return max(now.Sub(at), 0) }

// nanos is ns nanoseconds, capped at what a time.Duration holds.
func nanos(ns uint64) time.Duration {
	if ns > math.MaxInt64 {
		return time.Duration(math.MaxInt64)
	}
	return time.Duration(ns)
}

// StartupFaults is the faults counted while the mixer started. Telemetry
// leaves them out entirely: they say nothing about how RC2FX runs. The
// mixer's logs and counters are untouched.
type StartupFaults struct {
	Xruns        uint64
	overruns     uint64
	clockErrors  uint64
	helperLate   uint64
	helperStalls uint64
	// paths is each path's own counters, by path id.
	paths map[string]pathFaults
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

// sharedPerformance is the first path's performance block: the audio
// callback, xruns and helper are shared by every path.
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
	// phaseClosed: the startup faults are known, or none can be told apart.
	// The zero value.
	phaseClosed startupPhase = iota
	// phaseOpen: audio is younger than the window: everything counted is
	// startup.
	phaseOpen
	// phaseUnread: first seen after the window: only the event log can say
	// what fell inside it. Faults stay in the headline until it has.
	phaseUnread
)

// generationEpoch is a (generation, diagnostics epoch) key.
type generationEpoch struct{ generation, epoch uint64 }

// StartupWindow separates what the mixer counted while starting from what
// came after. The shown counters start at the first reading of an engine
// generation, so audio processed since then is a measured age, and a new
// generation is a restart. The zero value has seen nothing yet.
type StartupWindow struct {
	// seen is the generation and diagnostics epoch the counters belong to.
	seen *generationEpoch
	// started is when this generation's audio started, on this computer's
	// clock; nil after a diagnostics reset, which hides the true start.
	started *time.Time
	phase   startupPhase
	Faults  StartupFaults
}

// Observe takes one reading of the whole meter.
func (w *StartupWindow) Observe(meter *core.Meter, sampleRate uint32, now time.Time) {
	// The engine's own running time when the mixer reports it; otherwise
	// audio processed, which starts counting only once paths are bound, so it
	// runs a few seconds behind.
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
		// A diagnostics reset restarts the counters, not the mixer, and one
		// from before this app connected hides the true start.
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

// Range is the startup window on this computer's clock, once the start is
// known; nil before.
func (w *StartupWindow) Range() *[2]time.Time {
	if w.started == nil {
		return nil
	}
	return &[2]time.Time{*w.started, w.started.Add(StartupAudio)}
}

// Open is whether the mixer is still inside the window.
func (w *StartupWindow) Open() bool { return w.phase == phaseOpen }

// WantsEvents is whether the event log is needed to place the startup
// faults.
func (w *StartupWindow) WantsEvents() bool { return w.phase == phaseUnread }

// ReadEvents is called once the event log is read: xruns counts those in the
// range it is given.
func (w *StartupWindow) ReadEvents(xruns func(from, until time.Time) uint64) {
	if w.phase == phaseUnread {
		// started is always set when the phase becomes phaseUnread.
		started := *w.started
		w.Faults.Xruns = xruns(started, started.Add(StartupAudio))
		w.phase = phaseClosed
	}
}

// GiveUp is called when the event log can't be read: every fault stays in
// the headline.
func (w *StartupWindow) GiveUp() {
	if w.WantsEvents() {
		w.phase = phaseClosed
	}
}

// Settle takes the startup faults out of everything telemetry counts.
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
			// Wall clock errors go first; the rest come off the CPU clock.
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

// SettledXruns is the xruns counted since startup, from a reading of the
// whole meter; false without a performance block.
func (w *StartupWindow) SettledXruns(meter *core.Meter) (uint64, bool) {
	p := sharedPerformance(meter)
	if p == nil {
		return 0, false
	}
	return num.SatSub(p.Xruns, w.Faults.Xruns), true
}

// CloneTargetMeter is a copy of t whose performance block (the
// one part Settle changes through a pointer) is its own.
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

// RecentWindow is how long RecentCount looks back (the name Recent is the
// struct's).
const RecentWindow = 60 * time.Second

// Recent is a counter as telemetry shows it: how much it rose in the last
// RecentWindow, and when this app last saw it rise (nil for never).
type Recent struct {
	Count uint64
	Last  *time.Time
}

type reading struct {
	at    time.Time
	count uint64
}

// RecentCount is how much a cumulative counter rose within the last RecentWindow,
// from the readings this app received, and when it last rose. A new
// generation or diagnostics reset starts over, since the counter does too.
// The first reading is only a baseline: what happened before this app was
// watching is not news.
type RecentCount struct {
	key *generationEpoch
	// readings in arrival order; the oldest may predate the window, as the
	// baseline for the rise inside it.
	readings []reading
	lastRise *time.Time
}

// Observe takes one reading: key is (generation, diagnostics epoch), now
// the monotonic receipt time and wall this computer's clock.
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
	// Keep one reading at or before the window's start as the baseline.
	drop := 0
	for len(r.readings)-drop > 1 && sinceSat(now, r.readings[drop+1].at) >= RecentWindow {
		drop++
	}
	if drop > 0 {
		r.readings = slices.Delete(r.readings, 0, drop)
	}
}

// Within is the rise within the window; false before any reading.
func (r *RecentCount) Within() (uint64, bool) {
	if len(r.readings) == 0 {
		return 0, false
	}
	return num.SatSub(r.readings[len(r.readings)-1].count, r.readings[0].count), true
}

// Recent is the counter as telemetry shows it.
func (r *RecentCount) Recent() Recent {
	count, _ := r.Within()
	return Recent{Count: count, Last: r.lastRise}
}

// RecentFaults is the counters that mean something was audible, each
// tracked as a RecentCount: dropouts for the engine, and per path late
// frames (original audio played), dropped frames and clipping. Startup faults
// are already taken out. The zero value is ready.
type RecentFaults struct {
	counts map[string]*RecentCount
}

// PathRecent is one path's audible counters.
type PathRecent struct {
	Late    Recent
	Dropped Recent
	Clipped Recent
}

// Observe takes one reading of the whole meter (now monotonic, wall this
// computer's clock).
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

// Xruns is the engine's dropouts.
func (f *RecentFaults) Xruns() Recent { return f.get("xruns") }

// Path is one path's audible counters.
func (f *RecentFaults) Path(id string) PathRecent {
	return PathRecent{
		Late:    f.get("late/" + id),
		Dropped: f.get("dropped/" + id),
		Clipped: f.get("clipped/" + id),
	}
}

// MeterMatchesEngine is meter_matches_engine.
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

// ReceiveMeter is the desktop receive path. Invalid, stale-generation and
// unrelated-revision telemetry must not alter the progress baseline or renew
// engine freshness. engineSeen is when the engine was last seen working (nil
// for never); new work from every active path sets it to now.
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
		// State is published only when it changes. New work from every path
		// also confirms observation of this same engine/revision.
		seen := now
		*engineSeen = &seen
	}
	return true
}

// FreshMeter is the meter/status rendering predicate after the caller has
// confirmed both the selected connection and its event stream are
// connected: the path's reading and whether it is synthetic, nil for None.
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

// observeMeterProgress: call only for a validated meter matching the current
// engine generation, revision and complete target set. Receipt alone does not
// renew the engine observation: every active path must advance from its
// previous fresh sample, and at least one path must advance. A native
// passthrough path whose stock source is idle (unplugged USB, disconnected
// Bluetooth) keeps its own per-path freshness but does not hold back the
// engine observation.
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

// EngineErrorNotice is a terminal engine failure as the app states it.
type EngineErrorNotice struct {
	Message        string
	RecoveryDetail string
}

// EngineErrorNoticeTitle is the title of every EngineErrorNotice.
const EngineErrorNoticeTitle = "Audio processing error"

// EngineError: terminal failures outlive meter freshness.
// Read the saved engine/routing state, never infer recovery from missing
// meters or a stopped worker.
func EngineError(state *core.DeviceState) (EngineErrorNotice, bool) {
	if state.Engine.Status != core.EnginePhaseError {
		return EngineErrorNotice{}, false
	}
	var recovery string
	// Mixers report only native and offline modes; anything else confirms
	// nothing.
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

// Warning is an audible fault worth a status line.
type Warning int

const (
	WarningWorkerFailed Warning = iota
	WarningAudioDeadline
	WarningSuppressionFallback
	WarningClockFailure
)

// Label is the status line for the warning.
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
	// audio is (callback overruns, xruns); clock the three clock error
	// counts.
	audio [2]uint64
	clock [3]uint64
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

// increased: no counter fell and one rose. A decreasing counter is a new
// baseline, not an unsigned fault delta.
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

// Progress: receipt alone is not progress. The first sample and counter
// resets require another increasing sample before the display treats
// telemetry as current.
type Progress struct {
	frames     uint64
	advancedAt *time.Time
	observedAt *time.Time
	// baseline is the meter revision and diagnostics epoch the counters
	// belong to; a change of either (a new config, or a diagnostics reset)
	// starts a new baseline.
	baseline          *generationEpoch
	counters          *faultCounters
	recentWarning     *timedWarning
	recentSuppression *time.Time
	workerFailed      bool
}

// NewProgress is progress at frames, not yet seen advancing.
func NewProgress(frames uint64) *Progress { return &Progress{frames: frames} }

// Observe is a frame count read at now.
func (p *Progress) Observe(frames uint64, now time.Time) {
	if frames > p.frames {
		at := now
		p.advancedAt = &at
	} else if frames < p.frames {
		p.advancedAt = nil
	}
	p.frames = frames
}

// ObserveMeter takes one path's reading; (revision, epoch) is its baseline.
// It reports whether the path advanced.
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

// ForgetStartupDeadline: an audio deadline missed while the mixer starts is
// expected, so it does not warn. Other faults still do.
func (p *Progress) ForgetStartupDeadline() {
	if p.recentWarning != nil && p.recentWarning.warning == WarningAudioDeadline {
		p.recentWarning = nil
	}
}

// WarningFor is the path's warning at now, if any.
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

// Fresh is whether the path advanced within the last second.
func (p *Progress) Fresh(now time.Time) bool {
	return p.advancedAt != nil && sinceSat(now, *p.advancedAt) < time.Second
}
