package core

// Shared fixtures in fixtures/image_check. Rust tests `records` (mixer-side
// report generation); Go tests `reports` (desktop-side decoding).

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

type imageReportCase struct {
	Name      string  `json:"name"`
	File      string  `json:"file"`
	Accept    bool    `json:"accept"`
	Canonical *string `json:"canonical"`
	Why       string  `json:"why"`
}

type imageFixtures struct {
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
	if len(fixtures.Reports) == 0 {
		t.Fatal("no image-check fixtures")
	}
	return fixtures
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
		if c.Canonical == nil {
			t.Fatalf("%s: an accepted answer needs its canonical form", c.Name)
		}
		var canonical ImageCheck
		if err := DecodeStrict([]byte(*c.Canonical), &canonical); err != nil {
			t.Fatalf("%s: canonical: %v", c.Name, err)
		}
		if !reflect.DeepEqual(normalImageCheck(check), normalImageCheck(canonical)) {
			t.Errorf("%s:\n got %+v\nwant %+v", c.Name, check, canonical)
		}
	}
}

// normalImageCheck treats an empty list as no list, as serde's
// skip_serializing_if does when the mixer writes the report.
func normalImageCheck(c ImageCheck) ImageCheck {
	if len(c.Repaired) == 0 {
		c.Repaired = nil
	}
	if len(c.Damaged) == 0 {
		c.Damaged = nil
	}
	return c
}
