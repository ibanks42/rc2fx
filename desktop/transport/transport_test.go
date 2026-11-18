package transport

import (
	"bytes"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
)

func isProtocol(failed *requestError) bool {
	return failed != nil && failed.kind == requestProtocol
}

func TestVersionAndSSHRepliesWithUnfitTextAreProtocolErrors(t *testing.T) {
	bad := "1.0‮"
	if _, failed := versionReply(object(member{"firmware", jsonString(bad)}), nil); !isProtocol(failed) {
		t.Fatalf("want a protocol error, got %v", failed)
	}
	good, failed := versionReply(object(member{"firmware", jsonString("1.7.7")}), nil)
	if failed != nil || good == nil || good.Firmware == nil || *good.Firmware != "1.7.7" {
		t.Fatalf("got %+v, %v", good, failed)
	}

	status := func(host string) json.RawMessage {
		return object(
			member{"managed", jsonBool(false)},
			member{"unmanagedKeys", jsonUint(0)},
			member{"hostKeys", []byte("[" + string(jsonString(host)) + "]")},
		)
	}
	if _, failed := sshReply(status("ssh-ed25519 AAAA\nssh-ed25519 BBBB"), nil); !isProtocol(failed) {
		t.Fatalf("want a protocol error, got %v", failed)
	}
	if reply, failed := sshReply(status("ssh-ed25519 AAAA"), nil); failed != nil || reply == nil {
		t.Fatalf("got %+v, %v", reply, failed)
	}
}

func TestSSHRepliesTreatOldMixerSoftwareAsUnsupported(t *testing.T) {
	if reply, failed := sshReply(nil, statusError(404, nil)); reply != nil || failed != nil {
		t.Fatalf("got %+v, %v", reply, failed)
	}
	status, failed := sshReply(json.RawMessage(`{
		"managed": true,
		"managedFingerprint": "SHA256:abc",
		"unmanagedKeys": 1,
		"hostKeys": [],
		"serverRunning": null,
		"future": 1
	}`), nil)
	if failed != nil || status == nil {
		t.Fatalf("got %+v, %v", status, failed)
	}
	if !status.Managed || status.UnmanagedKeys != 1 || status.ServerRunning != nil {
		t.Fatalf("got %+v", status)
	}
	if _, failed := sshReply(json.RawMessage(`[]`), nil); !isProtocol(failed) {
		t.Fatalf("want a protocol error, got %v", failed)
	}
	busy := "busy"
	if _, failed := sshReply(nil, statusError(409, &busy)); !failed.isStatus(409) {
		t.Fatalf("want the 409, got %v", failed)
	}
}

func TestVersionRepliesTreatOldRuntimesAsUnknown(t *testing.T) {
	if versions, failed := versionReply(nil, statusError(404, nil)); versions != nil || failed != nil {
		t.Fatalf("got %+v, %v", versions, failed)
	}
	known, failed := versionReply(json.RawMessage(`{"runtime":"0.3.0","firmware":"1.7.7","future":1}`), nil)
	if failed != nil || known == nil {
		t.Fatalf("got %+v, %v", known, failed)
	}
	if known.Runtime == nil || *known.Runtime != "0.3.0" {
		t.Fatalf("runtime %v", known.Runtime)
	}
	if known.Firmware == nil || *known.Firmware != "1.7.7" {
		t.Fatalf("firmware %v", known.Firmware)
	}
	if versions, failed := versionReply(json.RawMessage(`{}`), nil); versions == nil || failed != nil {
		t.Fatalf("got %+v, %v", versions, failed)
	}
	if _, failed := versionReply(json.RawMessage(`[]`), nil); !isProtocol(failed) {
		t.Fatalf("want a protocol error, got %v", failed)
	}
	if _, failed := versionReply(nil, statusError(500, nil)); !failed.isStatus(500) {
		t.Fatalf("want the 500, got %v", failed)
	}
}

func TestASettingsSaveRoundTripsTheCompleteUpdate(t *testing.T) {
	update := core.ConfigUpdate{
		ExpectedRevision: 7,
		Targets:          core.DefaultConfig().Targets,
	}
	body := applyBody(update)
	if revision, ok := uintField(body, "expectedRevision"); !ok || revision != 7 {
		t.Fatalf("expectedRevision %d, %v", revision, ok)
	}
	if fields, ok := members(body); !ok || len(fields) != 2 {
		t.Fatalf("fields %v", fields)
	}
	var decoded core.ConfigUpdate
	if err := decode(body, &decoded); err != nil {
		t.Fatal(err)
	}
	// Equal updates encode alike.
	again, err := marshal(decoded)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(again, body) {
		t.Fatalf("the update changed on the way back:\n%s\n%s", body, again)
	}
}

func TestPresetIDsAndListsAreBoundedAndStrict(t *testing.T) {
	if path, failed := presetPath("preset-1", true); failed != nil || path != "api/v1/presets/preset-1/apply" {
		t.Fatalf("got %q, %v", path, failed)
	}
	for _, id := range []string{"../state", "x/apply", "x?token=1", "x%2fy", "", "UPPER"} {
		if _, failed := presetPath(id, false); failed == nil {
			t.Errorf("%s", id)
		}
	}
	list, failed := presetList(json.RawMessage(`{"presets":[{"id":"preset-1","name":"Voice"}]}`))
	if failed != nil || len(list) != 1 {
		t.Fatalf("got %+v, %v", list, failed)
	}
	for _, invalid := range []string{
		`{"presets":null}`,
		`{"presets":[],"extra":true}`,
		`{"presets":[{"id":"p","name":"Voice","extra":true}]}`,
		`{"presets":[{"id":"p","name":"Voice"},{"id":"p","name":"Other"}]}`,
	} {
		if _, failed := presetList(json.RawMessage(invalid)); failed == nil {
			t.Errorf("accepted %s", invalid)
		}
	}
}

func TestDroppingOwnerStopsWorkerWithoutDrainingCommands(t *testing.T) {
	transport, err := New()
	if err != nil {
		t.Fatal(err)
	}
	// A bounded queue full of commands must not keep the owner alive.
	for generation := uint64(1); generation <= 16; generation++ {
		_ = transport.Send(CommandDisconnect{Generation: generation})
	}
	worker := transport.done
	transport.Close()
	select {
	case <-worker:
	case <-time.After(2 * time.Second):
		t.Fatal("device worker did not stop after its owner dropped")
	}
}

func TestUpdateChunksAreStandardBase64(t *testing.T) {
	for _, c := range []struct {
		in   []byte
		want string
	}{
		{[]byte(""), ""},
		{[]byte("f"), "Zg=="},
		{[]byte("fo"), "Zm8="},
		{[]byte("foo"), "Zm9v"},
		{[]byte("foobar"), "Zm9vYmFy"},
		{[]byte{0xff, 0xfe, 0x00}, "//4A"},
	} {
		if got := encodeBase64(c.in); got != c.want {
			t.Errorf("%v: got %q, want %q", c.in, got, c.want)
		}
	}
	// The largest chunk plus its JSON stays within one request.
	chunk := encodeBase64(make([]byte, updateChunk))
	if len(chunk)+64 >= requestLimit {
		t.Fatalf("a chunk takes %d bytes", len(chunk))
	}
}

// The tests below cover the JSON helpers on json.RawMessage.

func TestStringsAreWrittenAsSerdeWritesThem(t *testing.T) {
	if got := string(jsonString("\"\\\b\f\n\r\t\x01\x7f é")); got != "\"\\\"\\\\\\b\\f\\n\\r\\t\\u0001\x7f é\"" {
		t.Fatalf("got %s", got)
	}
}

func TestMembersKeepTheirPlaceWhenSet(t *testing.T) {
	edited, ok := withMembers(json.RawMessage(`{"version":"1", "encoding":"whole","size":3, "metadata":{"a": 1}}`),
		member{"encoding", jsonString("delta-1")},
		member{"uploadSize", jsonUint(2)},
	)
	want := `{"version":"1","encoding":"delta-1","size":3,"metadata":{"a":1},"uploadSize":2}`
	if !ok || string(edited) != want {
		t.Fatalf("got %s, want %s", edited, want)
	}
	if edited, ok := withMembers(json.RawMessage("null"), member{"a", jsonUint(1)}); !ok || string(edited) != `{"a":1}` {
		t.Fatalf("got %s", edited)
	}
	if _, ok := withMembers(json.RawMessage("[]"), member{"a", jsonUint(1)}); ok {
		t.Fatal("an array took a member")
	}
}

func TestNumbersReadAsU64OnlyWhenTheyAreIntegers(t *testing.T) {
	for text, want := range map[string]bool{"0": true, "18446744073709551615": true, "18446744073709551616": false, "1.0": false, "1e2": false, "-1": false, `"1"`: false} {
		if _, ok := asUint(json.RawMessage(text)); ok != want {
			t.Errorf("%s: %v", text, ok)
		}
	}
	if !strings.Contains(string(object(member{"k", jsonUint(1)})), `"k":1`) {
		t.Fatal("object")
	}
}
