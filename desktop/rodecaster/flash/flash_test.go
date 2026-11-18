package flash

import (
	"strings"
	"testing"
)

func TestAPlaceholderSerialIsRecognized(t *testing.T) {
	// serial is a real serial as the tests of an application see it.
	const serial = "fixture-device"
	for _, s := range []string{"RD0000000", "GV0000000"} {
		if !placeholderSerial(s) {
			t.Fatalf("%s", s)
		}
	}
	for _, s := range []string{"GV0044100", serial, "", "RD", "rd0000000", "RD0000001"} {
		if placeholderSerial(s) {
			t.Fatalf("%s", s)
		}
	}
}

func TestUpdateDriveSizeIsACeilingNotAConstant(t *testing.T) {
	const (
		volumeTotal = 283_189_248
		diskBytes   = 560_000 * 512
	)
	must := func(err error) {
		t.Helper()
		if err != nil {
			t.Fatal(err)
		}
	}
	contains := func(err error, want string) {
		t.Helper()
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("error %v does not contain %q", err, want)
		}
	}
	// 270 MB and 224 MB drives, as Windows showed them on two Pro IIs.
	if !UpdateSize(volumeTotal) || !UpdateSize(224<<20) || !UpdateSize(diskBytes) {
		t.Fatal("real drives refused")
	}
	if UpdateSize(diskBytes + 1) {
		t.Fatal("larger than RØDE makes")
	}
	if UpdateSize(0) {
		t.Fatal("the mixer's empty normal-mode drive")
	}
	must(Fits(175<<20, 224<<20))
	must(Fits(224<<20, 224<<20))
	err := Fits((224<<20)+1, 224<<20)
	contains(err, "225 MB")
	contains(err, "224 MB")
}

func TestTheUSBReleaseNumberReadsBackAsRodesVersion(t *testing.T) {
	for release, want := range map[uint16]string{0x0177: "1.7.7", 0x0176: "1.7.6", 0x0200: "2.0.0"} {
		if got, ok := FirmwareVersion(release); !ok || got != want {
			t.Fatalf("%#x: %q", release, got)
		}
	}
	// Not a dotless version: unknown, never guessed.
	for _, release := range []uint16{0, 0x0017, 0x1234, 0x01a7} {
		if got, ok := FirmwareVersion(release); ok {
			t.Fatalf("%#x: %q", release, got)
		}
	}
}
