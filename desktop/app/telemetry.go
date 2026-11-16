package app

// telemetry.rs lives in app/telemetry (no drawing). These are the names
// package app has called it by since WP1's stand-ins (stubs.go's naming), so
// the call sites in app.go and view.go stay as they are.

import (
	"time"

	"github.com/ibanks42/rc2fx/desktop/app/telemetry"
	"github.com/ibanks42/rc2fx/desktop/core"
)

type (
	telemetryProgress          = telemetry.Progress
	telemetryWarning           = telemetry.Warning
	telemetryStartupWindow     = telemetry.StartupWindow
	telemetryRecentFaults      = telemetry.RecentFaults
	telemetryEngineErrorNotice = telemetry.EngineErrorNotice
)

// telemetryEngineErrorNoticeTitle is EngineErrorNotice::TITLE.
const telemetryEngineErrorNoticeTitle = telemetry.EngineErrorNoticeTitle

// telemetryReceiveMeter is telemetry::receive_meter.
func telemetryReceiveMeter(state *core.DeviceState, meter *core.Meter, progress map[string]*telemetryProgress,
	engineSeen **time.Time, now time.Time) bool {
	return telemetry.ReceiveMeter(state, meter, progress, engineSeen, now)
}

// telemetryFreshMeter is telemetry::fresh_meter: the path's reading and
// whether it is synthetic; nil for None.
func telemetryFreshMeter(state *core.DeviceState, meter *core.Meter, progress map[string]*telemetryProgress,
	targetID string, engineSeen *time.Time, now time.Time) (*core.TargetMeter, bool) {
	return telemetry.FreshMeter(state, meter, progress, targetID, engineSeen, now)
}

// telemetryEngineError is telemetry::engine_error.
func telemetryEngineError(state *core.DeviceState) (telemetryEngineErrorNotice, bool) {
	return telemetry.EngineError(state)
}
