//go:build !rc2fxdev

package app

import "gpuikit.dev/gk"

// Development-only stubs for release and ordinary builds (`!rc2fxdev`).

// autoFlashPoll is the flash autopilot (autoflash_on.go); release and
// ordinary builds have none.
func (a *App) autoFlashPoll(cx *gk.Cx) {}

// fixDebug: a release build never asks for the storage fix's debug log.
func fixDebug() bool { return false }

// devFlows: only development builds open flow:<state> pages
// (flow_fixtures_on.go); this build refuses them.
const devFlows = false

// openFlowFixture is development builds' made-up flow pages; this build has
// none.
func (a *App) openFlowFixture(cx *gk.Cx, id string) {}

// previewMode is the telemetry preview (`--telemetry-preview`, preview_on.go);
// release builds have no previews so it never takes over.
func previewMode(args []string) bool { return false }
