package installer

import (
	"bytes"
	"fmt"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

func TestDuoIsRecognizedButNeverOpenedOrFlashedWithoutOptIn(t *testing.T) {
	normal := firmware.ModelDuo.NormalPIDs()
	pids := append(normal[:], firmware.ModelDuo.UpdatePID())
	for _, pid := range pids {
		x := newFixture(t)
		x.with(func(f *fake) { f.items[0].PID = pid })
		status, err := x.engine.probe()
		must(t, err)
		if len(status.USB) != 1 || status.USB[0].Model != firmware.ModelDuo || status.USBError == nil || !strings.Contains(*status.USBError, "experimental") {
			t.Fatalf("%+v", status)
		}
		contains(t, x.engine.enterUSBMode(serial), "experimental")
		contains(t, x.engine.flash(serial), "experimental")
		if x.counts() != [3]int{} {
			t.Fatal("device accessed")
		}
	}
	x := newFixture(t)
	x.with(func(f *fake) {
		duo := f.items[0]
		duo.PID = firmware.ModelDuo.NormalPIDs()[0]
		duo.Serial = "DUO"
		f.items = append(f.items, duo)
	})
	if x.engine.enterUSBMode("") == nil || x.counts() != [3]int{} {
		t.Fatal("mixed models must not auto-select")
	}
	for _, stock := range []bool{false, true} {
		x := newFixture(t)
		p := x.prepared.clone()
		duo := firmware.ModelDuo
		p.Model = &duo
		p.Stock = stock
		contains(t, x.engine.verifyPrepared(p), "experimental")
		if x.counts() != [3]int{} {
			t.Fatal("device accessed")
		}
	}
}

func realCatalog(t *testing.T) *Catalog {
	var catalog Catalog
	must(t, sjson.Unmarshal(readConfig(t, "firmware-profiles.json"), &catalog))
	return &catalog
}

func TestReleaseDiscoveryKeepsSameVersionModelsSeparate(t *testing.T) {
	catalog := realCatalog(t)
	for _, model := range firmware.ModelAll {
		releases, err := releasesFromCatalog(model, catalog)
		must(t, err)
		if len(releases) != 2 || releases[0].Version != "1.7.7" || releases[1].Version != "1.7.6" {
			t.Fatalf("%s: %+v", model.ID(), releases)
		}
		other := firmware.ModelProIi
		if model == firmware.ModelProIi {
			other = firmware.ModelDuo
		}
		for _, r := range releases {
			if r.Model != model.ID() || !r.Supported || r.Channel != "stable" ||
				r.URL != fmt.Sprintf("https://update.rode.com/%s/%s/os.tar.gz", model.FirmwareDirectory(), r.Version) {
				t.Fatalf("%+v", r)
			}
			if _, err := locationForModel(other, r.URL, "os.tar.gz", r.Version); err == nil {
				t.Fatalf("%s accepted for %s", r.URL, other.ID())
			}
		}
	}
}

func TestKeyGenerationIsReusedAndNeverHalfReplaced(t *testing.T) {
	path := filepath.Join(t.TempDir(), "id")
	key, public, err := ensureKey(path)
	must(t, err)
	_, again, err := ensureKey(key)
	must(t, err)
	if !bytes.Equal(public, again) {
		t.Fatal("existing key replaced")
	}
	line := string(public)
	if !strings.HasPrefix(line, "ssh-ed25519 ") || !strings.HasSuffix(line, "\n") || len(strings.Split(line, " ")) != 2 {
		t.Fatalf("%q", line)
	}
	private, err := os.ReadFile(key)
	must(t, err)
	if !strings.HasPrefix(string(private), "-----BEGIN OPENSSH PRIVATE KEY-----") {
		t.Fatalf("%s", private)
	}
	must(t, os.Remove(path+".pub"))
	if _, _, err := ensureKey(path); err == nil {
		t.Fatal("half a key pair accepted")
	}
}

func TestRequestsTakeTheFolderLock(t *testing.T) {
	x := newFixture(t)
	lock, err := openLocalFile(filepath.Join(x.engine.Work, "installer.lock"), false)
	must(t, err)
	defer lock.Close()
	unlock, err := lockAction(lock)
	must(t, err)
	_, err = x.engine.Handle(Request{Kind: RequestProbe})
	contains(t, err, "another RC2FX installer")
	unlock()
	reply, err := x.engine.Handle(Request{Kind: RequestProbe})
	must(t, err)
	if reply.Kind != ReplyStatus {
		t.Fatalf("%+v", reply)
	}
}

func TestPIDGuards(t *testing.T) {
	has := func(pid uint16) bool {
		for _, p := range normalPIDs {
			if p == pid {
				return true
			}
		}
		return false
	}
	if !has(0x37) || has(0x26) {
		t.Fatal("normal PIDs")
	}
}

func TestUTCFormatsRFC3339(t *testing.T) {
	now := utc()
	if len(now) != 20 || !strings.HasSuffix(now, "Z") || now[10] != 'T' {
		t.Fatalf("%q", now)
	}
}

func appendData(t *testing.T, w *tarWriter, base *tarHeader, path string, data []byte) {
	t.Helper()
	h := *base
	name, err := archivePath(path)
	must(t, err)
	for i := 0; i < 100; i++ {
		h[i] = 0
	}
	if len(name) > 100 {
		var long tarHeader
		copy(long[0:], "././@LongLink")
		octal(long[100:108], 0o644)
		octal(long[108:116], 0)
		octal(long[116:124], 0)
		octal(long[124:136], uint64(len(path)+1))
		octal(long[136:148], 0)
		long[156] = typeLongName
		copy(long[257:263], "ustar ")
		copy(long[263:265], " \x00")
		setChecksum(&long)
		must(t, w.Append(&long, bytes.NewReader(append([]byte(path), 0))))
		name = name[:100]
	}
	copy(h[0:100], name)
	octal(h[124:136], uint64(len(data)))
	setChecksum(&h)
	must(t, w.Append(&h, bytes.NewReader(data)))
}

func TestPatchChangesOnlyAuthorizedKeyAndKeepsLongNames(t *testing.T) {
	dir := t.TempDir()
	src, dst := filepath.Join(dir, "source.gz"), filepath.Join(dir, "patched.gz")
	long := "./usr/share/" + strings.Repeat("n", 120) + "/file"
	names := []string{"./" + keyMember, "./etc/hostname", "./etc/ssh/sshd_config", "./usr/bin/rc_audio_mixer", "./usr/bin/audio-start.sh", "./etc/unchanged", long}
	data := [][]byte{
		{},
		[]byte("rodecaster-pro-ii\n"),
		[]byte("PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n"),
		[]byte("mixer"),
		[]byte("launcher"),
		[]byte("retain me"),
		[]byte("deep"),
	}
	// Built with GNU long-name support for the last entry.
	var out bytes.Buffer
	gz := newGzip(&out)
	w := &tarWriter{W: gz}
	for i, name := range names {
		appendData(t, w, testHeader(t, "placeholder", typeRegular, len(data[i])), name, data[i])
	}
	must(t, w.Finish())
	must(t, gz.Close())
	put(t, src, out.Bytes())
	mix, launcher, err := patchArchive(src, dst, nil)
	must(t, err)
	if mix != common.SHA256Hex([]byte("mixer")) || launcher != common.SHA256Hex([]byte("launcher")) {
		t.Fatal("hashes")
	}
	i := 0
	must(t, archive.Walk(dst, 1<<20, false, func(entry *archive.Member) error {
		b, err := archive.ReadAll(entry)
		must(t, err)
		if !bytes.Equal(b, data[i]) {
			t.Fatalf("member %d: %q", i, b)
		}
		mode, _ := numeric(entry.Header[100:108])
		uid, _ := numeric(entry.Header[108:116])
		gid, _ := numeric(entry.Header[116:124])
		mtime, _ := numeric(entry.Header[136:148])
		if mode != 0o755 || uid != 4 || gid != 5 || mtime != 123 {
			t.Fatalf("member %d: %o %d %d %d", i, mode, uid, gid, mtime)
		}
		if archive.Normalized(entry.Path) != archive.Normalized(names[i]) {
			t.Fatalf("member %d: %s", i, entry.Path)
		}
		i++
		return nil
	}))
	if i != len(names) {
		t.Fatalf("%d members", i)
	}
	// A duplicate key member is refused.
	var items []tarItem
	for i := range 6 {
		items = append(items, tarItem{testHeader(t, names[i], typeRegular, len(data[i])), data[i]})
	}
	items = append(items, items[0])
	testTar(t, src, items)
	if _, _, err := patchArchive(src, filepath.Join(dir, "bad.gz"), nil); err == nil {
		t.Fatal("duplicate key member accepted")
	}
	// A non-empty authorized_keys is refused.
	items = items[:len(items)-1]
	items[0] = tarItem{testHeader(t, names[0], typeRegular, 3), []byte("key")}
	testTar(t, src, items)
	if _, _, err := patchArchive(src, filepath.Join(dir, "bad.gz"), nil); err == nil {
		t.Fatal("non-empty authorized_keys accepted")
	}
}

func TestActionLockIsExclusiveAndReleased(t *testing.T) {
	path := filepath.Join(t.TempDir(), "lock")
	a, err := openLocalFile(path, false)
	must(t, err)
	defer a.Close()
	b, err := openLocalFile(path, false)
	must(t, err)
	defer b.Close()
	unlock, err := lockAction(a)
	must(t, err)
	if _, err := lockAction(b); err == nil {
		t.Fatal("concurrent action acquired lock")
	}
	unlock()
	again, err := lockAction(b)
	must(t, err)
	again()
}

func TestReleasesAreOfflineAndOnlyOfferQualifiedCatalogEntries(t *testing.T) {
	catalog := realCatalog(t)
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; p.Model == firmware.ModelDuo.ID() && p.Version == "1.7.7" {
			p.Runtime = nil
		}
	}
	releases, err := releasesFromCatalog(firmware.ModelDuo, catalog)
	must(t, err)
	if len(releases) != 1 || releases[0].Version != "1.7.6" {
		t.Fatalf("%+v", releases)
	}
	x := newFixture(t)
	releases, err = x.engine.releases()
	must(t, err)
	if len(releases) != 1 || releases[0].Version != "1.7.7" {
		t.Fatalf("%+v", releases)
	}
	if x.engine.http != nil {
		t.Fatal("listing must not initialize a network client")
	}
	// Stock firmware is checked against RØDE's manifest instead.
	for _, c := range [][2]string{{"stable", "1.7.8"}, {"beta", "1.7.7"}} {
		_, err := x.engine.prepare(c[0], c[1], "", false)
		contains(t, err, "qualified catalog")
	}
	if x.engine.http != nil {
		t.Fatal("unknown releases must fail before network access")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
}

func TestCatalogURLAndReadinessGuards(t *testing.T) {
	r := sampleProfile().Runtime
	ready := `{"config":{"revision":42},"engine":{"generation":15,"restartAllowed":false,"mode":"native","status":"running","appliedRevision":42,"sampleRate":48000,"blockSize":112,"message":"Native audio paths applied and advancing","audioAdvancing":true}}`
	if why := readinessError([]byte(ready), r); why != "" {
		t.Fatal(why)
	}
	for _, c := range [][3]string{
		{`"appliedRevision":42`, `"appliedRevision":41`, "appliedRevision=41"},
		{`"audioAdvancing":true`, `"audioAdvancing":false`, "advancing=false"},
		{`"mode":"native"`, `"mode":"simulation"`, `mode="simulation"`},
		{`"blockSize":112`, `"blockSize":128`, "blockSize=128"},
		{`"sampleRate":48000`, `"sampleRate":44100`, "sampleRate=44100"},
		{`"revision":42`, `"other":42`, "configRevision=missing"},
	} {
		why := readinessError([]byte(strings.Replace(ready, c[0], c[1], 1)), r)
		if !strings.Contains(why, c[2]) {
			t.Fatalf("%s: %s", c[2], why)
		}
	}
	if !strings.Contains(readinessError([]byte("{"), r), "invalid state JSON") {
		t.Fatal("bad JSON")
	}
}

func realEnv(t *testing.T, name string) string {
	value := os.Getenv(name)
	if value == "" {
		t.Skipf("set %s", name)
	}
	return value
}

// Offline test requiring RC2FX_DUO_ARCHIVE and RC2FX_REAL_ROOT.
func TestRealDuoImagePreservesBoardFilesAndCarriesExplicitRuntimeOptIn(t *testing.T) {
	source := realEnv(t, "RC2FX_DUO_ARCHIVE")
	root := realEnv(t, "RC2FX_REAL_ROOT")
	model := firmware.ModelDuo
	inspected, err := InspectArchive(model, source)
	must(t, err)
	data, err := os.ReadFile(filepath.Join(root, "config/firmware-profiles.json"))
	must(t, err)
	var catalog Catalog
	must(t, sjson.Unmarshal(data, &catalog))
	var profile *Profile
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; p.Model == model.ID() && p.ID == inspected.Profile {
			profile = p
		}
	}
	if profile == nil {
		t.Fatal("no profile")
	}
	bundle, err := BundleFiles(root)
	must(t, err)
	entries, _, err := imageEntries(bundle, profile, nil)
	must(t, err)
	destination := filepath.Join(t.TempDir(), "duo.tar.gz")
	patched, err := patchArchiveForModel(model, source, destination, entries, nil)
	must(t, err)
	if patched.Mixer != profile.MixerSHA || patched.Launcher != profile.LauncherSHA {
		t.Fatal("hashes")
	}
	must(t, verifyArchiveModel(model, destination))
	if verifyArchiveModel(firmware.ModelProIi, destination) == nil {
		t.Fatal("a Duo image passed as Pro II")
	}
	info, err := os.Stat(destination)
	must(t, err)
	if info.Size() > 275<<20 {
		t.Fatal("too large")
	}
	expected := map[string]string{}
	for _, e := range entries {
		if e.Kind == archive.EntryFile {
			expected[e.Path] = common.SHA256Hex(e.Data)
		}
	}
	protected := func(archivePath string) (map[string]string, map[string]string) {
		hashes, added := map[string]string{}, map[string]string{}
		must(t, archive.Walk(archivePath, 4<<30, false, func(entry *archive.Member) error {
			name := archive.Normalized(entry.Path)
			if strings.HasPrefix(name, "boot/") || strings.HasPrefix(name, "Application/mcu-images/") || name == "etc/hostname" {
				contents, err := archive.ReadAll(entry)
				if err != nil {
					return err
				}
				hashes[name] = fmt.Sprintf("%x %s", entry.Header[:], common.SHA256Hex(contents))
			} else if _, ok := expected[name]; ok {
				contents, err := archive.ReadAll(entry)
				if err != nil {
					return err
				}
				if _, dup := added[name]; dup {
					t.Fatalf("%s twice", name)
				}
				added[name] = common.SHA256Hex(contents)
			}
			return nil
		}))
		return hashes, added
	}
	original, originalAdded := protected(source)
	patchedHashes, patchedAdded := protected(destination)
	if fmt.Sprint(original) != fmt.Sprint(patchedHashes) {
		t.Fatal("board/controller content and metadata must stay unchanged")
	}
	if len(originalAdded) != 0 || fmt.Sprint(patchedAdded) != fmt.Sprint(expected) {
		t.Fatal("all runtime files, including the opt-in marker, must actually be in the image")
	}
	if patchedAdded["opt/rc2fx/experimental-duo"] != common.SHA256Hex([]byte("1\n")) {
		t.Fatal("no opt-in marker")
	}
}

func TestRealImageBuildsWithinTheUpdateDrive(t *testing.T) {
	work := realEnv(t, "RC2FX_REAL_WORK")
	root := realEnv(t, "RC2FX_REAL_ROOT")
	serial := realEnv(t, "RC2FX_REAL_SERIAL")
	engine := WithPlatform(root, work, fakePlatform{mu: new(sync.Mutex), f: &fake{}})
	catalog, err := engine.catalog()
	must(t, err)
	var profile *Profile
	for i := range catalog.Profiles {
		if catalog.Profiles[i].Version == "1.7.7" {
			profile = &catalog.Profiles[i]
			break
		}
	}
	saved, err := LoadSaved(work)
	must(t, err)
	var settings []byte
	if b := saved.BackupFor(serial); b != nil {
		settings, err = backupSettings(work, b)
		must(t, err)
	}
	t.Logf("settings: %d bytes", len(settings))
	bundle, err := BundleFiles(root)
	must(t, err)
	entries, digest, err := imageEntries(bundle, profile, settings)
	must(t, err)
	out := filepath.Join(t.TempDir(), "archive.tar.gz")
	mix, launcher, err := patchArchive(filepath.Join(work, "rode-original.tar.gz"), out, entries)
	must(t, err)
	if mix != profile.MixerSHA || launcher != profile.LauncherSHA {
		t.Fatal("hashes")
	}
	info, err := os.Stat(out)
	must(t, err)
	t.Logf("image %d bytes, runtime %s, %d entries added", info.Size(), digest, len(entries))
}

func TestRealPayloadReleases(t *testing.T) {
	root := realEnv(t, "RC2FX_REAL_ROOT")
	engine := WithPlatform(root, t.TempDir(), fakePlatform{mu: new(sync.Mutex), f: &fake{}})
	releases, err := engine.releases()
	must(t, err)
	t.Logf("%+v", releases)
	found := false
	for _, r := range releases {
		found = found || r.Channel == "stable"
	}
	if !found {
		t.Fatal("no stable release")
	}
}

func stockImage(t *testing.T, path string) {
	testTar(t, path, patchableMembers(t, true))
}

func patchableMembers(t *testing.T, dotted bool) []tarItem {
	prefix := ""
	if dotted {
		prefix = "./"
	}
	data := []struct {
		name string
		data []byte
	}{
		{keyMember, nil},
		{"etc/hostname", []byte("rodecaster-pro-ii\n")},
		{"etc/ssh/sshd_config", []byte("PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n")},
		{"usr/bin/rc_audio_mixer", []byte("mixer")},
		{"usr/bin/audio-start.sh", []byte("launcher")},
	}
	var items []tarItem
	for _, d := range data {
		items = append(items, tarItem{testHeader(t, prefix+d.name, typeRegular, len(d.data)), d.data})
	}
	return items
}

type found struct {
	kind byte
	mode uint64
	data []byte
}

func TestAnRC2FXImageListsRodesFilesForTheMixerToCheck(t *testing.T) {
	x := newFixture(t)
	bundle, err := BundleFiles(x.root)
	must(t, err)
	profile := sampleProfile()
	entries, _, err := imageEntries(bundle, &profile, nil)
	must(t, err)
	dir := t.TempDir()
	src, dst := filepath.Join(dir, "stock.gz"), filepath.Join(dir, "rc2fx.gz")
	stockImage(t, src)
	manifest := func(archivePath string) *string {
		var found *string
		must(t, archive.Walk(archivePath, 1<<24, false, func(entry *archive.Member) error {
			if archive.Normalized(entry.Path) == manifestMember {
				if mode, _ := numeric(entry.Header[100:108]); mode != 0o600 {
					t.Fatalf("manifest mode %o", mode)
				}
				data, err := archive.ReadAll(entry)
				if err != nil {
					return err
				}
				text := string(data)
				found = &text
			}
			return nil
		}))
		return found
	}

	_, _, err = patchArchive(src, dst, entries)
	must(t, err)
	var want strings.Builder
	for _, m := range []struct {
		name string
		data []byte
	}{
		{"etc/hostname", []byte("rodecaster-pro-ii\n")},
		{"etc/ssh/sshd_config", []byte("PermitRootLogin yes\nAuthorizedKeysFile .ssh/authorized_keys\n")},
		{"usr/bin/rc_audio_mixer", []byte("mixer")},
		{"usr/bin/audio-start.sh", []byte("launcher")},
	} {
		fmt.Fprintf(&want, "%s  %s\n", common.SHA256Hex(m.data), m.name)
	}
	// RØDE files in archive order; excludes authorized_keys and RC2FX's own files.
	if got := manifest(dst); got == nil || *got != want.String() {
		t.Fatalf("manifest %v, want %q", got, want.String())
	}

	// A stock image has no launcher to read it.
	_, _, err = patchArchive(src, dst, nil)
	must(t, err)
	if got := manifest(dst); got != nil {
		t.Fatalf("a stock image got a manifest: %q", *got)
	}
}

func TestImageGetsRC2FXAutostartAndSettingsAndNothingIsReplaced(t *testing.T) {
	x := newFixture(t)
	bundle, err := BundleFiles(x.root)
	must(t, err)
	settings := []byte(`{"config":{"revision":3,"targets":[{"id":"analog-1"}]}}`)
	profile := sampleProfile()
	entries, digest, err := imageEntries(bundle, &profile, settings)
	must(t, err)
	dir := t.TempDir()
	src, dst := filepath.Join(dir, "stock.gz"), filepath.Join(dir, "rc2fx.gz")
	stockImage(t, src)
	_, _, err = patchArchive(src, dst, entries)
	must(t, err)
	seen := map[string]found{}
	must(t, archive.Walk(dst, 1<<24, false, func(entry *archive.Member) error {
		name := archive.Normalized(entry.Path)
		mode, _ := numeric(entry.Header[100:108])
		uid, _ := numeric(entry.Header[108:116])
		gid, _ := numeric(entry.Header[116:124])
		data, err := archive.ReadAll(entry)
		must(t, err)
		if strings.HasPrefix(name, "opt") || name == "usr/sbin/rc_audio_mixer" || name == "usr/sbin/rc_gui_juce" {
			if uid != 0 || gid != 0 {
				t.Fatalf("%s owned by %d:%d", name, uid, gid)
			}
		}
		seen[name] = found{entry.Header.EntryType(), mode, data}
		return nil
	}))
	script := []byte("#!/bin/sh\nexit 0\n")
	checks := []struct {
		name string
		mode uint64
		data []byte
	}{
		{"opt", 0o755, nil}, // the stock image has no /opt
		{"opt/rc2fx/rc2fxd", 0o700, nil},
		{"opt/rc2fx/rc2fx-supervisor", 0o700, nil},
		{"opt/rc2fx/firmware-profiles.json", 0o600, nil},
		// The installed release identity the device reports as its version.
		{"opt/rc2fx/mixer-release.json", 0o600, readConfig(t, "mixer-release.json")},
		{"opt/rc2fx/install.sh", 0, script},
		{"opt/rc2fx/bundle.sha256", 0, []byte(digest + "\n")},
		{"opt/rc2fx/data/state.json", 0, settings},
		{"usr/sbin/rc_audio_mixer", 0o755, script},
		{"usr/sbin/rc_gui_juce", 0o755, script},
		{"opt/rc2fx/librc2fx_gui.so", 0o700, nil},
		{"opt/rc2fx/librc2fx_jackerr.so", 0o700, bundle["librc2fx_jackerr.so"]},
		{"opt/rc2fx/rc2fx_jackerr_snapshot", 0o700, bundle["rc2fx_jackerr_snapshot"]},
		{"opt/rc2fx/rc2fx-gui-shim.sh", 0o700, nil},
		{"usr/bin/rc_audio_mixer", 0, []byte("mixer")},
	}
	for _, c := range checks {
		got, ok := seen[c.name]
		if !ok {
			t.Fatalf("%s missing", c.name)
		}
		if c.mode != 0 && got.mode != c.mode {
			t.Fatalf("%s mode %o", c.name, got.mode)
		}
		if c.data != nil && !bytes.Equal(got.data, c.data) {
			t.Fatalf("%s: %q", c.name, got.data)
		}
	}
	if seen["opt/rc2fx/logs"].kind != '5' {
		t.Fatal("logs is not a directory")
	}
	if len(seen[keyMember].data) != 0 {
		t.Fatal("authorized_keys stays as shipped")
	}

	// Without settings, no settings file is added.
	bare, same, err := imageEntries(bundle, &profile, nil)
	must(t, err)
	if same != digest {
		t.Fatal("the runtime digest does not depend on settings")
	}
	for _, e := range bare {
		if e.Path == "opt/rc2fx/data/state.json" {
			t.Fatal("settings without settings")
		}
	}

	// An image that already has RC2FX paths is refused rather than merged.
	taken := []tarItem{{testHeader(t, "./opt/rc2fx/rc2fxd", typeRegular, 1), []byte("x")}}
	must(t, archive.Walk(src, 1<<20, false, func(entry *archive.Member) error {
		data, err := archive.ReadAll(entry)
		must(t, err)
		h := entry.Header
		taken = append(taken, tarItem{&h, data})
		return nil
	}))
	testTar(t, src, taken)
	if _, _, err := patchArchive(src, filepath.Join(dir, "bad.gz"), entries); err == nil {
		t.Fatal("merged into an image with RC2FX paths")
	}
}

func TestImageEntriesRefuseARuntimeThatDoesNotSupportTheFirmware(t *testing.T) {
	x := newFixture(t)
	bundle, err := BundleFiles(x.root)
	must(t, err)
	other := sampleProfile()
	other.MixerSHA = strings.Repeat("c", 64)
	if _, _, err := imageEntries(bundle, &other, nil); err == nil {
		t.Fatal("unsupported firmware accepted")
	}
	profile := sampleProfile()
	entries, _, err := imageEntries(bundle, &profile, nil)
	must(t, err)
	has := func(path string, mode uint32) bool {
		for _, e := range entries {
			if e.Kind == archive.EntryFile && e.Path == path && (mode == 0 || e.Mode == mode) {
				return true
			}
		}
		return false
	}
	if !has("usr/sbin/rc_audio_mixer", 0) || !has("usr/sbin/rc_gui_juce", 0o755) ||
		!has("opt/rc2fx/librc2fx_jackerr.so", 0o700) || !has("opt/rc2fx/rc2fx_jackerr_snapshot", 0o700) {
		t.Fatalf("%+v", entries)
	}
}

func TestRecorderPayloadsAreRequiredAndMustBeARM64ELFs(t *testing.T) {
	x := newFixture(t)
	names := []string{"librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"}
	catalog, err := os.ReadFile(filepath.Join(x.root, "config", "firmware-profiles.json"))
	must(t, err)
	bundle, err := BundleFiles(x.root)
	must(t, err)
	for _, name := range names {
		if _, ok := bundle[name]; !ok {
			t.Fatal(name)
		}
	}
	for _, name := range names {
		must(t, os.Remove(filepath.Join(x.root, "runtime", name)))
		_, err := BundleFiles(x.root)
		contains(t, err, name)
		writeRuntime(t, x.root, catalog)
	}
	for _, name := range names {
		path := filepath.Join(x.root, "runtime", name)
		good, err := os.ReadFile(path)
		must(t, err)
		corrupt := []byte("not an ARM64 ELF")
		must(t, os.WriteFile(path, corrupt, 0o644))
		sumsPath := filepath.Join(x.root, "runtime", "SHA256SUMS")
		sums, err := os.ReadFile(sumsPath)
		must(t, err)
		old := common.SHA256Hex(good) + "  " + name + "\n"
		replaced := common.SHA256Hex(corrupt) + "  " + name + "\n"
		if !strings.Contains(string(sums), old) {
			t.Fatalf("%s manifest line", name)
		}
		must(t, os.WriteFile(sumsPath, []byte(strings.Replace(string(sums), old, replaced, 1)), 0o644))
		_, err = BundleFiles(x.root)
		contains(t, err, "is not ARM64 ELF")
		writeRuntime(t, x.root, catalog)
	}
}

func TestSettingsReadOverUSBAreKeptPerMixerAndUsedForTheImage(t *testing.T) {
	x := newFixture(t)
	body := `{"config":{"revision":7},"presets":[]}`
	saved, err := x.engine.saveSettings(serial, body)
	must(t, err)
	if saved.SHA != common.SHA256Hex([]byte(body)) {
		t.Fatal("sha")
	}
	data, err := backupSettings(x.engine.Work, x.saved().BackupFor(serial))
	must(t, err)
	if string(data) != body {
		t.Fatal(string(data))
	}
	if x.saved().BackupFor("another-mixer") != nil || x.saved().BackupFor("") != nil {
		t.Fatal("an unknown mixer gets no settings")
	}
	// A changed file is refused.
	must(t, os.WriteFile(saved.File, []byte("{}"), 0o600))
	if _, err := backupSettings(x.engine.Work, saved); err == nil {
		t.Fatal("a changed backup was used")
	}
	// Without a mixer serial, or with something that is not settings,
	// nothing is kept.
	if _, err := x.engine.saveSettings("", body); err == nil {
		t.Fatal("kept without a serial")
	}
	if _, err := x.engine.saveSettings(serial, "[]"); err == nil {
		t.Fatal("kept something that is not settings")
	}
}

func TestServiceFollowsTheStateTheAppReadsOverUSB(t *testing.T) {
	x := newFixture(t)
	if ServiceFromState(x.root, []byte("not json")).Kind != ServiceStarting {
		t.Fatal("bad JSON is ready")
	}
	if ServiceFromState(x.root, []byte(`{"engine":{"mode":"native","status":"starting"}}`)).Kind != ServiceStarting {
		t.Fatal("starting is ready")
	}
}

func TestPatchingRefusesOversizedMembersBeforeReadingThem(t *testing.T) {
	dir := t.TempDir()
	src, dst := filepath.Join(dir, "src.tar.gz"), filepath.Join(dir, "dst.tar.gz")
	testTar(t, src, patchableMembers(t, false))
	_, _, err := patchArchive(src, dst, nil)
	must(t, err)
	small := archive.MaxSmallMember + 1
	launcher := func(size int) []tarItem {
		members := patchableMembers(t, false)
		members[4] = tarItem{testHeader(t, "usr/bin/audio-start.sh", typeRegular, size), bytes.Repeat([]byte("x"), size)}
		return members
	}
	testTar(t, src, launcher(small-1))
	_, _, err = patchArchive(src, dst, nil)
	must(t, err)
	testTar(t, src, launcher(small))
	_, _, err = patchArchive(src, dst, nil)
	contains(t, err, "oversized")

	members := patchableMembers(t, false)
	size := archive.MaxMixerMember + 1
	members[3] = tarItem{testHeader(t, "usr/bin/rc_audio_mixer", typeRegular, size), make([]byte, size)}
	testTar(t, src, members)
	_, _, err = patchArchive(src, dst, nil)
	contains(t, err, "oversized")

	for _, kind := range []byte{typeLongName, typePaxLocal} {
		members := patchableMembers(t, false)
		members = append([]tarItem{{testHeader(t, "meta", kind, small), bytes.Repeat([]byte("x"), small)}}, members...)
		testTar(t, src, members)
		_, _, err := patchArchive(src, dst, nil)
		contains(t, err, "oversized")
	}
}

func TestPatchingHashesWhatItReadAndWroteNotWhatThePathsHold(t *testing.T) {
	dir := t.TempDir()
	src, dst := filepath.Join(dir, "src.tar.gz"), filepath.Join(dir, "dst.tar.gz")
	testTar(t, src, patchableMembers(t, false))
	sha, err := archive.FileHash(src, archive.SHA256)
	must(t, err)
	patch := func(want string) (*archive.Patched, error) {
		return patchArchiveForModel(firmware.ModelProIi, src, dst, nil, &want)
	}
	p, err := patch(sha)
	must(t, err)
	gotSha, err := archive.FileHash(dst, archive.SHA256)
	must(t, err)
	gotMd5, err := archive.FileHash(dst, archive.MD5)
	must(t, err)
	if p.ArchiveSHA != gotSha || p.ArchiveMD5 != gotMd5 {
		t.Fatal("hashes are not of what was written")
	}
	// The source was checked by path earlier; the bytes read differ now.
	must(t, os.Remove(dst))
	if _, err := patch(strings.Repeat("0", 64)); err == nil {
		t.Fatal("a swapped source produced an image")
	}
	if common.PathExists(dst) {
		t.Fatal("a swapped source must not produce an image")
	}
}
