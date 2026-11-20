package theme

import (
	"encoding/json"
	"testing"
)

// The hex helpers must give gpui's Hsla::from(rgb(x)) exactly.
func TestHexColorsConvertAsGpuiDoes(t *testing.T) {
	for _, c := range []struct {
		hex  uint32
		want [4]float32
	}{
		{0xffffffff, [4]float32{0, 0, 1, 1}},
		{0x000000ff, [4]float32{0, 0, 0, 1}},
		{0xff0000ff, [4]float32{0, 1, 0.5, 1}},
		{0x00ff00ff, [4]float32{float32(2) / 6, 1, 0.5, 1}},
		{0x0000ffff, [4]float32{float32(4) / 6, 1, 0.5, 1}},
		{0xff00ffff, [4]float32{float32(5) / 6, 1, 0.5, 1}},
	} {
		if got := HslaOf(c.hex); got != c.want {
			t.Errorf("HslaOf(%08x) = %v, want %v", c.hex, got, c.want)
		}
	}
}

func TestAlphaIsTheLowByte(t *testing.T) {
	if got := HslaOf(0x5ce1e640)[3]; got != float32(0x40)/255 {
		t.Fatalf("alpha %v", got)
	}
}

func TestBandColorsRunFromCyanToViolet(t *testing.T) {
	if BandColor(0) != Accent || BandColor(7) != Violet || BandColor(8) != Accent {
		t.Fatalf("%06x %06x %06x", BandColor(0), BandColor(7), BandColor(8))
	}
}

func TestReadoutsFormatAsTheRustDid(t *testing.T) {
	for _, c := range []struct {
		peak float64
		want string
	}{
		{1, "0 dBFS"},
		{0.5, "−6 dBFS"},
		{0.0001, "below −60 dBFS"},
		{0, "below −60 dBFS"},
	} {
		if got := DBFS(c.peak); got != c.want {
			t.Errorf("DBFS(%v) = %q, want %q", c.peak, got, c.want)
		}
	}
	if Level(1) != 1 || Level(0) != 0 || Level(1e-7) != 0 || Level(10) != 1 {
		t.Errorf("level: %v %v %v %v", Level(1), Level(0), Level(1e-7), Level(10))
	}
	if FormatHz(80) != "80 Hz" || FormatHz(1500) != "1.5 kHz" {
		t.Errorf("%q %q", FormatHz(80), FormatHz(1500))
	}
}

func TestTheThemeCarriesEveryAppearance(t *testing.T) {
	var set struct {
		Name   string
		Themes []struct {
			Name     string            `json:"name"`
			Mode     string            `json:"mode"`
			FontSize float32           `json:"font.size"`
			Radius   int               `json:"radius"`
			RadiusLg int               `json:"radius.lg"`
			Colors   map[string]string `json:"colors"`
		} `json:"themes"`
	}
	if err := json.Unmarshal([]byte(SetJSON()), &set); err != nil {
		t.Fatal(err)
	}
	if len(set.Themes) != len(AppearanceAll) {
		t.Fatalf("%d themes", len(set.Themes))
	}
	for i, th := range set.Themes {
		a := AppearanceAll[i]
		if th.Name != Name(a) || th.Mode != "dark" || th.FontSize != RootText || th.Radius != 8 || th.RadiusLg != 14 {
			t.Errorf("%+v", th)
		}
		if th.Colors["background"] != HexColor(a.surface().Bg) || th.Colors["button.primary.background"] != "#f2f4f7" ||
			th.Colors["selection.background"] != "#5ce1e640" {
			t.Errorf("%s: %v", th.Name, th.Colors)
		}
		// Everything RC2FX does not set keeps the kit's default dark color.
		for k, v := range kitDefaultDarkColors {
			if !rc2fxKey(k) && th.Colors[k] != v {
				t.Errorf("%s: %s = %s, kit default %s", th.Name, k, th.Colors[k], v)
			}
		}
	}
}

func rc2fxKey(key string) bool {
	for _, k := range []string{
		"background", "foreground", "border", "input.border", "muted.background", "muted.foreground",
		"accent.background", "accent.foreground", "popover.background", "popover.foreground",
		"primary.background", "primary.hover.background", "primary.active.background", "primary.foreground",
		"button.primary.background", "button.primary.hover.background", "button.primary.active.background",
		"button.primary.foreground", "button.background", "button.hover.background", "button.active.background",
		"button.foreground", "secondary.background", "secondary.hover.background", "secondary.active.background",
		"secondary.foreground", "ring", "caret", "selection.background", "list.active.background",
		"switch.background", "slider.background", "slider.thumb.background", "scrollbar.thumb.background",
	} {
		if k == key {
			return true
		}
	}
	return false
}
