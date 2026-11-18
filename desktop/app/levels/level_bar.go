package levels

// The overview's level bars: which part of its track a bar fills. The app
// draws the fill as a div inside the track.

const (
	// Lowest is the shortest bar with a fresh reading, as a fraction of its
	// track.
	Lowest float32 = 0.02
	// Stub is the stub shown without a fresh reading, in pixels.
	Stub float32 = 6
)

// Bar is a level bar's direction.
type Bar int

const (
	// BarVertical is a fader's level: filled from the bottom.
	BarVertical Bar = iota
	// BarHorizontal is an output's level: filled from the left.
	BarHorizontal
)

// FillFraction is the part of the track's length a fresh level fills.
func FillFraction(level float32) float32 { return min(max(level, Lowest), 1) }

// Radius is the fill's corner radius in pixels.
func (b Bar) Radius() float32 {
	if b == BarVertical {
		return 6
	}
	return 4
}
