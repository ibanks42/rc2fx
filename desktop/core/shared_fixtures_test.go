package core

import (
	"bytes"
	"math"
	"slices"
	"testing"
	"unicode/utf8"
)

// Shared accept/reject fixtures in fixtures/core (also run by
// crates/rc2fx-core/tests/shared_fixtures.rs). Rust owns the canonical and
// pretty byte encodings and exact error wording. Here Go checks that it reaches
// the same accept or reject verdict, that accepted encodings parse to the same
// JSON as the canonical golden (object key order ignored, numbers compared as
// text), and that pretty encodings parse back to the same JSON as compact
// encodings.

type fixtureCase struct {
	Name      string  `json:"name"`
	Type      string  `json:"type"`
	File      string  `json:"file"`
	Accept    bool    `json:"accept"`
	Canonical *string `json:"canonical"`
	Pretty    *string `json:"pretty"`
	Error     *string `json:"error"`
	Why       string  `json:"why"`
}

type fixtureManifest struct {
	Comment string        `json:"comment"`
	Cases   []fixtureCase `json:"cases"`
}

// jsonValuesEqual reports whether two parsed trees are the same document apart
// from object key order. Numbers compare by their text: the mixer treats 1 and
// 1.0 as different kinds, and its float reader can give two spellings of one
// number different doubles, so the app has to write the digits Rust writes.
func jsonValuesEqual(a, b Value) bool {
	if a.Kind() != b.Kind() {
		return false
	}
	switch a.Kind() {
	case ValueNumber:
		return bytes.Equal(a.Raw(), b.Raw())
	case ValueArray:
		ai, _ := a.AsArray()
		bi, _ := b.AsArray()
		return slices.EqualFunc(ai, bi, jsonValuesEqual)
	case ValueObject:
		if a.Len() != b.Len() {
			return false
		}
		am, _ := a.AsObject()
		for _, member := range am {
			match, ok := b.Get(member.Key)
			if !ok || !jsonValuesEqual(member.Value, match) {
				return false
			}
		}
		return true
	}
	return a.Equal(b) // null, bool, string
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

		got, err := Encode(value)
		if err != nil {
			t.Errorf("%s: encode: %v", c.Name, err)
			continue
		}
		gotTree, err := ParseJSONBytes(got, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
		if err != nil {
			t.Errorf("%s: parse encoded output: %v", c.Name, err)
			continue
		}

		if c.Canonical != nil {
			wantBytes := repoFile(t, "fixtures", "core", *c.Canonical)
			wantTree, err := ParseJSONBytes(wantBytes, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
			if err != nil {
				t.Errorf("%s: parse %s: %v", c.Name, *c.Canonical, err)
			} else if !jsonValuesEqual(gotTree, wantTree) {
				t.Errorf("%s: %s value differs\n got %s\nwant %s", c.Name, *c.Canonical, got, wantBytes)
			}
		}

		pretty, err := EncodePretty(value)
		if err != nil {
			t.Errorf("%s: encode pretty: %v", c.Name, err)
			continue
		}
		prettyTree, err := ParseJSONBytes(pretty, DefaultMaxJSONBytes, DefaultMaxJSONNodes)
		if err != nil {
			t.Errorf("%s: parse pretty output: %v", c.Name, err)
			continue
		}
		if !jsonValuesEqual(prettyTree, gotTree) {
			t.Errorf("%s: pretty output differs in value from compact output\npretty  %s\ncompact %s", c.Name, pretty, got)
		}
	}
}

func TestJSONValuesEqual(t *testing.T) {
	cases := []struct {
		name string
		a, b string
		want bool
	}{
		{"key order ignored", `{"a":1,"b":[true,null,"x"]}`, `{"b":[true,null,"x"],"a":1}`, true},
		{"array order differs", `[1,2]`, `[2,1]`, false},
		{"integer vs float spelling", `1`, `1.0`, false},
		{"positive zero vs negative zero", `0.0`, `-0.0`, false},
		{"exponent vs decimal spelling", `1e2`, `100.0`, false},
		{"scientific vs expanded float spelling", `1.2345678901234567e+20`, `123456789012345670000.0`, false},
		{"null member vs missing member", `{"a":null}`, `{}`, false},
		{"escaped slash matches unescaped slash", `"é\/"`, `"é/"`, true},
		{"identical nested document", `{"a":[1,2.5,{"b":false,"c":null,"d":"ok"}]}`, `{"a":[1,2.5,{"b":false,"c":null,"d":"ok"}]}`, true},
	}
	for _, tc := range cases {
		if got := jsonValuesEqual(parse(t, tc.a), parse(t, tc.b)); got != tc.want {
			t.Errorf("%s: jsonValuesEqual(%s, %s) = %v, want %v", tc.name, tc.a, tc.b, got, tc.want)
		}
	}
}
