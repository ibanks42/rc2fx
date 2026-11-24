package num

import (
	"math"
	"testing"
)

func TestMaxAndMinIgnoreNaN(t *testing.T) {
	nan := math.NaN()
	for _, c := range []struct {
		a, b, max, min float64
	}{
		{1, 2, 2, 1},
		{2, 1, 2, 1},
		{nan, 3, 3, 3},
		{3, nan, 3, 3},
	} {
		if got := Max(c.a, c.b); got != c.max {
			t.Errorf("Max(%v, %v) = %v", c.a, c.b, got)
		}
		if got := Min(c.a, c.b); got != c.min {
			t.Errorf("Min(%v, %v) = %v", c.a, c.b, got)
		}
	}
	if !math.IsNaN(Max(nan, nan)) || !math.IsNaN(Min(nan, nan)) {
		t.Error("two NaNs give a number")
	}
	if got := Max(float32(math.NaN()), 0.5); got != 0.5 {
		t.Errorf("float32 Max = %v", got)
	}
}

func TestClampKeepsNaN(t *testing.T) {
	if Clamp(-1.0, 0, 1) != 0 || Clamp(2.0, 0, 1) != 1 || Clamp(0.5, 0, 1) != 0.5 {
		t.Error("clamp out of range")
	}
	if !math.IsNaN(Clamp(math.NaN(), 0, 1)) {
		t.Error("NaN clamped to a number")
	}
}

func TestSatSub(t *testing.T) {
	if SatSub(5, 3) != 2 || SatSub(3, 5) != 0 || SatSub(0, math.MaxUint64) != 0 {
		t.Error("saturating subtraction")
	}
}

func TestFormatDecimal(t *testing.T) {
	for _, c := range []struct {
		v    float64
		want string
	}{
		{0, "0"},
		{1.5, "1.5"},
		{math.NaN(), "NaN"},
		{math.Inf(1), "inf"},
		{math.Inf(-1), "-inf"},
	} {
		if got := FormatDecimal(c.v); got != c.want {
			t.Errorf("FormatDecimal(%v) = %q, want %q", c.v, got, c.want)
		}
	}
}
