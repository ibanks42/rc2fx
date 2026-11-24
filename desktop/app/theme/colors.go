package theme

import (
	"fmt"
	"math"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/num"
)

// Shared palette across all appearances. Cyan/violet indicate active
// processing; amber/red are reserved for warnings and errors.
const (
	Text          uint32 = 0xf2f4f7
	Muted         uint32 = 0x939cad
	Faint         uint32 = 0x737c91
	Accent        uint32 = 0x5ce1e6
	AccentHover   uint32 = 0x7ee8ec
	AccentActive  uint32 = 0x3fc9cf
	Violet        uint32 = 0xa78bfa
	Warn          uint32 = 0xf5c451
	Danger        uint32 = 0xf17e7e
	DangerBg      uint32 = 0x3a2030
	PrimaryHover  uint32 = 0xffffff
	PrimaryActive uint32 = 0xd9dde4
	Dim           uint32 = 0x4c5468
)

// HslaOf converts 0xRRGGBBAA to HSLA using float32 arithmetic matching GPUI's
// Hsla::from(rgba(hex)).
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

func remEuclid(x, m float32) float32 {
	r := float32(math.Mod(float64(x), float64(m)))
	if r < 0 {
		r += float32(math.Abs(float64(m)))
	}
	return r
}

// MixHex linearly interpolates two 0xRRGGBB colors for t in [0, 1].
func MixHex(from, to uint32, t float32) uint32 {
	mix := func(shift uint32) uint32 {
		a := float32(from >> shift & 0xff)
		b := float32(to >> shift & 0xff)
		return uint32(roundF32(a+(b-a)*t)) << shift
	}
	return mix(16) | mix(8) | mix(0)
}

func roundF32(v float32) float32 { return float32(math.Round(float64(v))) }

// BandColor cycles through eight steps from Accent to Violet for EQ bands.
func BandColor(index int) uint32 {
	t := float32(index%8) / 7
	return MixHex(Accent, Violet, t)
}

// Level maps a linear peak amplitude to a [0, 1] meter position over 60 dB.
func Level(peak float64) float32 {
	v := (20*math.Log10(num.Max(peak, 1e-6)) + 60) / 60
	return float32(num.Clamp(v, 0, 1))
}

func FormatHz(frequency float64) string {
	if frequency >= 1000 {
		return fmt.Sprintf("%.1f kHz", frequency/1000)
	}
	return fmt.Sprintf("%.0f Hz", frequency)
}

func DBFS(peak float64) string {
	db := 20 * math.Log10(num.Max(peak, 1e-9))
	if db <= -60 {
		return "below −60 dBFS"
	}
	return strings.ReplaceAll(fmt.Sprintf("%.0f dBFS", db), "-", "−")
}
