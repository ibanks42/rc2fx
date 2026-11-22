package usbproto

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"math"
	"strings"
	"testing"
)

// Inline wire contract tests for USB HID framing, descriptors, hello4,
// packed messages, and request/reply JSON codecs. Live Go <-> Rust interop
// is verified by crates/rc2fx-usb/tests/shared_fixtures.rs.

func TestHello4WindowContract(t *testing.T) {
	for _, c := range []struct {
		data []byte
		want int
		ok   bool
	}{
		{[]byte("RC2FX USB 4 1"), 1, true},
		{[]byte("RC2FX USB 4 32"), 32, true},
		{[]byte("RC2FX USB 4 64"), 64, true},
		{[]byte("RC2FX USB 4 007"), 7, true},
		{[]byte("RC2FX USB 4 0"), 0, false},
		{[]byte("RC2FX USB 4 65"), 0, false},
		{[]byte("RC2FX USB 4 "), 0, false},
		{[]byte("RC2FX USB 4  8"), 0, false},
		{[]byte("RC2FX USB 4 8x"), 0, false},
		{[]byte("RC2FX USB 4 -1"), 0, false},
		{[]byte("RC2FX USB 4 +8"), 0, false},
		{[]byte("RC2FX USB 4"), 0, false},
		{[]byte("RC2FX USB 3"), 0, false},
		{[]byte("RC2FX USB 4 999999999999999999999999"), 0, false},
		{[]byte("RC2FX USB 4 8 "), 0, false},
		{[]byte("rc2fx usb 4 8"), 0, false},
		{nil, 0, false},
		{[]byte("RC2FX USB 4 \xff"), 0, false},
	} {
		got, ok := Hello4Window(c.data)
		if ok != c.ok || (ok && got != c.want) {
			t.Errorf("Hello4Window(%q) = %d, %v; want %d, %v", c.data, got, ok, c.want, c.ok)
		}
	}
}

func TestDescriptorControlDetection(t *testing.T) {
	for _, c := range []struct {
		name    string
		hex     string
		control bool
	}{
		{"stock", "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff8102c0", false},
		{"staged", "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff81020906850595ff91020907850695ff8102c0", true},
		{"staged-without-output", "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff810209060907850695ff8102c0", false},
		{"staged-without-input", "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff81020906850595ff91020907c0", false},
		{"staged-truncated", "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff81020906850595ff91020907850695ff", false},
		{"ids-swapped", "0600ff0901a101850695ff9102850595ff8102c0", false},
		{"push-pop-restores-report", "0600ff0901a101850585ffa4850695ff8102b49102c0", false},
		{"pop-without-push", "0600ff0901a101b4850595ff9102850695ff8102c0", false},
		{"long-item-skipped", "fe0200aabb850595ff9102850695ff8102", true},
		{"long-item-truncated", "850595ff9102850695ff8102fe", false},
		{"four-byte-report-id", "870500000095ff9102870600000095ff8102", true},
		{"report-id-high-bytes", "86050195ff9102850695ff8102", false},
		{"empty", "", false},
	} {
		if got := DeclaresControl(mustHex(t, c.hex)); got != c.control {
			t.Errorf("descriptor %s: DeclaresControl = %v, want %v", c.name, got, c.control)
		}
	}
}

func TestFrameAndMessageContract(t *testing.T) {
	// Valid frames round-trip across all kinds and boundary values.
	for i, kind := range []Kind{KindHello, KindRequest, KindFetch, KindAck, KindReply, KindError} {
		f := Frame{Kind: kind, Session: uint64(i + 1), Request: uint32((i + 1) * 3), Data: []byte(Hello4)}
		raw := f.Encode(OutputID)
		if !Marked(raw[:], OutputID) {
			t.Fatalf("kind %d not marked", kind)
		}
		got, ok := DecodeFrame(raw[:], OutputID)
		if !ok || got.Kind != kind || !bytes.Equal(got.Data, []byte(Hello4)) {
			t.Fatalf("kind %d round-trip failed: %+v (%v)", kind, got, ok)
		}
	}
	maxFrame := Frame{Kind: KindReply, Session: math.MaxUint64, Request: math.MaxUint32, Offset: math.MaxUint32, Total: math.MaxUint32}
	maxRaw := maxFrame.Encode(InputID)
	if got, ok := DecodeFrame(maxRaw[:], InputID); !ok || got.Session != math.MaxUint64 || got.Total != math.MaxUint32 {
		t.Fatalf("max bounds failed: %+v (%v)", got, ok)
	}

	// Malformed frame mutations.
	baseFrame := Frame{Kind: KindReply, Session: 1, Request: 1, Total: 4, Data: []byte("data")}
	base := baseFrame.Encode(InputID)
	for _, c := range []struct {
		name   string
		mutate func([]byte) []byte
		marked bool
	}{
		{"bad-magic", func(b []byte) []byte { b[1] = 0x4d; return b }, false},
		{"bad-magic-nul", func(b []byte) []byte { b[7] = 1; return b }, false},
		{"version-2", func(b []byte) []byte { b[8] = 2; return b }, true},
		{"version-4", func(b []byte) []byte { b[8] = 4; return b }, true},
		{"kind-0", func(b []byte) []byte { b[9] = 0; return b }, true},
		{"kind-7", func(b []byte) []byte { b[9] = 7; return b }, true},
		{"length-over-payload", func(b []byte) []byte { b[30], b[31] = 0xe1, 0; return b }, true},
		{"short-255", func(b []byte) []byte { return b[:255] }, true},
		{"long-257", func(b []byte) []byte { return append(b, 1) }, true},
		{"header-only", func(b []byte) []byte { return b[:8] }, true},
		{"short-7", func(b []byte) []byte { return b[:7] }, false},
		{"empty", func(b []byte) []byte { return nil }, false},
	} {
		buf := c.mutate(bytes.Clone(base[:]))
		if got := Marked(buf, InputID); got != c.marked {
			t.Errorf("%s: Marked = %v, want %v", c.name, got, c.marked)
		}
		if _, ok := DecodeFrame(buf, InputID); ok {
			t.Errorf("%s: accepted invalid frame", c.name)
		}
	}

	// Pack/Unpack round-trip + malformed packed messages.
	compressed, err := Pack([]byte(strings.Repeat(`{"target":"mic-1","gain":0.0},`, 120)), 8192)
	if err != nil {
		t.Fatal(err)
	}
	understated := bytes.Clone(compressed)
	binary.LittleEndian.PutUint32(understated[1:5], 10)
	overstated := bytes.Clone(compressed)
	binary.LittleEndian.PutUint32(overstated[1:5], 7000)
	overLimit := bytes.Clone(compressed)
	binary.LittleEndian.PutUint32(overLimit[1:5], 9000)
	badChecksum := bytes.Clone(compressed)
	badChecksum[len(badChecksum)-1] ^= 0xff
	badHeader := bytes.Clone(compressed)
	badHeader[5], badHeader[6] = 0, 0

	for _, c := range []struct {
		name  string
		data  []byte
		limit int
		want  string
	}{
		{"truncated", compressed[:len(compressed)-4], 8192, "Invalid compressed USB message"},
		{"truncated-header", compressed[:6], 8192, "Invalid compressed USB message"},
		{"size-field-only", compressed[:5], 8192, "Invalid compressed USB message"},
		{"trailing-byte", append(bytes.Clone(compressed), 0), 8192, "Invalid compressed USB message"},
		{"size-understated", understated, 8192, "Invalid compressed USB message"},
		{"size-overstated", overstated, 8192, "Invalid compressed USB message"},
		{"size-over-limit", overLimit, 8192, "Decoded USB message too large"},
		{"message-over-limit", compressed, len(compressed) - 1, "USB message too large"},
		{"plain-over-limit", []byte("Jabc"), 2, "USB message too large"},
		{"unknown-encoding", []byte{9, 1, 2}, 64, "Invalid USB message encoding"},
		{"z-short", []byte("Z\x00\x00"), 8192, "Invalid USB message encoding"},
		{"empty", nil, 64, "Invalid USB message encoding"},
		{"bad-checksum", badChecksum, 8192, "*"},
		{"bad-header", badHeader, 8192, "*"},
	} {
		_, err := Unpack(c.data, c.limit)
		if err == nil {
			t.Errorf("message %s: accepted", c.name)
		} else if c.want != "*" && err.Error() != c.want {
			t.Errorf("message %s: error %q, want %q", c.name, err, c.want)
		}
	}
}

func TestRequestAndReplyJSONContract(t *testing.T) {
	for _, c := range []struct {
		method, path string
		body         json.RawMessage
		want         string
	}{
		{"GET", "api/v1/state", nil, `{"method":"GET","path":"api/v1/state","body":null}`},
		{"PUT", "api/v1/config", json.RawMessage(`{"a":[1,2.5,-3,true,false,null],"b":{"c":"d"}}`), `{"method":"PUT","path":"api/v1/config","body":{"a":[1,2.5,-3,true,false,null],"b":{"c":"d"}}}`},
		{"POST", "a\"b\\c\x01\x1f\b\f\n\r\t é \u2028 <>&", json.RawMessage(`"x\u0000y"`), `{"method":"POST","path":"a\"b\\c\u0001\u001f\b\f\n\r\t é ` + "\u2028" + ` <>&","body":"x\u0000y"}`},
		{"PATCH", "", json.RawMessage(`[]`), `{"method":"PATCH","path":"","body":[]}`},
	} {
		got, err := (Request{Method: c.method, Path: c.path, Body: c.body}).MarshalJSON()
		if err != nil || string(got) != c.want {
			t.Errorf("request %q: got %s (%v), want %s", c.path, got, err, c.want)
		}
	}

	for _, c := range []struct {
		raw    string
		ok     bool
		status uint16
	}{
		{`{"status":200,"body":{"ok":true}}`, true, 200},
		{`{"body":null,"status":0}`, true, 0},
		{`{"status":65535,"body":[]}`, true, 65535},
		{` { "status" : 404 , "body" : "missing" } `, true, 404},
		{`{"status":200}`, false, 0},
		{`{"body":{}}`, false, 0},
		{`{"status":65536,"body":{}}`, false, 0},
		{`{"status":-1,"body":{}}`, false, 0},
		{`{"status":200.0,"body":{}}`, false, 0},
		{`{"status":2e2,"body":{}}`, false, 0},
		{`{"status":null,"body":{}}`, false, 0},
		{`{"status":"200","body":{}}`, false, 0},
		{`{"status":200,"body":{},"extra":1}`, false, 0},
		{`{"status":200,"status":201,"body":{}}`, false, 0},
		{`{"status":200,"body":{},"body":{}}`, false, 0},
		{`{"status":200,"body":"\udc00"}`, false, 0},
		{`{"status":200,"body":{}} x`, false, 0},
		{`null`, false, 0},
		{`[200,{}]`, false, 0},
	} {
		var reply Reply
		err := json.Unmarshal([]byte(c.raw), &reply)
		if (err == nil) != c.ok {
			t.Errorf("reply %s: err=%v, want ok=%v", c.raw, err, c.ok)
		} else if c.ok && reply.Status != c.status {
			t.Errorf("reply %s: status %d, want %d", c.raw, reply.Status, c.status)
		}
	}

	for _, c := range []struct {
		raw string
		ok  bool
	}{
		{`{"method":"GET","path":"p"}`, true},
		{`{"method":"GET","path":"p","body":null}`, true},
		{`{"path":"p","method":"GET","body":{"x":1}}`, true},
		{`{"method":"GET"}`, false},
		{`{"path":"p"}`, false},
		{`{"method":"GET","path":"p","extra":1}`, false},
		{`{"method":"GET","method":"PUT","path":"p"}`, false},
		{`{"method":"GET","path":"p","body":1,"body":2}`, false},
		{`{"method":null,"path":"p"}`, false},
		{`{"method":1,"path":"p"}`, false},
		{`{"method":"\ud800","path":"p"}`, false},
		{`{"method":"\ud83d\ude00","path":"p"}`, true},
		{"{\"method\":\"😀\",\"path\":\"p\"}", true},
		{`null`, false},
	} {
		var req Request
		if err := json.Unmarshal([]byte(c.raw), &req); (err == nil) != c.ok {
			t.Errorf("request %s: err=%v, want ok=%v", c.raw, err, c.ok)
		}
	}
}
