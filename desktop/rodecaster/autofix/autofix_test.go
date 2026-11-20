package autofix

import (
	"bytes"
	"compress/gzip"
	"crypto/md5"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

// clock is the tests' time: sleeping moves it on at once.
type clock struct{ t time.Time }

func (c *clock) now() time.Time        { return c.t }
func (c *clock) sleep(d time.Duration) { c.t = c.t.Add(d) }

// flashed is one flash the simulated mixer received.
type flashed struct {
	md5     string
	carrier bool
	// members maps each member's name to its type and link target.
	members map[string]string
	// script is the first copy of the carrier script, nil without one.
	script []byte
}

// mixer simulates one RØDECaster on USB: HID control in normal and update
// mode, an update drive that is a folder, and the restarts after a flash.
type mixer struct {
	t        *testing.T
	clock    *clock
	model    model.Model
	serial   string
	release  uint16
	drive    string
	update   bool
	updateAt time.Time
	// flashing: restarting after a flash done at flashedAt.
	flashing  bool
	flashedAt time.Time
	neverBack bool
	// oneBoot: after a flash the mixer is off 60 s and then on for good, as
	// the PC saw it on every hardware run.
	oneBoot  bool
	ejected  bool
	commands []byte
	flashes  []flashed
	// marker is what the script leaves after the n-th carrier flash
	// (counting from 1); "" leaves nothing.
	marker   func(n int) string
	carriers int
	// report is the marker the script offers on its USB disk after the
	// n-th carrier flash, 80 s after that flash ("" offers none); only
	// with the reporting platform. reportDone: the disk was acknowledged,
	// ejected and went away. ignoreEject: ejecting it changes nothing.
	// vanishOnce: the first read finds the disk gone (the slot was emptied
	// on the mixer). ackFail: the acknowledgement cannot be written.
	report       func(n int) string
	reportDone   bool
	ignoreEject  bool
	vanishOnce   bool
	ackFail      bool
	reportEjects int
	reportReads  int
	reportAcks   int
	// acked: the acknowledgement is on the disk. offAt and offers: when the
	// disk last left the slot without one, and how often; the script
	// offers it again 25 s later, three times at most.
	acked  bool
	offAt  time.Time
	offers int
}

func newMixer(t *testing.T, c *clock) *mixer {
	return &mixer{t: t, clock: c, model: model.ProIi, serial: "RCP2X1234567", release: 0x0177, drive: t.TempDir()}
}

// state is "normal", "update" or "off". After a flash the mixer is off
// 20 s, on 30 s, off 10 s and then on for good: the restarts of RØDE's
// update.
func (m *mixer) state() string {
	now := m.clock.now()
	if m.flashing {
		if m.neverBack {
			return "off"
		}
		e := now.Sub(m.flashedAt)
		switch {
		case e < 20*time.Second, m.oneBoot && e < 60*time.Second:
			return "off"
		case e < 50*time.Second:
			return "normal"
		case e < 60*time.Second:
			return "off"
		}
		m.flashing = false
	}
	if m.update {
		if now.Before(m.updateAt) {
			return "off"
		}
		return "update"
	}
	return "normal"
}

func (m *mixer) Inventory() ([]platform.HID, error) {
	h := platform.HID{Path: "hid0", Serial: m.serial, Usage: 0xff00, Input: 64, Output: 64}
	switch m.state() {
	case "normal":
		h.PID, h.Release = m.model.NormalPIDs()[0], m.release
	case "update":
		h.PID = m.model.UpdatePID()
	default:
		return nil, nil
	}
	return []platform.HID{h}, nil
}

func (m *mixer) Open(h *platform.HID) (platform.Reports, error) {
	if m.state() == "off" {
		return nil, errors.New("the device is gone")
	}
	return &reports{m}, nil
}

func (m *mixer) OpenControl(h *platform.HID) (platform.Reports, error) { return m.Open(h) }

func (m *mixer) Volumes() ([]platform.Volume, error) {
	if m.state() != "update" {
		return nil, nil
	}
	return []platform.Volume{{Path: m.drive, Device: "fake0", Serial: m.serial, FS: "vfat", Total: 250 << 20, Free: 250 << 20}}, nil
}

func (m *mixer) Mount(v platform.Volume) (platform.Volume, error) { return v, nil }
func (m *mixer) MediaPresent(*platform.Volume) (bool, error)      { return !m.ejected, nil }
func (m *mixer) Eject(*platform.Volume) error                     { m.ejected = true; return nil }

type reports struct{ m *mixer }

func (r *reports) Write(report []byte, _ time.Duration) (int, error) {
	m := r.m
	letter := report[1]
	m.commands = append(m.commands, letter)
	switch letter {
	case 'M':
		if m.state() != "normal" {
			m.t.Errorf("update-mode command while the mixer is %s", m.state())
		}
		m.update, m.updateAt, m.ejected = true, m.clock.now().Add(3*time.Second), false
	case 'U':
		if m.state() != "update" || !m.ejected {
			m.t.Errorf("flash command while the mixer is %s, drive ejected %v", m.state(), m.ejected)
		}
		m.received()
		m.update, m.flashing, m.flashedAt = false, true, m.clock.now()
	default:
		m.t.Errorf("unexpected command %q", letter)
	}
	return len(report), nil
}

func (r *reports) Read(buf []byte, _ time.Duration) (int, error) {
	buf[0], buf[1] = 2, 'A'
	return 2, nil
}

func (r *reports) Close() error { return nil }

// received records the staged archive and does what the mixer does with
// the drive: RØDE's software empties it, and the script may write its
// marker.
func (m *mixer) received() {
	path := filepath.Join(m.drive, flash.UpdateFiles[0])
	data, err := os.ReadFile(path)
	if err != nil {
		m.t.Fatalf("no archive on the drive at the flash command: %v", err)
	}
	sum := md5.Sum(data)
	f := flashed{md5: hex.EncodeToString(sum[:]), members: map[string]string{}}
	err = archive.Walk(path, 1<<30, false, func(e *archive.Member) error {
		f.members[archive.Normalized(e.Path)] = fmt.Sprintf("%c>%s", e.Header.EntryType(), e.Header.LinkName())
		if archive.Normalized(e.Path) == "etc/init.d/rcfix-a" {
			f.script, _ = archive.ReadAll(e)
		}
		return nil
	})
	if err != nil {
		m.t.Fatalf("the staged archive is not readable: %v", err)
	}
	_, f.carrier = f.members["etc/init.d/rcfix-a"]
	m.flashes = append(m.flashes, f)
	entries, _ := os.ReadDir(m.drive)
	for _, e := range entries {
		os.Remove(filepath.Join(m.drive, e.Name()))
	}
	if f.carrier {
		m.carriers++
		m.reportDone, m.acked, m.offAt, m.offers = false, false, time.Time{}, 0
		if m.marker != nil {
			if text := m.marker(m.carriers); text != "" {
				if err := os.WriteFile(filepath.Join(m.drive, carrier.MarkerName), []byte(text), 0o644); err != nil {
					m.t.Fatal(err)
				}
			}
		}
	}
}

// reporting is the mixer on a platform that can see its USB disk in normal
// mode, where the script offers its marker (mixer.report).
type reporting struct{ *mixer }

const reportDevice = "report0"

func (r reporting) reportText() string {
	m := r.mixer
	if m.report == nil || m.carriers == 0 || m.reportDone {
		return ""
	}
	now := m.clock.now()
	if m.state() != "normal" || m.flashing || now.Sub(m.flashedAt) < 80*time.Second {
		return ""
	}
	// The script's limit: it gives the slot back 20 + 120 s after its
	// first offer, and offers the disk at most three times.
	if now.Sub(m.flashedAt) >= 220*time.Second || m.offers >= 3 ||
		(!m.offAt.IsZero() && now.Sub(m.offAt) < 25*time.Second) {
		return ""
	}
	return m.report(m.carriers)
}

func (r reporting) ReportDisks() ([]platform.Volume, error) {
	if r.reportText() == "" {
		return nil, nil
	}
	return []platform.Volume{{Device: reportDevice, Serial: r.serial, FS: "vfat", Total: 1 << 20}}, nil
}

func (r reporting) ReadReport(v platform.Volume, name string, limit int64) ([]byte, error) {
	m := r.mixer
	m.reportReads++
	text := r.reportText()
	if v.Device != reportDevice || name != carrier.MarkerName || text == "" {
		return nil, errors.New("no such file")
	}
	if m.vanishOnce {
		m.vanishOnce = false
		m.offAt, m.offers = m.clock.now(), m.offers+1
		return nil, errors.New("the device is gone")
	}
	return []byte(text), nil
}

func (r reporting) WriteReport(v platform.Volume, name string, data []byte) error {
	m := r.mixer
	if v.Device != reportDevice || name != carrier.AckName || string(data) != "ack=1\n" || r.reportText() == "" {
		m.t.Errorf("acknowledgement %q to %s as %s", data, v.Device, name)
		return errors.New("no such disk")
	}
	m.reportAcks++
	if m.ackFail {
		return errors.New("read-only file system")
	}
	m.acked = true
	return nil
}

// Eject of the USB disk is the script's signal: it finishes without
// writing its marker to the update drive.
func (r reporting) Eject(v *platform.Volume) error {
	m := r.mixer
	if v.Device != reportDevice {
		return m.Eject(v)
	}
	m.reportEjects++
	switch {
	case m.ignoreEject:
	case m.acked:
		// The script finds the acknowledgement and finishes.
		m.reportDone = true
		os.Remove(filepath.Join(m.drive, carrier.MarkerName))
	default:
		m.offAt, m.offers = m.clock.now(), m.offers+1
	}
	return nil
}

func (r reporting) MediaPresent(v *platform.Volume) (bool, error) {
	if v.Device != reportDevice {
		return r.mixer.MediaPresent(v)
	}
	m := r.mixer
	return !m.reportDone && (m.ignoreEject || m.offAt.IsZero() || m.clock.now().Sub(m.offAt) >= 25*time.Second), nil
}

// firmware is a tiny archive with the members the script needs.
type firmware struct {
	hostname string
	skip     []string
	add      []archive.Entry
}

func (fw firmware) build(t *testing.T) []byte {
	t.Helper()
	host := fw.hostname
	if host == "" {
		host = model.ProIi.Hostname()
	}
	entries := []archive.Entry{
		{Kind: archive.EntryDir, Path: "bin", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "etc", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "etc/init.d", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "etc/rc5.d", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "sbin", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "usr", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "usr/bin", Mode: 0o755},
		{Kind: archive.EntryDir, Path: "usr/sbin", Mode: 0o755},
		{Kind: archive.EntryFile, Path: "etc/hostname", Mode: 0o644, Data: []byte(host + "\n")},
		{Kind: archive.EntryFile, Path: "usr/bin/rc_audio_mixer", Mode: 0o755, Data: []byte("mixer")},
		{Kind: archive.EntryFile, Path: "usr/bin/audio-start.sh", Mode: 0o755, Data: []byte("#!/bin/sh\n")},
		{Kind: archive.EntryFile, Path: "etc/init.d/rode-audio-service", Mode: 0o755, Data: []byte("#!/bin/sh\n")},
		{Kind: archive.EntryLink, Path: "etc/rc5.d/S03rode-audio-service", Mode: 0o777, Target: "../init.d/rode-audio-service"},
		{Kind: archive.EntryFile, Path: "bin/bash.bash", Mode: 0o755, Data: []byte("bash")},
		{Kind: archive.EntryLink, Path: "bin/bash", Mode: 0o777, Target: "bash.bash"},
	}
	for _, tool := range []string{"usr/sbin/sfdisk", "usr/sbin/resizepart", "sbin/fw_setenv", "usr/bin/sha256sum", "usr/bin/cmp", "usr/bin/setsid"} {
		entries = append(entries, archive.Entry{Kind: archive.EntryFile, Path: tool, Mode: 0o755, Data: []byte(tool)})
	}
	entries = append(entries, fw.add...)
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	w := &archive.Writer{W: gz}
	for _, e := range entries {
		if slices.Contains(fw.skip, e.Path) {
			continue
		}
		if err := archive.AppendEntry(w, e); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Finish(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func md5hex(data []byte) string {
	sum := md5.Sum(data)
	return hex.EncodeToString(sum[:])
}

// server is RØDE's update server for one release, answered in process.
type server struct {
	t       *testing.T
	archive []byte
	md5     string
	// requests counts what was asked for.
	requests []string
}

func (s *server) RoundTrip(req *http.Request) (*http.Response, error) {
	s.requests = append(s.requests, req.URL.String())
	rec := httptest.NewRecorder()
	switch req.URL.String() {
	case "https://update.rode.com/rcp2/1.7.7/os.md5", "https://update.rode.com/rcp2-duo/1.7.7/os.md5":
		io.WriteString(rec, s.md5+"\n")
	case "https://update.rode.com/rcp2/1.7.7/os.tar.gz", "https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz":
		rec.Header().Set("Content-Length", fmt.Sprint(len(s.archive)))
		rec.Write(s.archive)
	default:
		rec.WriteHeader(http.StatusNotFound)
	}
	res := rec.Result()
	res.Request = req
	return res, nil
}

const table = "/dev/mmcblk1p1 : start=8192, size=1193984, type=83|/dev/mmcblk1p2 : start=2000896, size=1193984, type=83"

func sector0() []byte {
	b := make([]byte, 512)
	copy(b[440:444], []byte{0x24, 0x93, 0x3a, 0xed})
	b[510], b[511] = 0x55, 0xaa
	return b
}

func marker(result, extra string) string {
	return "rcfix=1\nresult=" + result + "\nslot=2\nhopped=1\np1_sectors=1193984\ntable=" + table + "\n" + extra + "script=1\nend=1\n"
}

var (
	fixedMarker   = marker("fixed", "sector0_before="+hex.EncodeToString(sector0())+"\n")
	alreadyMarker = marker("already-fixed", "")
	unknownMarker = marker("unrecognized", "reason=partition 3 does not start at 3223552\n")
	failedMarker  = marker("failed", "reason=slot 2 could not be mounted\n")
)

type harness struct {
	clock  *clock
	mixer  *mixer
	server *server
	opts   Options
	events []Event
}

func newHarness(t *testing.T, fw firmware) *harness {
	c := &clock{t: time.Date(2026, 11, 17, 12, 0, 0, 0, time.UTC)}
	data := fw.build(t)
	h := &harness{clock: c, mixer: newMixer(t, c), server: &server{t: t, archive: data, md5: md5hex(data)}}
	h.opts = Options{
		Platform:  h.mixer,
		Work:      t.TempDir(),
		BackupDir: filepath.Join(t.TempDir(), "backups"),
		Client:    func() *http.Client { return &http.Client{Transport: h.server} },
		Events:    func(e Event) { h.events = append(h.events, e) },
		now:       c.now,
		sleep:     c.sleep,
	}
	return h
}

func (h *harness) run() (Result, error) { return Run(h.opts) }

func (h *harness) commands() string { return string(h.mixer.commands) }

// checkSafety: no event between an update-mode command and the flash
// command's acknowledgement says it is safe to close, and every wait does.
func (h *harness) checkSafety(t *testing.T) {
	t.Helper()
	for i, e := range h.events {
		switch e.Stage {
		case StageUpdateMode, StageCopy, StageFlash:
			if e.SafeToClose {
				t.Errorf("event %d (%s: %s) says it is safe to close", i, e.Stage, e.Text)
			}
		case StageWait, StageReport, StageFinish, StageFind, StageDownload, StageInspect, StageBuild:
			if !e.SafeToClose {
				t.Errorf("event %d (%s: %s) says it is not safe to close", i, e.Stage, e.Text)
			}
		}
	}
}

func TestFixed(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return fixedMarker }
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeFixed || res.Flashes != 2 || res.Firmware != "1.7.7" || res.Model != "rodecaster-pro-ii" || res.Marker == nil {
		t.Fatalf("result %+v", res)
	}
	if got := h.commands(); got != "MUMU" {
		t.Errorf("commands %q, want MUMU", got)
	}
	f := h.mixer.flashes
	if len(f) != 2 || !f[0].carrier || f[1].carrier || f[1].md5 != h.server.md5 {
		t.Fatalf("flashes %+v", f)
	}
	for _, n := range []string{"a", "b", "c"} {
		if got := f[0].members["etc/rc5.d/S02rcfix-"+n]; got != "2>../init.d/rcfix-"+n {
			t.Errorf("link %s is %q", n, got)
		}
		if got := f[0].members["etc/init.d/rcfix-"+n]; got != "0>" {
			t.Errorf("script %s is %q", n, got)
		}
	}
	data, err := os.ReadFile(res.Backup)
	if err != nil || !bytes.Equal(data, sector0()) {
		t.Errorf("backup %s: %v", res.Backup, err)
	}
	if !strings.HasPrefix(filepath.Base(res.Backup), "rcfix-sector0-ed3a9324-") {
		t.Errorf("backup name %s", res.Backup)
	}
	h.checkSafety(t)
	last := h.events[len(h.events)-1]
	if last.Stage != StageFinish || !last.SafeToClose || last.Text != res.Summary {
		t.Errorf("last event %+v", last)
	}
	// The download is removed after a finished run.
	if _, err := os.Stat(filepath.Join(h.opts.Work, plainDir)); !os.IsNotExist(err) {
		t.Errorf("work folder kept: %v", err)
	}
}

func TestOutcomes(t *testing.T) {
	for _, tc := range []struct {
		marker  string
		outcome Outcome
		reason  string
	}{
		{alreadyMarker, OutcomeAlreadyFixed, ""},
		{unknownMarker, OutcomeUnrecognized, "partition 3 does not start at 3223552"},
		{failedMarker, OutcomeFailed, "slot 2 could not be mounted"},
	} {
		t.Run(string(tc.outcome), func(t *testing.T) {
			h := newHarness(t, firmware{})
			h.mixer.marker = func(int) string { return tc.marker }
			res, err := h.run()
			if err != nil {
				t.Fatal(err)
			}
			if res.Outcome != tc.outcome || res.Marker.Reason != tc.reason || res.Backup != "" || res.Flashes != 2 {
				t.Fatalf("result %+v", res)
			}
			if tc.reason != "" && !strings.Contains(res.Summary, tc.reason) {
				t.Errorf("summary %q lacks the reason", res.Summary)
			}
			if len(res.Marker.Table) != 2 {
				t.Errorf("table %q", res.Marker.Table)
			}
			// The plain archive is flashed whatever the script said.
			if f := h.mixer.flashes; len(f) != 2 || f[1].carrier || f[1].md5 != h.server.md5 {
				t.Errorf("flashes %+v", f)
			}
			h.checkSafety(t)
		})
	}
}

func TestMarkerOnSecondAttempt(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(n int) string {
		if n == 2 {
			return fixedMarker
		}
		return ""
	}
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeFixed || res.Flashes != 3 {
		t.Fatalf("result %+v", res)
	}
	if got := h.commands(); got != "MUMUMU" {
		t.Errorf("commands %q", got)
	}
	f := h.mixer.flashes
	if !f[0].carrier || !f[1].carrier || f[2].carrier {
		t.Errorf("flashes %+v", f)
	}
}

func TestNeverAMarker(t *testing.T) {
	h := newHarness(t, firmware{})
	// A marker an earlier run left must not count for this one.
	if err := os.WriteFile(filepath.Join(h.mixer.drive, carrier.MarkerName), []byte(fixedMarker), 0o644); err != nil {
		t.Fatal(err)
	}
	res, err := h.run()
	// Without a result nobody knows which slot the last flash would format,
	// so it is not sent: the mixer is left in update mode, to be switched
	// off and on.
	var stopped *NotApplied
	if !errors.As(err, &stopped) || !strings.Contains(err.Error(), "switch it off and on") {
		t.Fatalf("error %v", err)
	}
	if res.Outcome != OutcomeUnconfirmed || res.Marker != nil || res.Flashes != carrierAttempts || res.MarkerProblem == "" {
		t.Fatalf("result %+v", res)
	}
	for i, f := range h.mixer.flashes {
		if !f.carrier {
			t.Errorf("flash %d is not the carrier: %+v", i, f)
		}
	}
	h.checkSafety(t)
}

// factoryMarker is a result on RØDE's factory table from slot.
func factoryMarker(result string, slot int) string {
	factory := "/dev/mmcblk1p1 : start=        8192, size=     1039458, type=83|" +
		"/dev/mmcblk1p2 : start=     2000896, size=     1193984, type=83|" +
		"/dev/mmcblk1p3 : start=     3223552, size=     1710080, type=83|" +
		"/dev/mmcblk1p4 : start=     4941824, size=    10328064, type=83"
	return fmt.Sprintf("rcfix=1\nresult=%s\nslot=%d\nhopped=1\np1_sectors=1039458\ntable=%s\nreason=the space partition 1 would grow into is not empty\nscript=1\nend=1\n", result, slot, factory)
}

// A fix that did not finish while the mixer runs slot 2 leaves slot 1 at
// its original size, where the last flash would get 1 KB blocks: it is not
// sent, with or without final firmware.
func TestAnUnfixedSlotIsNeverFlashed(t *testing.T) {
	for _, final := range []bool{false, true} {
		t.Run(fmt.Sprint("final=", final), func(t *testing.T) {
			h := newHarness(t, firmware{})
			h.mixer.marker = func(int) string { return factoryMarker("failed", 2) }
			if final {
				h.opts.FinalDir, h.opts.FinalMD5 = finalDir(t, firmware{})
			}
			res, err := h.run()
			var stopped *NotApplied
			if !errors.As(err, &stopped) {
				t.Fatalf("error %v", err)
			}
			for _, want := range []string{"would grow into is not empty", "system slot 1 is still its original size", "Nothing more was sent"} {
				if !strings.Contains(err.Error(), want) {
					t.Errorf("error %q lacks %q", err, want)
				}
			}
			if res.Outcome != OutcomeFailed || res.Flashes != 1 || len(h.mixer.flashes) != 1 || !h.mixer.flashes[0].carrier {
				t.Fatalf("result %+v, flashes %+v", res, h.mixer.flashes)
			}
			h.checkSafety(t)
		})
	}
}

// From slot 1 the last flash formats slot 2, which is large enough on the
// factory table, so it goes ahead even though the fix did not finish.
func TestAnUnfinishedFixFromSlotOneStillFlashesSlotTwo(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return factoryMarker("failed", 1) }
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeFailed || res.Flashes != 2 {
		t.Fatalf("result %+v", res)
	}
	if f := h.mixer.flashes; len(f) != 2 || f[1].carrier {
		t.Errorf("flashes %+v", f)
	}
}

func TestLastFlashSafe(t *testing.T) {
	for _, tc := range []struct {
		marker string
		ok     bool
	}{
		{fixedMarker, true},
		{factoryMarker("failed", 1), true},
		{factoryMarker("failed", 2), false},
		{factoryMarker("unrecognized", 2), false},
		{strings.Replace(factoryMarker("failed", 1), "slot=1", "slot=0", 1), false},
	} {
		m, err := carrier.ParseMarker([]byte(tc.marker))
		if err != nil {
			t.Fatal(err)
		}
		if ok, why := lastFlashSafe(&m); ok != tc.ok || ok != (why == "") {
			t.Errorf("%q: %v %q", tc.marker, ok, why)
		}
	}
	if ok, _ := lastFlashSafe(nil); ok {
		t.Error("no marker was safe")
	}
}

func TestUnparsableMarkerRetries(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(n int) string {
		if n == 1 {
			return "rcfix=1\nresult=fixed\n"
		}
		return alreadyMarker
	}
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeAlreadyFixed || res.Flashes != 3 {
		t.Fatalf("result %+v", res)
	}
}

func TestUnsupportedFirmware(t *testing.T) {
	for name, fw := range map[string]firmware{
		"no sfdisk": {skip: []string{"usr/sbin/sfdisk"}},
		"no bash":   {skip: []string{"bin/bash", "bin/bash.bash"}},
		"has rcfix": {add: []archive.Entry{{Kind: archive.EntryDir, Path: "var/lib/rcfix", Mode: 0o755}}},
	} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, fw)
			_, err := h.run()
			if err == nil || !strings.Contains(err.Error(), "not supported by this fix yet") {
				t.Fatalf("error %v", err)
			}
			if h.commands() != "" {
				t.Errorf("commands sent: %q", h.commands())
			}
		})
	}
}

func TestBashAlternative(t *testing.T) {
	h := newHarness(t, firmware{skip: []string{"bin/bash.bash"}})
	h.mixer.marker = func(int) string { return alreadyMarker }
	if _, err := h.run(); err != nil {
		t.Fatal(err)
	}
}

func TestWrongMD5(t *testing.T) {
	h := newHarness(t, firmware{})
	h.server.md5 = strings.Repeat("0", 32)
	_, err := h.run()
	if err == nil || !strings.Contains(err.Error(), "Nothing was changed") {
		t.Fatalf("error %v", err)
	}
	if h.commands() != "" {
		t.Errorf("commands sent: %q", h.commands())
	}
}

func TestNeverComesBack(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.neverBack = true
	start := h.clock.now()
	_, err := h.run()
	if err == nil || !strings.Contains(err.Error(), "has not come back") {
		t.Fatalf("error %v", err)
	}
	if h.commands() != "MU" {
		t.Errorf("commands %q", h.commands())
	}
	if waited := h.clock.now().Sub(start); waited < 15*time.Minute || waited > 16*time.Minute {
		t.Errorf("waited %s", waited)
	}
}

func TestSettleRestartsOnDisappearance(t *testing.T) {
	// The mixer is on 20..50 s and for good from 60 s; the first marker
	// read may only come 150 s after that.
	var readAt time.Time
	h2 := newHarness(t, firmware{})
	h2.mixer.marker = func(int) string { return alreadyMarker }
	var flashAt time.Time
	h2.opts.Events = func(e Event) {
		if e.Stage == StageWait && flashAt.IsZero() {
			flashAt = h2.clock.now()
		}
		if e.Stage == StageReadResult && readAt.IsZero() {
			readAt = h2.clock.now()
		}
	}
	if _, err := h2.run(); err != nil {
		t.Fatal(err)
	}
	if d := readAt.Sub(flashAt); d < 210*time.Second || d > 215*time.Second {
		t.Errorf("marker read %s after the flash, want about 210 s", d)
	}
}

func TestCancelDuringDownload(t *testing.T) {
	h := newHarness(t, firmware{})
	h.opts.Cancel = func() error {
		if slices.ContainsFunc(h.server.requests, func(u string) bool { return strings.HasSuffix(u, "os.tar.gz") }) {
			return errors.New("the window was closed")
		}
		return nil
	}
	_, err := h.run()
	if !errors.Is(err, ErrCanceled) || !strings.Contains(err.Error(), "Nothing was changed") {
		t.Fatalf("error %v", err)
	}
	if h.commands() != "" {
		t.Errorf("commands sent: %q", h.commands())
	}
}

func TestCancelDuringWait(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return fixedMarker }
	h.opts.Cancel = func() error {
		if len(h.mixer.flashes) > 0 {
			return errors.New("the window was closed")
		}
		return nil
	}
	_, err := h.run()
	if !errors.Is(err, ErrCanceled) {
		t.Fatalf("error %v", err)
	}
	if h.commands() != "MU" || h.mixer.update {
		t.Errorf("commands %q, update mode %v", h.commands(), h.mixer.update)
	}
	h.checkSafety(t)
}

func TestDuoRefusedByDefault(t *testing.T) {
	h := newHarness(t, firmware{hostname: model.Duo.Hostname()})
	h.mixer.model = model.Duo
	_, err := h.run()
	if err == nil || !strings.Contains(err.Error(), "RØDECaster Duo") {
		t.Fatalf("error %v", err)
	}
	if h.commands() != "" || len(h.server.requests) != 0 {
		t.Errorf("commands %q, requests %v", h.commands(), h.server.requests)
	}
}

func TestDuoWhenAllowed(t *testing.T) {
	h := newHarness(t, firmware{hostname: model.Duo.Hostname()})
	h.mixer.model = model.Duo
	h.mixer.marker = func(int) string { return alreadyMarker }
	h.opts.Allowed = func(model.Model) error { return nil }
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Model != "rodecaster-duo" || res.Outcome != OutcomeAlreadyFixed {
		t.Errorf("result %+v", res)
	}
}

func TestRefusals(t *testing.T) {
	for name, tc := range map[string]struct {
		setup func(*harness)
		want  string
	}{
		"no mixer":    {func(h *harness) { h.mixer.flashing, h.mixer.neverBack = true, true }, "no RØDECaster was found"},
		"update mode": {func(h *harness) { h.mixer.update = true }, "30 seconds"},
		"placeholder": {func(h *harness) { h.mixer.serial = "RD0000000" }, "placeholder serial"},
		"no version":  {func(h *harness) { h.mixer.release = 0x1000 }, "did not report its firmware version"},
		"bad version": {func(h *harness) { h.opts.Version = "1.7" }, "not a firmware version"},
	} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, firmware{})
			tc.setup(h)
			_, err := h.run()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("error %v, want %q", err, tc.want)
			}
			if h.commands() != "" || len(h.server.requests) != 0 {
				t.Errorf("commands %q, requests %v", h.commands(), h.server.requests)
			}
		})
	}
}

func TestVersionOverride(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.release = 0x1000
	h.mixer.marker = func(int) string { return alreadyMarker }
	h.opts.Version = "1.7.7"
	res, err := h.run()
	if err != nil || res.Firmware != "1.7.7" {
		t.Fatalf("%+v %v", res, err)
	}
}

func TestLocalArchive(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return fixedMarker }
	dir := t.TempDir()
	h.opts.LocalArchive, h.opts.LocalMD5 = filepath.Join(dir, "os.tar.gz"), filepath.Join(dir, "os.md5")
	if err := os.WriteFile(h.opts.LocalArchive, h.server.archive, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(h.opts.LocalMD5, []byte(h.server.md5+"  os.tar.gz\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	res, err := h.run()
	if err != nil || res.Outcome != OutcomeFixed {
		t.Fatalf("%+v %v", res, err)
	}
	if len(h.server.requests) != 0 {
		t.Errorf("requests %v", h.server.requests)
	}

	h = newHarness(t, firmware{})
	h.opts.LocalArchive, h.opts.LocalMD5 = filepath.Join(dir, "os.tar.gz"), filepath.Join(dir, "os.md5")
	if err := os.WriteFile(h.opts.LocalMD5, []byte(strings.Repeat("1", 32)), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := h.run(); err == nil || !strings.Contains(err.Error(), "does not match its MD5") || h.commands() != "" {
		t.Fatalf("a wrong MD5 gave %v, commands %q", err, h.commands())
	}
}

func TestExtraEntriesOnlyInCarrier(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return alreadyMarker }
	h.opts.ExtraEntries = []archive.Entry{{Kind: archive.EntryFile, Path: "etc/rcfix-test", Mode: 0o644, Data: []byte("x")}}
	if _, err := h.run(); err != nil {
		t.Fatal(err)
	}
	f := h.mixer.flashes
	if _, ok := f[0].members["etc/rcfix-test"]; !ok {
		t.Error("the extra file is not in the carrier archive")
	}
	if _, ok := f[1].members["etc/rcfix-test"]; ok {
		t.Error("the extra file is in the plain archive")
	}
}

func TestParseMD5(t *testing.T) {
	good := strings.Repeat("ab", 16)
	for _, in := range []string{good, good + "\n", strings.ToUpper(good) + "  os.tar.gz\n"} {
		if got, err := parseMD5([]byte(in)); err != nil || got != good {
			t.Errorf("%q: %q %v", in, got, err)
		}
	}
	for _, in := range []string{"", "abc", strings.Repeat("g", 32)} {
		if _, err := parseMD5([]byte(in)); err == nil {
			t.Errorf("%q was taken", in)
		}
	}
}

// finalDir writes fw as the firmware to flash last, as an installer stages
// it: flash.UpdateFiles in a folder. It returns the folder and the MD5.
func finalDir(t *testing.T, fw firmware) (string, string) {
	t.Helper()
	data := fw.build(t)
	dir := t.TempDir()
	md := md5hex(data)
	if err := os.WriteFile(filepath.Join(dir, flash.UpdateFiles[0]), data, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, flash.UpdateFiles[1]), []byte(md), 0o600); err != nil {
		t.Fatal(err)
	}
	return dir, md
}

func TestFinalFirmwareIsFlashedLast(t *testing.T) {
	added := firmware{add: []archive.Entry{{Kind: archive.EntryFile, Path: "usr/bin/rc2fx", Mode: 0o755, Data: []byte("rc2fx")}}}
	for name, marker := range map[string]string{"fixed": fixedMarker, "unrecognized": unknownMarker} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, firmware{})
			h.mixer.marker = func(int) string { return marker }
			h.opts.FinalDir, h.opts.FinalMD5 = finalDir(t, added)
			res, err := h.run()
			if err != nil {
				t.Fatal(err)
			}
			f := h.mixer.flashes
			last := f[len(f)-1]
			if last.carrier || last.md5 != h.opts.FinalMD5 || last.members["usr/bin/rc2fx"] == "" {
				t.Fatalf("last flash %+v", last)
			}
			// The carrier is still RØDE's archive with the script, without
			// the final firmware's additions.
			if !f[0].carrier || f[0].members["usr/bin/rc2fx"] != "" {
				t.Errorf("carrier flash %+v", f[0])
			}
			if strings.Contains(res.Summary, "RØDE's firmware was installed") {
				t.Errorf("summary %q names RØDE's firmware as the last one", res.Summary)
			}
			h.checkSafety(t)
		})
	}
}

func TestFinalFirmwareNeedsItsMD5(t *testing.T) {
	h := newHarness(t, firmware{})
	h.opts.FinalDir = t.TempDir()
	if _, err := h.run(); err == nil || h.commands() != "" {
		t.Fatalf("no MD5 gave %v, commands %q", err, h.commands())
	}
	h = newHarness(t, firmware{})
	h.opts.FinalDir, h.opts.FinalMD5 = t.TempDir(), "not an md5"
	if _, err := h.run(); err == nil || h.commands() != "" {
		t.Fatalf("a bad MD5 gave %v, commands %q", err, h.commands())
	}
}

func TestFinalFirmwareThatDoesNotMatchIsNotFlashed(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return alreadyMarker }
	dir, _ := finalDir(t, firmware{})
	h.opts.FinalDir, h.opts.FinalMD5 = dir, strings.Repeat("1", 32)
	res, err := h.run()
	if err == nil || !strings.Contains(err.Error(), "installing the new firmware") {
		t.Fatalf("error %v", err)
	}
	// The carrier flash and the marker read happened; the final flash
	// command was never sent.
	if res.Outcome != OutcomeAlreadyFixed || h.commands() != "MUM" {
		t.Errorf("result %+v, commands %q", res, h.commands())
	}
}

func TestCommandsAreRecorded(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return alreadyMarker }
	var names []string
	h.opts.Record = func(name, serial string) (func(error) error, error) {
		if serial != h.mixer.serial {
			t.Errorf("recorded serial %q", serial)
		}
		names = append(names, name)
		return func(error) error { return nil }, nil
	}
	if _, err := h.run(); err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(names, " "); got != "usb-mode flash usb-mode flash" {
		t.Errorf("recorded %q", got)
	}
}

func TestRefusalsAreMarked(t *testing.T) {
	var refusal *Refusal
	h := newHarness(t, firmware{hostname: model.Duo.Hostname()})
	h.mixer.model = model.Duo
	if _, err := h.run(); !errors.As(err, &refusal) {
		t.Errorf("the Duo gave %v", err)
	}
	h = newHarness(t, firmware{skip: []string{"usr/sbin/sfdisk"}})
	if _, err := h.run(); !errors.As(err, &refusal) || !strings.Contains(err.Error(), "not supported by this fix yet") {
		t.Errorf("unsupported firmware gave %v", err)
	}
	// What can change by starting again is not a refusal.
	h = newHarness(t, firmware{})
	h.server.md5 = strings.Repeat("0", 32)
	if _, err := h.run(); err == nil || errors.As(err, &refusal) {
		t.Errorf("a failed download gave %v", err)
	}
	h = newHarness(t, firmware{})
	h.mixer.update = true
	if _, err := h.run(); err == nil || errors.As(err, &refusal) {
		t.Errorf("update mode gave %v", err)
	}
}

// ---- The USB report ----

func reportingHarness(t *testing.T) *harness {
	h := newHarness(t, firmware{})
	h.opts.Platform = reporting{h.mixer}
	return h
}

// recordStages records when the first event of each stage came.
func (h *harness) recordStages() map[string]time.Time {
	at := map[string]time.Time{}
	events := h.opts.Events
	h.opts.Events = func(e Event) {
		if _, ok := at[e.Stage]; !ok {
			at[e.Stage] = h.clock.now()
		}
		events(e)
	}
	return at
}

// The script's marker on the USB disk ends the wait at once: no update-mode
// read, and the last flash starts from normal mode.
func TestReportTaken(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.report = func(int) string { return fixedMarker }
	// Were the update drive read, it would say something else.
	h.mixer.marker = func(int) string { return failedMarker }
	at := h.recordStages()
	var flashedAt time.Time
	h.opts.Record = func(name, _ string) (func(error) error, error) {
		if name == "flash" && flashedAt.IsZero() {
			flashedAt = h.clock.now()
		}
		return func(error) error { return nil }, nil
	}
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeFixed || res.Flashes != 2 || res.Backup == "" || res.MarkerProblem != "" {
		t.Fatalf("result %+v", res)
	}
	if got := h.commands(); got != "MUMU" {
		t.Errorf("commands %q, want MUMU", got)
	}
	if _, read := at[StageReadResult]; read {
		t.Error("the update drive was read as well")
	}
	if h.mixer.reportEjects != 1 || h.mixer.reportAcks != 1 {
		t.Errorf("the USB disk was acknowledged %d and ejected %d times", h.mixer.reportAcks, h.mixer.reportEjects)
	}
	// Back for good 60 s after the flash, the disk 20 s later, then the
	// eject's grace: far sooner than the 150 s settle.
	if d := at[StageReport].Sub(flashedAt); d < 80*time.Second || d > 82*time.Second {
		t.Errorf("report read %s after the flash", d)
	}
	f := h.mixer.flashes
	if len(f) != 2 || !f[0].carrier || f[1].carrier {
		t.Fatalf("flashes %+v", f)
	}
	if !bytes.Equal(f[0].script, carrier.ScriptFor(carrier.Options{USBReport: true})) {
		t.Error("the carrier script does not offer the USB report")
	}
	h.checkSafety(t)
}

// The disk vanishes as it is read (the slot was emptied on the mixer): the
// run keeps looking, reads it when the script offers it again, and goes
// on from there.
func TestReportVanishesThenIsRead(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.vanishOnce = true
	h.mixer.report = func(int) string { return fixedMarker }
	h.mixer.marker = func(int) string { return failedMarker }
	at := h.recordStages()
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeFixed || res.Flashes != 2 || h.commands() != "MUMU" {
		t.Fatalf("result %+v, commands %q", res, h.commands())
	}
	if m := h.mixer; m.reportReads != 2 || m.reportAcks != 1 || m.reportEjects != 1 {
		t.Errorf("reads %d, acknowledgements %d, ejects %d", m.reportReads, m.reportAcks, m.reportEjects)
	}
	if _, read := at[StageReadResult]; read {
		t.Error("the update drive was read as well")
	}
}

// The acknowledgement cannot be written: the disk is not ejected and does
// not count, the run keeps trying while the script offers it, and then
// waits long enough for the script to write the marker to the update
// drive, where it reads it.
func TestReportAckFails(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.ackFail = true
	h.mixer.report = func(int) string { return fixedMarker }
	h.mixer.marker = func(int) string { return alreadyMarker }
	at := h.recordStages()
	var flashedAt time.Time
	h.opts.Record = func(name, _ string) (func(error) error, error) {
		if name == "flash" && flashedAt.IsZero() {
			flashedAt = h.clock.now()
		}
		return func(error) error { return nil }, nil
	}
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeAlreadyFixed || res.Flashes != 2 || h.commands() != "MUMU" {
		t.Fatalf("result %+v, commands %q", res, h.commands())
	}
	if m := h.mixer; m.reportEjects != 0 || m.reportAcks < 2 {
		t.Errorf("acknowledgements tried %d times, ejects %d", m.reportAcks, m.reportEjects)
	}
	// Back for good 60 s after the flash, then the whole settle.
	if d := at[StageReadResult].Sub(flashedAt); d < 60*time.Second+reportSettle {
		t.Errorf("the update drive was read %s after the flash, before the script could have written to it", d)
	}
	h.checkSafety(t)
}

// A mixer that already had the fix, reported over USB, is left as it is:
// it runs RØDE's plain firmware and the script has removed itself.
func TestReportAlreadyFixedSkipsTheLastFlash(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.report = func(int) string { return alreadyMarker }
	h.mixer.marker = func(int) string { return failedMarker }
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeAlreadyFixed || res.Flashes != 1 || h.commands() != "MU" {
		t.Fatalf("result %+v, commands %q", res, h.commands())
	}
	if strings.Contains(res.Summary, "installed") {
		t.Errorf("summary %q", res.Summary)
	}
	h.checkSafety(t)
}

// With firmware of its own to install, the last flash still happens.
func TestReportAlreadyFixedStillInstallsTheFinalFirmware(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.report = func(int) string { return alreadyMarker }
	h.opts.FinalDir, h.opts.FinalMD5 = finalDir(t, firmware{})
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeAlreadyFixed || res.Flashes != 2 || h.commands() != "MUMU" {
		t.Fatalf("result %+v, commands %q", res, h.commands())
	}
	h.checkSafety(t)
}

// After the last flash the run ends finalSettle after the mixer is back,
// not after the full Settle.
func TestFinalWaitIsShort(t *testing.T) {
	h := newHarness(t, firmware{})
	h.mixer.oneBoot = true
	h.mixer.marker = func(int) string { return fixedMarker }
	at := h.recordStages()
	var lastFlash time.Time
	h.opts.Record = func(name, _ string) (func(error) error, error) {
		if name == "flash" {
			lastFlash = h.clock.now()
		}
		return func(error) error { return nil }, nil
	}
	if _, err := h.run(); err != nil {
		t.Fatal(err)
	}
	// Back 60 s after the flash, as in the other tests.
	if d := at[StageFinish].Sub(lastFlash); d < 60*time.Second+finalSettle || d > 63*time.Second+finalSettle {
		t.Errorf("finished %s after the last flash", d)
	}
}

// The settle wait after a carrier flash with the USB report covers the
// script's whole fallback, from the script's own defaults.
func TestReportSettle(t *testing.T) {
	if reportSettle != 288*time.Second {
		t.Fatalf("reportSettle is %s", reportSettle)
	}
	script := string(carrier.Script())
	for name, want := range map[string]int{
		"RCFIX_BOUND_SECS":    scriptBoundSecs,
		"RCFIX_REPORT_SECS":   scriptReportSecs,
		"RCFIX_POLL_SECS":     scriptPollSecs,
		"RCFIX_QUIET_POLLS":   scriptQuietPolls,
		"RCFIX_CONFIRM_POLLS": scriptConfirmPolls,
	} {
		line := fmt.Sprintf("%s=${%s:-%d}\n", name, name, want)
		if !strings.Contains(script, line) {
			t.Errorf("the script lacks %q", line)
		}
	}
	// Without the USB report the wait stays as it was.
	h := newHarness(t, firmware{})
	h.mixer.marker = func(int) string { return alreadyMarker }
	at := h.recordStages()
	var flashedAt time.Time
	h.opts.Record = func(name, _ string) (func(error) error, error) {
		if name == "flash" && flashedAt.IsZero() {
			flashedAt = h.clock.now()
		}
		return func(error) error { return nil }, nil
	}
	if _, err := h.run(); err != nil {
		t.Fatal(err)
	}
	if d := at[StageReadResult].Sub(flashedAt); d < 210*time.Second || d > 215*time.Second {
		t.Errorf("the update drive was read %s after the flash, want the 150 s settle", d)
	}
}

// No USB disk appears: the run is today's, and the script it installs
// offers the report only where the platform can read it.
func TestReportAbsent(t *testing.T) {
	for name, reports := range map[string]bool{"reporting platform": true, "other platform": false} {
		t.Run(name, func(t *testing.T) {
			h := newHarness(t, firmware{})
			if reports {
				h.opts.Platform = reporting{h.mixer}
			}
			h.mixer.marker = func(int) string { return alreadyMarker }
			at := h.recordStages()
			res, err := h.run()
			if err != nil {
				t.Fatal(err)
			}
			if res.Outcome != OutcomeAlreadyFixed || h.commands() != "MUMU" {
				t.Fatalf("result %+v, commands %q", res, h.commands())
			}
			if _, ok := at[StageReport]; ok {
				t.Error("a USB report was read")
			}
			if !bytes.Equal(h.mixer.flashes[0].script, carrier.ScriptFor(carrier.Options{USBReport: reports})) {
				t.Errorf("the carrier script's USB report is not %v", reports)
			}
		})
	}
}

// An unusable marker on the USB disk is not an outcome: the disk is ejected
// (the script then finishes without writing to the update drive), and the
// run goes on as today, which installs the fix again.
func TestReportUnparsable(t *testing.T) {
	h := reportingHarness(t)
	h.mixer.report = func(n int) string {
		if n == 1 {
			return "rcfix=1\nresult=fixed\n"
		}
		return ""
	}
	h.mixer.marker = func(int) string { return alreadyMarker }
	at := h.recordStages()
	res, err := h.run()
	if err != nil {
		t.Fatal(err)
	}
	if res.Outcome != OutcomeAlreadyFixed || res.Flashes != 3 {
		t.Fatalf("result %+v", res)
	}
	if got := h.commands(); got != "MUMUMU" {
		t.Errorf("commands %q", got)
	}
	if _, ok := at[StageReadResult]; !ok {
		t.Error("the update drive was never read")
	}
	if h.mixer.reportEjects != 1 {
		t.Errorf("the USB disk was ejected %d times", h.mixer.reportEjects)
	}
}

// The USB disk does not go away when ejected: the run waits as today and
// reads the update drive; where that has no marker, the one from the USB
// disk counts.
func TestReportEjectNotAcknowledged(t *testing.T) {
	for name, drive := range map[string]string{"drive has it": alreadyMarker, "drive lacks it": ""} {
		t.Run(name, func(t *testing.T) {
			h := reportingHarness(t)
			h.mixer.ignoreEject = true
			h.mixer.report = func(int) string { return fixedMarker }
			h.mixer.marker = func(int) string { return drive }
			at := h.recordStages()
			res, err := h.run()
			if err != nil {
				t.Fatal(err)
			}
			want := OutcomeFixed
			if drive != "" {
				want = OutcomeAlreadyFixed
			}
			if res.Outcome != want || res.Flashes != 2 {
				t.Fatalf("result %+v", res)
			}
			if got := h.commands(); got != "MUMU" {
				t.Errorf("commands %q", got)
			}
			if _, ok := at[StageReadResult]; !ok {
				t.Error("the update drive was never read")
			}
			// Ejecting is tried again while the disk stays.
			if h.mixer.reportEjects < 1 {
				t.Errorf("the USB disk was ejected %d times", h.mixer.reportEjects)
			}
		})
	}
}
