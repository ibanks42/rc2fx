package embed

import (
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"testing"
)

// Keep Go payload file lists in sync with fixtures/payload/lists.json
// (also checked by crates/rc2fx-device/src/mixer_update/install.rs).
func TestPayloadListsMatchTheSharedFixture(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "fixtures", "payload", "lists.json"))
	if err != nil {
		t.Fatal(err)
	}
	var lists struct {
		PayloadNames []string `json:"payloadNames"`
		Pinned       []string `json:"pinned"`
		Binaries     []string `json:"binaries"`
		Scripts      []string `json:"scripts"`
	}
	if err := json.Unmarshal(data, &lists); err != nil {
		t.Fatal(err)
	}
	for _, c := range []struct {
		name      string
		got, want []string
	}{
		{"PayloadNames", PayloadNames[:], lists.PayloadNames},
		{"Pinned", Pinned[:], lists.Pinned},
		{"Binaries", Binaries[:], lists.Binaries},
		{"Scripts", Scripts[:], lists.Scripts},
	} {
		if !slices.Equal(c.got, c.want) {
			t.Errorf("%s\n%v\nfixtures/payload/lists.json\n%v", c.name, c.got, c.want)
		}
	}
}
