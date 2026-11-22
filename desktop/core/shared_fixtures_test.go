package core

import (
	"bytes"
	"slices"
	"testing"
)

// Inline Go-side wire contract and backward-compatibility tests. Live
// Rust <-> Go byte-for-byte JSON parity across all document kinds is verified
// by crates/rc2fx-core/tests/shared_fixtures.rs.

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
	return a.Equal(b)
}

func TestWireBackwardCompatAndStrictRejections(t *testing.T) {
	// Forward-compatible structs accept unknown future fields.
	ver, err := FromJSON[DeviceVersions](`{"runtime":"0.2.3","protocol":1,"inPlaceUpdates":2,"futureField":true}`)
	ok(t, err)
	ok(t, ver.Validate())
	if ver.Runtime == nil || *ver.Runtime != "0.2.3" {
		t.Fatalf("unexpected versions: %+v", ver)
	}

	ssh, err := FromJSON[SSHStatus](`{"managed":false,"unmanagedKeys":0,"hostKeys":[],"future":1}`)
	ok(t, err)
	ok(t, ssh.Validate())
	// Missing required fields must fail.
	_, err = FromJSON[SSHStatus](`{"managed":false}`)
	fails(t, err, "missing required sshStatus fields")

	// Older mixer meter without diagnosticsEpoch or runningForNs, plus future fields.
	meter, err := FromJSON[Meter](`{"type":"meter","revision":1,"generation":2,"synthetic":false,"future":true,"targets":[]}`)
	ok(t, err)
	if meter.DiagnosticsEpoch != 0 || meter.RunningForNs != nil {
		t.Fatalf("unexpected legacy meter defaults: %+v", meter)
	}

	// Unknown diagnostics event kind from newer firmware decodes as Other.
	ev, err := FromJSON[DiagnosticsEvents](`{"nowNs":10,"nextSeq":2,"lost":0,"more":false,"events":[{"seq":1,"kind":"brandNewKind","atNs":5,"message":"hi"}]}`)
	ok(t, err)
	if len(ev.Events) != 1 || ev.Events[0].Kind != DiagnosticsKindOther {
		t.Fatalf("unknown diagnostics kind did not fall back to Other: %+v", ev.Events)
	}

	// Strict structs reject unknown fields and duplicate keys.
	_, err = FromJSON[ConfigUpdate](`{"expectedRevision":1,"targets":[],"extra":true}`)
	fails(t, err, "unknown field in ConfigUpdate")
	_, err = ParseMixerRelease([]byte(`{"schemaVersion":1,"version":"0.2.3","version":"0.2.4","protocolVersion":1}`))
	fails(t, err, "duplicate key in MixerRelease")

	// Compact and pretty encodings parse to equal JSON trees with matching number spellings.
	cfg := DefaultConfig()
	compact, err := Encode(cfg)
	ok(t, err)
	pretty, err := EncodePretty(cfg)
	ok(t, err)
	if !jsonValuesEqual(parse(t, string(compact)), parse(t, string(pretty))) {
		t.Fatal("pretty encoding differs from compact encoding")
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
