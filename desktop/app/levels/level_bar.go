package levels

const (
	// Minimum fill fraction for a live reading so silence stays visible.
	Lowest float32 = 0.02
	// Fixed stub length in pixels when no fresh reading is available.
	Stub float32 = 6
)

// Bar selects the fill orientation of a level bar.
type Bar int

const (
	BarVertical Bar = iota
	BarHorizontal
)

// FillFraction clamps a live level to [Lowest, 1].
func FillFraction(level float32) float32 { return min(max(level, Lowest), 1) }

// Radius returns the fill corner radius in pixels.
func (b Bar) Radius() float32 {
	if b == BarVertical {
		return 6
	}
	return 4
}
