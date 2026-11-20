package installer

import (
	"bytes"
	"compress/gzip"
	"errors"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

const volumeTotal = 283_189_248

// The Pro II's USB modes, from the official `uac2_gadget.sh`.
var (
	updatePID  = firmware.ModelProIi.UpdatePID()
	normalPIDs = firmware.ModelProIi.NormalPIDs()
)

const repoConfig = "../../config"

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func contains(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), want) {
		t.Fatalf("error %v does not contain %q", err, want)
	}
}

func put(t *testing.T, path string, data []byte) {
	t.Helper()
	must(t, os.MkdirAll(filepath.Dir(path), 0o755))
	must(t, os.WriteFile(path, data, 0o644))
}

func readConfig(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(repoConfig, name))
	must(t, err)
	return data
}

func testHeader(t *testing.T, name string, kind byte, size int) *tarHeader {
	t.Helper()
	var h tarHeader
	if cleaned, err := archivePath(name); err == nil {
		name = cleaned
	}
	if len(name) > 100 {
		t.Fatalf("name too long: %s", name)
	}
	copy(h[0:100], name)
	octal(h[100:108], 0o755)
	octal(h[108:116], 4)
	octal(h[116:124], 5)
	octal(h[124:136], uint64(size))
	octal(h[136:148], 123)
	h[156] = kind
	copy(h[257:263], "ustar ")
	copy(h[263:265], " \x00")
	setChecksum(&h)
	return &h
}

func setChecksum(h *tarHeader) {
	for i := 148; i < 156; i++ {
		h[i] = ' '
	}
	var total uint64
	for _, b := range h {
		total += uint64(b)
	}
	for i := 148; i < 156; i++ {
		h[i] = 0
	}
	octal(h[148:156], total)
}

func newGzip(w io.Writer) *gzip.Writer { return gzip.NewWriter(w) }

type tarItem struct {
	header *tarHeader
	data   []byte
}

func testTar(t *testing.T, path string, items []tarItem) {
	t.Helper()
	var out bytes.Buffer
	gz := gzip.NewWriter(&out)
	w := &tarWriter{W: gz}
	for _, item := range items {
		must(t, w.Append(item.header, bytes.NewReader(item.data)))
	}
	must(t, w.Finish())
	must(t, gz.Close())
	put(t, path, out.Bytes())
}

// Fill in fields omitted from Profile by cloning a matching entry from the
// real catalog so strict parsing succeeds.
func catalogJSON(t *testing.T, profiles []Profile) []byte {
	t.Helper()
	real, err := sjson.Value(readConfig(t, "firmware-profiles.json"))
	must(t, err)
	realProfiles := real.(map[string]any)["profiles"].([]any)
	var full []any
	for _, p := range profiles {
		var base map[string]any
		for _, r := range realProfiles {
			if r.(map[string]any)["model"] == p.Model {
				base = r.(map[string]any)
				break
			}
		}
		if base == nil {
			t.Fatalf("no real profile for %s", p.Model)
		}
		item := map[string]any{}
		for k, v := range base {
			item[k] = v
		}
		runtime := item["runtime"]
		leanData, err := sjson.Marshal(p)
		must(t, err)
		lean, err := sjson.Value(leanData)
		must(t, err)
		for k, v := range lean.(map[string]any) {
			if k != "runtime" {
				item[k] = v
			}
		}
		if p.Runtime != nil {
			item["runtime"] = runtime
		} else {
			item["runtime"] = nil
		}
		full = append(full, item)
	}
	data, err := sjson.Marshal(map[string]any{"schemaVersion": 1, "profiles": full})
	must(t, err)
	return data
}

func sampleProfile() Profile {
	return Profile{
		MinimumAppVersion: "0.1.1",
		ID:                "fixture",
		Model:             "rodecaster-pro-ii",
		Version:           "1.7.7",
		MixerSHA:          strings.Repeat("a", 64),
		LauncherSHA:       strings.Repeat("b", 64),
		ArchiveSHA:        strings.Repeat("c", 64),
		ArchiveMD5:        strings.Repeat("d", 32),
		Runtime: &RuntimeProfile{
			CallbackABI: "jack-process-v1",
			RouteLayout: "pro-ii-25-v1",
			SourceEnum:  "pro-ii-sources-v1",
			SampleRate:  48000,
			BlockFrames: 112,
			JobLifetime: JobLifetime{ABI: "capture-values-v1"},
		},
	}
}

type fake struct {
	items       []HID
	volumes     []Volume
	volumeError *string
	present     bool
	opens       int
	writes      int
	ejects      int
	ack         []byte
	// written is the bytes the OS reports written; nil means the full
	// report.
	written    *int
	writeError bool
	ejectError bool
	logAtWrite [][]CommandRecord
	reports    [][]byte
	work       string
}

type fakePlatform struct {
	mu *sync.Mutex
	f  *fake
}

type fakeReports struct{ p fakePlatform }

func (r *fakeReports) Write(report []byte, _ time.Duration) (int, error) {
	r.p.mu.Lock()
	defer r.p.mu.Unlock()
	f := r.p.f
	f.writes++
	f.reports = append(f.reports, slices.Clone(report))
	saved, err := LoadSaved(f.work)
	if err != nil {
		return 0, err
	}
	f.logAtWrite = append(f.logAtWrite, slices.Clone(saved.Commands))
	if f.writeError {
		return 0, Error("device disconnected after write")
	}
	if f.written != nil {
		return *f.written, nil
	}
	return len(report), nil
}

func (r *fakeReports) Read(buf []byte, _ time.Duration) (int, error) {
	r.p.mu.Lock()
	defer r.p.mu.Unlock()
	if len(r.p.f.ack) == 0 {
		return 0, Error("timed out")
	}
	return copy(buf, r.p.f.ack), nil
}

func (r *fakeReports) Close() error { return nil }

func (p fakePlatform) Inventory() ([]HID, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return slices.Clone(p.f.items), nil
}

func (p fakePlatform) Open(*HID) (Reports, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.f.opens++
	return &fakeReports{p}, nil
}

func (p fakePlatform) OpenControl(h *HID) (Reports, error) { return p.Open(h) }

func (p fakePlatform) Volumes() ([]Volume, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.f.volumeError != nil {
		return nil, Error(*p.f.volumeError)
	}
	return slices.Clone(p.f.volumes), nil
}

func (p fakePlatform) Mount(v Volume) (Volume, error) { return v, nil }

func (p fakePlatform) MediaPresent(*Volume) (bool, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.f.present, nil
}

func (p fakePlatform) Eject(*Volume) error {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.f.ejects++
	if p.f.ejectError {
		return Error("busy")
	}
	p.f.present = false
	return nil
}

type fixture struct {
	t        *testing.T
	root     string
	engine   *Engine
	prepared Prepared
	p        fakePlatform
}

const serial = "fixture-device"

func (x *fixture) with(f func(*fake)) {
	x.p.mu.Lock()
	defer x.p.mu.Unlock()
	f(x.p.f)
}

func (x *fixture) counts() [3]int {
	var out [3]int
	x.with(func(f *fake) { out = [3]int{f.opens, f.writes, f.ejects} })
	return out
}

func (x *fixture) saved() *Saved {
	saved, err := LoadSaved(x.engine.Work)
	must(x.t, err)
	return saved
}

func (x *fixture) updateMode() {
	x.with(func(f *fake) { f.items[0].PID = updatePID })
}

func (x *fixture) drive(serial string) string {
	usb := filepath.Join(x.root, "usb")
	must(x.t, os.MkdirAll(usb, 0o755))
	x.with(func(f *fake) {
		f.volumes = []Volume{{
			Path:   usb,
			Device: "fixture-disk",
			Serial: serial,
			FS:     "FAT",
			Total:  volumeTotal,
			Free:   200 << 20,
		}}
		f.present = true
	})
	return usb
}

func elf(marker byte, size int) []byte {
	b := bytes.Repeat([]byte{marker}, size)
	copy(b, []byte{127, 'E', 'L', 'F', 2, 1})
	b[18] = 0xb7
	b[19] = 0
	return b
}

func writeRuntime(t *testing.T, root string, catalog []byte) {
	bundle := map[string][]byte{"firmware-profiles.json": catalog}
	release := readConfig(t, "mixer-release.json")
	bundle["mixer-release.json"] = release
	put(t, filepath.Join(root, "config", "mixer-release.json"), release)
	for _, n := range []string{"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware", "librc2fx_gui.so", "librc2fx_jackerr.so", "rc2fx_jackerr_snapshot"} {
		bundle[n] = elf(0, 64)
	}
	var manifest strings.Builder
	for _, n := range []string{"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware", "firmware-profiles.json", "librc2fx_gui.so", "librc2fx_jackerr.so", "rc2fx_jackerr_snapshot", "mixer-release.json"} {
		manifest.WriteString(archive.Sum(bundle[n]) + "  " + n + "\n")
		put(t, filepath.Join(root, "runtime", n), bundle[n])
	}
	put(t, filepath.Join(root, "runtime", "SHA256SUMS"), []byte(manifest.String()))
	for _, n := range []string{"rc2fx.sh", "rc2fx-shim.sh", "rc2fx-gui-shim.sh", "install.sh"} {
		// The launcher says what kind of build it is; the payload checks that.
		data := []byte("#!/bin/sh\r\nexit 0\r\n")
		if n == "rc2fx.sh" {
			data = []byte("#!/bin/sh\r\nrc2fx_build=development\r\nexit 0\r\n")
		}
		put(t, filepath.Join(root, "mixer", "scripts", n), data)
	}
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	root, err := filepath.Abs(t.TempDir())
	must(t, err)
	work := filepath.Join(root, "work")
	must(t, os.MkdirAll(work, 0o755))
	catalog := catalogJSON(t, []Profile{sampleProfile()})
	put(t, filepath.Join(root, "config", "firmware-profiles.json"), catalog)
	writeRuntime(t, root, catalog)
	data := []byte("fixture firmware")
	put(t, filepath.Join(work, "archive.tar.gz"), data)
	md, err := archive.FileHash(filepath.Join(work, "archive.tar.gz"), archive.MD5)
	must(t, err)
	put(t, filepath.Join(work, "archive.md5"), []byte(md))
	bundle, err := BundleFiles(root)
	must(t, err)
	profile := sampleProfile()
	_, runtime, err := imageEntries(bundle, &profile, nil)
	must(t, err)
	prepared := Prepared{
		Model:      nil, // legacy Pro II cache
		Version:    "1.7.7",
		Channel:    "beta",
		Profile:    "fixture",
		ArchiveSHA: archive.Sum(data),
		ArchiveMD5: md,
		Runtime:    runtime,
	}
	p := fakePlatform{mu: new(sync.Mutex), f: &fake{
		items: []HID{{
			Path:    "fake",
			Serial:  serial,
			PID:     normalPIDs[0],
			Usage:   0xff00,
			Input:   256,
			Output:  256,
			Control: false,
			Release: 0x0177,
		}},
		ack:  []byte{2, 'A'},
		work: work,
	}}
	engine := WithPlatform(root, work, p)
	must(t, engine.save(&Saved{Prepared: prepared.clone()}))
	return &fixture{t: t, root: root, engine: engine, prepared: prepared, p: p}
}

func TestFailedBundledSelectionDiscardsPreviousSource(t *testing.T) {
	// The installer test executable has no embedded payload. Start with a
	// local source and ensure a missing bundled choice cannot retain it.
	work, root := t.TempDir(), t.TempDir()
	engine := WithPlatform(root, work, fakePlatform{mu: new(sync.Mutex), f: &fake{}})
	if engine.source.Kind == SourceDownload {
		t.Fatal("started as download")
	}
	if _, err := engine.Handle(Request{Kind: RequestSelectBundledMixer}); err == nil {
		t.Fatal("selected a missing bundled payload")
	}
	if engine.source.Kind != SourceDownload {
		t.Fatal("the previous source was kept")
	}
}

func TestUSBModeIsLoggedBeforeTheSingleWrite(t *testing.T) {
	x := newFixture(t)
	must(t, x.engine.enterUSBMode(""))
	x.with(func(f *fake) {
		if f.writes != 1 || !bytes.Equal(f.reports[0][:2], []byte{1, 'M'}) || len(f.reports[0]) != 256 {
			t.Fatalf("writes %d, report %v", f.writes, f.reports)
		}
		log := f.logAtWrite[0]
		if len(log) != 1 || log[0].Command != "usb-mode" || log[0].Acknowledged || log[0].Serial != serial {
			t.Fatalf("logged before writing: %+v", log)
		}
	})
	commands := x.saved().Commands
	c := commands[len(commands)-1]
	if !c.Acknowledged || c.Error != "" {
		t.Fatalf("%+v", c)
	}
}

func TestPaddedWindowsWriteIsAcceptedButShortWriteAndMissingAckAreNot(t *testing.T) {
	// Regression for the 2026-10-24 incident: Windows padded a 64-byte
	// report and returned 256.
	x := newFixture(t)
	padded := 256
	x.with(func(f *fake) { f.items[0].Output = 64; f.written = &padded })
	must(t, x.engine.enterUSBMode(""))
	short := 63
	for _, c := range []struct {
		written *int
		ack     []byte
		err     bool
	}{
		{&short, []byte{2, 'A'}, false},
		{nil, nil, false},
		{nil, []byte{2, 'N'}, false},
		{nil, []byte{2, 'A'}, true},
	} {
		x := newFixture(t)
		x.with(func(f *fake) {
			f.items[0].Output = 64
			f.written, f.ack, f.writeError = c.written, c.ack, c.err
		})
		if x.engine.enterUSBMode("") == nil {
			t.Fatalf("%+v accepted", c)
		}
		if x.counts()[1] != 1 {
			t.Fatal("retried")
		}
		commands := x.saved().Commands
		last := commands[len(commands)-1]
		if last.Acknowledged || last.Error == "" {
			t.Fatalf("outcome not recorded as unknown: %+v", last)
		}
	}
}

func TestDamagedPreparationBlocksUSBBeforeOpeningTheDevice(t *testing.T) {
	for _, damage := range []string{"firmware", "md5", "runtime", "profile", "key", "host-key"} {
		x := newFixture(t)
		saved := x.saved()
		p := saved.Prepared
		switch damage {
		// What an older installer's SSH option left in the cache.
		case "key":
			p.Key = "/old/key"
		case "host-key":
			p.HostKey = "abc"
		case "firmware":
			put(t, filepath.Join(x.engine.Work, "archive.tar.gz"), []byte("changed"))
		case "md5":
			put(t, filepath.Join(x.engine.Work, "archive.md5"), []byte("changed"))
		case "runtime":
			put(t, filepath.Join(x.root, "runtime", "rc2fxd"), []byte("changed"))
		default:
			p.Profile = "unknown"
		}
		must(t, x.engine.save(saved))
		if x.engine.enterUSBMode("") == nil {
			t.Fatalf("%s accepted", damage)
		}
		if x.counts() != [3]int{} {
			t.Fatalf("%s: device accessed", damage)
		}
	}
	x := newFixture(t)
	must(t, x.engine.save(&Saved{}))
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("unprepared")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
}

func TestTheHelperRefusesAnImageRewrittenTogetherWithItsCacheRecord(t *testing.T) {
	anchors := t.TempDir()
	x := newFixture(t)
	x.engine.anchorDir = &anchors
	x.updateMode()
	usb := x.drive(serial)

	// Nothing recorded: a cache the helper did not build is not trusted.
	if x.engine.flash("") == nil {
		t.Fatal("unrecorded image accepted")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}

	// Simulate a same-user process rewriting archive + cache metadata together.
	must(t, x.engine.anchorPrepared(&x.prepared))
	forged := []byte("forged firmware")
	work := x.engine.Work
	put(t, filepath.Join(work, "archive.tar.gz"), forged)
	md, err := archive.FileHash(filepath.Join(work, "archive.tar.gz"), archive.MD5)
	must(t, err)
	put(t, filepath.Join(work, "archive.md5"), []byte(md))
	saved := x.saved()
	saved.Prepared.ArchiveSHA = archive.Sum(forged)
	saved.Prepared.ArchiveMD5 = md
	must(t, x.engine.save(saved))
	if x.engine.flash("") == nil {
		t.Fatal("consistent rewrite accepted")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
	if exists(filepath.Join(usb, "archive.tar.gz")) {
		t.Fatal("forged image staged")
	}

	// The same check still lets through what the helper recorded.
	must(t, x.engine.anchorPrepared(saved.Prepared))
	must(t, x.engine.flash(""))
	if x.counts() != [3]int{1, 1, 1} {
		t.Fatalf("counts %v", x.counts())
	}
}

func TestOnlyTheHelperThatRecordsAnImageCanFlashIt(t *testing.T) {
	anchors := t.TempDir()
	x := newFixture(t)

	// Unprivileged desktop process skips anchoring; only the helper catches it.
	desktop := func() *Engine { return WithPlatform(x.root, x.engine.Work, x.p) }
	must(t, desktop().anchorPrepared(&x.prepared))
	must(t, desktop().verifyPrepared(&x.prepared))

	helper := func() *Engine {
		e := desktop()
		e.anchorDir = &anchors
		return e
	}
	contains(t, helper().verifyPrepared(&x.prepared), "not prepared by the installer helper")

	// A helper's record outlives it, so a later one flashes the same image.
	must(t, helper().anchorPrepared(&x.prepared))
	must(t, helper().verifyPrepared(&x.prepared))
}

func TestAHelperStartedBeforeTheMixerAppearedPreparesForThatMixer(t *testing.T) {
	x := newFixture(t)
	x.with(func(f *fake) { f.items[0].PID = firmware.ModelDuo.NormalPIDs()[0] })
	for _, stock := range []bool{false, true} {
		_, err := x.engine.Handle(Request{Kind: RequestPrepare, Channel: "beta", Version: "1.7.7", Stock: stock})
		contains(t, err, "experimental")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
}

func TestACatalogWithoutTheFullPinsIsRefusedFromEverySource(t *testing.T) {
	x := newFixture(t)
	catalog := catalogJSON(t, []Profile{sampleProfile()})
	if _, err := parseCatalog(catalog); err != nil {
		t.Fatal(err)
	}
	value, err := sjson.Value(catalog)
	must(t, err)
	delete(value.(map[string]any)["profiles"].([]any)[0].(map[string]any), "archiveSha256")
	catalog, err = sjson.Marshal(value)
	must(t, err)
	if _, err := parseCatalog(catalog); err == nil {
		t.Fatal("a catalog without archiveSha256 was accepted")
	}

	// The same file through a folder, as an embedded payload is read.
	put(t, filepath.Join(x.root, "config/firmware-profiles.json"), catalog)
	if _, err := x.engine.catalog(); err == nil {
		t.Fatal("the folder's catalog was accepted")
	}
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("USB mode with a bad catalog")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}

	lean, err := sjson.Marshal(Catalog{Schema: 1, Profiles: []Profile{sampleProfile()}})
	must(t, err)
	if _, err := parseCatalog(lean); err == nil {
		t.Fatal("lean form accepted")
	}
}

func TestADownloadChecksRodesMD5AndAnySHA256TheCatalogPins(t *testing.T) {
	x := newFixture(t)
	dest := filepath.Join(x.engine.Work, "cached.tar.gz")
	put(t, dest, []byte("cached firmware"))
	md, err := archive.FileHash(dest, archive.MD5)
	must(t, err)
	url := "https://update.rode.com/rodecaster-pro-ii/1.7.7/os.tar.gz"
	empty := ""
	contains(t, x.engine.download(url, dest, &empty, md), "firmware checksums")
	contains(t, x.engine.download(url, dest, nil, ""), "firmware checksums")
	sha, err := archive.FileHash(dest, archive.SHA256)
	must(t, err)
	must(t, x.engine.download(url, dest, &sha, md))
	// Stock firmware RØDE lists without a catalog pin: RØDE's MD5 alone.
	must(t, x.engine.download(url, dest, nil, md))
	if x.engine.http != nil {
		t.Fatal("a matching file is kept, without a request")
	}
}

// rodeManifest as published, with an S3 testing build, a private beta and a
// version listed on several channels.
const rodeManifestText = `{
  "name" : "rcp2-update-manifest",
  "master-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "update.rode.com/rcp2/1.7.7/app.zip", "os-URL": "update.rode.com/rcp2/1.7.7/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.7/app.md5", "os-md5": "update.rode.com/rcp2/1.7.7/os.md5"},
  "main-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "update.rode.com/rcp2/1.7.7/app.zip", "os-URL": "update.rode.com/rcp2/1.7.7/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.7/app.md5", "os-md5": "update.rode.com/rcp2/1.7.7/os.md5"},
  "beta-version": {"app-version": "1.7.8", "os-version": "1.7.8", "os-required": "1.7.8",
    "app-URL": "update.rode.com/rcp2/1.7.8/app.zip", "os-URL": "update.rode.com/rcp2/1.7.8/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.8/app.md5", "os-md5": "update.rode.com/rcp2/1.7.8/os.md5"},
  "testing-version": {"app-version": "2.0.0", "os-version": "2.0.0", "os-required": "2.0.0",
    "app-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/app.zip",
    "os-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/os.tar.gz",
    "app-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/app.md5",
    "os-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2/testing/os.md5"},
  "privatebeta-version": {"app-version": "1.7.6", "os-version": "1.7.6", "os-required": "1.7.6",
    "app-URL": "update.rode.com/rcp2/1.7.6/app.zip", "os-URL": "update.rode.com/rcp2/1.7.6/os.tar.gz",
    "app-md5": "update.rode.com/rcp2/1.7.6/app.md5", "os-md5": "update.rode.com/rcp2/1.7.6/os.md5"},
  "futurechannel-version": {"os-version": "9.9.9"}
}`

func TestStockReleasesAreWhatRodesManifestListsOnItsOwnServer(t *testing.T) {
	releases, err := releasesFromManifest(firmware.ModelProIi, []byte(rodeManifestText))
	must(t, err)
	var listed []string
	for _, r := range releases {
		listed = append(listed, r.Version+" "+r.Channel)
	}
	// Newest first, stable and beta only: master, private beta, the S3
	// testing build and an unknown channel are left out.
	if !slices.Equal(listed, []string{"1.7.8 beta", "1.7.7 stable"}) {
		t.Fatalf("%v", listed)
	}
	r := releases[1]
	if r.URL != "https://update.rode.com/rcp2/1.7.7/os.tar.gz" || r.DigestURL != "https://update.rode.com/rcp2/1.7.7/os.md5" {
		t.Fatalf("%+v", r)
	}
	if !r.Supported || r.Profile != "" {
		t.Fatalf("%+v", r)
	}
	// Another model's files never count: the Pro II's list offers a Duo
	// nothing.
	duo, err := releasesFromManifest(firmware.ModelDuo, []byte(rodeManifestText))
	must(t, err)
	if len(duo) != 0 {
		t.Fatalf("%v", duo)
	}
	if _, err := releasesFromManifest(firmware.ModelProIi, []byte("<html>")); err == nil {
		t.Fatal("HTML accepted")
	}
}

// rodeDuoManifest as published: the same layout, naming `rcp2-duo/`.
const rodeDuoManifestText = `{
  "name": "rcp2-update-manifest",
  "main-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "", "os-URL": "update.rode.com/rcp2-duo/1.7.7/os.tar.gz",
    "app-md5": "", "os-md5": "update.rode.com/rcp2-duo/1.7.7/os.md5"},
  "beta-version": {"app-version": "1.7.7", "os-version": "1.7.7", "os-required": "1.7.7",
    "app-URL": "", "os-URL": "update.rode.com/rcp2-duo/1.7.7/os.tar.gz",
    "app-md5": "", "os-md5": "update.rode.com/rcp2-duo/1.7.7/os.md5"},
  "privatebeta-version": {"app-version": "1.7.6", "os-version": "1.7.6", "os-required": "1.7.6",
    "app-URL": "update.rode.com/rcp2-duo/1.7.6/app.zip", "os-URL": "update.rode.com/rcp2-duo/1.7.6/os.tar.gz",
    "app-md5": "update.rode.com/rcp2-duo/1.7.6/app.md5", "os-md5": "update.rode.com/rcp2-duo/1.7.6/os.md5"},
  "sp-version": {"app-version": "2.0.0", "os-version": "2.0.0", "os-required": "2.0.0",
    "app-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/app.zip",
    "os-URL": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/os.tar.gz",
    "app-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/app.md5",
    "os-md5": "update-rode-com.s3.ap-southeast-2.amazonaws.com/rcp2-duo/sp/os.md5"}
}`

func TestDuoStockReleasesAreWhatRodesDuoManifestLists(t *testing.T) {
	if firmware.ModelDuo.Hardware().ManifestURL() != "https://update.rode.com/rcp2-mini-manifest.json" ||
		firmware.ModelProIi.Hardware().ManifestURL() != "https://update.rode.com/manifest.json" {
		t.Fatal("manifest URLs")
	}
	duo, err := releasesFromManifest(firmware.ModelDuo, []byte(rodeDuoManifestText))
	must(t, err)
	// One version on both public channels is listed once, as stable.
	if len(duo) != 1 || duo[0].Version != "1.7.7" || duo[0].Channel != "stable" {
		t.Fatalf("%+v", duo)
	}
	if duo[0].URL != "https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz" ||
		duo[0].DigestURL != "https://update.rode.com/rcp2-duo/1.7.7/os.md5" ||
		duo[0].Model != firmware.ModelDuo.ID() {
		t.Fatalf("%+v", duo[0])
	}
	// A Pro II is never offered the Duo's files.
	pro, err := releasesFromManifest(firmware.ModelProIi, []byte(rodeDuoManifestText))
	must(t, err)
	if len(pro) != 0 {
		t.Fatalf("%+v", pro)
	}
}

func TestUnpinnedStockFirmwareIsFlashedOnlyAsTheHelperRecordedIt(t *testing.T) {
	anchors := t.TempDir()
	x := newFixture(t)
	x.engine.anchorDir = &anchors
	saved := x.saved()
	saved.Prepared.Stock = true
	saved.Prepared.Profile = ""
	saved.Prepared.Runtime = ""
	p := saved.Prepared.clone()
	must(t, x.engine.save(saved))
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("unrecorded stock image")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
	must(t, x.engine.anchorPrepared(p))
	must(t, x.engine.enterUSBMode(""))
	if x.counts()[1] != 1 {
		t.Fatal("not written")
	}
	put(t, filepath.Join(x.engine.Work, "archive.tar.gz"), []byte("changed"))
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("damaged stock image")
	}
	if x.counts()[1] != 1 {
		t.Fatal("written again")
	}
}

var _ = errors.New
