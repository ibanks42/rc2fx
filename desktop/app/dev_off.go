//go:build !rc2fxdev

package app

import "gpuikit.dev/gk"

// No-op stubs for development-only hooks in non-dev builds.

func (a *App) autoFlashPoll(cx *gk.Cx) {}

func fixDebug() bool { return false }

const devFlows = false

func (a *App) openFlowFixture(cx *gk.Cx, id string) {}

func previewMode(args []string) bool { return false }
