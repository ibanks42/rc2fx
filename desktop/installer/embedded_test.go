package installer

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
)

func TestAPayloadThatCannotBeReadSaysWhyAndIsNeverReady(t *testing.T) {
	// Whatever this build embeds, a source that cannot produce a catalog must
	// not claim readiness.
	missing := folderSource("/nonexistent/rc2fx-installer")
	state := []byte(`{"config":{"revision":1},"engine":{"mode":"native","status":"running","appliedRevision":1,"sampleRate":48000,"blockSize":112,"audioAdvancing":true}}`)
	service := missing.service(state)
	if service.Kind != ServiceStarting || service.Detail == "" {
		t.Fatalf("a missing payload claimed readiness: %+v", service)
	}
}

func TestTheNewestSupportedFirmwareIsPerModelAndNumeric(t *testing.T) {
	catalog := realCatalog(t)
	pro := firmware.ModelProIi
	var reported string
	for _, p := range catalog.Profiles {
		if p.Model == pro.ID() {
			reported = p.ID
			break
		}
	}
	newest, ok := newestSupported(catalog, reported)
	if !ok {
		t.Fatal("a supported Pro II profile")
	}
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; p.Model == pro.ID() && supported(p) {
			if order, _ := core.CompareVersions(p.Version, newest); order > 0 {
				t.Fatalf("%s is newer than %s", p.Version, newest)
			}
		}
	}
	// 1.7.10 is newer than 1.7.9, though it sorts lower as text; an
	// unsupported profile never counts, nor does another model's.
	var template Profile
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; p.Model == pro.ID() && supported(p) {
			template = p.Clone()
			break
		}
	}
	if _, ok := newestSupported(catalog, "no-such-profile"); ok {
		t.Fatal("an unknown profile")
	}
	with := func(f func(*Profile)) Profile {
		p := template.Clone()
		f(&p)
		return p
	}
	catalog.Profiles = []Profile{
		with(func(p *Profile) { p.Version = "1.7.9" }),
		with(func(p *Profile) { p.Version = "1.7.10" }),
		with(func(p *Profile) { p.Version = "9.0.0"; p.Runtime = nil }),
		with(func(p *Profile) { p.Version = "1.8-beta" }),
		with(func(p *Profile) { p.ID = "duo-newer"; p.Version = "8.0.0"; p.Model = firmware.ModelDuo.ID() }),
	}
	// Every Pro II entry above shares the template's id.
	if got, ok := newestSupported(catalog, template.ID); !ok || got != "1.7.10" {
		t.Fatalf("%q", got)
	}
	// The Duo entry carries a Pro II runtime, so it is not supported and its
	// higher version never counts for either model.
	if got, ok := newestSupported(catalog, "duo-newer"); ok {
		t.Fatalf("%q", got)
	}
}

func TestTheBundledReleaseIsTheSourceManifest(t *testing.T) {
	if !embed.Present {
		if _, err := BundledRelease(); err == nil {
			t.Fatal("a release without a payload")
		}
		if got, ok := NewestSupportedFirmware("rodecaster-pro-ii-1.7.7"); ok {
			t.Fatalf("%q without a payload", got)
		}
		return
	}
	source, err := core.ParseMixerRelease(readConfig(t, "mixer-release.json"))
	must(t, err)
	release, err := BundledRelease()
	must(t, err)
	if release != source {
		t.Fatalf("%+v", release)
	}
	identity, err := EmbeddedIdentity()
	must(t, err)
	if identity.Release != source.Version {
		t.Fatal(identity.Release)
	}
}

func TestTheEmbeddedIdentityMatchesThePayloadItCarries(t *testing.T) {
	if !embed.Present {
		t.Log("this build embeds no payload; identity is reported as unavailable")
		if _, err := EmbeddedIdentity(); err == nil {
			t.Fatal("an identity without a payload")
		}
		return
	}
	identity, err := EmbeddedIdentity()
	must(t, err)
	if identity.Kind != "embedded" || (identity.Build != embed.Release && identity.Build != embed.Development) {
		t.Fatalf("%+v", identity)
	}
	if len(identity.Files) != len(embed.PayloadNames) {
		t.Fatal("files")
	}
	for i, pin := range identity.Files {
		if pin.Name != embed.PayloadNames[i] || len(pin.SHA256) != 64 || pin.Size == 0 {
			t.Fatalf("%+v", pin)
		}
	}
	if !slices.ContainsFunc(identity.Profiles, func(p ProfilePin) bool { return p.Supported }) || identity.Catalog == "" || identity.Runtime == "" {
		t.Fatalf("%+v", identity)
	}
}

func TestTheEmbeddedPayloadBuildsTheImageItsIdentityNames(t *testing.T) {
	if !embed.Present {
		return
	}
	identity, err := EmbeddedIdentity()
	must(t, err)
	bundle, err := embeddedBundle()
	must(t, err)
	catalog, err := Source{Kind: SourceEmbedded}.catalog()
	must(t, err)
	var profile *Profile
	for i := range catalog.Profiles {
		if supported(&catalog.Profiles[i]) {
			profile = &catalog.Profiles[i]
			break
		}
	}
	if profile == nil {
		t.Fatal("a supported profile")
	}
	// What prepare embeds, from the same bytes it verified.
	entries, runtime, err := imageEntries(bundle, profile, nil)
	must(t, err)
	if runtime != identity.Runtime {
		t.Fatal("the prepared runtime digest")
	}
	paths := map[string]bool{}
	for _, e := range entries {
		paths[e.Path] = true
	}
	for _, name := range embed.PayloadNames {
		if !paths["opt/rc2fx/"+name] {
			t.Fatalf("%s is not in the image", name)
		}
	}
	for _, shim := range []string{"usr/sbin/rc_audio_mixer", "usr/sbin/rc_gui_juce"} {
		if !paths[shim] {
			t.Fatalf("%s is not in the image", shim)
		}
	}
	// And the same digest is what a prepared record has to carry.
	if digest(bundle) != identity.Runtime {
		t.Fatal("one runtime identity everywhere")
	}
}

func TestPayloadModeNamesTheBuild(t *testing.T) {
	want := "RC2FX-PAYLOAD-MODE:download:1"
	if embed.Present {
		want = "RC2FX-PAYLOAD-MODE:embedded:1"
	}
	if PayloadMode() != want {
		t.Fatal(PayloadMode())
	}
}

// The packager refuses an executable without the marker, so it has to
// survive compiling and linking with the release flags.
func TestPayloadModeSurvivesLinking(t *testing.T) {
	if testing.Short() {
		t.Skip("links a program")
	}
	exe := filepath.Join(t.TempDir(), "keepmode")
	args := []string{"build", "-trimpath", "-ldflags=-s -w", "-o", exe}
	if embed.Present {
		args = append(args, "-tags", "embedded")
	}
	command := exec.Command("go", append(args, "./testdata/keepmode")...)
	if out, err := command.CombinedOutput(); err != nil {
		t.Fatalf("%v\n%s", err, out)
	}
	data, err := os.ReadFile(exe)
	must(t, err)
	if !bytes.Contains(data, []byte(PayloadMode())) {
		t.Fatalf("the linked program lacks %q", PayloadMode())
	}
}
