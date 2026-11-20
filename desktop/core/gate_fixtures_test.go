package core

// The shared gate shapes in fixtures/gate. The Rust crate
// (crates/rc2fx-core/tests/gate_fixtures.rs) checks GateShape::for_strength
// against the same file bit for bit. fixtures/gate/generate.py writes it.
//
// Go's math.Pow is not glibc's pow: the two fields that take a power
// (reduction and release) may land up to two ulps from the mixer's. Every other
// field must match exactly.

import (
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

func TestGateShapesMatchTheSharedFixture(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "fixtures", "gate", "shapes.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cases []map[string]string
	if err := json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	if len(cases) == 0 {
		t.Fatal("no cases")
	}
	float := func(c map[string]string, field string) float64 {
		bits, err := strconv.ParseUint(c[field], 16, 64)
		if err != nil {
			t.Fatalf("%s: %v", field, err)
		}
		return math.Float64frombits(bits)
	}
	for _, c := range cases {
		strength := float(c, "strength")
		shape := GateShapeForStrength(strength)
		for _, f := range []struct {
			field string
			got   float64
			ulps  uint64
		}{
			{"threshold", shape.Threshold, 0},
			{"closeThreshold", shape.CloseThreshold, 0},
			{"reductionDb", shape.ReductionDB, 2},
			{"attackMs", shape.AttackMs, 0},
			{"holdMs", shape.HoldMs, 0},
			{"releaseMs", shape.ReleaseMs, 2},
		} {
			want := float(c, f.field)
			got, exp := math.Float64bits(f.got), math.Float64bits(want)
			if max(got, exp)-min(got, exp) > f.ulps {
				t.Errorf("%s at strength %v: %v != %v", f.field, strength, f.got, want)
			}
		}
	}
}
