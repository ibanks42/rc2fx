package carrier

import (
	"bytes"
	"strings"
	"testing"
)

const goodTable = "/dev/mmcblk1p1 : start=        8192, size=     1193984, type=83|" +
	"/dev/mmcblk1p2 : start=     2000896, size=     1193984, type=83|" +
	"/dev/mmcblk1p3 : start=     3223552, size=     1710080, type=83|" +
	"/dev/mmcblk1p4 : start=     4941824, size=    10328064, type=83"

var sector0Hex = strings.Repeat("00", 510) + "55aa"

func markerOf(lines ...string) []byte {
	return []byte(strings.Join(lines, "\n") + "\n")
}

var fixedMarker = markerOf(
	"rcfix=1",
	"result=fixed",
	"slot=2",
	"hopped=1",
	"p1_sectors=1193984",
	"table="+goodTable,
	"sector0_before="+sector0Hex,
	"script=1",
	"end=1",
)

func TestParseMarkerFixed(t *testing.T) {
	m, err := ParseMarker(fixedMarker)
	if err != nil {
		t.Fatal(err)
	}
	if m.Result != ResultFixed || m.Slot != 2 || !m.Hopped || m.P1Sectors != 1193984 ||
		len(m.Table) != 4 || m.Reason != "" || m.Script != "1" || len(m.Sector0Before) != 512 ||
		m.Sector0Before[510] != 0x55 || m.Sector0Before[511] != 0xaa {
		t.Fatalf("unexpected marker %+v", m)
	}
	if !strings.HasPrefix(m.Table[3], "/dev/mmcblk1p4 ") {
		t.Fatalf("table line 4 is %q", m.Table[3])
	}
}

func TestParseMarker(t *testing.T) {
	base := func(extra ...string) []string {
		return append([]string{"rcfix=1", "result=failed", "slot=1", "hopped=0", "script=1"}, extra...)
	}
	ok := func(lines ...string) []byte { return markerOf(append(lines, "end=1")...) }
	tests := []struct {
		name string
		data []byte
		ok   bool
	}{
		{"minimal failed", ok(base("reason=could not")...), true},
		{"already fixed", markerOf("rcfix=1", "result=already-fixed", "slot=1", "hopped=0", "script=1", "end=1"), true},
		{"unrecognized", ok("rcfix=1", "result=unrecognized", "slot=2", "hopped=0", "table=a|b|c", "reason=x", "script=1"), true},
		{"slot 0", ok("rcfix=1", "result=failed", "slot=0", "hopped=0", "reason=r", "script=1"), true},
		{"crlf", bytes.ReplaceAll(fixedMarker, []byte("\n"), []byte("\r\n")), true},
		{"nul padding", append(append([]byte(nil), fixedMarker...), make([]byte, 4096)...), true},
		{"no final newline", bytes.TrimSuffix(fixedMarker, []byte("\n")), true},
		{"blank padding", append(append([]byte(nil), fixedMarker...), "\n\n  \n"...), true},

		{"empty", nil, false},
		{"only nuls", make([]byte, 512), false},
		{"no first line", markerOf("result=fixed", "slot=2", "hopped=0", "script=1", "end=1"), false},
		{"wrong first line", markerOf("rcfix=2", "result=fixed", "slot=2", "hopped=0", "script=1", "end=1"), false},
		{"no last line", markerOf(base("reason=r")...), false},
		{"end=0", markerOf(append(base("reason=r"), "end=0")...), false},
		{"unknown result", ok("rcfix=1", "result=maybe", "slot=2", "hopped=0", "script=1"), false},
		{"unknown key", ok(base("reason=r", "color=red")...), false},
		{"duplicate key", ok(base("reason=r", "reason=s")...), false},
		{"no equals", ok(base("reason=r", "garbage")...), false},
		{"blank line inside", ok(base("reason=r", "")...), false},
		{"missing slot", ok("rcfix=1", "result=fixed", "hopped=0", "script=1"), false},
		{"missing script", ok("rcfix=1", "result=fixed", "slot=2", "hopped=0"), false},
		{"bad slot", ok("rcfix=1", "result=fixed", "slot=3", "hopped=0", "script=1"), false},
		{"bad hopped", ok("rcfix=1", "result=fixed", "slot=2", "hopped=yes", "script=1"), false},
		{"failed without reason", ok(base()...), false},
		{"bad p1", ok(base("reason=r", "p1_sectors=12x")...), false},
		{"zero p1", ok(base("reason=r", "p1_sectors=0")...), false},
		{"leading zero p1", ok(base("reason=r", "p1_sectors=0123")...), false},
		{"huge p1", ok(base("reason=r", "p1_sectors=99999999999")...), false},
		{"short hex", ok(base("reason=r", "sector0_before="+sector0Hex[2:])...), false},
		{"bad hex", ok(base("reason=r", "sector0_before=zz"+sector0Hex[2:])...), false},
		{"empty table", ok(base("reason=r", "table=")...), false},
		{"empty table line", ok(base("reason=r", "table=a||b")...), false},
		{"bad version", ok("rcfix=1", "result=fixed", "slot=2", "hopped=0", "script=1 2"), false},
		{"non-ascii", ok(base("reason=caf\xc3\xa9")...), false},
		{"nul inside", ok(base("reason=a\x00b")...), false},
		{"too long", ok(base("reason=" + strings.Repeat("x", 20000))...), false},
		{"too long raw", append(append([]byte(nil), fixedMarker...), make([]byte, 2<<20)...), false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := ParseMarker(tt.data)
			if (err == nil) != tt.ok {
				t.Fatalf("ok=%v, err=%v", tt.ok, err)
			}
		})
	}
}

// A marker cut anywhere (the 1 KB-block damage, or a copy interrupted) is
// never taken as complete, whether the tail is gone or zero-filled.
func TestParseMarkerTruncated(t *testing.T) {
	full := fixedMarker
	// The last point that still parses is the full text minus its newline.
	for cut := 0; cut < len(full)-1; cut++ {
		if _, err := ParseMarker(full[:cut]); err == nil {
			t.Fatalf("cut at %d parsed", cut)
		}
		zeroed := append(append([]byte(nil), full[:cut]...), make([]byte, len(full)-cut)...)
		if _, err := ParseMarker(zeroed); err == nil {
			t.Fatalf("zero tail from %d parsed", cut)
		}
	}
}
