package app

// This file does not import gk: its functions are tested with plain `go test`.

import (
	"encoding/json"
	"fmt"
)

// The Rust app's desk::apply_theme wrote 34 colours, the font size and two
// radii straight into the kit's Theme (`Theme::update`). The Go package
// cannot: gk.KitThemeColor has no field setters (only its default), and
// Theme.Colors returns a different record (KitThemeColor2). What the binding
// does carry is the kit's theme file format: ThemeRegistry.LoadThemesFromStr
// and Theme.ApplyConfig. So the same values go in as one theme per
// appearance, applied by name (applyTheme in desk.go).
//
// TODO(B1): a KitThemeColor with setters (or SetColors taking the record
// Colors returns) would let applyTheme write the fields as the Rust did.

// themeSetName is the theme set RC2FX loads into the kit's registry.
const themeSetName = "RC2FX"

// themeName is the registry name of an appearance's theme.
func themeName(a Appearance) string { return "RC2FX " + a.Label() }

// hexColor is a 0xRRGGBB colour in the theme file's "#rrggbb" form.
func hexColor(c uint32) string { return fmt.Sprintf("#%06x", c&0xffffff) }

// themeColors is desk.rs apply_theme's colours for one appearance, by the
// kit's theme-file keys (gpui-component src/theme/schema.rs
// ThemeConfigColors), over the kit's default dark colours.
func themeColors(a Appearance) map[string]string {
	sf := a.surface()
	colors := make(map[string]string, len(kitDefaultDarkColors)+34)
	for k, v := range kitDefaultDarkColors {
		colors[k] = v
	}
	set := func(key string, c uint32) { colors[key] = hexColor(c) }
	set("background", sf.Bg)
	set("foreground", Text)
	set("border", sf.Line)
	set("input.border", sf.LineStrong)
	set("muted.background", sf.Raised)
	set("muted.foreground", Muted)
	// Hovered menu and list items; distinct from the popover behind them.
	set("accent.background", sf.RaisedHover)
	set("accent.foreground", Text)
	set("popover.background", sf.Panel)
	set("popover.foreground", Text)
	// Switches, checkboxes, tabs and focus use `primary`: RC2FX cyan.
	set("primary.background", Accent)
	set("primary.hover.background", AccentHover)
	set("primary.active.background", AccentActive)
	set("primary.foreground", sf.Bg)
	// Primary buttons are white pills; cyan marks processing.
	set("button.primary.background", Text)
	set("button.primary.hover.background", PrimaryHover)
	set("button.primary.active.background", PrimaryActive)
	set("button.primary.foreground", sf.Bg)
	set("button.background", sf.Raised)
	set("button.hover.background", sf.RaisedHover)
	set("button.active.background", sf.LineStrong)
	set("button.foreground", Text)
	set("secondary.background", sf.Raised)
	set("secondary.hover.background", sf.RaisedHover)
	set("secondary.active.background", sf.LineStrong)
	set("secondary.foreground", Text)
	set("ring", Accent)
	set("caret", Accent)
	colors["selection.background"] = "#5ce1e640"
	set("list.active.background", sf.Selected)
	set("switch.background", sf.LineStrong)
	set("slider.background", Accent)
	set("slider.thumb.background", Text)
	set("scrollbar.thumb.background", sf.LineStrong)
	return colors
}

type themeFile struct {
	Name     string            `json:"name"`
	Mode     string            `json:"mode"`
	FontSize float32           `json:"font.size"`
	Radius   int               `json:"radius"`
	RadiusLg int               `json:"radius.lg"`
	Colors   map[string]string `json:"colors"`
}

// themeSetJSON is the kit theme file holding one theme per appearance.
func themeSetJSON() string {
	set := struct {
		Name   string      `json:"name"`
		Themes []themeFile `json:"themes"`
	}{Name: themeSetName}
	for _, a := range AppearanceAll {
		set.Themes = append(set.Themes, themeFile{
			Name:     themeName(a),
			Mode:     "dark",
			FontSize: RootText,
			Radius:   8,
			RadiusLg: 14,
			Colors:   themeColors(a),
		})
	}
	out, err := json.Marshal(set)
	if err != nil {
		panic(err) // only strings and numbers: cannot fail
	}
	return string(out)
}
