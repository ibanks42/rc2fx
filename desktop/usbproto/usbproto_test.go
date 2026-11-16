package usbproto

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"strings"
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

func TestOnlyAProtocolFourAcknowledgementCarriesAWindow(t *testing.T) {
	for data, want := range map[string]int{"RC2FX USB 4 1": 1, "RC2FX USB 4 32": 32, "RC2FX USB 4 64": 64} {
		if got, ok := Hello4Window([]byte(data)); !ok || got != want {
			t.Errorf("Hello4Window(%q) = %d, %v", data, got, ok)
		}
	}
	for _, bad := range []string{
		"RC2FX USB 4 0",
		"RC2FX USB 4 65",
		"RC2FX USB 4 ",
		"RC2FX USB 4  8",
		"RC2FX USB 4 8x",
		"RC2FX USB 4 -1",
		"RC2FX USB 4",
		"RC2FX USB 3",
		"RC2FX USB 4 999999999999999999999999",
		"",
	} {
		if got, ok := Hello4Window([]byte(bad)); ok {
			t.Errorf("Hello4Window(%q) = %d", bad, got)
		}
	}
}

func TestIsolatedInputReportRejectsVendorIDsAndExcessPayload(t *testing.T) {
	data := bytes.Repeat([]byte{42}, Payload)
	f := Frame{Kind: KindReply, Session: 1, Request: 1, Offset: 0, Total: Payload, Data: data}
	packet := f.Encode(InputID)
	if got, ok := DecodeFrame(packet[:InputReportSize], InputID); !ok || !bytes.Equal(got.Data, data) {
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

// RODE's 1.7.6/1.7.7 descriptor and the copy `rc2fx.sh` stages from it.
const (
	stock  = "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff8102c0"
	staged = "0600ff0901a1017508150025ff09028501953f910209038502953f81020904850395ff91020905850495ff81020906850595ff91020907850695ff8102c0"
)

func mustHex(t *testing.T, text string) []byte {
	t.Helper()
	b, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestStagedLayoutIsOneCollectionWithTheControlReports(t *testing.T) {
	stockBytes, stagedBytes := mustHex(t, stock), mustHex(t, staged)
	if !bytes.Equal(stagedBytes[:len(stockBytes)-1], stockBytes[:len(stockBytes)-1]) {
		t.Fatal("staged descriptor does not extend the stock one")
	}
	// One application collection, closed once, at the very end.
	if bytes.Count(stagedBytes, []byte{0xa1}) != 1 || stagedBytes[len(stagedBytes)-1] != 0xc0 {
		t.Fatal("staged descriptor is not one collection")
	}
	if !DeclaresControl(stagedBytes) || DeclaresControl(stockBytes) {
		t.Fatal("control detection")
	}
	// Each half alone, or the IDs swapped, is not RC2FX's layout.
	if DeclaresControl(mustHex(t, strings.ReplaceAll(staged, "850595ff9102", ""))) ||
		DeclaresControl(mustHex(t, strings.ReplaceAll(staged, "850695ff8102", ""))) ||
		DeclaresControl(stagedBytes[:len(stagedBytes)-3]) {
		t.Fatal("a partial layout declares control")
	}
}

func TestAcceptsExactlyOneTransportByteBeyondTheDescriptorReport(t *testing.T) {
	b := make([]byte, ReportSize+1)
	if got, ok := CanonicalOutput(b); !ok || len(got) != ReportSize {
		t.Fatal("257 bytes refused")
	}
	b[ReportSize] = 1
	if got, ok := CanonicalOutput(b); !ok || len(got) != ReportSize {
		t.Fatal("257 bytes ending in 1 refused")
	}
	b = append(b, 0)
	if _, ok := CanonicalOutput(b); ok {
		t.Fatal("258 bytes accepted")
	}
	if _, ok := CanonicalOutput(b[:ReportSize-1]); ok {
		t.Fatal("255 bytes accepted")
	}
}

// The Rust structs are (de)serialized by serde; these pin the same shape.
func TestRequestAndReplyJSONMatchSerde(t *testing.T) {
	request := Request{Method: "PUT", Path: "api/v1/x</> \x01", Body: json.RawMessage(`{ "a" : [1, 2] }`)}
	// json.Marshal would HTML-escape the result; the bytes the mixer reads
	// come from MarshalJSON itself.
	got, err := request.MarshalJSON()
	if err != nil {
		t.Fatal(err)
	}
	if want := `{"method":"PUT","path":"api/v1/x</>` + " " + `\u0001","body":{"a":[1,2]}}`; string(got) != want {
		t.Fatalf("got %s want %s", got, want)
	}
	got, _ = Request{Method: "GET", Path: "p"}.MarshalJSON()
	if string(got) != `{"method":"GET","path":"p","body":null}` {
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
