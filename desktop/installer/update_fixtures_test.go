package installer

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

func TestMixerTargetFixturesMatchRust(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "fixtures", "update", "dto.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		MixerTargets struct {
			Accept []struct {
				Name string          `json:"name"`
				JSON json.RawMessage `json:"json"`
			} `json:"accept"`
			Reject []struct {
				Name string          `json:"name"`
				JSON json.RawMessage `json:"json"`
			} `json:"reject"`
		} `json:"mixerTargets"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		t.Fatal(err)
	}
	for _, c := range doc.MixerTargets.Accept {
		var target MixerTarget
		if err := sjson.DecodeStrict(c.JSON, &target); err != nil {
			t.Fatalf("accept %s: unmarshal: %v", c.Name, err)
		}
		if err := target.validate(); err != nil {
			t.Fatalf("accept %s: validate: %v", c.Name, err)
		}
	}
	for _, c := range doc.MixerTargets.Reject {
		var target MixerTarget
		err := sjson.DecodeStrict(c.JSON, &target)
		if err == nil {
			err = target.validate()
		}
		if err == nil {
			t.Errorf("reject %s: unexpectedly accepted %+v", c.Name, target)
		}
	}
}

func TestPayloadEnvelopeFixturesMatchRust(t *testing.T) {
	dir := filepath.Join("..", "..", "fixtures", "update")
	valid, err := os.ReadFile(filepath.Join(dir, "envelope.bin"))
	if err != nil {
		t.Fatal(err)
	}
	env, err := embed.Find(valid)
	if err != nil {
		t.Fatalf("embed.Find(valid): %v", err)
	}
	bundle, err := env.Bundle()
	if err != nil {
		t.Fatalf("env.Bundle(): %v", err)
	}
	if len(bundle) != len(PayloadNames) {
		t.Fatalf("bundle has %d files, want %d", len(bundle), len(PayloadNames))
	}
	if !bytes.Contains(bundle["rc2fx.sh"], []byte("rc2fx_build=release\n")) {
		t.Fatalf("valid envelope rc2fx.sh missing release line")
	}

	entries, err := os.ReadDir(filepath.Join(dir, "bad-envelopes"))
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) == 0 {
		t.Fatal("no bad-envelopes fixtures")
	}
	for _, entry := range entries {
		raw, err := os.ReadFile(filepath.Join(dir, "bad-envelopes", entry.Name()))
		if err != nil {
			t.Fatal(err)
		}
		env, err := embed.Find(raw)
		if err == nil {
			var b map[string][]byte
			b, err = env.Bundle()
			if err == nil && entry.Name() == "dev-build.bin" {
				// embed.Find parses structurally; release policy check happens at
				// install/packaging time. Confirm dev-build lacks release policy.
				if bytes.Contains(b["rc2fx.sh"], []byte("rc2fx_build=release\n")) {
					t.Errorf("%s unexpectedly marked release", entry.Name())
				}
				continue
			}
		}
		if err == nil {
			t.Errorf("bad envelope %s was unexpectedly accepted", entry.Name())
		}
	}
}
