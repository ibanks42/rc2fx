package core

// Shared EQ biquad fixture tests (fixtures/eq/biquads.json).
// Rust checks the same file in crates/rc2fx-core/tests/eq_fixtures.rs.
//
// Go's math.Pow/Sin/Cos are not glibc's libm, so derived biquad coefficients
// may land a few ULPs from the mixer's; the tolerance below catches any
// formula or sign drift while permitting standard libm rounding differences.

import (
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

func TestEQBiquadsMatchTheSharedFixture(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "fixtures", "eq", "biquads.json"))
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
	parseFloat := func(c map[string]string, field string) float64 {
		bits, err := strconv.ParseUint(c[field], 16, 64)
		if err != nil {
			t.Fatalf("%s: %v", field, err)
		}
		return math.Float64frombits(bits)
	}
	checkULP := func(label string, got, want float64, maxULP uint64) {
		t.Helper()
		if got == want {
			return
		}
		gb, wb := math.Float64bits(got), math.Float64bits(want)
		if (gb>>63) != (wb>>63) || max(gb, wb)-min(gb, wb) > maxULP {
			t.Errorf("%s: got %.17g (%016x), want %.17g (%016x)", label, got, gb, want, wb)
		}
	}
	for i, c := range cases {
		band := Band{
			ID:        "b1",
			Enabled:   true,
			BandType:  BandType(c["bandType"]),
			Mode:      BandMode(c["mode"]),
			Frequency: parseFloat(c, "frequency"),
			Gain:      parseFloat(c, "gain"),
			Q:         parseFloat(c, "q"),
		}
		checkULP(c["bandType"]+"/"+c["mode"]+"/designQ", DesignQ(band), parseFloat(c, "designQ"), 4)
		coeffs, err := DesignCoefficients(band)
		if err != nil {
			t.Fatalf("case %d (%+v): %v", i, band, err)
		}
		for _, f := range []struct {
			name string
			got  float64
		}{
			{"b0", coeffs.B0},
			{"b1", coeffs.B1},
			{"b2", coeffs.B2},
			{"a1", coeffs.A1},
			{"a2", coeffs.A2},
		} {
			checkULP(c["bandType"]+"/"+c["mode"]+"/"+f.name, f.got, parseFloat(c, f.name), 16)
		}
	}
}
