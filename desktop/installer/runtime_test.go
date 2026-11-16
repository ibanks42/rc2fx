package installer

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
)

func TestPassiveRecorderIsInTheInstalledAndChecksummedPayload(t *testing.T) {
	for _, name := range []string{"librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"} {
		if !embed.IsPayloadName(name) || !embed.IsPinned(name) || !embed.IsBinary(name) {
			t.Fatalf("%s is not an installed, checksummed and checked ELF", name)
		}
	}
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
