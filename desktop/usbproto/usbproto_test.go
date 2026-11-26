package usbproto

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"testing"
)

func TestBoundedMessageCodecRejectsTruncationTrailingDataAndExpansionLies(t *testing.T) {
	json := bytes.Repeat([]byte(`{"value":"repeat repeat repeat repeat repeat repeat"}`), 100)
	encoded, err := Pack(json, 8192)
	if err != nil {
		t.Fatal(err)
	}
	if encoded[0] != 'Z' {
		t.Fatalf("encoding %q", encoded[0])
	}
	if len(encoded) >= len(json)/4 {
		t.Fatalf("%d bytes packed to %d", len(json), len(encoded))
	}
	decoded, err := Unpack(encoded, 8192)
	if err != nil || !bytes.Equal(decoded, json) {
		t.Fatalf("round trip: %v", err)
	}
	for length := range len(encoded) {
		if _, err := Unpack(encoded[:length], 8192); err == nil {
			t.Fatalf("truncated to %d bytes accepted", length)
		}
	}
	trailing := append(bytes.Clone(encoded), 0)
	if _, err := Unpack(trailing, 8192); err == nil {
		t.Fatal("trailing byte accepted")
	}
	bomb := bytes.Clone(encoded)
	binary.LittleEndian.PutUint32(bomb[1:5], 1)
	if _, err := Unpack(bomb, 8192); err == nil {
		t.Fatal("understated size accepted")
	}
	binary.LittleEndian.PutUint32(bomb[1:5], 8193)
	if _, err := Unpack(bomb, 8192); err == nil {
		t.Fatal("size over the limit accepted")
	}
	if packed, err := Pack([]byte("{}"), 64); err != nil || string(packed) != "J{}" {
		t.Fatalf("Pack({}) = %q, %v", packed, err)
	}
	if unpacked, err := Unpack([]byte("J{}"), 64); err != nil || string(unpacked) != "{}" {
		t.Fatalf("Unpack(J{}) = %q, %v", unpacked, err)
	}
	if _, err := Pack([]byte("{}"), 1); err == nil {
		t.Fatal("Pack over the limit accepted")
	}
	if _, err := Unpack([]byte("J{}"), 2); err == nil {
		t.Fatal("Unpack over the limit accepted")
	}
	if _, err := Unpack([]byte("X{}"), 64); err == nil {
		t.Fatal("unknown encoding accepted")
	}
}

func TestIsolatedInputReportRejectsVendorIDsAndExcessPayload(t *testing.T) {
	data := bytes.Repeat([]byte{42}, Payload)
	f := Frame{Kind: KindReply, Session: 1, Request: 1, Offset: 0, Total: Payload, Data: data}
	packet := f.Encode(InputID)
	if got, ok := DecodeFrame(packet[:ReportSize], InputID); !ok || !bytes.Equal(got.Data, data) {
		t.Fatal("input report did not decode")
	}
	if got, ok := DecodeFrame(packet[:], InputID); !ok || !bytes.Equal(got.Data, data) {
		t.Fatal("input report did not decode")
	}
	if Marked(packet[:], 2) || Marked(packet[:], 4) {
		t.Fatal("a vendor report ID is marked")
	}
	packet[30] = byte(Payload + 1)
	if _, ok := DecodeFrame(packet[:], InputID); ok {
		t.Fatal("excess payload length accepted")
	}
}

func TestBoundaryRoundtripsAndRejectsReorderedOrOversizedMessages(t *testing.T) {
	for _, size := range []int{1, Payload, Payload + 1, 65537} {
		original := bytes.Repeat([]byte{0xa7}, size)
		var assembly Assembly
		var result []byte
		for index := 0; index*Payload < size; index++ {
			chunk := original[index*Payload : min((index+1)*Payload, size)]
			f := Frame{Kind: KindRequest, Session: 7, Request: 12, Offset: index * Payload, Total: size, Data: chunk}
			report := f.Encode(OutputID)
			decoded, ok := DecodeFrame(report[:], OutputID)
			if !ok {
				t.Fatal("frame did not decode")
			}
			if decoded.Session != 7 {
				t.Fatalf("session %d", decoded.Session)
			}
			var err error
			result, err = assembly.Push(&decoded, MaxRequest)
			if err != nil {
				t.Fatal(err)
			}
		}
		if !bytes.Equal(result, original) {
			t.Fatalf("size %d did not reassemble", size)
		}
	}
	var a Assembly
	f := Frame{Kind: KindRequest, Session: 1, Request: 1, Offset: 1, Total: 2, Data: []byte{1}}
	if _, err := a.Push(&f, 10); err == nil {
		t.Fatal("a fragment without its start accepted")
	}
	f.Offset = 0
	f.Total = 11
	if _, err := a.Push(&f, 10); err == nil {
		t.Fatal("a total over the limit accepted")
	}
	f.Total = 2
	if got, err := a.Push(&f, 10); err != nil || got != nil {
		t.Fatalf("first fragment: %q, %v", got, err)
	}
	if _, err := a.Push(&f, 10); err == nil {
		t.Fatal("a repeated fragment accepted")
	}
}

func TestUnrelatedReportsPassButMalformedMarkedReportsAreOwned(t *testing.T) {
	var b [ReportSize]byte
	b[0] = OutputID
	copy(b[1:8], Magic)
	if !Marked(b[:], OutputID) {
		t.Fatal("not marked")
	}
	if _, ok := DecodeFrame(b[:], OutputID); ok {
		t.Fatal("a marked report without a version decoded")
	}
	b[1] = 'M'
	if Marked(b[:], OutputID) {
		t.Fatal("marked without the magic")
	}
}

func mustHex(t *testing.T, text string) []byte {
	t.Helper()
	b, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestRequestAndReplyJSON(t *testing.T) {
	request := Request{Method: "PUT", Path: "api/v1/x</> \x01", Body: json.RawMessage(`{ "a" : [1, 2] }`)}
	// Call MarshalJSON directly since json.Marshal HTML-escapes the output.
	got, err := request.MarshalJSON()
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"method":"PUT","path":"api/v1/x</>` + " " + `\u0001","body":{"a":[1,2]}}`; string(got) != want {
		t.Fatalf("got %s want %s", got, want)
	}
	got, _ = Request{Method: "GET", Path: "p"}.MarshalJSON()
	if string(got) != `{"method":"GET","path":"p"}` {
		t.Fatalf("got %s", got)
	}
	var back Request
	if err := json.Unmarshal([]byte(`{"method":"GET","path":"p"}`), &back); err != nil || back.Body != nil {
		t.Fatalf("missing body: %v %q", err, back.Body)
	}
	if err := json.Unmarshal([]byte(`{"method":"GET","path":"p","body":null}`), &back); err != nil || back.Body != nil {
		t.Fatalf("null body: %v %q", err, back.Body)
	}
	for _, bad := range []string{
		`{"method":"GET"}`,
		`{"method":"GET","path":"p","extra":1}`,
		`{"method":"GET","method":"PUT","path":"p"}`,
		`{"method":null,"path":"p"}`,
		`{"method":1,"path":"p"}`,
		`{"method":"\ud800","path":"p"}`,
		`null`,
		`[]`,
	} {
		if err := json.Unmarshal([]byte(bad), &back); err == nil {
			t.Errorf("request %s accepted", bad)
		}
	}
	var reply Reply
	if err := json.Unmarshal([]byte(`{"status":200,"body":null}`), &reply); err != nil || reply.Status != 200 || string(reply.Body) != "null" {
		t.Fatalf("reply: %v %+v", err, reply)
	}
	got, _ = reply.MarshalJSON()
	if string(got) != `{"status":200,"body":null}` {
		t.Fatalf("got %s", got)
	}
	for _, bad := range []string{
		`{"status":200}`,
		`{"body":{}}`,
		`{"status":65536,"body":{}}`,
		`{"status":-1,"body":{}}`,
		`{"status":200.0,"body":{}}`,
		`{"status":null,"body":{}}`,
		`{"status":200,"body":{},"x":1}`,
		`{"status":200,"status":201,"body":{}}`,
		`{"status":200,"body":"\udc00"}`,
	} {
		if err := json.Unmarshal([]byte(bad), &reply); err == nil {
			t.Errorf("reply %s accepted", bad)
		}
	}
}
