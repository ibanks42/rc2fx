package usbproto

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The same files are read by crates/rc2fx-usb/tests/shared_fixtures.rs.
const fixtureDir = "../../fixtures/usb"

type fixtureCases struct {
	Hello4 []struct {
		Data   string
		Window *int
	}
	Descriptors []struct {
		Name    string
		Hex     string
		Control bool
	}
	Frames []struct {
		Name   string
		Report uint8
		Hex    string
		Marked bool
		Frame  *struct {
			Kind    uint8
			Session uint64
			Request uint32
			Offset  int
			Total   int
			Data    string
		}
	}
	Messages []struct {
		Name    string
		Limit   int
		Decoded *string
		Error   *string
	}
	Requests []struct {
		Method string
		Path   string
		Body   json.RawMessage
		JSON   string
	}
	Replies []struct {
		JSON   string
		OK     bool
		Status *uint16
	}
	RequestsIn []struct {
		JSON string
		OK   bool
	}
}

func loadFixtures(t *testing.T) fixtureCases {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(fixtureDir, "cases.json"))
	if err != nil {
		t.Fatal(err)
	}
	var cases fixtureCases
	if err := json.Unmarshal(data, &cases); err != nil {
		t.Fatal(err)
	}
	return cases
}

func TestSharedFixtures(t *testing.T) {
	cases := loadFixtures(t)
	if len(cases.Hello4) == 0 || len(cases.Frames) == 0 || len(cases.Messages) == 0 {
		t.Fatal("fixture file is missing cases")
	}
	for _, c := range cases.Hello4 {
		data := mustHex(t, c.Data)
		window, ok := Hello4Window(data)
		if ok != (c.Window != nil) || (ok && window != *c.Window) {
			t.Errorf("Hello4Window(%q) = %d, %v; want %v", data, window, ok, c.Window)
		}
	}
	for _, c := range cases.Descriptors {
		if got := DeclaresControl(mustHex(t, c.Hex)); got != c.Control {
			t.Errorf("descriptor %s: DeclaresControl = %v", c.Name, got)
		}
	}
	for _, c := range cases.Frames {
		report := mustHex(t, c.Hex)
		if got := Marked(report, c.Report); got != c.Marked {
			t.Errorf("frame %s: Marked = %v", c.Name, got)
		}
		f, ok := DecodeFrame(report, c.Report)
		if ok != (c.Frame != nil) {
			t.Errorf("frame %s: decoded = %v", c.Name, ok)
			continue
		}
		if !ok {
			continue
		}
		want := c.Frame
		if uint8(f.Kind) != want.Kind || f.Session != want.Session || f.Request != want.Request ||
			f.Offset != want.Offset || f.Total != want.Total || !bytes.Equal(f.Data, mustHex(t, want.Data)) {
			t.Errorf("frame %s: decoded %+v", c.Name, f)
		}
		if again := f.Encode(c.Report); !bytes.Equal(again[:], report) {
			t.Errorf("frame %s: re-encoding differs", c.Name)
		}
	}
	for _, c := range cases.Messages {
		packed, err := os.ReadFile(filepath.Join(fixtureDir, "messages", c.Name+".bin"))
		if err != nil {
			t.Fatal(err)
		}
		got, err := Unpack(packed, c.Limit)
		switch {
		case c.Decoded != nil:
			want, rerr := os.ReadFile(filepath.Join(fixtureDir, *c.Decoded))
			if rerr != nil {
				t.Fatal(rerr)
			}
			if err != nil || !bytes.Equal(got, want) {
				t.Errorf("message %s: %v", c.Name, err)
			}
			// What Go packs today must unpack the same way.
			repacked, perr := Pack(want, c.Limit)
			if perr != nil {
				t.Errorf("message %s: pack: %v", c.Name, perr)
			} else if back, uerr := Unpack(repacked, c.Limit); uerr != nil || !bytes.Equal(back, want) {
				t.Errorf("message %s: repack round trip: %v", c.Name, uerr)
			}
		case err == nil:
			t.Errorf("message %s: accepted", c.Name)
		case *c.Error != "*" && err.Error() != *c.Error:
			t.Errorf("message %s: error %q, want %q", c.Name, err, *c.Error)
		}
	}
	for _, c := range cases.Requests {
		request := Request{Method: c.Method, Path: c.Path}
		if string(c.Body) != "null" {
			request.Body = c.Body
		}
		got, err := request.MarshalJSON()
		if err != nil || string(got) != c.JSON {
			t.Errorf("request %q: %s, %v; want %s", c.Path, got, err, c.JSON)
		}
	}
	for _, c := range cases.Replies {
		var reply Reply
		err := json.Unmarshal([]byte(c.JSON), &reply)
		if (err == nil) != c.OK {
			t.Errorf("reply %s: %v", c.JSON, err)
		} else if c.OK && c.Status != nil && reply.Status != *c.Status {
			t.Errorf("reply %s: status %d", c.JSON, reply.Status)
		}
	}
	for _, c := range cases.RequestsIn {
		var request Request
		if err := json.Unmarshal([]byte(c.JSON), &request); (err == nil) != c.OK {
			t.Errorf("request %s: %v", c.JSON, err)
		}
	}
}
