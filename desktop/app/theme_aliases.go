package app

import (
	"github.com/ibanks42/rc2fx/desktop/app/theme"
	"github.com/ibanks42/rc2fx/desktop/core"
)

// Re-exports of the cgo-free desktop/app/theme package so UI code stays
// concise while theme/color/layout logic and tests stay independent of gk.

type (
	Surface    = theme.Surface
	Appearance = theme.Appearance
)

const (
	Text          = theme.Text
	Muted         = theme.Muted
	Faint         = theme.Faint
	Accent        = theme.Accent
	AccentHover   = theme.AccentHover
	AccentActive  = theme.AccentActive
	Violet        = theme.Violet
	Warn          = theme.Warn
	Danger        = theme.Danger
	DangerBg      = theme.DangerBg
	PrimaryHover  = theme.PrimaryHover
	PrimaryActive = theme.PrimaryActive
	Dim           = theme.Dim

	AppearanceDusk     = theme.AppearanceDusk
	AppearanceGraphite = theme.AppearanceGraphite
	AppearanceBlack    = theme.AppearanceBlack

	RootText          = theme.RootText
	DisplayText       = theme.DisplayText
	DisplayTextSmall  = theme.DisplayTextSmall
	TitleText         = theme.TitleText
	HeadingText       = theme.HeadingText
	SubheadingText    = theme.SubheadingText
	LeadText          = theme.LeadText
	PageX             = theme.PageX
	PageTop           = theme.PageTop
	PageBottom        = theme.PageBottom
	SectionGap        = theme.SectionGap
	SidebarWidth      = theme.SidebarWidth
	RailWidth         = theme.RailWidth
	EditorBottom      = theme.EditorBottom
	NarrowEditor      = theme.NarrowEditor
	EditorMinHeight   = theme.EditorMinHeight
	MeterHeight       = theme.MeterHeight
	LargeButtonHeight = theme.LargeButtonHeight
	InfoWidth         = theme.InfoWidth
	appID             = theme.AppID
)

var (
	Window        = theme.Window
	WindowMin     = theme.WindowMin
	AppearanceAll = theme.AppearanceAll
)

func hslaOf(hex uint32) [4]float32                { return theme.HslaOf(hex) }
func mixHex(from, to uint32, t float32) uint32    { return theme.MixHex(from, to, t) }
func bandColor(index int) uint32                  { return theme.BandColor(index) }
func level(peak float64) float32                  { return theme.Level(peak) }
func formatHz(frequency float64) string           { return theme.FormatHz(frequency) }
func dbfs(peak float64) string                    { return theme.DBFS(peak) }
func appearance() Appearance                      { return theme.CurrentAppearance() }
func surface() *Surface                           { return theme.CurrentSurface() }
func storeAppearance(a Appearance)                { theme.StoreAppearance(a) }
func themeName(a Appearance) string               { return theme.Name(a) }
func themeSetJSON() string                        { return theme.SetJSON() }
func shortEffect(effect core.Effect) string       { return theme.ShortEffect(effect) }
func effectIndex(effect core.Effect) int          { return theme.EffectIndex(effect) }
func visibleEffects(t *core.Target) []core.Effect { return theme.VisibleEffects(t) }
func stageSummary(t *core.Target, e core.Effect, c core.Channel) string {
	return theme.StageSummary(t, e, c)
}
