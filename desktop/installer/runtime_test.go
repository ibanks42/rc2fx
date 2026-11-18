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
