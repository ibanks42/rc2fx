package levels

// The overview's level bars (level_bar.rs): which part of its track a bar
// fills. The app draws the fill as a div inside the track (audit 2.3).

const (
	// Lowest is the shortest bar with a fresh reading, as a fraction of its
	// track.
	Lowest float32 = 0.02
	// Stub is the stub shown without a fresh reading, in pixels.
	Stub float32 = 6
)

// Bar is level_bar.rs Bar.
type Bar int

const (
	// BarVertical is a fader's level: filled from the bottom.
	BarVertical Bar = iota
	// BarHorizontal is an output's level: filled from the left.
	BarHorizontal
)

// Rect is a rectangle in pixels: its top left and its size.
type Rect struct{ X, Y, W, H float32 }

// Bottom is the rectangle's bottom edge.
func (r Rect) Bottom() float32 { return r.Y + r.H }

// FillFraction is the part of the track's length a fresh level fills.
func FillFraction(level float32) float32 { return min(max(level, Lowest), 1) }

// Fill is the part of track to fill for level; nil level is the stub.
func (b Bar) Fill(track Rect, level *float32) Rect {
	length := func(whole float32) float32 {
		if level == nil {
			return Stub
		}
		return whole * FillFraction(*level)
	}
	switch b {
	case BarVertical:
		height := length(track.H)
		return Rect{track.X, track.Y + track.H - height, track.W, height}
	default:
		return Rect{track.X, track.Y, length(track.W), track.H}
	}
}

// Radius is the fill's corner radius in pixels.
func (b Bar) Radius() float32 {
	if b == BarVertical {
		return 6
	}
	return 4
}
