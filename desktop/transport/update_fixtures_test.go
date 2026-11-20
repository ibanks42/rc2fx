package transport

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestUpdateWireDTOFixturesMatchRust(t *testing.T) {
	data, err := os.ReadFile(filepath.Join("..", "..", "fixtures", "update", "dto.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc struct {
		StartRequests struct {
			Accept []struct {
				Name string          `json:"name"`
				JSON json.RawMessage `json:"json"`
			} `json:"accept"`
		} `json:"startRequests"`
		Chunks struct {
			Accept []struct {
				Name   string          `json:"name"`
				RawHex string          `json:"rawHex"`
				JSON   json.RawMessage `json:"json"`
			} `json:"accept"`
		} `json:"chunks"`
		Progress []struct {
			Name string          `json:"name"`
			JSON json.RawMessage `json:"json"`
		} `json:"progress"`
	}
	if err := json.Unmarshal(data, &doc); err != nil {
		t.Fatal(err)
	}

	// Verify delta start request augmentation matches transport/update.go's withMembers format.
	for _, c := range doc.StartRequests.Accept {
		if c.Name != "whole-envelope" {
			continue
		}
		edited, ok := withMembers(c.JSON,
			member{"encoding", jsonString("delta-1")},
			member{"uploadSize", jsonUint(1024)},
		)
		if !ok {
			t.Fatalf("withMembers failed on %s", c.Name)
		}
		if enc, ok := stringField(edited, "encoding"); !ok || enc != "delta-1" {
			t.Fatalf("encoding = %q (%v)", enc, ok)
		}
		if sz, ok := uintField(edited, "uploadSize"); !ok || sz != 1024 {
			t.Fatalf("uploadSize = %d (%v)", sz, ok)
		}
	}

	// Verify chunk encoding matches the shared fixture bytes bit-for-bit.
	for _, c := range doc.Chunks.Accept {
		raw, err := hex.DecodeString(c.RawHex)
		if err != nil {
			t.Fatalf("chunk %s hex: %v", c.Name, err)
		}
		offset, ok := uintField(c.JSON, "offset")
		if !ok {
			t.Fatalf("chunk %s missing offset", c.Name)
		}
		wantData, ok := stringField(c.JSON, "data")
		if !ok {
			t.Fatalf("chunk %s missing data", c.Name)
		}
		body := object(
			member{"offset", jsonUint(offset)},
			member{"data", jsonString(encodeBase64(raw))},
		)
		gotOffset, _ := uintField(body, "offset")
		gotData, _ := stringField(body, "data")
		if gotOffset != offset || gotData != wantData {
			t.Errorf("chunk %s: got (%d, %q), want (%d, %q)", c.Name, gotOffset, gotData, offset, wantData)
		}
	}

	// Verify Progress fields transport reads (id, state, received, error) parse cleanly.
	for _, c := range doc.Progress {
		id, ok := stringField(c.JSON, "id")
		if !ok || len(id) != 16 || !isHex(id) {
			t.Errorf("progress %s: bad id %q", c.Name, id)
		}
		if _, ok := stringField(c.JSON, "state"); !ok {
			t.Errorf("progress %s: missing state", c.Name)
		}
		if _, ok := uintField(c.JSON, "received"); !ok {
			t.Errorf("progress %s: missing received", c.Name)
		}
		if c.Name == "failed" {
			detail := errorDetail(c.JSON)
			if detail == nil || *detail != "verification failed" {
				t.Errorf("progress %s: errorDetail = %v", c.Name, detail)
			}
		}
	}
}
