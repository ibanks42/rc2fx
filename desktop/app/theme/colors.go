package theme

// This file does not import gk: its functions are tested with plain `go test`.

import (
	"fmt"
	"math"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/num"
)

// Text, accent and problem colors are shared by every appearance; the
// surfaces come from `surface()` (desk.go). Cyan and violet mark processing;
// amber and red mark problems.
const (
	Text         uint32 = 0xf2f4f7
	Muted        uint32 = 0x939cad
	Faint        uint32 = 0x737c91
	Accent       uint32 = 0x5ce1e6
	AccentHover  uint32 = 0x7ee8ec
	AccentActive uint32 = 0x3fc9cf
	Violet       uint32 = 0xa78bfa
	Warn         uint32 = 0xf5c451
	Danger       uint32 = 0xf17e7e
	DangerBg     uint32 = 0x3a2030
	// Primary actions are white pills.
	PrimaryHover  uint32 = 0xffffff
	PrimaryActive uint32 = 0xd9dde4
	// The draft's dimmest text (`--dim`): off values and empty readouts.
	Dim uint32 = 0x4c5468
)

// hslaOf is gpui's `Hsla::from(rgba(hex))`: the same f32 arithmetic, so the
// colors match what gpui computes for the same hex. hex is 0xRRGGBBAA.
func HslaOf(hex uint32) [4]float32 {
	r := float32(hex>>24&0xff) / 255
	g := float32(hex>>16&0xff) / 255
	b := float32(hex>>8&0xff) / 255
	a := float32(hex&0xff) / 255
	maxC := max(r, g, b)
	minC := min(r, g, b)
	delta := maxC - minC
	l := (maxC + minC) / 2
	var s float32
	switch {
	case l == 0 || l == 1:
		s = 0
	case l < 0.5:
		s = delta / (2 * l)
	default:
		s = delta / (2 - 2*l)
	}
	var h float32
	switch {
	case delta == 0:
		h = 0
	case maxC == r:
		h = remEuclid((g-b)/delta, 6) / 6
	case maxC == g:
		h = ((b-r)/delta + 2) / 6
	default:
		h = ((r-g)/delta + 4) / 6
	}
	return [4]float32{h, s, l, a}
}

// remEuclid is x modulo m, never negative. fmod is exact, so doing it in
// float64 gives the float32 result.
func remEuclid(x, m float32) float32 {
	r := float32(math.Mod(float64(x), float64(m)))
	if r < 0 {
		r += float32(math.Abs(float64(m)))
	}
	return r
}

// mixHex blends two 0xRRGGBB colors, t from 0 (from) to 1 (to), rounding
// each channel half away from zero (roundF32).
func MixHex(from, to uint32, t float32) uint32 {
	mix := func(shift uint32) uint32 {
		a := float32(from >> shift & 0xff)
		b := float32(to >> shift & 0xff)
		return uint32(roundF32(a+(b-a)*t)) << shift
	}
	return mix(16) | mix(8) | mix(0)
}

// roundF32 rounds to a whole number, halves away from zero.
func roundF32(v float32) float32 { return float32(math.Round(float64(v))) }

// bandColor is a band's color: eight steps from RC2FX cyan to violet,
// repeating. Amber and red stay reserved for problems.
func BandColor(index int) uint32 {
	t := float32(index%8) / 7
	return MixHex(Accent, Violet, t)
}

// level is peak amplitude as a 0..1 meter position over a 60 dB range.
func Level(peak float64) float32 {
	v := (20*math.Log10(num.Max(peak, 1e-6)) + 60) / 60
	return float32(num.Clamp(v, 0, 1))
}

// formatHz is a frequency as text: Hz below 1 kHz, kHz with one decimal
// from there.
func FormatHz(frequency float64) string {
	if frequency >= 1000 {
		return fmt.Sprintf("%.1f kHz", frequency/1000)
	}
	return fmt.Sprintf("%.0f Hz", frequency)
}

// dbfs is a reported peak as dBFS text.
func DBFS(peak float64) string {
	db := 20 * math.Log10(num.Max(peak, 1e-9))
	if db <= -60 {
		return "below −60 dBFS"
	}
	return strings.ReplaceAll(fmt.Sprintf("%.0f dBFS", db), "-", "−")
}
