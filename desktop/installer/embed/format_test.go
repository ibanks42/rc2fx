package embed

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

var (
	testCatalog = []byte(`{"schemaVersion":1,"profiles":[]}`)
	releaseJSON = []byte(`{"schemaVersion":1,"version":"0.1.0","protocolVersion":1}`)
)

func elf(marker byte) []byte {
	b := bytes.Repeat([]byte{marker}, 96)
	copy(b, []byte{127, 'E', 'L', 'F', 2, 1})
	b[18], b[19] = 0xb7, 0
	return b
}

func resum(t *testing.T, files Files) {
	var manifest strings.Builder
	for _, n := range Pinned {
		data, err := dataOf(files, n)
		if err != nil {
			t.Fatal(err)
		}
		fmt.Fprintf(&manifest, "%s  %s\n", common.SHA256Hex(data), n)
	}
	for i := range files {
		if files[i].Name == "SHA256SUMS" {
			files[i].Data = []byte(manifest.String())
		}
	}
}

func payload(t *testing.T, marker byte) Files {
	var files Files
	for _, name := range PayloadNames {
		var data []byte
		switch {
		case name == "firmware-profiles.json":
			data = testCatalog
		case name == ReleaseManifest:
			data = releaseJSON
		case name == "SHA256SUMS":
			data = nil
		case name == "rc2fx.sh":
			data = []byte("#!/bin/sh\n" + BuildLine + "\nexit 0\n")
		case IsScript(name):
			data = []byte("#!/bin/sh\nexit 0\n")
		default:
			data = elf(marker)
		}
		files = append(files, File{Name: name, Data: data})
	}
	resum(t, files)
	return files
}

func set(t *testing.T, files Files, name string, data []byte) {
	for i := range files {
		if files[i].Name == name {
			files[i].Data = data
		}
	}
	resum(t, files)
}

func asRelease(t *testing.T, files Files) {
	script, _ := dataOf(files, "rc2fx.sh")
	released, err := ReleaseScript(script)
	if err != nil {
		t.Fatal(err)
	}
	set(t, files, "rc2fx.sh", released)
}

func wrapped(t *testing.T, marker byte) []byte {
	files := payload(t, marker)
	if err := Validate(files); err != nil {
		t.Fatal(err)
	}
	return Wrap(Body(files))
}

func clone(files Files) Files {
	out := make(Files, len(files))
	for i, f := range files {
		out[i] = File{Name: f.Name, Data: bytes.Clone(f.Data)}
	}
	return out
}

func wantError(t *testing.T, err error, any ...string) {
	t.Helper()
	if err == nil {
		t.Fatalf("no error, want one of %q", any)
	}
	for _, want := range any {
		if strings.Contains(err.Error(), want) {
			return
		}
	}
	t.Fatalf("%v does not contain any of %q", err, any)
}

func TestStandalonePayloadRequiresExactFramingAndValidContents(t *testing.T) {
	data := wrapped(t, 1)
	if _, err := Standalone(data); err != nil {
		t.Fatal(err)
	}
	if _, err := Standalone(append([]byte("executable"), data...)); err == nil {
		t.Fatal("prefixed")
	}
	if _, err := Standalone(append(bytes.Clone(data), 0)); err == nil {
		t.Fatal("trailing")
	}
	if _, err := Standalone(data[:len(data)-1]); err == nil {
		t.Fatal("truncated")
	}
	damaged := bytes.Clone(data)
	damaged[len(damaged)-1] ^= 1
	if _, err := Standalone(damaged); err == nil {
		t.Fatal("damaged")
	}
	files := payload(t, 1)
	set(t, files, "rc2fxd", []byte("not an ARM64 executable"))
	// Correct envelope and file checksums must not bypass ELF validation.
	if _, err := Standalone(Wrap(Body(files))); err == nil {
		t.Fatal("a non-ELF passed")
	}
}

func TestAPayloadRoundTripsThroughTheEnvelope(t *testing.T) {
	files := payload(t, 1)
	if err := Validate(files); err != nil {
		t.Fatalf("the fixture itself is valid: %v", err)
	}
	blob := wrapped(t, 1)
	envelope, err := Find(blob)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(envelope.AsBytes(), blob) || !bytes.Equal(envelope.Body(), Body(files)) {
		t.Fatal("not read back as written")
	}
	back, err := envelope.Files()
	if err != nil {
		t.Fatal(err)
	}
	for i, f := range files {
		if back[i].Name != f.Name || !bytes.Equal(back[i].Data, f.Data) {
			t.Fatalf("%s differs", f.Name)
		}
	}
	bundle, err := envelope.Bundle()
	if err != nil {
		t.Fatal(err)
	}
	script, _ := dataOf(files, "rc2fx.sh")
	if len(bundle) != len(PayloadNames) || !bytes.Equal(bundle["rc2fx.sh"], script) {
		t.Fatal("bundle")
	}
	// Fake executable bytes wrapping the envelope.
	exe := append([]byte("\x7fELF not really an executable"), blob...)
	exe = append(exe, "trailing .rodata"...)
	found, err := Find(exe)
	if err != nil || !bytes.Equal(found.AsBytes(), envelope.AsBytes()) || !bytes.Equal(found.Body(), envelope.Body()) {
		t.Fatalf("%v", err)
	}
}

func TestExactlyOneCompleteEnvelopeIsRequired(t *testing.T) {
	one := wrapped(t, 1)
	// Stray Magic bytes in unrelated data should be ignored.
	exe := append(bytes.Clone(one), Magic...)
	exe = append(exe, "not a header\nand not a body"...)
	if _, err := Find(exe); err != nil {
		t.Fatalf("an invalid candidate is ignored: %v", err)
	}
	// Two valid payloads are ambiguous and must fail.
	twice := append(bytes.Clone(one), wrapped(t, 2)...)
	_, err := Find(twice)
	wantError(t, err, "2 installer payloads")
	_, err = Find([]byte("\x7fELF with no payload"))
	wantError(t, err, "does not embed an installer payload")
}

func TestADamagedOrTruncatedEnvelopeIsRefused(t *testing.T) {
	blob := wrapped(t, 1)
	header := len(Magic)
	cases := map[string]func([]byte) []byte{
		"body byte":           func(b []byte) []byte { b[len(b)-1] ^= 0xff; return b },
		"digest":              func(b []byte) []byte { b[header+20] = '0'; return b },
		"length not a number": func(b []byte) []byte { b[header+7] = 'x'; return b },
		"length larger":       func(b []byte) []byte { b[header+7] = '9'; return b },
		"truncated":           func(b []byte) []byte { return b[:len(b)-1] },
		"header":              func(b []byte) []byte { return append(b[:header:header], b[header+8:]...) },
	}
	for what, damage := range cases {
		_, err := Find(damage(bytes.Clone(blob)))
		if err == nil {
			t.Fatalf("%s accepted", what)
		}
		wantError(t, err, "does not embed", "damaged", "truncated")
	}
	// Outer envelope digest is valid, but per-file manifest checksum is wrong.
	wrong := Body(payload(t, 1))
	copy(wrong[:64], bytes.Repeat([]byte("0"), 64))
	_, err := Find(Wrap(wrong))
	wantError(t, err, "librc2fx_native.so does not match its embedded checksum")
}

func TestTheManifestPinsNamesOrderCountAndTrailingBytes(t *testing.T) {
	files := payload(t, 1)
	swapped := clone(files)
	swapped[0], swapped[1] = swapped[1], swapped[0]
	if Validate(swapped) == nil {
		t.Fatal("swapped order")
	}
	renamed := clone(files)
	renamed[0].Name = "librc2fx_native.so "
	if Validate(renamed) == nil {
		t.Fatal("renamed")
	}
	if Validate(clone(files)[:len(files)-1]) == nil {
		t.Fatal("short")
	}
	extra := append(Body(files), "extra"...)
	_, err := Find(Wrap(extra))
	wantError(t, err, "bytes after its last file")
	last := files[len(files)-1]
	old := fmt.Sprintf("%s  %d  %s\n", common.SHA256Hex(last.Data), len(last.Data), last.Name)
	shorter := fmt.Sprintf("%s  %d  %s\n", common.SHA256Hex(last.Data), len(last.Data)-1, last.Name)
	sizes := bytes.Replace(Body(files), []byte(old), []byte(shorter), 1)
	_, err = Find(Wrap(sizes))
	wantError(t, err, "does not match its embedded checksum", "bytes after its last file")
	// A size one byte too large cannot be met either.
	longer := fmt.Sprintf("%s  %d  %s\n", common.SHA256Hex(last.Data), len(last.Data)+1, last.Name)
	sizes = bytes.Replace(Body(files), []byte(old), []byte(longer), 1)
	if _, err := Find(Wrap(sizes)); err == nil {
		t.Fatal("a size too large was met")
	}
}

func TestChecksumsTheSumsManifestAndARM64ELFsAreChecked(t *testing.T) {
	files := payload(t, 1)
	set(t, files, "rc2fxd", []byte("not an ARM64 ELF"))
	wantError(t, Validate(files), "is not ARM64 ELF")
	files = payload(t, 1)
	for i := range files {
		if files[i].Name == "SHA256SUMS" {
			files[i].Data = append(files[i].Data, '\n')
		}
	}
	wantError(t, Validate(files), "checksum mismatch")
	files = payload(t, 1)
	set(t, files, "firmware-profiles.json", []byte(`{"schemaVersion":2}`))
	wantError(t, Validate(files), "unsupported runtime catalog")
	// A catalog that is not JSON at all.
	files = payload(t, 1)
	set(t, files, "firmware-profiles.json", []byte("{"))
	wantError(t, Validate(files), "not JSON")
}

func TestABuildLineWithTrailingSpacesStillRewritesToRelease(t *testing.T) {
	// A stray space must not let a development payload ship as a release.
	for _, script := range []string{
		"#!/bin/sh\nrc2fx_build=development  \nexit 0\n",
		"#!/bin/sh\nrc2fx_build=development\t\nexit 0\n",
		"#!/bin/sh\r\nrc2fx_build=development\r\nexit 0\r\n",
	} {
		if kind, err := BuildKind([]byte(script)); err != nil || kind != Development {
			t.Fatalf("%q: %s %v", script, kind, err)
		}
		released, err := ReleaseScript([]byte(script))
		if err != nil {
			t.Fatal(err)
		}
		if kind, err := BuildKind(released); err != nil || kind != Release {
			t.Fatalf("%q: %s %v", script, kind, err)
		}
		if strings.Contains(string(released), "rc2fx_build=development") {
			t.Fatalf("%q", released)
		}
	}
	released, err := ReleaseScript([]byte("#!/bin/sh\nrc2fx_build=development  \nexit 0\n"))
	if err != nil || string(released) != "#!/bin/sh\nrc2fx_build=release\nexit 0\n" {
		t.Fatalf("the rewrite is exact: %q %v", released, err)
	}
}

func TestASecondOrUnsupportedBuildAssignmentIsRefused(t *testing.T) {
	for _, script := range []string{
		"#!/bin/sh\nrc2fx_build=development\nexport rc2fx_build=release\n",
		"#!/bin/sh\nrc2fx_build=development\nrc2fx_build = release\n",
		"#!/bin/sh\nrc2fx_build=development\nrc2fx_build=development2\n",
		"#!/bin/sh\nrc2fx_build=development\nrc2fx_build=release\n",
	} {
		_, err := BuildKind([]byte(script))
		wantError(t, err, "rc2fx_build")
		if _, err := ReleaseScript([]byte(script)); err == nil {
			t.Fatalf("%q rewritten", script)
		}
	}
	// A comment or a use of the variable is not another assignment.
	if kind, err := BuildKind([]byte("#!/bin/sh\n# rc2fx_build is set below\nrc2fx_build=development\n")); err != nil || kind != Development {
		t.Fatalf("%s %v", kind, err)
	}
}

func TestACatalogThatDoesNotDeserializeIsRefused(t *testing.T) {
	for _, catalog := range []string{
		`{"schemaVersion":1,"profiles":42}`,
		`{"schemaVersion":1,"profiles":[{"id":"x","runtime":{"sampleRate":"x"}}]}`,
		`{"schemaVersion":1,"profiles":[{"runtime":{"blockFrames":-1}}]}`,
		`{"schemaVersion":"1"}`,
	} {
		files := payload(t, 1)
		set(t, files, "firmware-profiles.json", []byte(catalog))
		wantError(t, Validate(files), "catalog")
	}
}

func TestExactlyOneBuildLineSaysWhichKindOfBuildThisIs(t *testing.T) {
	development := []byte("#!/bin/sh\nrc2fx_build=development\nexit 0\n")
	if kind, _ := BuildKind(development); kind != Development {
		t.Fatal(kind)
	}
	release, err := ReleaseScript(development)
	if err != nil {
		t.Fatal(err)
	}
	if kind, _ := BuildKind(release); kind != Release || string(release) != "#!/bin/sh\nrc2fx_build=release\nexit 0\n" {
		t.Fatalf("%q", release)
	}
	for _, script := range []string{"#!/bin/sh\nexit 0\n", "rc2fx_build=development\nrc2fx_build=release\n", " # rc2fx_build=development\n"} {
		if _, err := BuildKind([]byte(script)); err == nil {
			t.Fatalf("%q accepted", script)
		}
	}
	same, err := ReleaseScript([]byte("#!/bin/sh\nrc2fx_build=release\nexit 0\n"))
	if err != nil || string(same) != "#!/bin/sh\nrc2fx_build=release\nexit 0\n" {
		t.Fatal("rewriting a release script changes nothing")
	}
	// The payload carries the same rule even in a development build.
	files := payload(t, 1)
	set(t, files, "rc2fx.sh", []byte("#!/bin/sh\nexit 0\n"))
	wantError(t, Validate(files), "exactly one `rc2fx_build")
}

func TestAReleasePayloadDiffersFromADevelopmentOne(t *testing.T) {
	development := payload(t, 1)
	release := payload(t, 1)
	asRelease(t, release)
	if err := Validate(release); err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(Body(development), Body(release)) {
		t.Fatal("a release payload is not a development payload")
	}
	script, _ := dataOf(release, "rc2fx.sh")
	if kind, _ := BuildKind(script); kind != Release {
		t.Fatal(kind)
	}
}

func TestCRLFScriptsAreNormalized(t *testing.T) {
	for in, want := range map[string]string{"a\r\nb\rc\n": "a\nb\rc\n", "": "", "\r\r\n": "\r\n"} {
		if got := string(CRLFToLF([]byte(in))); got != want {
			t.Fatalf("%q: %q", in, got)
		}
	}
}

func write(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
}

// folder creates a release-layout directory (runtime/, mixer/scripts/, config/).
func folder(t *testing.T, marker byte, release bool) string {
	dir := t.TempDir()
	files := payload(t, marker)
	if release {
		asRelease(t, files)
	}
	for _, f := range files {
		path := filepath.Join(dir, "runtime", f.Name)
		if IsScript(f.Name) {
			path = filepath.Join(dir, "mixer", "scripts", f.Name)
		}
		write(t, path, f.Data)
	}
	write(t, filepath.Join(dir, "config", "firmware-profiles.json"), testCatalog)
	write(t, filepath.Join(dir, "config", ReleaseManifest), releaseJSON)
	return dir
}

func scriptOf(t *testing.T, e *Envelope) []byte {
	files, err := e.Files()
	if err != nil {
		t.Fatal(err)
	}
	script, _ := dataOf(files, "rc2fx.sh")
	return script
}

func sameFiles(t *testing.T, got, want Files) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("%d files, want %d", len(got), len(want))
	}
	for i := range want {
		if got[i].Name != want[i].Name || !bytes.Equal(got[i].Data, want[i].Data) {
			t.Fatalf("%s differs", want[i].Name)
		}
	}
}

func TestExpectedBuildsTheSameBlobFromAFolder(t *testing.T) {
	dir := folder(t, 1, false)
	expected, err := Expected(dir, false)
	if err != nil {
		t.Fatal(err)
	}
	files := payload(t, 1)
	if !bytes.Equal(expected.Body(), Body(files)) {
		t.Fatal("body")
	}
	read, err := Read(dir, false)
	if err != nil {
		t.Fatal(err)
	}
	sameFiles(t, read, files)
	// A release folder is embedded as a release payload.
	release := folder(t, 1, true)
	expected, err = Expected(release, true)
	if err != nil {
		t.Fatal(err)
	}
	if kind, _ := BuildKind(scriptOf(t, expected)); kind != Release {
		t.Fatal(kind)
	}
	// The folder's own script already says release, and reading it again for
	// development does not turn it back.
	expected, err = Expected(release, false)
	if err != nil {
		t.Fatal(err)
	}
	if kind, _ := BuildKind(scriptOf(t, expected)); kind != Release {
		t.Fatal(kind)
	}
}

func TestASourceCheckoutUsesTheStagedMixerEvenWithARuntimeDirectory(t *testing.T) {
	root := folder(t, 1, false)
	write(t, filepath.Join(root, "Cargo.toml"), []byte("[workspace]\n"))
	if err := os.MkdirAll(filepath.Join(root, "target"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(filepath.Join(root, "runtime"), filepath.Join(root, "target/mixer")); err != nil {
		t.Fatal(err)
	}
	// An empty leftover directory must not hide a successful mixer-build.
	if err := os.Mkdir(filepath.Join(root, "runtime"), 0o755); err != nil {
		t.Fatal(err)
	}
	read, err := Read(root, false)
	if err != nil {
		t.Fatal(err)
	}
	sameFiles(t, read, payload(t, 1))
	// Nor may an older, otherwise valid bundle override the staged one.
	for _, f := range payload(t, 2) {
		if !IsScript(f.Name) {
			write(t, filepath.Join(root, "runtime", f.Name), f.Data)
		}
	}
	read, err = Read(root, false)
	if err != nil {
		t.Fatal(err)
	}
	sameFiles(t, read, payload(t, 1))
	// A missing staged file must fail instead of silently using old files.
	if err := os.Remove(filepath.Join(root, "target/mixer/rc2fxd")); err != nil {
		t.Fatal(err)
	}
	_, err = Read(root, false)
	wantError(t, err, "rc2fxd")
}

func TestAReleaseFolderUsesRuntimeEvenWithAStagedMixerDirectory(t *testing.T) {
	root := folder(t, 1, false)
	for _, f := range payload(t, 2) {
		if !IsScript(f.Name) {
			write(t, filepath.Join(root, "target/mixer", f.Name), f.Data)
		}
	}
	read, err := Read(root, false)
	if err != nil {
		t.Fatal(err)
	}
	sameFiles(t, read, payload(t, 1))
	if err := os.Remove(filepath.Join(root, "runtime/rc2fxd")); err != nil {
		t.Fatal(err)
	}
	_, err = Read(root, false)
	wantError(t, err, "rc2fxd")
}

func TestAFolderWhoseCatalogsDifferOrWhoseFilesAreMissingIsRefused(t *testing.T) {
	dir := folder(t, 1, false)
	write(t, filepath.Join(dir, "config", "firmware-profiles.json"), []byte(`{"schemaVersion":1,"profiles":[{"id":"other"}]}`))
	_, err := Read(dir, false)
	wantError(t, err, "catalogs differ")
	dir = folder(t, 1, false)
	os.Remove(filepath.Join(dir, "runtime", "rc2fxd"))
	_, err = Read(dir, false)
	wantError(t, err, "rc2fxd")
	// An old bundle without the recorder is a missing file, not a pass.
	dir = folder(t, 1, false)
	os.Remove(filepath.Join(dir, "runtime", "rc2fx_jackerr_snapshot"))
	_, err = Read(dir, false)
	wantError(t, err, "rc2fx_jackerr_snapshot")
}

func TestTheReleaseManifestIsRequiredCheckedAndMustMatchItsSource(t *testing.T) {
	// A runtime built for another release than the tree names is refused.
	dir := folder(t, 1, false)
	write(t, filepath.Join(dir, "config", ReleaseManifest), []byte(`{"schemaVersion":1,"version":"0.2.0","protocolVersion":1}`))
	_, err := Read(dir, false)
	wantError(t, err, "differs from config/")
	// No source manifest at all.
	dir = folder(t, 1, false)
	os.Remove(filepath.Join(dir, "config", ReleaseManifest))
	_, err = Read(dir, false)
	wantError(t, err, "manifest missing")
	// A runtime that predates the manifest.
	dir = folder(t, 1, false)
	os.Remove(filepath.Join(dir, "runtime", ReleaseManifest))
	_, err = Read(dir, false)
	wantError(t, err, ReleaseManifest)
	// A pinned, well-formed manifest is still checked for its shape.
	for _, bad := range []string{
		`{"schemaVersion":2,"version":"0.1.0","protocolVersion":1}`,
		`{"schemaVersion":1,"version":"0.1.0-rc1","protocolVersion":1}`,
		`{"schemaVersion":1,"version":"0.1.0","protocolVersion":0}`,
		`{"schemaVersion":1,"version":"0.1.0","protocolVersion":1,"extra":1}`,
		"not json",
	} {
		files := payload(t, 1)
		set(t, files, ReleaseManifest, []byte(bad))
		if Validate(files) == nil {
			t.Fatalf("%s accepted", bad)
		}
	}
	if version, err := ReleaseVersion(releaseJSON); err != nil || version != "0.1.0" {
		t.Fatalf("%s %v", version, err)
	}
	// The real manifest passes the payload rules too.
	real, err := os.ReadFile("../../../config/mixer-release.json")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ReleaseVersion(real); err != nil {
		t.Fatal(err)
	}
}

func TestRequireMatchesComparesTheWholeBlobAndNamesWhatDiffers(t *testing.T) {
	one, err := Find(wrapped(t, 1))
	if err != nil {
		t.Fatal(err)
	}
	same, _ := Find(wrapped(t, 1))
	if err := one.RequireMatches(same); err != nil {
		t.Fatal(err)
	}
	other, _ := Find(wrapped(t, 2))
	err = one.RequireMatches(other)
	wantError(t, err, "is not this build's")
	wantError(t, err, "librc2fx_native.so")
}
