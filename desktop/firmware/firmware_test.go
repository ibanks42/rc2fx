package firmware

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"os"
	"slices"
	"strings"
	"testing"
)

// The mixer's firmware crate embeds this file as DEFAULT_CATALOG.
const defaultCatalogPath = "../../config/firmware-profiles.json"

func defaultCatalog(t *testing.T) string {
	t.Helper()
	text, err := os.ReadFile(defaultCatalogPath)
	if err != nil {
		t.Fatal(err)
	}
	return string(text)
}

func mustParse(t *testing.T, text string) *Catalog {
	t.Helper()
	c, err := ParseCatalog(text)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func toJSON(t *testing.T, c *Catalog) string {
	t.Helper()
	text, err := json.Marshal(c)
	if err != nil {
		t.Fatal(err)
	}
	return string(text)
}

func clone(t *testing.T, c *Catalog) *Catalog {
	t.Helper()
	var out Catalog
	if err := json.Unmarshal([]byte(toJSON(t, c)), &out); err != nil {
		t.Fatal(err)
	}
	return &out
}

func cloneRuntime(r *Runtime) *Runtime {
	if r == nil {
		return nil
	}
	copied := *r
	return &copied
}

func errorContains(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), want) {
		t.Fatalf("error %v, want one containing %q", err, want)
	}
}

func TestUSBModesAreModelSpecificAndSecondaryUSBIsNotControl(t *testing.T) {
	for _, model := range ModelAll {
		pids := model.NormalPIDs()
		for _, pid := range append(pids[:], model.UpdatePID()) {
			if got, ok := ModelFromUSB(VendorID, pid); !ok || got != model {
				t.Errorf("ModelFromUSB(%#x) = %v, %v", pid, got, ok)
			}
			if _, ok := ModelFromUSB(0, pid); ok {
				t.Errorf("vendor 0 product %#x is a model", pid)
			}
		}
	}
	for _, pid := range []uint16{0x0026, 0xffff} {
		if _, ok := ModelFromUSB(VendorID, pid); ok {
			t.Errorf("%#x is a model", pid)
		}
	}
	if ModelDuo.RequireRuntimeWith(false) == nil {
		t.Error("the Duo needs no opt-in")
	}
}

func TestBoardEvidenceMustAgreeAndBeUnambiguous(t *testing.T) {
	for _, model := range ModelAll {
		boot := fmt.Sprintf("LABEL primary\n  FDT %s\n", model.BoardDTB())
		if got, err := ModelFromBoard(model.Hostname(), boot); err != nil || got != model {
			t.Errorf("%v: %v, %v", model, got, err)
		}
		other := ModelDuo
		if model == ModelDuo {
			other = ModelProIi
		}
		if _, err := ModelFromBoard(other.Hostname(), boot); err == nil {
			t.Errorf("%v: another model's hostname accepted", model)
		}
		if _, err := ModelFromBoard(model.Hostname(), boot+boot); err == nil {
			t.Errorf("%v: two boards accepted", model)
		}
		if _, err := ModelFromBoard(model.Hostname(), "# FDT ignored"); err == nil {
			t.Errorf("%v: a comment accepted", model)
		}
	}
}

func TestCatalogRejectsUnknownABIsDuplicateIdentitiesAndUnsafeTrampolines(t *testing.T) {
	defaults := mustParse(t, defaultCatalog(t))
	if defaults.Profiles[0].Runtime == nil || defaults.Profiles[1].Runtime == nil {
		t.Fatal("the first two default profiles are not runtime-qualified")
	}
	bad := clone(t, defaults)
	bad.Profiles[0].Runtime.CallbackABI = "unknown"
	if _, err := ParseCatalog(toJSON(t, bad)); err == nil {
		t.Error("unknown callback ABI accepted")
	}
	bad = clone(t, defaults)
	bad.Profiles = append(bad.Profiles, clone(t, defaults).Profiles[0])
	if _, err := ParseCatalog(toJSON(t, bad)); err == nil {
		t.Error("duplicate profile accepted")
	}
	bad = clone(t, defaults)
	bad.Profiles[0].Runtime.JobLifetime.Enqueue.Expected = "00000014000000000000000000000000" // b +0 cannot be replayed elsewhere
	if _, err := ParseCatalog(toJSON(t, bad)); err == nil {
		t.Error("PC-relative enqueue prologue accepted")
	}
}

// testImage builds a minimal AArch64 ELF image populated with the first
// default profile's hook site bytes.
func testImage(t *testing.T) (*Catalog, []byte) {
	t.Helper()
	catalog := mustParse(t, defaultCatalog(t))
	catalog.Profiles = catalog.Profiles[:1]
	runtime := catalog.Profiles[0].Runtime
	image := make([]byte, 0x400)
	copy(image, "\x7fELF\x02\x01")
	binary.LittleEndian.PutUint16(image[16:], 3)
	binary.LittleEndian.PutUint16(image[18:], 183)
	binary.LittleEndian.PutUint64(image[32:], 64)
	binary.LittleEndian.PutUint16(image[54:], 56)
	binary.LittleEndian.PutUint16(image[56:], 1)
	binary.LittleEndian.PutUint32(image[64:], 1) // PT_LOAD
	binary.LittleEndian.PutUint32(image[68:], 5) // PF_R|PF_X
	binary.LittleEndian.PutUint64(image[72:], 0x200)
	binary.LittleEndian.PutUint64(image[80:], 0x1000)
	binary.LittleEndian.PutUint64(image[96:], 0x100)
	for index, site := range runtime.Sites() {
		site.Offset = fmt.Sprintf("0x%x", 0x1000+16*index)
		b, err := site.Bytes()
		if err != nil {
			t.Fatal(err)
		}
		copy(image[0x200+index*16:], b[:])
	}
	catalog.Profiles[0].MixerSHA256 = sum(image)
	return catalog, image
}

func TestSharedExecutableIsNotADeviceIdentityOrRuntimeQualification(t *testing.T) {
	catalog, image := testImage(t)
	launcher := []byte("test launcher")
	catalog.Profiles[0].LauncherSHA256 = sum(launcher)
	duo := catalog.Profiles[0]
	duo.ID = "rodecaster-duo-1.7.6"
	duo.Model = ModelDuo.ID()
	duo.Runtime = nil
	catalog.Profiles = append(catalog.Profiles, duo)
	catalog = mustParse(t, toJSON(t, catalog))
	if p, err := catalog.selectForModel(ModelProIi, image, false); err != nil || p.Model != ModelProIi.ID() {
		t.Fatalf("Pro II: %v", err)
	}
	_, err := catalog.selectForModel(ModelDuo, image, false)
	errorContains(t, err, "experimental")
	inspected, err := catalog.InspectForModel(ModelDuo, image, launcher)
	if err != nil {
		t.Fatal(err)
	}
	if inspected.Model != ModelDuo.ID() || inspected.Runtime != nil {
		t.Fatalf("inspected %+v", inspected)
	}
	if _, err := catalog.InspectForModel(ModelDuo, image, []byte("wrong launcher")); err == nil {
		t.Error("wrong launcher accepted")
	}
	bad := clone(t, catalog)
	bad.Profiles[1].Runtime = cloneRuntime(bad.Profiles[0].Runtime)
	if _, err := ParseCatalog(toJSON(t, bad)); err == nil {
		t.Error("the Pro II routes granted to a Duo")
	}
	duplicate := clone(t, catalog)
	profile := clone(t, catalog).Profiles[0]
	profile.ID = "another-id"
	duplicate.Profiles = append(duplicate.Profiles, profile)
	if _, err := ParseCatalog(toJSON(t, duplicate)); err == nil {
		t.Error("duplicate executable accepted")
	}
}

func TestExperimentalOptInNeverGrantsTheProIiLayoutToADuo(t *testing.T) {
	catalog, image := testImage(t)
	duo := clone(t, catalog).Profiles[0]
	duo.ID = "duo-fixture"
	duo.Model = ModelDuo.ID()
	duo.Runtime.RouteLayout = ModelDuo.RouteLayout()
	duo.Runtime.SourceEnum = ModelDuo.SourceEnum()
	catalog.Profiles = append(catalog.Profiles, duo)
	if _, err := catalog.selectForModel(ModelDuo, image, false); err == nil {
		t.Error("Duo admitted without the opt-in")
	}
	if p, err := catalog.selectForModel(ModelDuo, image, true); err != nil || p.Model != ModelDuo.ID() {
		t.Fatalf("Duo with the opt-in: %v", err)
	}
	catalog.Profiles[1].Runtime = cloneRuntime(catalog.Profiles[0].Runtime)
	if _, err := catalog.selectForModel(ModelDuo, image, true); err == nil {
		t.Error("the Pro II layout granted to a Duo")
	}
	if _, err := ParseCatalog(toJSON(t, catalog)); err == nil {
		t.Error("the Pro II layout parsed for a Duo")
	}
	if _, err := ModelFromBoard(ModelDuo.Hostname(), "FDT "+ModelProIi.BoardDTB()); err == nil {
		t.Error("a Duo hostname on a Pro II board accepted")
	}
}

func TestResolvesExecutableVirtualOffsetsAndRefusesHashOrInstructionMismatches(t *testing.T) {
	catalog, image := testImage(t)
	profile := &catalog.Profiles[0]
	if err := profile.VerifyImage(image); err != nil {
		t.Fatal(err)
	}
	image[0x200] ^= 1
	errorContains(t, profile.VerifyImage(image), "differs from its profile hash")
	profile.MixerSHA256 = sum(image)
	errorContains(t, profile.VerifyImage(image), "instructions differ")
}

func TestKnownButUnqualifiedAndNonExecutableSitesAreRefused(t *testing.T) {
	catalog, image := testImage(t)
	catalog.Profiles[0].Runtime = nil
	errorContains(t, catalog.Profiles[0].VerifyImage(image), "not runtime-qualified")
	catalog, _ = testImage(t)
	binary.LittleEndian.PutUint32(image[68:], 4) // data, no PF_X
	catalog.Profiles[0].MixerSHA256 = sum(image)
	errorContains(t, catalog.Profiles[0].VerifyImage(image), "outside executable")
}

func TestCatalogJSONIsReadAsStrictlyAsSerde(t *testing.T) {
	text := defaultCatalog(t)
	mustParse(t, text)
	for name, edit := range map[string]func(string) string{
		"unknown top-level field": func(s string) string { return strings.Replace(s, `"schemaVersion"`, `"extra": 1, "schemaVersion"`, 1) },
		"unknown profile field":   func(s string) string { return strings.Replace(s, `"id":`, `"extra": 1, "id":`, 1) },
		"unknown site field":      func(s string) string { return strings.Replace(s, `"offset":`, `"extra": 1, "offset":`, 1) },
		"duplicate key":           func(s string) string { return strings.Replace(s, `"notes":`, `"notes": "x", "notes":`, 1) },
		"missing notes":           func(s string) string { return strings.Replace(s, `"notes":`, `"notez":`, 1) },
		"null string":             func(s string) string { return strings.Replace(s, `"version": "1.7.6"`, `"version": null`, 1) },
		"null default string": func(s string) string {
			return strings.Replace(s, `"minimumAppVersion": "0.1.1"`, `"minimumAppVersion": null`, 1)
		},
		"float rate": func(s string) string { return strings.Replace(s, `"sampleRate": 48000`, `"sampleRate": 48000.0`, 1) },
		// serde_json reads -0 as a float, which a u32 refuses.
		"negative zero rate": func(s string) string { return strings.Replace(s, `"sampleRate": 48000`, `"sampleRate": -0`, 1) },
		"field name case":    func(s string) string { return strings.Replace(s, `"notes":`, `"Notes":`, 1) },
		"duplicate site key": func(s string) string { return strings.Replace(s, `"offset":`, `"offset": "0x10", "offset":`, 1) },
		"invalid UTF-8":      func(s string) string { return strings.Replace(s, `"notes": "`, "\"notes\": \"\xff", 1) },
		"string rate":        func(s string) string { return strings.Replace(s, `"sampleRate": 48000`, `"sampleRate": "48000"`, 1) },
		"overflowing version": func(s string) string {
			return strings.Replace(s, `"schemaVersion": 1`, `"schemaVersion": 4294967297`, 1)
		},
		"lone surrogate":      func(s string) string { return strings.Replace(s, `"notes": "`, `"notes": "\udc00`, 1) },
		"trailing data":       func(s string) string { return s + "{}" },
		"profiles not a list": func(s string) string { return `{"schemaVersion": 1, "profiles": {}}` },
		"null catalog":        func(s string) string { return "null" },
		"over 128 KiB": func(s string) string {
			return strings.Replace(s, `"notes": "`, `"notes": "`+strings.Repeat("x", 128*1024), 1)
		},
	} {
		changed := edit(text)
		if changed == text {
			t.Fatalf("%s: the edit did not apply", name)
		}
		if _, err := ParseCatalog(changed); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	for name, edit := range map[string]func(string) string{
		"missing default string": func(s string) string { return strings.Replace(s, `"minimumAppVersion": "0.1.1",`, ``, 1) },
		"null runtime":           func(s string) string { return replaceRuntime(s, "null") },
		"missing runtime":        func(s string) string { return replaceRuntime(s, "") },
	} {
		changed := edit(text)
		if changed == text {
			t.Fatalf("%s: the edit did not apply", name)
		}
		if _, err := ParseCatalog(changed); err != nil {
			t.Errorf("%s refused: %v", name, err)
		}
	}
}

// replaceRuntime replaces the first profile's runtime with value, or drops
// the field when value is empty.
func replaceRuntime(text, value string) string {
	start := strings.Index(text, `"runtime": {`)
	depth := 0
	for i := start + len(`"runtime": `); i < len(text); i++ {
		switch text[i] {
		case '{':
			depth++
		case '}':
			depth--
			if depth == 0 {
				if value == "" {
					return text[:start] + strings.TrimLeft(text[i+1:], ", \n")
				}
				return text[:start] + `"runtime": ` + value + text[i+1:]
			}
		}
	}
	return text
}

func TestModelJSON(t *testing.T) {
	for _, model := range ModelAll {
		text, err := json.Marshal(model)
		if err != nil || string(text) != `"`+model.ID()+`"` {
			t.Fatalf("%v: %s %v", model, text, err)
		}
		var back Model
		if err := json.Unmarshal(text, &back); err != nil || back != model {
			t.Fatalf("%v: %v %v", model, back, err)
		}
	}
	var m Model
	for _, bad := range []string{`"ProIi"`, `null`, `0`, `"rodecaster-pro"`} {
		if err := json.Unmarshal([]byte(bad), &m); err == nil {
			t.Errorf("%s accepted", bad)
		}
	}
	if !slices.Equal(ModelAll[:], []Model{ModelProIi, ModelDuo}) || Model(0) != ModelProIi {
		t.Error("model order or default")
	}
}
