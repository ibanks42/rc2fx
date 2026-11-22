package core

// Analytical and boundary checks for GateShapeForStrength in Go. Live Go <-> Rust
// agreement across the full strength curve is verified by
// crates/rc2fx-core/tests/gate_fixtures.rs.

import (
	"math"
	"testing"
)

func TestGateShapeCurveEndpointsAndClamping(t *testing.T) {
	near := func(label string, got, want float64) {
		t.Helper()
		if math.Abs(got-want) > 1e-12 {
			t.Errorf("%s = %.17g, want %.17g", label, got, want)
		}
	}

	s0 := GateShapeForStrength(0)
	near("s0.threshold", s0.Threshold, 0.3)
	near("s0.closeThreshold", s0.CloseThreshold, 0.15)
	near("s0.reductionDb", s0.ReductionDB, 0)
	near("s0.attackMs", s0.AttackMs, 5)
	near("s0.holdMs", s0.HoldMs, 350)
	near("s0.releaseMs", s0.ReleaseMs, 300)

	sHalf := GateShapeForStrength(0.5)
	near("s0.5.threshold", sHalf.Threshold, 0.5)
	near("s0.5.reductionDb", sHalf.ReductionDB, 20)
	near("s0.5.holdMs", sHalf.HoldMs, 250)

	s1 := GateShapeForStrength(1)
	near("s1.threshold", s1.Threshold, 0.7)
	near("s1.closeThreshold", s1.CloseThreshold, 0.55)
	near("s1.reductionDb", s1.ReductionDB, 30)
	near("s1.holdMs", s1.HoldMs, 150)
	near("s1.releaseMs", s1.ReleaseMs, 90)

	if GateShapeForStrength(-0.5) != s0 {
		t.Error("negative strength did not clamp to 0")
	}
	if GateShapeForStrength(1.5) != s1 {
		t.Error("strength > 1 did not clamp to 1")
	}
}
