package installer

import (
	"bytes"
	"fmt"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

func TestABackupRecordCannotPointTheHelperOutsideTheCacheFolder(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("unix only")
	}
	x := newFixture(t)
	work := x.engine.Work
	settings := "{}"
	record, err := x.engine.saveSettings(serial, settings)
	must(t, err)
	if _, err := backupSettings(work, record); err != nil {
		t.Fatal(err)
	}
	outside := filepath.Join(x.root, "secret.json")
	put(t, outside, []byte(settings))
	elsewhere := func(file string) *BackupRecord {
		r := *record
		r.File = file
		return &r
	}
	for _, path := range []string{outside, filepath.Join(work, "../secret.json"), filepath.Join(work, "sub/settings.json")} {
		if _, err := backupSettings(work, elsewhere(path)); err == nil {
			t.Fatalf("%s accepted", path)
		}
	}
	// A path that only names the folder textually still must be the file.
	if _, err := backupSettings(work, elsewhere(work+"/../work/../secret.json")); err == nil {
		t.Fatal("a dotted path was accepted")
	}
	link := filepath.Join(work, "settings-link.json")
	must(t, os.Symlink(outside, link))
	if _, err := backupSettings(work, elsewhere(link)); err == nil {
		t.Fatal("a link was followed")
	}
}

func TestStockFirmwareRequiresCatalogPinsButNoRuntimeOrKey(t *testing.T) {
	x := newFixture(t)
	saved := x.saved()
	p := saved.Prepared
	p.Stock = true
	p.Runtime = ""
	profile := sampleProfile()
	profile.ArchiveSHA, profile.ArchiveMD5 = p.ArchiveSHA, p.ArchiveMD5
	catalog := catalogJSON(t, []Profile{profile})
	put(t, filepath.Join(x.root, "config/firmware-profiles.json"), catalog)
	writeRuntime(t, x.root, catalog)
	must(t, x.engine.save(saved))
	must(t, os.RemoveAll(filepath.Join(x.root, "runtime")))
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

func TestUnpinnedOrMisidentifiedCachedFirmwareNeverOpensUSB(t *testing.T) {
	for _, stock := range []bool{false, true} {
		for _, change := range []string{"version", "profile", "stock-hash"} {
			if !stock && change == "stock-hash" {
				continue
			}
			anchors := t.TempDir()
			x := newFixture(t)
			// A stock image without a pin is the helper's to vouch for.
			x.engine.anchorDir = &anchors
			saved := x.saved()
			p := saved.Prepared
			p.Stock = stock
			switch change {
			case "version":
				p.Version = "1.7.8"
			case "profile":
				p.Profile = ""
			}
			// stock-hash: fixture archive hashes are not official catalog pins.
			must(t, x.engine.save(saved))
			if x.engine.enterUSBMode("") == nil {
				t.Fatalf("%t %s accepted", stock, change)
			}
			if x.counts() != [3]int{} {
				t.Fatal("device accessed")
			}
		}
	}
}

func TestUSBModeNeedsExactlyOneNormalMixer(t *testing.T) {
	x := newFixture(t)
	x.updateMode()
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("already in update mode")
	}
	x = newFixture(t)
	x.with(func(f *fake) {
		second := f.items[0]
		second.Serial = "other"
		f.items = append(f.items, second)
	})
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("ambiguous")
	}
	must(t, x.engine.enterUSBMode(serial))
	// The USB 2 interface (no serial, PID 0x26) is never a candidate.
	x = newFixture(t)
	x.with(func(f *fake) { f.items[0].PID = 0x26 })
	if x.engine.enterUSBMode("") == nil {
		t.Fatal("USB 2 accepted")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
}

func TestFlashCopiesVerifiesEjectsThenSendsU(t *testing.T) {
	x := newFixture(t)
	x.updateMode()
	usb := x.drive(serial)
	put(t, filepath.Join(usb, "archive.tar.gz"), []byte("left over from an earlier attempt"))
	put(t, filepath.Join(usb, ".rc2fx-archive.tar.gz.part"), []byte("partial"))
	must(t, x.engine.flash(""))
	must(t, verifyArchive(usb, &x.prepared))
	if common.PathExists(filepath.Join(usb, ".rc2fx-archive.tar.gz.part")) {
		t.Fatal("partial file left")
	}
	if x.counts() != [3]int{1, 1, 1} {
		t.Fatalf("counts %v", x.counts())
	}
	x.with(func(f *fake) {
		if !bytes.Equal(f.reports[0][:2], []byte{1, 'U'}) {
			t.Fatalf("%v", f.reports[0][:2])
		}
	})
	commands := x.saved().Commands
	c := commands[len(commands)-1]
	if c.Command != "flash" || !c.Acknowledged {
		t.Fatalf("%+v", c)
	}
}

func TestAMixerWithAPlaceholderSerialIsNeverSwitchedOrFlashed(t *testing.T) {
	x := newFixture(t)
	x.with(func(f *fake) { f.items[0].Serial = "RD0000000" })
	err := x.engine.enterUSBMode("")
	contains(t, err, "placeholder serial number RD0000000")
	x.updateMode()
	usb := x.drive("RD0000000")
	err = x.engine.flash("")
	contains(t, err, "settings partition")
	x.with(func(f *fake) {
		if len(f.reports) != 0 {
			t.Fatal("no HID command was sent")
		}
	})
	if x.counts() != [3]int{0, 0, 0} {
		t.Fatalf("counts %v", x.counts())
	}
	if common.PathExists(filepath.Join(usb, "archive.tar.gz")) {
		t.Fatal("nothing is copied to the drive")
	}
}

func TestASmallerUpdateDriveIsFoundAndFlashed(t *testing.T) {
	x := newFixture(t)
	x.updateMode()
	usb := x.drive(serial)
	x.with(func(f *fake) { f.volumes[0].Total = 224 << 20 })
	status, err := x.engine.probe()
	must(t, err)
	if !status.UpdateDrive {
		t.Fatal("drive not found")
	}
	must(t, x.engine.flash(""))
	must(t, verifyArchive(usb, &x.prepared))
	if x.counts() != [3]int{1, 1, 1} {
		t.Fatalf("counts %v", x.counts())
	}
}

func TestAFailedDriveScanIsReportedInsteadOfWaitedOn(t *testing.T) {
	x := newFixture(t)
	x.updateMode()
	x.drive(serial)
	why := `cannot read drive F:\: access denied`
	x.with(func(f *fake) { f.volumeError = &why })
	status, err := x.engine.probe()
	must(t, err)
	if status.UpdateDrive || status.USBError == nil || !bytes.Contains([]byte(*status.USBError), []byte("F:")) {
		t.Fatalf("%+v", status)
	}
}

func TestFlashNeverWritesAnotherDriveOrFlashesMountedMedia(t *testing.T) {
	x := newFixture(t)
	x.updateMode()
	usb := x.drive("another-mixer")
	if x.engine.flash("") == nil {
		t.Fatal("flashed another mixer's drive")
	}
	if common.PathExists(filepath.Join(usb, "archive.tar.gz")) {
		t.Fatal("wrong drive written")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}

	x = newFixture(t)
	x.updateMode()
	x.drive(serial)
	x.with(func(f *fake) { f.ejectError = true })
	if x.engine.flash("") == nil {
		t.Fatal("flashed after a failed ejection")
	}
	if x.counts()[1] != 0 {
		t.Fatal("flash after failed ejection")
	}

	x = newFixture(t)
	usb = x.drive(serial)
	if x.engine.flash("") == nil {
		t.Fatal("normal mode is not update mode")
	}
	if common.PathExists(filepath.Join(usb, "archive.tar.gz")) {
		t.Fatal("written in normal mode")
	}

	x = newFixture(t)
	x.updateMode()
	usb = x.drive(serial)
	must(t, os.MkdirAll(filepath.Join(usb, "archive.md5"), 0o755))
	if x.engine.flash("") == nil {
		t.Fatal("a directory where a file belongs")
	}
	if x.counts()[1] != 0 {
		t.Fatal("written")
	}
}

func TestProbeReportsUSBAndCachedStateWithoutTouchingTheDevice(t *testing.T) {
	x := newFixture(t)
	status, err := x.engine.probe()
	must(t, err)
	version := "1.7.7"
	want := []USBMixer{{Model: firmware.ModelProIi, Serial: serial, UpdateMode: false, Firmware: &version}}
	if !reflect.DeepEqual(status.USB, want) {
		t.Fatalf("%+v", status.USB)
	}
	if !status.PreparedPresent {
		t.Fatal("prepared firmware not present")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
}

func duoFixture(t *testing.T) *fixture {
	x := newFixture(t)
	model := firmware.ModelDuo
	profile := sampleProfile()
	profile.ID = "duo-fixture"
	profile.Model = model.ID()
	profile.Runtime.RouteLayout = model.RouteLayout()
	profile.Runtime.SourceEnum = model.SourceEnum()
	contents := []struct {
		name string
		data []byte
	}{
		{"etc/hostname", []byte(model.Hostname())},
		{"boot/extlinux/extlinux.conf", []byte(fmt.Sprintf("FDT %s\n", model.BoardDTB()))},
		{model.BoardDTB()[1:], []byte("fixture dtb")},
		{"usr/bin/rc_audio_mixer", []byte("fixture mixer")},
		{"usr/bin/audio-start.sh", []byte("fixture launcher")},
	}
	var items []tarItem
	for _, c := range contents {
		items = append(items, tarItem{testHeader(t, c.name, typeRegular, len(c.data)), c.data})
	}
	archivePath := filepath.Join(x.engine.Work, "archive.tar.gz")
	testTar(t, archivePath, items)
	var err error
	profile.ArchiveSHA, err = archive.FileHash(archivePath, archive.SHA256)
	must(t, err)
	profile.ArchiveMD5, err = archive.FileHash(archivePath, archive.MD5)
	must(t, err)
	catalog := catalogJSON(t, []Profile{profile})
	put(t, filepath.Join(x.root, "config/firmware-profiles.json"), catalog)
	writeRuntime(t, x.root, catalog)
	bundle, err := BundleFiles(x.root)
	must(t, err)
	image, runtime, err := imageEntries(bundle, &profile, nil)
	must(t, err)
	found := false
	for _, e := range image {
		if e.Kind == archive.EntryFile && e.Path == "opt/rc2fx/experimental-duo" && bytes.Equal(e.Data, []byte("1\n")) {
			found = true
		}
	}
	if !found {
		t.Fatal("no experimental-duo marker")
	}
	if _, _, err := imageEntries(bundle, &profile, []byte(`{"targets":[{"id":"analog-1"}]}`)); err == nil {
		t.Fatal("Pro II settings accepted for a Duo")
	}
	x.prepared.Model = &model
	x.prepared.Profile = profile.ID
	x.prepared.Runtime = runtime
	x.prepared.ArchiveSHA = profile.ArchiveSHA
	x.prepared.ArchiveMD5 = profile.ArchiveMD5
	put(t, filepath.Join(x.engine.Work, "archive.md5"), []byte(x.prepared.ArchiveMD5))
	must(t, x.engine.save(&Saved{Prepared: x.prepared.clone()}))
	x.with(func(f *fake) { f.items[0].PID = model.NormalPIDs()[0] })
	_, err = x.engine.probe()
	must(t, err)
	return x
}

func allowDuo(t *testing.T, x *fixture, allow bool) {
	t.Helper()
	_, err := x.engine.Handle(Request{Kind: RequestAllowExperimentalDuo, Allow: allow})
	must(t, err)
}

func TestDuoOptInIsRequiredAndRevocationBlocksAPreparedImage(t *testing.T) {
	x := duoFixture(t)
	if x.engine.enterUSBMode(serial) == nil {
		t.Fatal("Duo without opt-in")
	}
	if x.counts() != [3]int{} {
		t.Fatal("device accessed")
	}
	allowDuo(t, x, true)
	must(t, x.engine.enterUSBMode(serial))
	x.with(func(f *fake) {
		if f.reports[0][1] != 'M' {
			t.Fatal("not M")
		}
		f.items[0].PID = firmware.ModelDuo.UpdatePID()
	})
	x.drive(serial)
	allowDuo(t, x, false)
	before := x.counts()
	if x.engine.flash(serial) == nil {
		t.Fatal("flashed after revocation")
	}
	if x.counts() != before {
		t.Fatal("device accessed")
	}
	allowDuo(t, x, true)
	must(t, x.engine.flash(serial))
	x.with(func(f *fake) {
		if f.reports[len(f.reports)-1][1] != 'U' || f.ejects != 1 {
			t.Fatalf("%v %d", f.reports, f.ejects)
		}
	})
}

func TestDuoOptInNeverBypassesModelSerialOrArchiveIdentity(t *testing.T) {
	act := func(x *fixture, update bool) error {
		if update {
			return x.engine.flash(serial)
		}
		return x.engine.enterUSBMode(serial)
	}
	for _, update := range []bool{false, true} {
		x := newFixture(t)
		allowDuo(t, x, true)
		x.with(func(f *fake) {
			f.items[0].PID = firmware.ModelDuo.NormalPIDs()[0]
			if update {
				f.items[0].PID = firmware.ModelDuo.UpdatePID()
			}
		})
		if act(x, update) == nil || x.counts() != [3]int{} {
			t.Fatal("Pro II image must not reach Duo")
		}
		x = duoFixture(t)
		allowDuo(t, x, true)
		x.with(func(f *fake) {
			f.items[0].PID = normalPIDs[0]
			if update {
				f.items[0].PID = updatePID
			}
		})
		if act(x, update) == nil || x.counts() != [3]int{} {
			t.Fatal("Duo image must not reach Pro II")
		}
	}
	x := duoFixture(t)
	allowDuo(t, x, true)
	if x.engine.enterUSBMode("wrong-serial") == nil || x.counts() != [3]int{} {
		t.Fatal("wrong serial accepted")
	}
	// A forged cache model/profile cannot legitimize a Pro II archive.
	x = newFixture(t)
	allowDuo(t, x, true)
	p := x.prepared.clone()
	duo := firmware.ModelDuo
	p.Model = &duo
	p.Stock = true
	if x.engine.verifyPrepared(p) == nil || x.counts() != [3]int{} {
		t.Fatal("forged model accepted")
	}
	x = duoFixture(t)
	allowDuo(t, x, true)
	x.with(func(f *fake) {
		second := f.items[0]
		second.PID = normalPIDs[0]
		second.Serial = "PRO"
		f.items = append(f.items, second)
	})
	if x.engine.enterUSBMode(serial) == nil || x.counts() != [3]int{} {
		t.Fatal("mixed models remain ambiguous")
	}
}

func TestDuoStockRecoveryChecksItsOwnPinnedArchive(t *testing.T) {
	x := duoFixture(t)
	p := x.prepared.clone()
	p.Stock = true
	allowDuo(t, x, true)
	must(t, x.engine.verifyPrepared(p))
	p.ArchiveSHA = string(bytes.Repeat([]byte("0"), 64))
	if x.engine.verifyPrepared(p) == nil || x.counts() != [3]int{} {
		t.Fatal("a changed pin was accepted")
	}
}
