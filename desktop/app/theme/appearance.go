package theme

import "sync/atomic"

// Surface defines the background and border colors for one appearance.
// Shared text, accent, and status colors live in colors.go.
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

type Appearance int

const (
	AppearanceDusk Appearance = iota
	AppearanceGraphite
	AppearanceBlack
)

var AppearanceAll = [3]Appearance{AppearanceDusk, AppearanceGraphite, AppearanceBlack}

// Key returns the identifier stored in view.json.
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

// Index into AppearanceAll; read on every render, updated by StoreAppearance.
var currentAppearance atomic.Uint32

func CurrentAppearance() Appearance {
	i := int(currentAppearance.Load())
	return AppearanceAll[min(i, len(AppearanceAll)-1)]
}

func CurrentSurface() *Surface { return CurrentAppearance().surface() }

func StoreAppearance(a Appearance) {
	i := 0
	for j, x := range AppearanceAll {
		if x == a {
			i = j
			break
		}
	}
	currentAppearance.Store(uint32(i))
}
