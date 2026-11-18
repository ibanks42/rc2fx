package firmware

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"testing"
)

// The same files are read by crates/rc2fx-firmware/tests/shared_fixtures.rs.
const (
	repoRoot   = "../.."
	fixtureDir = repoRoot + "/fixtures/firmware"
)

func readJSON(t *testing.T, path string, v any) {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, v); err != nil {
		t.Fatal(err)
	}
}

func TestSharedModelFixtures(t *testing.T) {
	var fixtures struct {
		VendorID uint16
		Models   []struct {
			ID, Hostname, BoardDTB, FirmwareDirectory, Label, RouteLayout, SourceEnum string
			NormalPIDs                                                                []uint16
			UpdatePID                                                                 uint16
			RuntimeEnabled                                                            bool
		}
		USB []struct {
			Vendor, Product uint16
			Model           *string
		}
		Boards []struct {
			Hostname, Extlinux string
			Model              *string
			Error              *string
		}
		Parse []struct {
			ID    string
			Model *string
		}
	}
	readJSON(t, filepath.Join(fixtureDir, "models.json"), &fixtures)
	if fixtures.VendorID != VendorID {
		t.Errorf("vendor ID %#x", fixtures.VendorID)
	}
	if len(fixtures.Models) != len(ModelAll) {
		t.Fatalf("%d models", len(fixtures.Models))
	}
	for i, want := range fixtures.Models {
		m := ModelAll[i]
		pids := m.NormalPIDs()
		if m.ID() != want.ID || m.Hostname() != want.Hostname || m.BoardDTB() != want.BoardDTB ||
			m.FirmwareDirectory() != want.FirmwareDirectory || m.Label() != want.Label ||
			m.RouteLayout() != want.RouteLayout || m.SourceEnum() != want.SourceEnum ||
			!slices.Equal(pids[:], want.NormalPIDs) || m.UpdatePID() != want.UpdatePID ||
			m.RuntimeEnabled() != want.RuntimeEnabled {
			t.Errorf("model %d differs from %+v", i, want)
		}
	}
	for _, c := range fixtures.USB {
		m, ok := ModelFromUSB(c.Vendor, c.Product)
		if ok != (c.Model != nil) || (ok && m.ID() != *c.Model) {
			t.Errorf("ModelFromUSB(%#x, %#x) = %v, %v", c.Vendor, c.Product, m, ok)
		}
	}
	for _, c := range fixtures.Boards {
		m, err := ModelFromBoard(c.Hostname, c.Extlinux)
		switch {
		case c.Model != nil:
			if err != nil || m.ID() != *c.Model {
				t.Errorf("ModelFromBoard(%q, %q) = %v, %v", c.Hostname, c.Extlinux, m, err)
			}
		case err == nil || err.Error() != *c.Error:
			t.Errorf("ModelFromBoard(%q, %q) error %v, want %q", c.Hostname, c.Extlinux, err, *c.Error)
		}
	}
	for _, c := range fixtures.Parse {
		m, err := ParseModel(c.ID)
		if (err == nil) != (c.Model != nil) || (err == nil && m.ID() != *c.Model) {
			t.Errorf("ParseModel(%q) = %v, %v", c.ID, m, err)
		}
	}
}

func TestSharedCatalogFixtures(t *testing.T) {
	var fixtures struct {
		Cases []struct {
			File  string
			OK    bool
			Error *string
		}
	}
	readJSON(t, filepath.Join(fixtureDir, "catalogs.json"), &fixtures)
	if len(fixtures.Cases) == 0 {
		t.Fatal("no catalog cases")
	}
	for _, c := range fixtures.Cases {
		text, err := os.ReadFile(filepath.Join(repoRoot, c.File))
		if err != nil {
			t.Fatal(err)
		}
		_, err = ParseCatalog(string(text))
		switch {
		case c.OK:
			if err != nil {
				t.Errorf("%s: %v", c.File, err)
			}
		case err == nil:
			t.Errorf("%s accepted", c.File)
		case *c.Error != "*" && err.Error() != *c.Error:
			t.Errorf("%s: error %q, want %q", c.File, err, *c.Error)
		}
	}
}

func TestSharedSelectFixtures(t *testing.T) {
	dir := filepath.Join(fixtureDir, "select")
	var fixtures struct {
		Cases []struct {
			Op, Catalog, Image, Model, Launcher string
			ExperimentalDuo                     bool
			Profile, Error                      *string
		}
	}
	readJSON(t, filepath.Join(dir, "cases.json"), &fixtures)
	if len(fixtures.Cases) == 0 {
		t.Fatal("no select cases")
	}
	read := func(name string) []byte {
		data, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			t.Fatal(err)
		}
		return data
	}
	for _, c := range fixtures.Cases {
		catalog, err := ParseCatalog(string(read(c.Catalog)))
		if err != nil {
			t.Fatalf("%s: %v", c.Catalog, err)
		}
		image := read(c.Image)
		var model Model
		if c.Model != "" {
			if model, err = ParseModel(c.Model); err != nil {
				t.Fatal(err)
			}
		}
		var profile *Profile
		switch c.Op {
		case "select":
			profile, err = catalog.selectAny(image)
		case "selectForModel":
			profile, err = catalog.selectForModel(model, image, c.ExperimentalDuo)
		case "inspectForModel":
			profile, err = catalog.InspectForModel(model, image, read(c.Launcher))
		case "verifyLauncher":
			profile = &catalog.Profiles[0]
			err = profile.VerifyLauncher(read(c.Launcher))
		default:
			t.Fatalf("unknown op %s", c.Op)
		}
		name := c.Op + " " + c.Catalog + " " + c.Image + " " + c.Model
		switch {
		case c.Profile != nil:
			if err != nil || profile.ID != *c.Profile {
				t.Errorf("%s: %v", name, err)
			}
		case err == nil || err.Error() != *c.Error:
			t.Errorf("%s: error %v, want %q", name, err, *c.Error)
		}
	}
}

// selectAny is the mixer's Rust Catalog::select: the one profile for a mixer
// executable, for a model that needs no opt-in. The desktop never selects a
// runtime; this stays here so the select fixtures keep checking the Go
// VerifyImage that InspectForModel uses.
func (c *Catalog) selectAny(image []byte) (*Profile, error) {
	hash := sum(image)
	var found *Profile
	for i := range c.Profiles {
		if c.Profiles[i].MixerSHA256 != hash {
			continue
		}
		if found != nil {
			return nil, errors.New("ambiguous mixer executable: select using verified device model")
		}
		found = &c.Profiles[i]
	}
	if found == nil {
		return nil, fmt.Errorf("unrecognized mixer executable SHA-256 %s", hash)
	}
	model, err := ParseModel(found.Model)
	if err != nil {
		return nil, err
	}
	if err := model.RequireRuntimeWith(false); err != nil {
		return nil, err
	}
	if err := found.VerifyImage(image); err != nil {
		return nil, err
	}
	return found, nil
}

// selectForModel is the mixer's Rust Catalog::select_for_model_with: the
// opt-in admits only the named model's own profile and route layout.
func (c *Catalog) selectForModel(model Model, image []byte, experimentalDuo bool) (*Profile, error) {
	if err := model.RequireRuntimeWith(experimentalDuo); err != nil {
		return nil, err
	}
	profile, err := c.identify(model, image)
	if err != nil {
		return nil, err
	}
	if err := profile.VerifyImage(image); err != nil {
		return nil, err
	}
	return profile, nil
}
