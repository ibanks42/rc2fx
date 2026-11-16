package core

// The shared image-check fixtures in fixtures/image_check. The Rust crate
// (crates/rc2fx-core/tests/image_check_fixtures.rs) reads the same files:
// both must make the same report from each record, accept and reject the
// same answers, and write accepted ones back byte for byte.

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

type imageRecordCase struct {
	Name   string  `json:"name"`
	Record *string `json:"record"`
	Digest *string `json:"digest"`
	Expect string  `json:"expect"`
}

type imageReportCase struct {
	Name      string  `json:"name"`
	File      string  `json:"file"`
	Accept    bool    `json:"accept"`
	Canonical *string `json:"canonical"`
	Why       string  `json:"why"`
}

type imageFixtures struct {
	Comment string            `json:"comment"`
	Records []imageRecordCase `json:"records"`
	Reports []imageReportCase `json:"reports"`
}

func imageFixture(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "fixtures", "image_check", name))
	if err != nil {
		t.Fatalf("%s: %v", name, err)
	}
	return data
}

func loadImageFixtures(t *testing.T) imageFixtures {
	t.Helper()
	var fixtures imageFixtures
	if err := json.Unmarshal(imageFixture(t, "cases.json"), &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Records) == 0 || len(fixtures.Reports) == 0 {
		t.Fatal("no image-check fixtures")
	}
	return fixtures
}

func TestImageCheckRecordFixtures(t *testing.T) {
	for _, c := range loadImageFixtures(t).Records {
		var record *string
		if c.Record != nil {
			text := string(imageFixture(t, *c.Record))
			record = &text
		}
		got, err := Encode(ImageCheckFromRecord(record, c.Digest))
		if err != nil {
			t.Fatalf("%s: %v", c.Name, err)
		}
		if string(got) != c.Expect {
			t.Errorf("%s:\n got %s\nwant %s", c.Name, got, c.Expect)
		}
	}
}

func TestImageCheckReportFixtures(t *testing.T) {
	for _, c := range loadImageFixtures(t).Reports {
		// What the transport does with the mixer's answer.
		var check ImageCheck
		err := DecodeStrict(imageFixture(t, c.File), &check)
		if (err == nil) != c.Accept {
			t.Errorf("%s (%s): accepted %t, want %t (%v)", c.Name, c.Why, err == nil, c.Accept, err)
			continue
		}
		if !c.Accept {
			continue
		}
		got, err := Encode(check)
		if err != nil {
			t.Fatalf("%s: %v", c.Name, err)
		}
		if c.Canonical == nil || string(got) != *c.Canonical {
			t.Errorf("%s:\n got %s\nwant %v", c.Name, got, c.Canonical)
		}
	}
}
