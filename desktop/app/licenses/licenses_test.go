package licenses

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestTheEmbeddedBundleParses(t *testing.T) {
	sections, err := Parse(embedded)
	if err != nil {
		t.Fatal(err)
	}
	if len(sections) == 0 {
		t.Fatal("no sections")
	}
}

func TestAValidBundleParsesInOrder(t *testing.T) {
	s, err := Parse(`{"schemaVersion":1,"sections":[{"title":"RC2FX","text":"MIT"},{"title":"serde","text":"MIT OR Apache-2.0"}]}`)
	if err != nil {
		t.Fatal(err)
	}
	if len(s) != 2 {
		t.Fatalf("%d sections", len(s))
	}
	if s[0].Title != "RC2FX" || s[1].Text != "MIT OR Apache-2.0" {
		t.Fatalf("sections %+v", s)
	}
}

func TestMalformedBundlesAreRefused(t *testing.T) {
	for _, bad := range []string{
		"",
		"not json",
		"[]",
		`{"sections":[{"title":"a","text":"b"}]}`,
		`{"schemaVersion":2,"sections":[{"title":"a","text":"b"}]}`,
		`{"schemaVersion":"1","sections":[{"title":"a","text":"b"}]}`,
		`{"schemaVersion":1}`,
		`{"schemaVersion":1,"sections":[]}`,
		`{"schemaVersion":1,"sections":[{"title":"a"}]}`,
		`{"schemaVersion":1,"sections":[{"title":" ","text":"b"}]}`,
		`{"schemaVersion":1,"sections":[{"title":"a","text":1}]}`,
		`{"schemaVersion":1,"sections":[{"title":"a","text":"b","x":1}]}`,
		`{"schemaVersion":1,"sections":[{"title":"a","text":"b"}],"extra":true}`,
		`{"schemaVersion":1,"sections":["a"]}`,
	} {
		if _, err := Parse(bad); err == nil {
			t.Fatalf("accepted %q", bad)
		}
	}
}

func TestThePlaceholderIsValidAndSaysItIsNotTheNotices(t *testing.T) {
	s, err := Parse(Placeholder)
	if err != nil {
		t.Fatal(err)
	}
	if len(s) != 1 {
		t.Fatalf("%d sections", len(s))
	}
	if !strings.Contains(s[0].Title, "not bundled") {
		t.Fatalf("title %q", s[0].Title)
	}
}

func TestDevelopmentBuildsWithoutAFileGetThePlaceholder(t *testing.T) {
	text, err := Resolve(false, "")
	if err != nil {
		t.Fatal(err)
	}
	if text != Placeholder {
		t.Fatalf("text %q", text)
	}
}

func TestReleaseBuildsNeedAValidFile(t *testing.T) {
	if _, err := Resolve(true, ""); err == nil || !strings.Contains(err.Error(), "RC2FX_LICENSES_JSON") {
		t.Fatalf("error %v", err)
	}
	dir := t.TempDir()
	missing := filepath.Join(dir, "missing.json")
	if _, err := Resolve(true, missing); err == nil || !strings.Contains(err.Error(), "cannot read") {
		t.Fatalf("error %v", err)
	}
	if _, err := Resolve(false, missing); err == nil {
		t.Fatal("a named file must exist")
	}
	bad := filepath.Join(dir, "bad.json")
	if err := os.WriteFile(bad, []byte(`{"schemaVersion":1,"sections":[]}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Resolve(true, bad); err == nil || !strings.Contains(err.Error(), "invalid license file") {
		t.Fatalf("error %v", err)
	}
	invalidUTF8 := filepath.Join(dir, "latin1.json")
	if err := os.WriteFile(invalidUTF8, []byte("{\"schemaVersion\":1,\"sections\":[{\"title\":\"\xff\",\"text\":\"\"}]}"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := Resolve(true, invalidUTF8); err == nil {
		t.Fatal("accepted invalid UTF-8")
	}
	good := filepath.Join(dir, "good.json")
	text := `{"schemaVersion":1,"sections":[{"title":"RC2FX","text":"MIT"}]}`
	if err := os.WriteFile(good, []byte(text), 0o644); err != nil {
		t.Fatal(err)
	}
	if got, err := Resolve(true, good); err != nil || got != text {
		t.Fatalf("resolve %q %v", got, err)
	}
}

func TestTheCommittedBundleIsThePlaceholder(t *testing.T) {
	// Release packaging regenerates licenses.json; the repository keeps the
	// placeholder so a plain build works.
	if os.Getenv("RC2FX_RELEASE_BUILD") == "1" {
		t.Skip("release build")
	}
	if embedded != Placeholder {
		t.Skip("licenses.json was regenerated")
	}
	if got := Sections(); len(got) != 1 || !strings.Contains(got[0].Title, "not bundled") {
		t.Fatalf("sections %+v", got)
	}
}
