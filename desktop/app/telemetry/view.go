package telemetry

// View model for the Diagnostics -> Telemetry section: headline status, four
// pass/warn checks, per-channel rows, and raw technical facts. Only recent
// audible problems trigger warnings; older issues degrade to quiet history.

import (
	"cmp"
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/num"
)

// Snapshot captures the app state needed to render the Telemetry section.
type Snapshot struct {
	HasEngine bool
	Engine    core.EngineStatus
	Routing   core.RoutingStatus
	// Reason shown when no path has fresh meters.
	Notice    string
	Synthetic bool
	Epoch     uint64
	Paths     []PathView
	Xruns     Recent
	Now       time.Time
	// Processed paths whose source is currently silent/idle.
	Idle    []string
	Details bool
}

// PathView pairs a channel's display info with its current meter and recent faults.
type PathView struct {
	Name     string
	Channels uint8 // 1 mono, 2 stereo, 0 unknown
	Meter    core.TargetMeter
	Recent   PathRecent
}

// DBFS converts a linear peak to dBFS, floored at -120 dBFS.
func DBFS(peak float64) float64 { return 20.0 * math.Log10(num.Max(peak, 1e-6)) }

// DBValue formats a peak for a dB column (e.g. "-14.9" or "silent").
func DBValue(peak float64) string {
	if peak <= 1e-6 {
		return "silent"
	}
	return fmt.Sprintf("%.1f", DBFS(peak))
}

// LevelWidth maps a peak over [-60, 0] dBFS to a [0, 1] bar fill and reports
// whether the signal is at or above 0 dBFS.
func LevelWidth(peak float64) (width float32, over bool) {
	db := DBFS(peak)
	return float32(math.Min(math.Max((db+60.)/60., 0.), 1.)), db >= 0.
}

func msBare(ns uint64) string { return fmt.Sprintf("%.2f", float64(ns)/1_000_000.) }

func msUnit(ns uint64) string { return msBare(ns) + " ms" }

func meanNs(timing *core.Timing) uint64 {
	if timing.Calls == 0 {
		return 0
	}
	return timing.TotalNs / timing.Calls
}

func share(timing *core.Timing, budgetNs uint64) float64 {
	return float64(meanNs(timing)) / float64(max(budgetNs, 1))
}

// Grouped formats value with comma thousands separators (e.g. "1,234,567").
func Grouped(value uint64) string {
	digits := strconv.FormatUint(value, 10)
	var out strings.Builder
	for index, digit := range digits {
		if index > 0 && (len(digits)-index)%3 == 0 {
			out.WriteByte(',')
		}
		out.WriteRune(digit)
	}
	return out.String()
}

// Duration formats processed audio frames as "43 s", "25 min 43 s", or "2 h 05 min".
func Duration(frames uint64, sampleRate uint32) string {
	seconds := frames / uint64(max(sampleRate, 1))
	switch {
	case seconds < 60:
		return fmt.Sprintf("%d s", seconds)
	case seconds < 3600:
		return fmt.Sprintf("%d min %02d s", seconds/60, seconds%60)
	}
	return fmt.Sprintf("%d h %02d min", seconds/3600, seconds/60%60)
}

func plural(count uint64, one, many string) string {
	if count == 1 {
		return Grouped(count) + " " + one
	}
	return Grouped(count) + " " + many
}

// PhaseLabel returns the badge text for an engine phase.
func PhaseLabel(phase core.EnginePhase) string {
	switch phase {
	case core.EnginePhaseStarting:
		return "Starting"
	case core.EnginePhaseRestarting:
		return "Restarting"
	case core.EnginePhaseRunning:
		return "Running"
	case core.EnginePhaseOffline:
		return "Offline"
	}
	return "Engine error"
}

// RoutingLabel returns the badge text for a routing phase.
func RoutingLabel(phase core.RoutingPhase) string {
	switch phase {
	case core.RoutingPhaseUnmanaged:
		return "Unmanaged"
	case core.RoutingPhaseStarting:
		return "Connecting"
	case core.RoutingPhaseActive:
		return "Active"
	case core.RoutingPhaseBypassing:
		return "Restoring stock audio"
	case core.RoutingPhaseBypassed:
		return "Stock audio"
	}
	return "Routing needs attention"
}

// StatusKind classifies the headline telemetry status badge.
type StatusKind int

const (
	StatusHealthy StatusKind = iota
	// Recent audible issue within the last minute.
	StatusRecent
	// A channel's noise suppression worker failed and stays off until restart.
	StatusFailed
	StatusStopped
)

// Status is the headline status badge; What holds the detail text for Recent and Failed.
type Status struct {
	Kind StatusKind
	What string
}

// Label returns the display text for s.
func (s Status) Label() string {
	switch s.Kind {
	case StatusHealthy:
		return "Healthy"
	case StatusStopped:
		return "RC2FX stopped"
	}
	return s.What
}

// Tone selects a semantic UI color.
type Tone int

const (
	ToneAccent Tone = iota
	ToneWarn
	ToneDanger
	ToneFaint
)

// Tone returns the badge color for s.
func (s Status) Tone() Tone {
	switch s.Kind {
	case StatusHealthy:
		return ToneAccent
	case StatusRecent:
		return ToneWarn
	}
	return ToneDanger
}

func where(names []string) string {
	if len(names) == 1 {
		return names[0]
	}
	return fmt.Sprintf("%d channels", len(names))
}

func sharedOf(paths []PathView) *core.Performance {
	for i := range paths {
		if p := paths[i].Meter.Performance; p != nil {
			return p
		}
	}
	return nil
}

// helperOf returns the native helper's (late, stalls) counts, or ok=false on JACK.
func helperOf(paths []PathView) (late, stalls uint64, ok bool) {
	for i := range paths {
		p := paths[i].Meter.Performance
		if p == nil || (p.HelperLate == nil && p.HelperStalls == nil) {
			continue
		}
		if p.HelperLate != nil {
			late = *p.HelperLate
		}
		if p.HelperStalls != nil {
			stalls = *p.HelperStalls
		}
		return late, stalls, true
	}
	return 0, 0, false
}

func workersOf(paths []PathView) []*PathView {
	var out []*PathView
	for i := range paths {
		if p := paths[i].Meter.Performance; p != nil && p.WorkerWall.Calls > 0 {
			out = append(out, &paths[i])
		}
	}
	return out
}

// busiest returns the worker using the highest average share of its budget (ties go to the last).
func busiest(workers []*PathView) *core.Performance {
	var best *core.Performance
	for _, w := range workers {
		p := w.Meter.Performance
		if p == nil {
			continue
		}
		if best == nil || cmp.Compare(share(&p.WorkerWall, p.WorkerBudgetNs), share(&best.WorkerWall, best.WorkerBudgetNs)) >= 0 {
			best = p
		}
	}
	return best
}

// When formats a timestamp with its relative age (e.g. "22:34 (20 min ago)").
func When(at, now time.Time) string {
	return fmt.Sprintf("%s (%s)", at.Local().Format("15:04"), Ago(at, now))
}

func latest(times ...*time.Time) *time.Time {
	var out *time.Time
	for _, t := range times {
		if t != nil && (out == nil || t.After(*out)) {
			out = t
		}
	}
	return out
}

// Issues formats a channel row's Issues cell: active/recent audible problems
// (recent=true), quiet timestamp of the last older issue (recent=false), or
// ok=false if none.
func Issues(path *PathView, now time.Time) (text string, recent, ok bool) {
	if path.Meter.WorkerFailed {
		return "Suppression failed", true, true
	}
	r := &path.Recent
	var parts []string
	for _, c := range []struct {
		count uint64
		name  string
	}{{r.Late.Count, "late"}, {r.Dropped.Count, "dropped"}, {r.Clipped.Count, "clipped"}} {
		if c.count > 0 {
			parts = append(parts, Grouped(c.count)+" "+c.name)
		}
	}
	if len(parts) > 0 {
		return strings.Join(parts, " · "), true, true
	}
	if at := latest(r.Late.Last, r.Dropped.Last, r.Clipped.Last); at != nil {
		return "Last " + When(*at, now), false, true
	}
	return "", false, false
}

// StatusOf computes the headline status badge, prioritizing stopped processing,
// worker failures, and recent audible faults over healthy state.
func StatusOf(engine *core.EngineStatus, routing *core.RoutingStatus, snapshot *Snapshot) Status {
	running := engine.Status == core.EnginePhaseRunning && engine.AudioAdvancing
	if !running || routing.Status == core.RoutingPhaseError {
		return Status{Kind: StatusStopped}
	}
	named := func(hit func(p *PathView) bool) []string {
		var names []string
		for i := range snapshot.Paths {
			if hit(&snapshot.Paths[i]) {
				names = append(names, snapshot.Paths[i].Name)
			}
		}
		return names
	}
	if failed := named(func(p *PathView) bool { return p.Meter.WorkerFailed }); len(failed) > 0 {
		return Status{StatusFailed, "Noise suppression failed on " + where(failed)}
	}
	if snapshot.Xruns.Count > 0 {
		return Status{StatusRecent, plural(snapshot.Xruns.Count, "dropout", "dropouts") + " just now"}
	}
	audible := []struct {
		hit  func(p *PathView) bool
		what string
	}{
		{func(p *PathView) bool { return p.Recent.Late.Count > 0 }, "Late audio"},
		{func(p *PathView) bool { return p.Recent.Dropped.Count > 0 }, "Dropped audio"},
		{func(p *PathView) bool { return p.Recent.Clipped.Count > 0 }, "Clipping"},
	}
	for _, a := range audible {
		if names := named(a.hit); len(names) > 0 {
			return Status{StatusRecent, fmt.Sprintf("%s on %s just now", a.what, where(names))}
		}
	}
	return Status{Kind: StatusHealthy}
}

// CheckState is the status of one telemetry health check.
type CheckState int

const (
	CheckOK CheckState = iota
	CheckWarn
	CheckOff
)

// Tone returns the indicator dot color for c.
func (c CheckState) Tone() Tone {
	switch c {
	case CheckOK:
		return ToneAccent
	case CheckWarn:
		return ToneWarn
	}
	return ToneFaint
}

// Check represents one telemetry health check row, with optional recent Problem
// warning or quiet History note.
type Check struct {
	Label   string
	Answer  string
	Problem string
	History string
	State   CheckState
}

func checkOK(label, answer string) Check { return Check{Label: label, Answer: answer, State: CheckOK} }

func checkNew(label, answer string, problems []string) Check {
	c := checkOK(label, answer)
	if len(problems) > 0 {
		c.Problem = strings.Join(problems, " · ")
		c.State = CheckWarn
	}
	return c
}

func checkOff(label, answer string) Check {
	c := checkOK(label, answer)
	c.State = CheckOff
	return c
}

func (c Check) last(at *time.Time, now time.Time) Check {
	if c.Problem == "" && at != nil {
		c.History = "last " + When(*at, now)
	}
	return c
}

func loadLabel(timing *core.Timing, budgetNs uint64) string {
	return fmt.Sprintf("%.0f %% load", share(timing, budgetNs)*100.)
}

// Checks builds the four fixed-order health checks; only recent audible faults warn.
func Checks(snapshot *Snapshot) [4]Check {
	const (
		audioLabel       = "Audio keeping up"
		dropoutsLabel    = "No dropouts"
		suppressionLabel = "Noise suppression on time"
		helperLabel      = "Background helper on time"
	)
	paths, now := snapshot.Paths, snapshot.Now
	p := sharedOf(paths)
	if p == nil {
		return [4]Check{
			checkOff(audioLabel, "Not reported"), checkOff(dropoutsLabel, "Not reported"),
			checkOff(suppressionLabel, "Not reported"), checkOff(helperLabel, "Not reported"),
		}
	}
	audio := checkOK(audioLabel, loadLabel(&p.Callback, p.CallbackBudgetNs))
	xruns := snapshot.Xruns
	var dropouts Check
	if xruns.Count > 0 {
		dropouts = checkNew(dropoutsLabel, plural(xruns.Count, "dropout", "dropouts"), []string{"in the last minute"})
	} else {
		dropouts = checkOK(dropoutsLabel, "0 dropouts").last(xruns.Last, now)
	}
	workers := workersOf(paths)
	var suppression Check
	if worst := busiest(workers); worst != nil {
		var problems []string
		var late, dropped uint64
		failed := false
		var lasts []*time.Time
		for _, w := range workers {
			late += w.Recent.Late.Count
			dropped += w.Recent.Dropped.Count
			failed = failed || w.Meter.WorkerFailed
			lasts = append(lasts, w.Recent.Late.Last, w.Recent.Dropped.Last)
		}
		if failed {
			problems = append(problems, "failed")
		}
		if late > 0 {
			problems = append(problems, Grouped(late)+" late")
		}
		if dropped > 0 {
			problems = append(problems, Grouped(dropped)+" dropped")
		}
		suppression = checkNew(suppressionLabel, loadLabel(&worst.WorkerWall, worst.WorkerBudgetNs), problems).
			last(latest(lasts...), now)
	} else {
		suppression = checkOff(suppressionLabel, "Not running")
	}
	// Late helper work isn't directly audible, so display it without warning.
	var helper Check
	if late, _, ok := helperOf(paths); ok {
		helper = checkOK(helperLabel, Grouped(late)+" late")
	} else {
		helper = checkOff(helperLabel, "Not reported")
	}
	return [4]Check{audio, dropouts, suppression, helper}
}

// Badge is a secondary pill shown beside the headline status.
type Badge struct {
	Name string
	Text string
	Tone Tone
}

// ToneText renders a badge using the default foreground text color.
const ToneText Tone = 100

// Badges builds the secondary status badges (engine/routing state when abnormal,
// uptime, audio format, and simulated flag).
func Badges(engine *core.EngineStatus, routing *core.RoutingStatus, snapshot *Snapshot) []Badge {
	var out []Badge
	if engine.Status != core.EnginePhaseRunning {
		out = append(out, Badge{Text: PhaseLabel(engine.Status), Tone: ToneWarn})
	}
	if routing.Status != core.RoutingPhaseActive && engine.Status == core.EnginePhaseRunning {
		tone := ToneWarn
		if routing.Status == core.RoutingPhaseError {
			tone = ToneDanger
		}
		out = append(out, Badge{Text: RoutingLabel(routing.Status), Tone: tone})
	}
	if len(snapshot.Paths) > 0 {
		var frames uint64
		for _, p := range snapshot.Paths {
			frames = max(frames, p.Meter.ProcessedFrames)
		}
		name := "Up"
		if snapshot.Epoch > 0 {
			name = "Since reset"
		}
		out = append(out, Badge{Name: name, Text: Duration(frames, engine.SampleRate), Tone: ToneText})
	}
	blockMs := float64(engine.BlockSize) * 1000. / float64(max(engine.SampleRate, 1))
	out = append(out, Badge{
		Text: fmt.Sprintf("%.0f kHz · %.2f ms blocks", float64(engine.SampleRate)/1000., blockMs),
		Tone: ToneText,
	})
	if snapshot.Synthetic {
		out = append(out, Badge{Text: "Simulated", Tone: ToneWarn})
	}
	return out
}

// FormatLabel formats a channel count as "Mono", "Stereo", "N ch", or "" if unknown.
func FormatLabel(channels uint8) string {
	switch channels {
	case 0:
		return ""
	case 1:
		return "Mono"
	case 2:
		return "Stereo"
	}
	return fmt.Sprintf("%d ch", channels)
}

// Delay formats a path's latency in milliseconds, or "" when unreported.
func Delay(meter *core.TargetMeter, sampleRate uint32) string {
	if meter.LatencySamples == 0 {
		return ""
	}
	return fmt.Sprintf("%.1f ms", float64(meter.LatencySamples)*1000./float64(max(sampleRate, 1)))
}

// Fact is one labeled metric and its explanation in the Technical details drawer.
type Fact struct {
	Label string
	Value string
	Help  string
}

// NotReported is displayed when the mixer omitted a metric.
const NotReported = "Not reported"

// Facts builds the raw technical metrics backing the health checks.
func Facts(engine *core.EngineStatus, paths []PathView) []Fact {
	pair := func(a, b uint64) string { return Grouped(a) + " · " + Grouped(b) }
	recent := func(ns *uint64) string {
		if ns == nil {
			return "–"
		}
		return msBare(*ns)
	}
	blockMs := float64(engine.BlockSize) * 1000. / float64(max(engine.SampleRate, 1))
	p := sharedOf(paths)
	workers := workersOf(paths)
	worst := busiest(workers)
	sum := func(f func(m *core.TargetMeter) uint64) uint64 {
		var n uint64
		for _, w := range workers {
			n += f(&w.Meter)
		}
		return n
	}
	suppression := func(value func(w *core.Performance) string) string {
		if worst != nil {
			return value(worst)
		}
		if p != nil {
			return "Not running"
		}
		return NotReported
	}
	reported := func(value func(p *core.Performance) string) string {
		if p == nil {
			return NotReported
		}
		return value(p)
	}
	helper := NotReported
	if late, stalls, ok := helperOf(paths); ok {
		helper = pair(late, stalls)
	}
	var blocks, suppressionFrames uint64
	for _, path := range paths {
		blocks = max(blocks, path.Meter.Callbacks)
		suppressionFrames += path.Meter.SuppressionFrames
	}
	return []Fact{
		{
			"Sample rate · block",
			fmt.Sprintf("%.0f kHz · %d (%.2f ms)", float64(engine.SampleRate)/1000., engine.BlockSize, blockMs),
			"How often RC2FX gets audio from the mixer: each block holds this many samples. The block's length is the deadline for processing it.",
		},
		{
			"Callback avg · worst",
			reported(func(p *core.Performance) string {
				return fmt.Sprintf("%s · %s ms", msBare(meanNs(&p.Callback)), recent(p.CallbackRecentMaxNs))
			}),
			"Time RC2FX's audio work takes for one block. The average covers everything since the engine started or the last reset; worst is the slowest block in the last 10 seconds. Both must stay under the block's length.",
		},
		{
			"Callback overruns · clock errors",
			reported(func(p *core.Performance) string { return pair(p.Callback.Overruns, p.Callback.ClockErrors) }),
			"Overruns: blocks whose work took longer than the block's length. Clock errors: times the clock could not be read, so a block went unmeasured. Both should stay at 0.",
		},
		{
			"Suppression avg · worst · limit",
			suppression(func(w *core.Performance) string {
				return fmt.Sprintf("%s · %s · %s", msBare(meanNs(&w.WorkerWall)), recent(w.WorkerRecentMaxNs), msUnit(w.WorkerBudgetNs))
			}),
			"Time noise suppression takes to return one frame, for the busiest channel. The average covers everything since the engine started or the last reset; worst is the slowest frame in the last 10 seconds. A frame must come back within the limit, or it plays unprocessed.",
		},
		{
			"Suppression CPU avg",
			suppression(func(w *core.Performance) string { return msUnit(meanNs(&w.WorkerCPU)) }),
			"Processor time noise suppression actually used per frame, for the busiest channel. Lower than the turnaround above because the worker also waits its turn for the processor.",
		},
		{
			"Suppression late · stale",
			suppression(func(*core.Performance) string {
				return pair(sum(func(m *core.TargetMeter) uint64 { return m.MissedDeadlines }),
					sum(func(m *core.TargetMeter) uint64 { return m.StaleResults }))
			}),
			"Late: frames whose result was not ready in time, so the original audio played. Stale: results that came back after their turn and were thrown away. All channels together.",
		},
		{
			"Suppression drops in · out",
			suppression(func(*core.Performance) string {
				return pair(sum(func(m *core.TargetMeter) uint64 { return m.InputDrops }),
					sum(func(m *core.TargetMeter) uint64 { return m.OutputDrops }))
			}),
			"Frames lost on the way to the suppression worker (in) or on the way back (out) because its queue was full. All channels together. Should stay at 0.",
		},
		{
			"Helper late · stalls",
			helper,
			"The helper does work that can't happen inside the audio callback. Late: helper work that finished after its cycle. Stalls: callbacks that found the helper still busy. Both should stay at 0.",
		},
		{
			"Blocks · suppression frames",
			pair(blocks, suppressionFrames),
			"How many audio blocks, and noise suppression frames across all channels, have been processed since the engine started or the last reset.",
		},
		{
			"Generation · revision",
			pair(engine.Generation, engine.AppliedRevision),
			"Generation goes up each time the audio engine starts or restarts. Revision is the version of your settings the engine is running. Useful when reporting a problem.",
		},
	}
}

// FactColumns splits facts into two columns, filling the left column first.
func FactColumns(facts []Fact) (left, right []Fact) {
	half := (len(facts) + 1) / 2
	return facts[:half], facts[half:]
}

// Empty-state notices shown when no paths have fresh meters.
const (
	NoticeOffline     = "Connect to the mixer to see its telemetry."
	NoticeWaiting     = "Waiting for the mixer's state."
	NoticeInterrupted = "Connection interrupted · telemetry paused."
	NoticeNotRunning  = "The engine is not running, so no channel reports audio."
	NoticeNoFresh     = "No processed channel has fresh audio telemetry yet."
)

// IdleLine formats the list of processed paths with no active audio.
func IdleLine(idle []string) string { return "No audio right now: " + strings.Join(idle, ", ") }
