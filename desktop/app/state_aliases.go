package app

import (
	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/app/state"
	"github.com/ibanks42/rc2fx/desktop/app/theme"
)

// Re-exports of the cgo-free desktop/app/state package so UI code stays
// concise while navigation, link lifecycle, dev-page parsing, preferences, and
// message classification stay testable without gk/cgo.

type (
	navigation  = state.Navigation
	linkIntent  = state.LinkIntent
	DevPageKind = state.DevPageKind
	DevPage     = state.DevPage
)

const (
	DevPageHome        = state.DevPageHome
	DevPageSettings    = state.DevPageSettings
	DevPageDiagnostics = state.DevPageDiagnostics
	DevPageInstaller   = state.DevPageInstaller
	DevPageRepair      = state.DevPageRepair
	DevPageChannel     = state.DevPageChannel
	DevPageFlow        = state.DevPageFlow

	mixerChangedElsewhere = state.MixerChangedElsewhere
	USBUndo               = state.USBUndo
)

func DevPageFromEnv() *DevPage                        { return state.DevPageFromEnv(devFlows) }
func findChannel(n [][2]string, w string) (int, bool) { return state.FindChannel(n, w) }
func progressMessage(message string) bool             { return state.ProgressMessage(message) }
func saveFailedMessage(reason string) string          { return state.SaveFailedMessage(reason) }
func rememberedTelemetry() bool                       { return state.RememberedTelemetry() }
func rememberTelemetry(on bool)                       { state.RememberTelemetry(on) }
func rememberedAppearance() theme.Appearance          { return state.RememberedAppearance() }
func rememberAppearance(a theme.Appearance)           { state.RememberAppearance(a) }
func rememberedDuo() bool                             { return state.RememberedDuo() }
func rememberDuo(allow bool) error                    { return state.RememberDuo(allow) }
func rememberedFirstChannelHint() bool                { return state.RememberedFirstChannelHint() }
func rememberFirstChannelHint()                       { state.RememberFirstChannelHint() }
func rememberedDeveloper() bool                       { return state.RememberedDeveloper() }
func rememberDeveloper(on bool) error                 { return state.RememberDeveloper(on) }
func usbPermission(lastResult string) bool            { return state.USBPermission(lastResult) }
func mixerFound(lastResult string) bool               { return state.MixerFound(lastResult) }
func accessOf(err error) installflow.Access           { return state.AccessOf(err) }
