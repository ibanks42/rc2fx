package firmware

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

const (
	fixtureBaseProfileCount = 4
	fixtureProfileIDMarker  = "\n      \"id\": "
	fixtureProfilesSuffix   = "\n  ]\n}\n"
	fixturePadMarker        = `"notes": "`
)

type fixtureEdit struct {
	Find    *string `json:"find"`
	Replace *string `json:"replace"`
}

type fixtureCaseInput struct {
	Name     string          `json:"name"`
	File     json.RawMessage `json:"file"`
	Base     json.RawMessage `json:"base"`
	Edits    json.RawMessage `json:"edits"`
	Generate json.RawMessage `json:"generate"`
}

func decodeFixtureString(raw json.RawMessage) (string, bool) {
	var value *string
	if err := json.Unmarshal(raw, &value); err != nil || value == nil {
		return "", false
	}
	return *value, true
}

func decodeFixtureUint(raw json.RawMessage) (int, bool) {
	var value *int
	if err := json.Unmarshal(raw, &value); err != nil || value == nil || *value < 0 {
		return 0, false
	}
	return *value, true
}

func resolveCaseText(t *testing.T, dir, defaultBase string, input fixtureCaseInput) string {
	t.Helper()
	if input.Name == "" {
		t.Fatal("case missing name")
	}
	hasFile := input.File != nil
	hasBase := input.Base != nil
	hasEdits := input.Edits != nil
	hasGenerate := input.Generate != nil

	if hasFile {
		if hasBase || hasEdits || hasGenerate {
			t.Fatalf("%s: file cannot be combined with base, edits, or generate", input.Name)
		}
		file, ok := decodeFixtureString(input.File)
		if !ok {
			t.Fatalf("%s: file must be a string", input.Name)
		}
		data, err := os.ReadFile(filepath.Join(dir, file))
		if err != nil {
			t.Fatalf("%s: read %s: %v", input.Name, file, err)
		}
		return string(data)
	}

	if hasEdits == hasGenerate {
		t.Fatalf("%s: case must specify exactly one of file, edits, or generate", input.Name)
	}

	baseRel := defaultBase
	if hasBase {
		var ok bool
		baseRel, ok = decodeFixtureString(input.Base)
		if !ok {
			t.Fatalf("%s: base must be a string", input.Name)
		}
	}
	data, err := os.ReadFile(filepath.Join(dir, baseRel))
	if err != nil {
		t.Fatalf("%s: read base %s: %v", input.Name, baseRel, err)
	}
	baseText := string(data)

	if hasEdits {
		var edits *[]fixtureEdit
		if err := json.Unmarshal(input.Edits, &edits); err != nil {
			t.Fatalf("%s: invalid edits: %v", input.Name, err)
		}
		if edits == nil {
			t.Fatalf("%s: edits must be an array", input.Name)
		}
		return applyFixtureEdits(t, input.Name, baseText, *edits)
	}
	return applyFixtureGenerate(t, input.Name, baseText, input.Generate)
}

func applyFixtureEdits(t *testing.T, name, text string, edits []fixtureEdit) string {
	t.Helper()
	if len(edits) == 0 {
		t.Fatalf("%s: edits must not be empty", name)
	}
	for idx, edit := range edits {
		if edit.Find == nil {
			t.Fatalf("%s: edit %d missing string find", name, idx)
		}
		if edit.Replace == nil {
			t.Fatalf("%s: edit %d missing string replace", name, idx)
		}
		find := *edit.Find
		replace := *edit.Replace
		if find == "" {
			t.Fatalf("%s: edit %d has empty find", name, idx)
		}
		count := strings.Count(text, find)
		if count != 1 {
			t.Fatalf("%s: edit %d find matched %d times, expected 1: %q", name, idx, count, find)
		}
		text = strings.Replace(text, find, replace, 1)
	}
	return text
}

func applyFixtureGenerate(t *testing.T, name, base string, raw json.RawMessage) string {
	t.Helper()
	var spec *map[string]json.RawMessage
	if err := json.Unmarshal(raw, &spec); err != nil || spec == nil {
		t.Fatalf("%s: generate must be an object", name)
	}
	if len(*spec) != 1 {
		t.Fatalf("%s: generate must have exactly one directive, got %s", name, string(raw))
	}
	if rawCount, ok := (*spec)["profiles"]; ok {
		count, ok := decodeFixtureUint(rawCount)
		if !ok {
			t.Fatalf("%s: generate.profiles must be a non-negative integer", name)
		}
		return generateFixtureProfiles(t, name, base, count)
	}
	if rawPad, ok := (*spec)["padBytes"]; ok {
		padBytes, ok := decodeFixtureUint(rawPad)
		if !ok {
			t.Fatalf("%s: generate.padBytes must be a non-negative integer", name)
		}
		return generateFixturePadBytes(t, name, base, padBytes)
	}
	t.Fatalf("%s: unknown generate directive %s", name, string(raw))
	return ""
}

func generateFixtureProfiles(t *testing.T, name, base string, count int) string {
	t.Helper()
	if count < fixtureBaseProfileCount || count > 256 {
		t.Fatalf("%s: generate.profiles must be in %d..256, got %d", name, fixtureBaseProfileCount, count)
	}
	if baseProfiles := strings.Count(base, fixtureProfileIDMarker); baseProfiles != fixtureBaseProfileCount {
		t.Fatalf("%s: generate.profiles requires a base with %d profiles, found %d", name, fixtureBaseProfileCount, baseProfiles)
	}
	if !strings.HasSuffix(base, fixtureProfilesSuffix) {
		t.Fatalf("%s: base missing expected closing suffix", name)
	}
	var out strings.Builder
	out.Grow(len(base) + (count-fixtureBaseProfileCount)*512)
	out.WriteString(strings.TrimSuffix(base, fixtureProfilesSuffix))
	for idx := fixtureBaseProfileCount; idx < count; idx++ {
		mixerHash := strings.Repeat(fmt.Sprintf("%02x", idx), 32)
		fmt.Fprintf(&out,
			",\n    {\n"+
				"      \"minimumAppVersion\": \"0.1.1\",\n"+
				"      \"id\": \"x-%d\",\n"+
				"      \"model\": \"rodecaster-duo\",\n"+
				"      \"version\": \"x-%d\",\n"+
				"      \"mixerSha256\": \"%s\",\n"+
				"      \"launcherSha256\": \"f27368e6d68296ebb9c6f22401f2fe00317643bee23c08b86d44d183408fa263\",\n"+
				"      \"archiveSha256\": \"4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed\",\n"+
				"      \"archiveMd5\": \"09393fef8a9f3b627a39801148d85d1d\",\n"+
				"      \"runtime\": null,\n"+
				"      \"notes\": \"\"\n"+
				"    }",
			idx, idx, mixerHash,
		)
	}
	out.WriteString(fixtureProfilesSuffix)
	return out.String()
}

func generateFixturePadBytes(t *testing.T, name, base string, padBytes int) string {
	t.Helper()
	if padBytes <= 0 {
		t.Fatalf("%s: generate.padBytes must be positive, got %d", name, padBytes)
	}
	pos := strings.Index(base, fixturePadMarker)
	if pos < 0 {
		t.Fatalf("%s: base missing notes marker", name)
	}
	insertAt := pos + len(fixturePadMarker)
	var out strings.Builder
	out.Grow(len(base) + padBytes)
	out.WriteString(base[:insertAt])
	out.WriteString(strings.Repeat("x", padBytes))
	out.WriteString(base[insertAt:])
	return out.String()
}
