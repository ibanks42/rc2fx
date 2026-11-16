package installflow

// signature.rs tests, case for case.

import (
	"math"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer"
)

func TestProgressIsPerOperationAndUnknownIsNotZero(t *testing.T) {
	want := func(got *float32, want *float32, why string) {
		t.Helper()
		if (got == nil) != (want == nil) || (got != nil && *got != *want) {
			t.Fatalf("%s: fraction %v, want %v", why, deref(got), deref(want))
		}
	}
	some := func(v float32) *float32 { return &v }
	want(Fraction(nil), nil, "no progress")
	var p installer.Progress
	want(Fraction(&p), nil, "unknown total")
	p.Total = 100
	want(Fraction(&p), some(0), "nothing yet")
	p.Completed = 37
	want(Fraction(&p), some(0.37), "37 of 100")
	p.Completed = 200
	want(Fraction(&p), some(1), "more than the total")
	p.Total = 0
	want(Fraction(&p), nil, "total unknown again")
	p.Completed = math.MaxUint64
	p.Total = math.MaxUint64
	want(Fraction(&p), some(1), "the largest sizes")
}

func deref(v *float32) any {
	if v == nil {
		return nil
	}
	return *v
}

func TestNoisySignatureHasFiniteTaperedEndpoints(t *testing.T) {
	for _, phase := range []float32{0, 0.016_667, 3.2, 1234.5} {
		for _, nv := range [][2]float32{{0.6, 0}, {1, 1}, {0, 1}} {
			for i := 0; i <= 600; i++ {
				y := Offset(float32(i)/600, phase, i, nv[0], nv[1])
				if math.IsNaN(float64(y)) || math.IsInf(float64(y), 0) {
					t.Fatalf("phase %v noise %v voice %v sample %d: %v is not finite", phase, nv[0], nv[1], i, y)
				}
				if (i == 0 || i == 600) && !(float32(math.Abs(float64(y))) < 1e-5) {
					t.Fatalf("phase %v noise %v voice %v sample %d: endpoint %v is not tapered", phase, nv[0], nv[1], i, y)
				}
			}
		}
	}
}
