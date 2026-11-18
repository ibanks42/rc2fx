//go:build !rc2fxdev

package app

import "gpuikit.dev/gk"

// autoFlashPoll is the flash autopilot (autoflash_on.go); release and
// ordinary builds have none.
func (a *App) autoFlashPoll(cx *gk.Cx) {}
