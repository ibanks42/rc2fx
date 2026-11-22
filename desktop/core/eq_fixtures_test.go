package core

// Analytical and boundary checks for EQ biquad design in Go. Live Go <-> Rust
// ULP agreement across all BandType x BandMode cases is verified by
// crates/rc2fx-core/tests/eq_fixtures.rs.

import (
	"math"
	"testing"
)

func TestEQBiquadInvariants(t *testing.T) {
	// Off band is identity regardless of parameters.
	off, err := DesignCoefficients(Band{
		ID: "b1", Enabled: true, BandType: BandTypeOff, Mode: BandModeRlcBt,
		Frequency: 1000, Gain: 6, Q: 1.41,
	})
	if err != nil {
		t.Fatal(err)
	}
	if off.B0 != 1 || off.B1 != 0 || off.B2 != 0 || off.A1 != 0 || off.A2 != 0 {
		t.Fatalf("off band = %+v, want identity", off)
	}

	// Zero-dB bell is transparent (b0=1, b1=a1, b2=a2).
	flat, err := DesignCoefficients(Band{
		ID: "b1", Enabled: true, BandType: BandTypeBell, Mode: BandModeApoDr,
		Frequency: 1000, Gain: 0, Q: 1,
	})
	if err != nil {
		t.Fatal(err)
	}
	if math.Abs(flat.B0-1) > 1e-12 || math.Abs(flat.B1-flat.A1) > 1e-12 || math.Abs(flat.B2-flat.A2) > 1e-12 {
		t.Fatalf("0 dB bell not transparent: %+v", flat)
	}

	// Every BandType x BandMode combination produces finite coefficients.
	for _, bt := range BandTypeAll {
		for _, mode := range []BandMode{BandModeApoDr, BandModeRlcBt} {
			b := Band{ID: "b1", Enabled: true, BandType: bt, Mode: mode, Frequency: 1000, Gain: 6, Q: 1.2}
			c, err := DesignCoefficients(b)
			if err != nil {
				t.Fatalf("%s/%s: %v", bt, mode, err)
			}
			for _, v := range []float64{c.B0, c.B1, c.B2, c.A1, c.A2} {
				if math.IsNaN(v) || math.IsInf(v, 0) {
					t.Fatalf("%s/%s non-finite coeff in %+v", bt, mode, c)
				}
			}
		}
	}
}
