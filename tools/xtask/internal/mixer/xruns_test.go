package mixer

import (
	"encoding/binary"
	"os/exec"
	"strings"
	"testing"
)

func snapshotHeader(s Snapshot, stable uint64) []byte {
	out := []byte(snapshotMagic)
	out = binary.LittleEndian.AppendUint32(out, 1)
	out = binary.LittleEndian.AppendUint32(out, recordSize)
	for _, v := range []uint64{
		s.PID, s.CreatedNs, 0, s.Dropped, 0, 0, s.SnapshotNs, s.FirstIndex, s.EndIndex,
		stable, s.Unpublished, s.Overwritten, s.Torn, 0,
	} {
		out = binary.LittleEndian.AppendUint64(out, v)
	}
	return out
}

func encodeRecord(kind uint16, flags, index, t uint64, values [2]int64, strs [2]string) []byte {
	kindFlags := uint64(kind) | flags<<16 | uint64(len(strs[0]))<<32 | uint64(len(strs[1]))<<40
	var out []byte
	for _, v := range []uint64{index, t, kindFlags, uint64(values[0]), uint64(values[1])} {
		out = binary.LittleEndian.AppendUint64(out, v)
	}
	s1 := make([]byte, 40)
	copy(s1, strs[0])
	s2 := make([]byte, 16)
	copy(s2, strs[1])
	return append(append(out, s1...), s2...)
}

func snapshotBytes(first, end, snapshotNs uint64, records ...[]byte) []byte {
	meta := Snapshot{PID: 16643, CreatedNs: 5, SnapshotNs: snapshotNs, FirstIndex: first, EndIndex: end}
	out := snapshotHeader(meta, uint64(len(records)))
	for _, r := range records {
		out = append(out, r...)
	}
	return out
}

func mustParse(t *testing.T, data []byte) *Snapshot {
	t.Helper()
	s, err := ParseSnapshot(data)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func TestASnapshotDecodesAsTheReferenceDecoderDoes(t *testing.T) {
	parsed := mustParse(t, snapshotBytes(0, 2, 9_000_000_000,
		encodeRecord(2, 0, 0, 1_000_000_000, [2]int64{}, [2]string{"rc2fxd", ""}),
		encodeRecord(4, 0, 1, 2_000_000_000, [2]int64{1234, 0}, [2]string{"", ""}),
	))
	if parsed.PID != 16643 || len(parsed.Records) != 2 {
		t.Fatalf("%+v", parsed)
	}
	if parsed.Records[0].Kind != KindClientFinishedLate || *parsed.Records[0].String1 != "rc2fxd" {
		t.Fatalf("%+v", parsed.Records[0])
	}
	if parsed.Records[1].Kind != KindAlsaXrun || parsed.Records[1].Value1 != 1234 {
		t.Fatalf("%+v", parsed.Records[1])
	}
	if d := parsed.Records[1].describe(); d != "alsaXrun  usecs=1234" {
		t.Fatal(d)
	}
}

func TestFlagsMarkNullTruncatedStringsAndAFailedClock(t *testing.T) {
	r := mustParse(t, snapshotBytes(0, 1, 0,
		encodeRecord(1, string2Null|string1Truncated|clockFailed, 0, 7, [2]int64{}, [2]string{"jackd-cl", ""}),
	)).Records[0]
	if r.TNs != nil || r.String1 == nil || *r.String1 != "jackd-cl…" || r.String2 != nil {
		t.Fatalf("%+v", r)
	}
	if d := r.describe(); d != "clientNotFinished  client=jackd-cl…  state=(null)" {
		t.Fatal(d)
	}
}

func TestDamagedSnapshotsAreRefused(t *testing.T) {
	good := snapshotBytes(0, 1, 1, encodeRecord(4, 0, 0, 1, [2]int64{1, 0}, [2]string{"", ""}))
	if _, err := ParseSnapshot(good[:len(good)-1]); err == nil {
		t.Fatal("truncated")
	}
	magic := append([]byte{}, good...)
	magic[0] = 'X'
	if _, err := ParseSnapshot(magic); err == nil {
		t.Fatal("magic")
	}
	// One stable record claimed for a window of two.
	accounting := append([]byte{}, good...)
	binary.LittleEndian.PutUint64(accounting[16+8*8:], 2)
	if _, err := ParseSnapshot(accounting); err == nil {
		t.Fatal("accounting")
	}
}

func TestTheReportShowsOnlyWhatArrivedWhileWatching(t *testing.T) {
	before := mustParse(t, snapshotBytes(0, 1, 10_000_000_000,
		encodeRecord(4, 0, 0, 1_000_000_000, [2]int64{99, 0}, [2]string{"", ""})))
	after := mustParse(t, snapshotBytes(0, 3, 70_000_000_000,
		encodeRecord(4, 0, 0, 1_000_000_000, [2]int64{99, 0}, [2]string{"", ""}),
		encodeRecord(2, 0, 1, 12_500_000_000, [2]int64{}, [2]string{"rc2fxd", ""}),
		encodeRecord(3, 0, 2, 13_000_000_000, [2]int64{250, 0}, [2]string{"", ""}),
	))
	text := Report(before, after, 60)
	for _, want := range []string{
		"+2.500 s  clientFinishedLate  client=rc2fxd",
		"+3.000 s  timedDriverXrun  usecs=250",
		"Late clients: rc2fxd 1",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("%q missing: %s", want, text)
		}
	}
	if !strings.HasPrefix(text, "Watched 60 s: 2 event(s): 1 driver xrun(s), 1 late client(s)\n") {
		t.Error(text)
	}
	if strings.Contains(text, "usecs=99") {
		t.Errorf("an earlier record is not in the window: %s", text)
	}
	if strings.Contains(text, "Incomplete") {
		t.Error(text)
	}
}

func TestTheReportSaysWhenTheRingLostPartOfTheWindow(t *testing.T) {
	before := mustParse(t, snapshotBytes(0, 2, 1,
		encodeRecord(4, 0, 0, 1, [2]int64{1, 0}, [2]string{"", ""}),
		encodeRecord(4, 0, 1, 1, [2]int64{1, 0}, [2]string{"", ""}),
	))
	// The window began at index 2; the ring now starts at 5.
	lapped := snapshotHeader(Snapshot{PID: 16643, CreatedNs: 5, SnapshotNs: 2, FirstIndex: 5, EndIndex: 6}, 1)
	lapped = append(lapped, encodeRecord(4, 0, 5, 2, [2]int64{1, 0}, [2]string{"", ""})...)
	after := mustParse(t, lapped)
	if text := Report(before, after, 5); !strings.Contains(text, "Incomplete: 3 overwritten before the snapshot") {
		t.Fatal(text)
	}
}

func TestZeroSecondsListsTheWholeRing(t *testing.T) {
	after := mustParse(t, snapshotBytes(0, 1, 5,
		encodeRecord(6, 0, 0, 3_000_000_000, [2]int64{12, 0}, [2]string{"", ""})))
	text := Report(nil, after, 0)
	if !strings.HasPrefix(text, "Recorded since this jackd started: 1 event(s): 1 driver xrun(s)") ||
		!strings.Contains(text, "3.000 s  internalXrun  frameOffset=12") {
		t.Fatal(text)
	}
}

func TestTheRemoteScriptIsValidShell(t *testing.T) {
	for _, seconds := range []uint32{0, 60} {
		if err := exec.Command("bash", "-n", "-c", remoteScript(seconds)).Run(); err != nil {
			t.Fatalf("%d: %v", seconds, err)
		}
	}
}
