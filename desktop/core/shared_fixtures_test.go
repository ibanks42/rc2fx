package core

import (
	"math"
	"testing"
	"unicode/utf8"
)

// The shared accept and reject fixtures in fixtures/core. The Rust crate
// (crates/rc2fx-core/tests/shared_fixtures.rs) reads the same files: both
// must accept and reject the same inputs, and write accepted values back byte
// for byte as the `canonical` (compact) and `pretty` files hold them.

type fixtureCase struct {
	Name      string  `json:"name"`
	Type      string  `json:"type"`
	File      string  `json:"file"`
	Accept    bool    `json:"accept"`
	Canonical *string `json:"canonical"`
	Pretty    *string `json:"pretty"`
	Why       string  `json:"why"`
}

type fixtureManifest struct {
	Comment string        `json:"comment"`
	Cases   []fixtureCase `json:"cases"`
}

func validated[T interface{ Validate() error }](data []byte) (any, error) {
	value, err := FromJSON[T](string(data))
	if err != nil {
		return nil, err
	}
	return value, value.Validate()
}

func decoded[T any](data []byte) (any, error) {
	return FromJSON[T](string(data))
}

// decodeFixture is how the desktop reads each kind of document.
func decodeFixture(t *testing.T, kind string, data []byte) (any, error) {
	if kind == "mixerRelease" {
		return ParseMixerRelease(data)
	}
	if !utf8.Valid(data) {
		// The Rust test reads text with str::from_utf8 first.
		return nil, CoreError("not UTF-8")
	}
	switch kind {
	case "config":
		return ConfigFromJSON(string(data))
	case "deviceState":
		return DeviceStateFromJSON(string(data))
	case "configUpdate":
		return decoded[ConfigUpdate](data)
	case "presetSummaries":
		return decoded[[]PresetSummary](data)
	case "deviceVersions":
		return validated[DeviceVersions](data)
	case "sshStatus":
		return validated[SSHStatus](data)
	case "meter":
		return decoded[Meter](data)
	case "diagnosticsEvents":
		return decoded[DiagnosticsEvents](data)
	case "stockInventory":
		return validated[StockInventory](data)
	case "json":
		return decoded[Value](data)
	}
	t.Fatalf("unknown fixture type %s", kind)
	return nil, nil
}

func TestSharedFixturesVerdictsAndEncodings(t *testing.T) {
	var manifest fixtureManifest
	ok(t, DecodeStrictLimited(repoFile(t, "fixtures", "core", "cases.json"), math.MaxInt, math.MaxInt, &manifest))
	if len(manifest.Cases) <= 100 {
		t.Fatalf("%d cases", len(manifest.Cases))
	}
	for _, c := range manifest.Cases {
		value, err := decodeFixture(t, c.Type, repoFile(t, "fixtures", "core", c.File))
		switch {
		case err != nil && c.Accept:
			t.Errorf("%s: rejected: %v", c.Name, err)
			continue
		case err == nil && !c.Accept:
			t.Errorf("%s: accepted, expected a rejection (%s)", c.Name, c.Why)
			continue
		case err != nil:
			continue
		}
		for _, want := range []struct {
			file   *string
			pretty bool
		}{{c.Canonical, false}, {c.Pretty, true}} {
			if want.file == nil {
				continue
			}
			got, err := Encode(value)
			if want.pretty {
				got, err = EncodePretty(value)
			}
			if err != nil {
				t.Errorf("%s: %v", c.Name, err)
				continue
			}
			if expected := string(repoFile(t, "fixtures", "core", *want.file)); string(got) != expected {
				t.Errorf("%s: %s differs\n got %s\nwant %s", c.Name, *want.file, got, expected)
			}
		}
	}
}
