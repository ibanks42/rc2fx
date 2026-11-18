package app

// This file does not import gk: its functions are tested with plain `go test`.

import "sync/atomic"

// Surface is the window's surfaces for one appearance. Text, accent and
// problem colors are shared (colors.go).
type Surface struct {
	Bg          uint32
	Sunken      uint32
	Panel       uint32
	Raised      uint32
	RaisedHover uint32
	Selected    uint32
	Line        uint32
	LineStrong  uint32
}

// Appearance is the app's color scheme, chosen in Settings.
type Appearance int

const (
	// AppearanceDusk is deep blue-gray, the default.
	AppearanceDusk Appearance = iota
	// AppearanceGraphite is neutral dark gray.
	AppearanceGraphite
	// AppearanceBlack is near black.
	AppearanceBlack
)

// AppearanceAll is every appearance, in menu order.
var AppearanceAll = [3]Appearance{AppearanceDusk, AppearanceGraphite, AppearanceBlack}

// Key is the appearance's name in view.json.
func (a Appearance) Key() string {
	switch a {
	case AppearanceGraphite:
		return "graphite"
	case AppearanceBlack:
		return "black"
	}
	return "dusk"
}

func (a Appearance) Label() string {
	switch a {
	case AppearanceGraphite:
		return "Graphite"
	case AppearanceBlack:
		return "Black"
	}
	return "Dusk"
}

// AppearanceFromKey is the appearance whose Key is key; false for none.
func AppearanceFromKey(key string) (Appearance, bool) {
	for _, a := range AppearanceAll {
		if a.Key() == key {
			return a, true
		}
	}
	return AppearanceDusk, false
}

func (a Appearance) surface() *Surface {
	switch a {
	case AppearanceGraphite:
		return &graphite
	case AppearanceBlack:
		return &black
	}
	return &dusk
}

var dusk = Surface{
	Bg:          0x131622,
	Sunken:      0x0f121b,
	Panel:       0x181c2a,
	Raised:      0x1f2434,
	RaisedHover: 0x272d40,
	Selected:    0x1e2438,
	Line:        0x232839,
	LineStrong:  0x2e3448,
}

var graphite = Surface{
	Bg:          0x15171c,
	Sunken:      0x101216,
	Panel:       0x1a1d23,
	Raised:      0x22252c,
	RaisedHover: 0x2a2e36,
	Selected:    0x252830,
	Line:        0x262930,
	LineStrong:  0x31353e,
}

var black = Surface{
	Bg:          0x050608,
	Sunken:      0x030405,
	Panel:       0x0b0d11,
	Raised:      0x12151b,
	RaisedHover: 0x1a1e26,
	Selected:    0x141820,
	Line:        0x16191f,
	LineStrong:  0x22262e,
}

// currentAppearance is the current appearance, as an index into
// AppearanceAll. Read on every render, written only by setAppearance.
var currentAppearance atomic.Uint32

func appearance() Appearance {
	i := int(currentAppearance.Load())
	return AppearanceAll[min(i, len(AppearanceAll)-1)]
}

// surface is the current appearance's surfaces.
func surface() *Surface { return appearance().surface() }

// storeAppearance is the state half of changing the appearance (the theme
// half is setAppearance in desk.go).
func storeAppearance(a Appearance) {
	i := 0
	for j, x := range AppearanceAll {
		if x == a {
			i = j
			break
		}
	}
	currentAppearance.Store(uint32(i))
}
