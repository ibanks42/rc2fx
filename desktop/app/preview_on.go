//go:build rc2fxdev

package app

// Dev-only layout previews (`rc2fx --telemetry-preview [old|events]`) populated
// with synthetic data and no mixer connection. Built only with `-tags rc2fxdev`.

import (
	"time"

	"github.com/ibanks42/goui-kit/gk"
	"github.com/ibanks42/goui-kit/gk/kit"
	"github.com/ibanks42/goui-kit/gk/s"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

const telemetryPreviewFlag = "--telemetry-preview"

// previewMode checks whether args request a telemetry preview and runs it.
func previewMode(args []string) bool {
	if len(args) < 2 || args[1] != telemetryPreviewFlag {
		return false
	}
	which := ""
	if len(args) > 2 {
		which = args[2]
	}
	if which == "events" {
		runEventsPreview()
	} else {
		runTelemetryPreview(which != "old")
	}
	return true
}

func previewTheme(cx *gk.Cx) {
	gk.QuitOnLastWindow(cx, true)
	gk.ThemeChange(cx, gk.ThemeModeDark)
	loadThemes(cx)
	applyTheme(cx)
}

func previewPath(id string, latency uint32, worker bool) core.TargetMeter {
	timing := func(calls, meanNs, maxNs, overruns uint64) core.Timing {
		return core.Timing{Calls: calls, TotalNs: calls * meanNs, MaxNs: maxNs, Overruns: overruns}
	}
	u64 := func(v uint64) *uint64 { return &v }
	m := core.TargetMeter{
		ID:              id,
		InputPeak:       0.32,
		OutputPeak:      0.21,
		RMS:             0.04,
		Spectrum:        []float64{},
		Callbacks:       663_214,
		ProcessedFrames: 74_279_968,
		LatencySamples:  latency,
	}
	p := &core.Performance{
		CallbackBudgetNs:    2_333_333,
		WorkerBudgetNs:      10_000_000,
		Xruns:               2,
		HelperLate:          u64(4),
		HelperStalls:        u64(0),
		CallbackRecentMaxNs: u64(640_000),
		WorkerRecentMaxNs:   u64(0),
		Callback:            timing(663_214, 410_000, 1_020_000, 0),
	}
	if worker {
		m.GainReduction, m.Voice, m.GateOpen = 3.4, 0.91, 1.
		m.SuppressionFrames = 154_747
		m.MissedDeadlines = 3
		p.WorkerRecentMaxNs = u64(6_260_000)
		p.WorkerWall = timing(154_747, 3_100_000, 0, 1)
		p.WorkerCPU = timing(154_747, 2_900_000, 6_000_000, 0)
	}
	m.Performance = p
	return m
}

func previewSnapshot(reported, details bool) TelemetrySnapshot {
	usb := previewPath("usb-main", 0, false)
	usb.OutputPeak = 1.0
	usb.Clipped = 12
	chat := previewPath("send-chat", 1184, true)
	chat.InputPeak, chat.OutputPeak = 0., 0.
	now := time.Now()
	paths := []telemetry.PathView{
		{
			Name: "Microphone 1", Channels: 1, Meter: previewPath("mic-1", 1184, true),
			Recent: telemetry.PathRecent{Late: telemetry.Recent{Count: 2, Last: &now}},
		},
		{Name: "USB main", Channels: 2, Meter: usb},
		{Name: "Chat Mix Out", Channels: 2, Meter: chat},
	}
	if !reported {
		for i := range paths {
			paths[i].Meter.Performance = nil
		}
	}
	var epoch uint64
	if reported {
		epoch = 1
	}
	earlier := now.Add(-20 * time.Minute)
	return TelemetrySnapshot{
		HasEngine: true,
		Engine: core.EngineStatus{
			Generation: 5, Mode: core.EngineModeNative, Status: core.EnginePhaseRunning,
			AppliedRevision: 235, SampleRate: 48_000, BlockSize: 112, AudioAdvancing: true,
		},
		Routing: core.RoutingStatus{Status: core.RoutingPhaseActive, Channels: 4},
		Epoch:   epoch,
		Paths:   paths,
		Xruns:   telemetry.Recent{Last: &earlier},
		Now:     now,
		Idle:    []string{"Bluetooth"},
		Details: details,
	}
}

type telemetryPreview struct {
	reported bool
	details  bool
}

func (p *telemetryPreview) view(cx *gk.Cx) gk.Element {
	mono := gk.KitTheme(cx).MonoFontFamily(cx)
	var since *time.Time
	if p.reported {
		at := time.Now().Add(-14 * time.Minute)
		since = &at
	}
	var epoch uint64
	if p.reported {
		epoch = 1
	}
	noop := func(*gk.Cx) {}
	snapshot := previewSnapshot(p.reported, p.details)
	aside := gk.HFlex(s.ItemsCenter, s.Gap3).
		Child(gk.Div(s.TextXs).TextColor(rgb(Faint)).Child(txt("Live · " + telemetry.SinceLabel(since, epoch)))).
		Child(kit.Button("show-events").Outline().Small().Icon(gk.IconExternalLink).Label("Events")).
		Child(resetControl("telemetry-reset", "Reset…", false, true, noop, noop, noop))
	toggle := kit.Switch("telemetry-details").Small().Checked(p.details).Label("Technical details").
		OnClick(func(cx *gk.Cx, on bool) { p.details = on })
	return gk.Div(s.SizeFull, s.Px6, s.Py5).Bg(rgb(surface().Bg)).TextColor(rgb(Text)).
		ID("telemetry-preview").OverflowYScroll().
		Child(gk.VFlex(s.Gap6, s.WFull, s.MxAuto).MaxW(px(760)).
			Child(telemetrySection(&snapshot, mono, aside, toggle)))
}

func runTelemetryPreview(reported bool) {
	p := &telemetryPreview{reported: reported}
	gk.Run(func(cx *gk.Cx) {
		// Theme initialization requires an open window, so apply it in the view init.
		w := gk.OpenWindowWith(cx, windowIdentity(gk.WindowConfig{
			Width: 900, Height: 1400, Title: "RC2FX telemetry preview",
		}), gk.NewView(previewTheme, p.view))
		gk.ActivateWindow(cx, w)
	})
}

func previewLog() EventLog {
	const S uint64 = 1_000_000_000
	u64 := func(v uint64) *uint64 { return &v }
	str := func(v string) *string { return &v }
	ev := func(seq uint64, kind core.DiagnosticsKind, path *string, at uint64, message string,
		details core.EventDetails) core.DiagnosticsEvent {
		return core.DiagnosticsEvent{Seq: seq, Kind: kind, Path: path, AtNs: at, Message: message, Details: details}
	}
	page := func(nowNs, lost uint64, events ...core.DiagnosticsEvent) core.DiagnosticsEvents {
		return core.DiagnosticsEvents{NowNs: nowNs, NextSeq: 45, Lost: lost, More: false, Events: events}
	}
	now := time.Now()
	log := newEventLog()
	log.Connected = true
	log.Epoch = 1
	log.PeriodNs = u64(2_333_333)
	name := func(id string) string {
		if label, ok := model.OutputLabel(id); ok {
			return label
		}
		if id == "mic-1" {
			return "Microphone 1"
		}
		return id
	}
	log.ReceivePage(page(5_000*S, 0,
		ev(1, core.DiagnosticsKindReset, nil, 4_155*S, "Diagnostics reset", core.EventDetails{}),
		ev(2, core.DiagnosticsKindXrun, nil, 4_388*S, "JACK xrun (jackd reported 0 us of backend delay)",
			core.EventDetails{ValueNs: u64(688_000), DelayedUs: u64(0)}),
		ev(3, core.DiagnosticsKindHelperLateFinish, str("mic-1"), 4_598*S,
			"the helper finished after its frame's cycle", core.EventDetails{ValueNs: u64(2_743_566)}),
		ev(4, core.DiagnosticsKindMissedDeadline, str("send-chat"), 4_905*S,
			"Suppression missed a frame; the delayed original played", core.EventDetails{}),
		ev(5, core.DiagnosticsKindLateEnd, nil, 4_931*S, "the callback ended late",
			core.EventDetails{ValueNs: u64(2_100_000)}),
	), now.Add(-60*time.Second), name)
	log.ReceivePage(page(9_000*S, 37,
		ev(42, core.DiagnosticsKindOutputDrops, str("mic-1"), 8_988*S, "3 output drops on Microphone 1",
			core.EventDetails{Delta: u64(3), IntervalNs: u64(500_000_000)}),
		ev(43, core.DiagnosticsKindStaleResults, str("send-chat"), 8_996*S, "1 stale result on Chat Mix Out",
			core.EventDetails{Delta: u64(1), IntervalNs: u64(500_000_000)}),
		ev(44, core.DiagnosticsKindHelperStall, nil, 8_999*S,
			"the callback passed its frame through (helper stall)", core.EventDetails{ValueNs: u64(200_000)}),
	), now, name)
	log.Expanded = u64(2)
	return log
}

func runEventsPreview() {
	log := gk.NewModel(previewLog())
	gk.Run(func(cx *gk.Cx) {
		w := openEventsWindow(cx, nil, &log, nil, nil)
		previewTheme(cx) // Must run after a window is open.
		gk.ActivateWindow(cx, *w)
	})
}
