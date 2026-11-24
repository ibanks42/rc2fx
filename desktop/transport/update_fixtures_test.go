package transport

import (
	"encoding/hex"
	"encoding/json"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/common"
)

func TestUpdateWireDTOContract(t *testing.T) {
	wholeStart := json.RawMessage(`{"version":"0.2.1","size":9403,"sha256":"8fb8f5ce993567895775f544a14450a6f9be0660f6ca146f04e6b75428a77a12","metadata":{"root":["{}"],"timestamp":"{}","snapshot":"{}","targets":"{}"}}`)
	edited, ok := withMembers(wholeStart,
		member{"encoding", jsonString("delta-1")},
		member{"uploadSize", jsonUint(1024)},
	)
	if !ok {
		t.Fatal("withMembers failed")
	}
	if enc, ok := stringField(edited, "encoding"); !ok || enc != "delta-1" {
		t.Fatalf("encoding = %q (%v)", enc, ok)
	}
	if sz, ok := uintField(edited, "uploadSize"); !ok || sz != 1024 {
		t.Fatalf("uploadSize = %d (%v)", sz, ok)
	}

	for _, c := range []struct {
		name   string
		rawHex string
		offset uint64
		want   string
	}{
		{"first-chunk", "72633266782d7570646174652d6368756e6b", 0, "cmMyZngtdXBkYXRlLWNodW5r"},
		{"offset-chunk", "0001feff", 196608, "AAH+/w=="},
	} {
		raw, err := hex.DecodeString(c.rawHex)
		if err != nil {
			t.Fatal(err)
		}
		body := object(
			member{"offset", jsonUint(c.offset)},
			member{"data", jsonString(encodeBase64(raw))},
		)
		gotOffset, _ := uintField(body, "offset")
		gotData, _ := stringField(body, "data")
		if gotOffset != c.offset || gotData != c.want {
			t.Errorf("chunk %s: got (%d, %q), want (%d, %q)", c.name, gotOffset, gotData, c.offset, c.want)
		}
	}

	for _, c := range []struct {
		name string
		raw  json.RawMessage
	}{
		{"accepting-plain", json.RawMessage(`{"id":"0123456789abcdef","state":"accepting","version":"0.2.1","size":9403,"received":4096}`)},
		{"accepting-delta", json.RawMessage(`{"id":"fedcba9876543210","state":"accepting","version":"0.2.1","size":9403,"received":512,"encoding":"delta-1","uploadSize":1024}`)},
		{"restarting", json.RawMessage(`{"id":"0123456789abcdef","state":"restarting","version":"0.2.1","size":9403,"received":9403}`)},
		{"failed", json.RawMessage(`{"id":"0123456789abcdef","state":"failed","version":"0.2.1","size":9403,"received":9403,"error":"verification failed"}`)},
	} {
		id, ok := stringField(c.raw, "id")
		if !ok || len(id) != 16 || !common.IsHex(id) {
			t.Errorf("progress %s: bad id %q", c.name, id)
		}
		if _, ok := stringField(c.raw, "state"); !ok {
			t.Errorf("progress %s: missing state", c.name)
		}
		if _, ok := uintField(c.raw, "received"); !ok {
			t.Errorf("progress %s: missing received", c.name)
		}
		if c.name == "failed" {
			detail := errorDetail(c.raw)
			if detail == nil || *detail != "verification failed" {
				t.Errorf("progress %s: errorDetail = %v", c.name, detail)
			}
		}
	}
}
