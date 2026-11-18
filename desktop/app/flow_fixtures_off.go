//go:build !rc2fxdev

package app

import "gpuikit.dev/gk"

// devFlows: only development builds open flow:<state> pages
// (flow_fixtures_on.go); this build refuses them.
const devFlows = false

// openFlowFixture is development builds' made-up flow pages; this build has
// none.
func (a *App) openFlowFixture(cx *gk.Cx, id string) {}
