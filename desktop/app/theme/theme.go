package theme

import (
	"encoding/json"
	"fmt"
)

// gk.KitThemeColor does not expose field setters, so we serialize each
// appearance as a theme-set JSON loaded via ThemeRegistry.LoadThemesFromStr
// and applied by name.
//
// TODO(B1): switch to direct field updates if KitThemeColor gains setters.

const themeSetName = "RC2FX"

func Name(a Appearance) string { return "RC2FX " + a.Label() }

func HexColor(c uint32) string { return fmt.Sprintf("#%06x", c&0xffffff) }

// themeColors builds the gpui-component color table for appearance a on top of
// the kit's default dark theme.
func themeColors(a Appearance) map[string]string {
	sf := a.surface()
	colors := make(map[string]string, len(kitDefaultDarkColors)+34)
	for k, v := range kitDefaultDarkColors {
		colors[k] = v
	}
	set := func(key string, c uint32) { colors[key] = HexColor(c) }
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

// SetJSON marshals the theme set JSON containing one theme per appearance.
func SetJSON() string {
	set := struct {
		Name   string      `json:"name"`
		Themes []themeFile `json:"themes"`
	}{Name: themeSetName}
	for _, a := range AppearanceAll {
		set.Themes = append(set.Themes, themeFile{
			Name:     Name(a),
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
