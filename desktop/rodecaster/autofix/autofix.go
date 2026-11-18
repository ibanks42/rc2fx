// Package autofix grows partition 1 of a stock RØDECaster without storage
// mode, through RØDE's own USB update. The mixer is flashed with RØDE's
// archive for the version it already runs plus the carrier script, which
// fixes the partition table on the mixer and leaves a marker on the update
// drive; the marker is read back, and RØDE's plain archive is flashed again
// so the mixer ends on a stock system whose freshly formatted slot has 4 KB
// blocks.
//
// Where the platform can see the mixer's USB disk in normal mode
// (flash.CanReport), the script is also asked to offer its marker there
// (carrier.Options.USBReport). While waiting for the mixer after the carrier
// flash, Run then reads the marker from that disk as soon as it appears,
// writes carrier.AckName onto it and ejects it, which tells the script to
// finish, and goes straight on to the last flash: no settle wait and no
// update-mode read. A disk that vanishes before it is read, or that could
// not be acknowledged, is looked for again (the script offers it again);
// a run that never completes the acknowledgement waits long enough for the
// script to give up on the disk and write the marker to the update drive,
// and reads it there as before (reportSettle).
//
// An installer can have the last flash carry its own firmware instead of
// RØDE's plain archive (Options.FinalDir), so a mixer that is getting new
// firmware anyway is updated twice rather than three times.
//
// A mixer that already had the fix, reported and acknowledged over USB, is
// left as it is when no FinalDir is given: it already runs RØDE's plain
// firmware on a 4 KB slot, the script has removed itself, and a second
// flash would change nothing.
//
// Run works on a platform.Platform and has no UI, so it is tested against a
// simulated mixer and any front end can drive it.
package autofix

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/fix"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/rode"
)

// Outcome is how a run ended.
type Outcome string

const (
	// OutcomeFixed: the script grew partition 1 and RØDE's plain firmware
	// was installed over the fixed slot.
	OutcomeFixed Outcome = "fixed"
	// OutcomeAlreadyFixed: partition 1 already had the grown size.
	OutcomeAlreadyFixed Outcome = "already-fixed"
	// OutcomeUnrecognized: the table is not RØDE's factory layout; the
	// script changed nothing.
	OutcomeUnrecognized Outcome = "unrecognized"
	// OutcomeFailed: the script could not finish; it says why.
	OutcomeFailed Outcome = "failed"
	// OutcomeUnconfirmed: the script left no result after every attempt.
	OutcomeUnconfirmed Outcome = "unconfirmed"
)

// Stages of Event. They are stable words a front end may match on.
const (
	StageFind       = "find"
	StageDownload   = "download"
	StageInspect    = "inspect"
	StageBuild      = "build"
	StageUpdateMode = "update-mode"
	StageCopy       = "copy"
	StageFlash      = "flash"
	StageWait       = "wait"
	// StageReport: reading the script's result from the mixer's USB disk
	// in normal mode, during the wait after a carrier flash.
	StageReport     = "report"
	StageReadResult = "read-result"
	StageFinish     = "finish"
)

// Flash says which flash an event belongs to.
type Flash struct {
	// Number counts from 1; a run does at most four.
	Number int `json:"number"`
	// Carrier: the archive carries the script (otherwise it is RØDE's
	// plain archive).
	Carrier bool `json:"carrier"`
}

// MaxFlashes is the most flashes a run does: three with the script, one
// plain.
const MaxFlashes = carrierAttempts + 1

const carrierAttempts = 3

// Event is one step of a run.
type Event struct {
	Stage string `json:"stage"`
	Text  string `json:"text"`
	// Completed and Total are progress numbers when Total is not zero:
	// bytes, or seconds while waiting for the mixer to settle.
	Completed uint64 `json:"completed,omitempty"`
	Total     uint64 `json:"total,omitempty"`
	Flash     *Flash `json:"flash,omitempty"`
	// SafeToClose: stopping the program now leaves the mixer usable. It is
	// false from the update-mode command until the mixer has accepted the
	// flash command.
	SafeToClose bool `json:"safe_to_close"`
}

// Result is a finished run.
type Result struct {
	Outcome Outcome `json:"outcome"`
	// Marker is what the script reported, nil when it reported nothing.
	Marker *carrier.Marker `json:"marker,omitempty"`
	// MarkerProblem is why the last marker read could not be used.
	MarkerProblem string `json:"marker_problem,omitempty"`
	// Backup is the saved copy of sector 0 from before the fix.
	Backup string `json:"backup,omitempty"`
	// BackupProblem is why no backup could be saved although the script
	// sent one.
	BackupProblem string `json:"backup_problem,omitempty"`
	// Model is the model ID (model.Model.ID).
	Model    string `json:"model"`
	Serial   string `json:"serial"`
	Firmware string `json:"firmware"`
	Flashes  int    `json:"flashes"`
	Summary  string `json:"summary"`
}

// Options is one run.
type Options struct {
	Platform platform.Platform
	// Work is a folder for the downloaded and built archives. On success
	// both are removed unless KeepWork; after an error the download is kept
	// so a new run does not fetch it again.
	Work string
	// BackupDir is where the copy of sector 0 the script sends is saved.
	BackupDir string
	// Client is the HTTP client for RØDE's server; rode.HTTPClient when nil.
	Client func() *http.Client
	Events func(Event)
	// Cancel, when set, is asked at points where stopping is safe; an error
	// stops the run with that error.
	Cancel func() error
	// Allowed refuses models; ProOnly when nil. The Duo is unverified.
	Allowed flash.Allowed
	// Settle is how long the mixer must stay present in normal mode, with
	// no disappearance, before it counts as back (default 150 s). The
	// script writes its marker about 90 s after RØDE's software starts.
	// After a carrier flash whose script offers the USB report the wait is
	// at least reportSettle, long enough for the script to give up on the
	// USB disk first; an acknowledged report ends the wait at once. After
	// the last flash the wait is at most finalSettle.
	Settle time.Duration
	// ReturnLimit is the longest wait for the mixer to come back after a
	// flash (default 15 minutes).
	ReturnLimit time.Duration
	// UpdateModeLimit is the longest wait for the update drive after the
	// update-mode command (default 2 minutes).
	UpdateModeLimit time.Duration
	// Poll is how often the mixer is looked for while waiting (default 1 s).
	Poll time.Duration
	// Version is the firmware version to download, for when the mixer's
	// USB release number does not give a usable one.
	Version string
	// LocalArchive and LocalMD5 are RØDE's os.tar.gz and os.md5 already on
	// this computer, used instead of downloading; the archive is still
	// checked against the MD5.
	LocalArchive, LocalMD5 string
	// ExtraEntries are added to the carrier archive only. For development:
	// they let a tester put a file on the mixer to watch it. The second,
	// plain flash replaces that system slot's contents with RØDE's again.
	ExtraEntries []archive.Entry
	KeepWork     bool
	// FinalDir and FinalMD5 are firmware to flash last instead of RØDE's
	// plain archive: flash.UpdateFiles in FinalDir, the archive with MD5
	// FinalMD5. The carrier flash still uses RØDE's archive, which is still
	// downloaded (or taken from LocalArchive).
	FinalDir, FinalMD5 string
	// Record logs each USB command before it is sent (flash.Options.Record).
	Record flash.Record
	// DebugLog has the carrier script keep its debug log on the mixer
	// (carrier.Options.DebugLog). For development builds only.
	DebugLog bool

	// Replaced in tests.
	now   func() time.Time
	sleep func(time.Duration)
}

// ProOnly allows the RØDECaster Pro II only.
func ProOnly(m model.Model) error {
	if m == model.ProIi {
		return nil
	}
	return fmt.Errorf("this fix has only been tried on the RØDECaster Pro II, so it does not run on the %s yet. Nothing was changed", m.Label())
}

// Refusal is the error of a run that would not start because the mixer or
// its firmware is not one the fix supports. Nothing was changed, and
// starting again gives the same answer.
type Refusal struct{ Err error }

func (r *Refusal) Error() string { return r.Err.Error() }
func (r *Refusal) Unwrap() error { return r.Err }

// ErrCanceled is wrapped by the error of a run stopped through
// Options.Cancel.
var ErrCanceled = errors.New("stopped")

// Members the carrier script needs in the firmware it runs on.
var requiredMembers = [][]string{
	{"usr/sbin/sfdisk"},
	{"usr/sbin/resizepart"},
	{"sbin/fw_setenv"},
	{"bin/bash", "bin/bash.bash"},
	{"usr/bin/sha256sum"},
	{"usr/bin/cmp"},
	{"usr/bin/setsid", "usr/bin/nohup"},
	{"etc/rc5.d/S03rode-audio-service"},
}

const (
	maxDownload = 600 << 20
	maxMarker   = 1<<20 + 1
	plainDir    = "plain"
	carrierDir  = "carrier"
)

// The USB report's timing on this side. The disk is read, acknowledged and
// ejected at the first poll that sees it. After the eject the script needs
// up to a second (it polls every second) to see the slot empty and find the
// acknowledgement; ackGrace keeps the next update-mode command, after which
// RØDE's program reuses the disk slot, mostly behind that. Script version 4
// also finds the acknowledgement when RØDE's program got there first. A
// sighting that did not end in an acknowledgement is tried again after
// reportRetry; the script offers the disk again within about a second.
const (
	ejectLimit  = 20 * time.Second
	ackGrace    = 2 * time.Second
	reportRetry = 3 * time.Second
)

// The carrier script's defaults that bound its USB report and its fallback
// to the update drive (rcfix.sh; a test checks they match).
const (
	scriptBoundSecs    = 6   // RCFIX_BOUND_SECS
	scriptReportSecs   = 120 // RCFIX_REPORT_SECS
	scriptPollSecs     = 2   // RCFIX_POLL_SECS
	scriptQuietPolls   = 5   // RCFIX_QUIET_POLLS
	scriptConfirmPolls = 30  // RCFIX_CONFIRM_POLLS
)

// finalSettle is how long the mixer must stay present in normal mode after
// the last flash. On every hardware run the mixer, once it showed up in
// normal mode after a flash, stayed until the app's next command (stock
// firmware's first-boot restart happens before USB comes up), so this only
// rides out a glitch; an installer checks its own firmware afterwards.
const finalSettle = 10 * time.Second

// reportSettle is the settle wait after a carrier flash whose script offers
// the USB report, counted from when the mixer is back in normal mode (the
// gadget is bound then). Without an acknowledgement the script holds the
// disk slot for at most RCFIX_REPORT_SECS from the start of its report,
// which starts no later than about the bind and whose first attach waits
// RCFIX_BOUND_SECS: 6 + 120 = 126 s. It then turns to the update drive:
// RCFIX_QUIET_POLLS quiet polls plus the one that writes, 6 x 2 = 12 s, and
// watches the marker for RCFIX_CONFIRM_POLLS polls, 30 x 2 = 60 s, before
// it finishes: 126 + 12 + 60 = 198 s. reportMargin covers a report that
// starts after the bind (when the script does not see RØDE's wipe it waits
// up to about a minute longer) and the time USB takes to show the mixer:
// 198 + 90 = 288 s.
const (
	reportMargin = 90 * time.Second
	reportSettle = (scriptBoundSecs+scriptReportSecs+(scriptQuietPolls+1)*scriptPollSecs+scriptConfirmPolls*scriptPollSecs)*time.Second + reportMargin
)

// RC2FX's advice for a mixer left in update mode with nothing flashed.
const (
	powerCycle      = "Disconnect the mixer's power for 30 seconds, turn it on and start again"
	powerCycleLower = "disconnect the mixer's power for 30 seconds, turn it on and start again"
)

type run struct {
	o       Options
	p       platform.Platform
	model   model.Model
	serial  string
	version string
	flashes int
	// current is the flash under way or last done.
	current *Flash
	// safe is SafeToClose for the next events.
	safe bool
	// flashSent: Flash has sent the flash command of the current flash.
	flashSent bool
	// updateMode: the mixer was left in update mode (by a marker read).
	updateMode bool
	// report: the script offers its marker on the USB disk, which this
	// platform can read (flash.CanReport).
	report bool
	last   Event
}

// Run runs the whole fix. An error means the run stopped early; its text
// says what state the mixer is in and what to do.
func Run(o Options) (Result, error) {
	if o.Platform == nil || o.Work == "" || o.BackupDir == "" {
		return Result{}, errors.New("autofix needs a platform, a work folder and a backup folder")
	}
	if o.Client == nil {
		o.Client = rode.HTTPClient
	}
	if o.Allowed == nil {
		o.Allowed = ProOnly
	}
	if o.Settle <= 0 {
		o.Settle = 150 * time.Second
	}
	if o.ReturnLimit <= 0 {
		o.ReturnLimit = 15 * time.Minute
	}
	if o.UpdateModeLimit <= 0 {
		o.UpdateModeLimit = 2 * time.Minute
	}
	if o.Poll <= 0 {
		o.Poll = time.Second
	}
	if o.now == nil {
		o.now = time.Now
	}
	if o.sleep == nil {
		o.sleep = time.Sleep
	}
	if (o.LocalArchive == "") != (o.LocalMD5 == "") {
		return Result{}, errors.New("a local firmware archive needs its MD5 file, and the other way round")
	}
	if (o.FinalDir == "") != (o.FinalMD5 == "") {
		return Result{}, errors.New("the final firmware needs its MD5, and the other way round")
	}
	if o.FinalDir != "" {
		md, err := parseMD5([]byte(o.FinalMD5))
		if err != nil {
			return Result{}, fmt.Errorf("the final firmware's MD5: %v", err)
		}
		o.FinalMD5 = md
	}
	r := &run{o: o, p: o.Platform, safe: true, report: flash.CanReport(o.Platform)}
	res, err := r.run()
	if !o.KeepWork {
		os.RemoveAll(filepath.Join(o.Work, carrierDir))
		if err == nil {
			os.RemoveAll(filepath.Join(o.Work, plainDir))
		}
	}
	return res, err
}

func (r *run) emit(stage, text string) {
	r.event(Event{Stage: stage, Text: text})
}

func (r *run) event(e Event) {
	e.SafeToClose = r.safe
	if e.Flash == nil && r.current != nil && stageHasFlash(e.Stage) {
		f := *r.current
		e.Flash = &f
	}
	r.last = e
	if r.o.Events != nil {
		r.o.Events(e)
	}
}

func stageHasFlash(stage string) bool {
	switch stage {
	case StageUpdateMode, StageCopy, StageFlash, StageWait, StageReport, StageReadResult:
		return true
	}
	return false
}

// progress throttles byte progress to about one event per percent.
func (r *run) progress(stage, text string) func(completed, total uint64) {
	var last uint64
	started := false
	return func(completed, total uint64) {
		if started && completed != total && total > 0 && completed-last < total/100 {
			return
		}
		started, last = true, completed
		r.event(Event{Stage: stage, Text: text, Completed: completed, Total: total})
	}
}

func (r *run) canceled() error {
	if r.o.Cancel == nil {
		return nil
	}
	if err := r.o.Cancel(); err != nil {
		return fmt.Errorf("%w: %v", ErrCanceled, err)
	}
	return nil
}

func (r *run) run() (Result, error) {
	if err := r.find(); err != nil {
		return Result{}, err
	}
	res := Result{Model: r.model.ID(), Serial: r.serial, Firmware: r.version}
	if err := r.canceled(); err != nil {
		return res, fmt.Errorf("%w. Nothing was changed", err)
	}
	plain := filepath.Join(r.o.Work, plainDir)
	plainMD5, err := r.download(plain)
	if err != nil {
		return res, err
	}
	if err := r.canceled(); err != nil {
		return res, fmt.Errorf("%w. Nothing was changed", err)
	}
	if err := r.inspect(filepath.Join(plain, flash.UpdateFiles[0])); err != nil {
		return res, err
	}
	carried := filepath.Join(r.o.Work, carrierDir)
	carrierMD5, err := r.build(filepath.Join(plain, flash.UpdateFiles[0]), carried)
	if err != nil {
		return res, err
	}
	if err := r.canceled(); err != nil {
		return res, fmt.Errorf("%w. Nothing was changed", err)
	}

	var marker *carrier.Marker
	problem := ""
	// acked: the marker came over USB and the script has finished, so the
	// mixer is in normal mode running RØDE's plain firmware.
	acked := false
	for attempt := 1; attempt <= carrierAttempts; attempt++ {
		// After the first attempt the marker read left the mixer in update
		// mode.
		if err := r.flash(carried, carrierMD5, true, r.updateMode); err != nil {
			res.Flashes = r.flashes
			return res, err
		}
		got, err := r.waitBack(true)
		if err != nil {
			res.Flashes = r.flashes
			return res, r.waitError(err, nil)
		}
		if got != nil && got.acked && got.marker != nil {
			// The script has finished and left the update drive alone.
			marker, problem, acked = got.marker, "", true
			break
		}
		marker, problem, err = r.readMarker()
		if err != nil {
			res.Flashes = r.flashes
			return res, err
		}
		if marker == nil && got != nil {
			// The USB disk's marker is the script's own result even when
			// its eject was not seen to arrive.
			if got.marker != nil {
				marker, problem = got.marker, ""
			} else if got.problem != "" {
				problem = got.problem
			}
		}
		if marker != nil {
			break
		}
		if attempt < carrierAttempts {
			r.emit(StageReadResult, fmt.Sprintf("The fix has not reported a result yet (%s). Installing it again, attempt %d of %d.", problem, attempt+1, carrierAttempts))
		} else {
			r.emit(StageReadResult, fmt.Sprintf("The fix has not reported a result after %d attempts (%s). Putting RØDE's firmware back.", carrierAttempts, problem))
		}
	}
	res.Marker, res.MarkerProblem = marker, problem

	if marker != nil && (marker.Result == carrier.ResultFixed || marker.Result == carrier.ResultAlreadyFixed) && marker.Sector0Before != nil {
		res.Backup, res.BackupProblem = saveBackup(r.o.BackupDir, marker.Sector0Before)
	}
	switch {
	case marker == nil:
		res.Outcome = OutcomeUnconfirmed
	default:
		res.Outcome = Outcome(marker.Result)
	}
	final := r.o.FinalDir != ""
	if !final && acked && !r.updateMode && res.Outcome == OutcomeAlreadyFixed {
		res.Flashes = r.flashes
		res.Summary = "The mixer already had the fix. Nothing on it was changed, and it runs RØDE's firmware as before."
		r.current = nil
		r.emit(StageFinish, res.Summary)
		return res, nil
	}
	res.Summary = summary(&res, final)

	// The mixer is in update mode after the marker read, or in normal mode
	// after the USB report. RØDE's plain archive (or the final firmware)
	// goes onto the slot that was just fixed, and is the clean way out of
	// update mode whatever the script said.
	last, lastMD5, step := plain, plainMD5, "putting RØDE's own firmware back"
	if final {
		last, lastMD5, step = r.o.FinalDir, r.o.FinalMD5, "installing the new firmware"
	}
	if err := r.flash(last, lastMD5, false, r.updateMode); err != nil {
		res.Flashes = r.flashes
		return res, fmt.Errorf("%s\n\nThe last step, %s, did not finish: %w", res.Summary, step, err)
	}
	if _, err := r.waitBack(false); err != nil {
		res.Flashes = r.flashes
		return res, r.waitError(err, &res)
	}
	res.Flashes = r.flashes
	r.current = nil
	r.emit(StageFinish, res.Summary)
	return res, nil
}

// find selects the one connected mixer and its firmware version.
func (r *run) find() error {
	r.emit(StageFind, "Looking for the mixer")
	inventory, err := r.p.Inventory()
	if err != nil {
		return fmt.Errorf("the mixer could not be looked for: %v. Nothing was changed", err)
	}
	if err := flash.Refuse(inventory, "", r.o.Allowed); err != nil {
		return &Refusal{err}
	}
	mixers := flash.Mixers(inventory)
	switch len(mixers) {
	case 0:
		return errors.New("no RØDECaster was found. Turn the mixer on, connect its USB 1 port to this computer and start again. Nothing was changed")
	case 1:
	default:
		return errors.New("more than one RØDECaster is connected. Connect only the one to fix and start again. Nothing was changed")
	}
	m := mixers[0]
	if m.UpdateMode {
		return fmt.Errorf("the mixer is in USB update mode. %s; nothing was changed", powerCycle)
	}
	if err := flash.RequireRealSerial(m.Serial); err != nil {
		return err
	}
	r.model, r.serial = m.Model, m.Serial
	switch {
	case r.o.Version != "":
		r.version = r.o.Version
	case m.Firmware != nil:
		r.version = *m.Firmware
	default:
		return errors.New("the mixer did not report its firmware version over USB, so the fix can't tell which of RØDE's firmware to use. Nothing was changed")
	}
	if !validVersion(r.version) {
		return fmt.Errorf("%q is not a firmware version like 1.7.7. Nothing was changed", r.version)
	}
	r.emit(StageFind, fmt.Sprintf("Found a %s with firmware %s", r.model.Label(), r.version))
	return nil
}

func validVersion(v string) bool {
	parts := strings.Split(v, ".")
	if len(parts) != 3 {
		return false
	}
	for _, p := range parts {
		if p == "" || len(p) > 4 || strings.Trim(p, "0123456789") != "" {
			return false
		}
	}
	return true
}

// workDir makes a folder in the work folder, owned by the person a
// privileged run works for.
func workDir(dir string) error {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	f, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer f.Close()
	return archive.LocalOwner(f)
}

// parseMD5 is the MD5 in RØDE's os.md5: the digest alone, or md5sum's
// "digest  name" line.
func parseMD5(data []byte) (string, error) {
	fields := strings.Fields(strings.ToValidUTF8(string(data), "�"))
	if len(fields) == 0 {
		return "", errors.New("the MD5 file is empty")
	}
	md := strings.ToLower(fields[0])
	if len(md) != 32 || strings.Trim(md, "0123456789abcdef") != "" {
		return "", errors.New("the MD5 file does not hold an MD5")
	}
	return md, nil
}

// download puts RØDE's archive for the mixer's version and its archive.md5
// in dir, checked against RØDE's MD5. It returns that MD5.
func (r *run) download(dir string) (string, error) {
	if err := workDir(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be made: %v. Nothing was changed", err)
	}
	dest := filepath.Join(dir, flash.UpdateFiles[0])
	var md string
	if r.o.LocalArchive != "" {
		r.emit(StageDownload, "Checking RØDE's firmware, downloaded earlier")
		data, err := readSmall(r.o.LocalMD5, 4096)
		if err == nil {
			md, err = parseMD5(data)
		}
		if err != nil {
			return "", fmt.Errorf("the checksum of RØDE's firmware, downloaded earlier, could not be used (%v). Nothing was changed", err)
		}
		if err := copyChecked(r.o.LocalArchive, dest, md, r.progress(StageDownload, "Copying RØDE's firmware")); err != nil {
			return "", fmt.Errorf("RØDE's firmware, downloaded earlier, could not be used (%v). Nothing was changed", err)
		}
	} else {
		base := fmt.Sprintf("https://update.rode.com/%s/%s/", r.model.FirmwareDirectory(), r.version)
		archiveURL, err1 := rode.Location(r.model, base+"os.tar.gz", "os.tar.gz", r.version)
		digestURL, err2 := rode.Location(r.model, base+"os.md5", "os.md5", r.version)
		if err := errors.Join(err1, err2); err != nil {
			return "", fmt.Errorf("%v. Nothing was changed", err)
		}
		r.emit(StageDownload, fmt.Sprintf("Asking RØDE's server for firmware %s", r.version))
		digest, err := rode.Get(r.o.Client(), digestURL, 128)
		if err == nil {
			md, err = parseMD5(digest)
		}
		if err != nil {
			return "", fmt.Errorf("RØDE's checksum for firmware %s could not be fetched (%v). Check the internet connection and start again. Nothing was changed", r.version, err)
		}
		text := fmt.Sprintf("Downloading RØDE's firmware %s", r.version)
		bytes := r.progress(StageDownload, text)
		check := r.progress(StageDownload, "Checking the download")
		err = rode.DownloadFile(rode.Download{
			Client: r.o.Client,
			URL:    archiveURL,
			Dest:   dest,
			MD5:    md,
			Limit:  maxDownload,
			Progress: func(phase string, completed, total uint64, _ string) {
				if phase == "download" {
					bytes(completed, total)
				} else {
					check(completed, total)
				}
			},
			Cancel: r.canceled,
		})
		if errors.Is(err, ErrCanceled) {
			return "", fmt.Errorf("%w. Nothing was changed", err)
		}
		if err != nil {
			return "", fmt.Errorf("RØDE's firmware %s could not be downloaded (%v). Check the internet connection and start again. Nothing was changed", r.version, err)
		}
	}
	if err := archive.AtomicFile(filepath.Join(dir, flash.UpdateFiles[1]), []byte(md)); err != nil {
		return "", fmt.Errorf("the work folder could not be written: %v. Nothing was changed", err)
	}
	return md, nil
}

func readSmall(path string, limit int64) ([]byte, error) {
	f, err := archive.OpenRegular(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return io.ReadAll(io.LimitReader(f, limit))
}

// copyChecked copies src to dst through a private temporary file, keeping
// it only when what was written has MD5 md.
func copyChecked(src, dst, md string, progress func(uint64, uint64)) error {
	in, err := archive.OpenRegular(src)
	if err != nil {
		return err
	}
	defer in.Close()
	info, err := in.Stat()
	if err != nil {
		return err
	}
	if info.Size() > maxDownload {
		return errors.New("the file is larger than any RØDE firmware")
	}
	t, err := archive.NewTemp(filepath.Dir(dst), ".local-")
	if err != nil {
		return err
	}
	defer t.Cleanup()
	hashed := archive.HashingWriter(t)
	total := uint64(info.Size())
	buf := make([]byte, 1<<20)
	var done uint64
	progress(0, total)
	for {
		n, err := in.Read(buf)
		if n > 0 {
			if _, err := hashed.Write(buf[:n]); err != nil {
				return err
			}
			done += uint64(n)
			progress(done, total)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	if _, got := hashed.Finish(); got != md {
		return errors.New("the file does not match its MD5")
	}
	return t.Persist(dst)
}

// inspect refuses an archive the carrier script cannot run on, reading its
// member list once.
func (r *run) inspect(path string) error {
	r.emit(StageInspect, "Checking that the fix can run on this firmware")
	names := map[string]bool{}
	err := archive.Walk(path, 4<<30, false, func(m *archive.Member) error {
		names[archive.Normalized(m.Path)] = true
		return nil
	})
	if err != nil {
		return fmt.Errorf("RØDE's firmware file could not be read (%v). Nothing was changed", err)
	}
	var missing []string
	for _, alternatives := range requiredMembers {
		found := false
		for _, name := range alternatives {
			found = found || names[name]
		}
		if !found {
			missing = append(missing, "/"+strings.Join(alternatives, " or /"))
		}
	}
	var present []string
	for _, path := range carrier.Paths() {
		if names[path] {
			present = append(present, "/"+path)
		}
	}
	if len(missing) == 0 && len(present) == 0 {
		return nil
	}
	why := ""
	if len(missing) > 0 {
		why = "it has no " + strings.Join(missing, ", ")
	}
	if len(present) > 0 {
		if why != "" {
			why += "; "
		}
		why += "it already contains " + strings.Join(present, ", ")
	}
	return &Refusal{fmt.Errorf("firmware %s is not supported by this fix yet (%s). Nothing was changed", r.version, why)}
}

// build writes RØDE's archive plus the carrier files and o.ExtraEntries to
// dir, with its archive.md5. It returns the new archive's MD5.
func (r *run) build(src, dir string) (string, error) {
	r.emit(StageBuild, "Adding the fix to RØDE's firmware")
	if err := os.RemoveAll(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be cleared: %v. Nothing was changed", err)
	}
	if err := workDir(dir); err != nil {
		return "", fmt.Errorf("the work folder could not be made: %v. Nothing was changed", err)
	}
	var extra []archive.Entry
	for _, f := range carrier.FilesFor(carrier.Options{USBReport: r.report, DebugLog: r.o.DebugLog}) {
		e := archive.Entry{Kind: archive.EntryFile, Path: f.Path, Mode: uint32(f.Mode.Perm()), Data: f.Data}
		if f.Symlink {
			e = archive.Entry{Kind: archive.EntryLink, Path: f.Path, Mode: uint32(f.Mode.Perm()), Target: f.Target}
		}
		extra = append(extra, e)
	}
	extra = append(extra, r.o.ExtraEntries...)
	patched, err := archive.Patch(src, filepath.Join(dir, flash.UpdateFiles[0]), archive.PatchOptions{Model: r.model, Extra: extra})
	if err != nil {
		return "", fmt.Errorf("the fix could not be added to RØDE's firmware (%v). Nothing was changed", err)
	}
	if err := archive.AtomicFile(filepath.Join(dir, flash.UpdateFiles[1]), []byte(patched.ArchiveMD5)); err != nil {
		return "", fmt.Errorf("the work folder could not be written: %v. Nothing was changed", err)
	}
	return patched.ArchiveMD5, nil
}

func (r *run) flashOptions() flash.Options {
	return flash.Options{
		Allowed: r.o.Allowed,
		Record:  r.o.Record,
		Step: func(phase, detail string) {
			switch phase {
			case "usb-mode":
				r.emit(StageUpdateMode, "Switching the mixer to USB update mode")
			case "eject":
				r.emit(StageCopy, "Safely ejecting the mixer's update drive")
			case "flash":
				r.flashSent = true
				r.emit(StageFlash, "Starting the update")
			default:
				r.emit(StageCopy, detail)
			}
		},
		Progress: func(phase, detail string) func(uint64, uint64) {
			if phase == "verify" {
				return r.progress(StageCopy, "Reading the firmware back from the mixer")
			}
			return r.progress(StageCopy, "Copying "+detail+" to the mixer")
		},
	}
}

// verify checks the staged files on the drive against md.
func verify(md string) flash.Verify {
	return func(drive string, progress func(uint64, uint64)) error {
		_, got, err := archive.HashBoth(filepath.Join(drive, flash.UpdateFiles[0]), progress)
		if err != nil {
			return err
		}
		if got != md {
			return errors.New("the firmware read back from the mixer's update drive does not match what was copied")
		}
		data, err := flash.ReadFile(drive, flash.UpdateFiles[1], 128)
		if err != nil {
			return err
		}
		if string(archive.TrimASCII(data)) != md {
			return errors.New("archive.md5 read back from the mixer's update drive does not match what was copied")
		}
		return nil
	}
}

// inUpdateMode is the error of a step that left the mixer in update mode
// with nothing flashed.
func inUpdateMode(err error) error {
	return fmt.Errorf("%v.\n\nThe mixer was left in USB update mode and nothing was flashed. %s", strings.TrimSuffix(err.Error(), "."), powerCycle)
}

// flash does one USB update with the files in dir. inUpdateMode: the
// mixer is already in update mode.
func (r *run) flash(dir, md string, carried, already bool) error {
	r.current = &Flash{Number: r.flashes + 1, Carrier: carried}
	r.flashSent = false
	r.safe = false
	opts := r.flashOptions()
	if !already {
		if err := flash.EnterUpdateMode(r.p, r.model, r.serial, opts); err != nil {
			return fmt.Errorf("%v.\n\nIf the mixer shows \"Updating via USB\", %s; nothing was flashed", strings.TrimSuffix(err.Error(), "."), powerCycleLower)
		}
	}
	if err := r.waitUpdateDrive(StageUpdateMode); err != nil {
		return err
	}
	if carried {
		if err := r.removeOldMarker(); err != nil {
			return inUpdateMode(err)
		}
	}
	err := flash.Flash(r.p, r.model, r.serial, dir, verify(md), opts)
	if r.flashSent {
		r.updateMode = false
	}
	if err != nil {
		if r.flashSent {
			return fmt.Errorf("%v.\n\nThe mixer may have started the update anyway. Leave it switched on and connected for 15 minutes. If it then shows its normal screen, start again; if it still shows \"Updating via USB\", %s", strings.TrimSuffix(err.Error(), "."), powerCycleLower)
		}
		return inUpdateMode(err)
	}
	r.flashes++
	r.safe = true
	return nil
}

// waitUpdateDrive waits for the mixer in update mode with its update drive.
// It cannot be canceled: the mixer is already in update mode.
func (r *run) waitUpdateDrive(stage string) error {
	r.emit(stage, "Waiting for the mixer's update drive")
	deadline := r.o.now().Add(r.o.UpdateModeLimit)
	for {
		inventory, err := r.p.Inventory()
		if err == nil {
			if _, err := flash.SelectHID(inventory, r.model, true, r.serial, r.o.Allowed); err == nil {
				volumes, err := r.p.Volumes()
				if err == nil {
					for _, v := range volumes {
						if v.Serial == r.serial && flash.UpdateSize(v.Total) {
							return nil
						}
					}
				}
			}
		}
		if !r.o.now().Before(deadline) {
			return fmt.Errorf("the mixer's update drive did not appear within %s. If the mixer shows \"Updating via USB\", %s; nothing was flashed", minutes(r.o.UpdateModeLimit), powerCycleLower)
		}
		r.o.sleep(r.o.Poll)
	}
}

// removeOldMarker deletes a marker an earlier run may have left on the
// update drive, so the next one read is this run's.
func (r *run) removeOldMarker() error {
	v, err := flash.UpdateDrive(r.p, r.serial)
	if err != nil {
		return err
	}
	path := filepath.Join(v.Path, carrier.MarkerName)
	info, err := os.Lstat(path)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return nil
	case err != nil:
		return err
	case !info.Mode().IsRegular():
		return fmt.Errorf("%s on the mixer's update drive is not a file", carrier.MarkerName)
	}
	return os.Remove(path)
}

// errTimeout is a wait for the mixer that ran out of time.
var errTimeout = errors.New("timeout")

// report is a marker read from the mixer's USB disk while waiting.
type report struct {
	// marker is nil when it could not be used; problem then says why.
	marker  *carrier.Marker
	problem string
	// acked: the acknowledgement was written onto the disk, and the disk was
	// ejected and went away, so the script has finished without writing to
	// the update drive.
	acked bool
}

// waitBack waits until the mixer has been present in normal mode, with no
// disappearance, for Settle (at most finalSettle after a plain flash). After a carrier flash (carried) whose script
// offers the USB report it also looks for that report on every poll, and
// returns as soon as one is acknowledged; the last report it read, if any,
// comes back either way. It can be canceled: the mixer finishes on its own.
func (r *run) waitBack(carried bool) (*report, error) {
	settle := r.o.Settle
	if !carried && settle > finalSettle {
		settle = finalSettle
	}
	look := carried && r.report
	if look && settle < reportSettle {
		settle = reportSettle
	}
	start := r.o.now()
	var since, nextLook time.Time
	var got *report
	lastText, lastAt := "", time.Time{}
	for {
		if err := r.canceled(); err != nil {
			return got, err
		}
		now := r.o.now()
		if r.present() {
			if since.IsZero() {
				since = now
			}
			if now.Sub(since) >= settle {
				return got, nil
			}
			if look && !now.Before(nextLook) {
				if v, ok, err := flash.ReportDisk(r.p, r.serial); err == nil && ok {
					took := r.takeReport(v)
					// A marker read earlier stays when a later try could not
					// read one: it is the script's result all the same.
					if got == nil || took.marker != nil || got.marker == nil {
						got = took
					} else {
						got.acked = got.acked || took.acked
					}
					switch {
					case got.acked && got.marker != nil:
						return got, nil
					case got.acked:
						// The script has finished; nothing more comes on the disk.
						look = false
					default:
						// The script offers the disk again after the slot
						// was emptied without the acknowledgement, or keeps
						// it until its limit.
						nextLook = r.o.now().Add(reportRetry)
					}
					lastText = ""
				}
			}
		} else {
			since = time.Time{}
		}
		if now.Sub(start) >= r.o.ReturnLimit {
			return got, errTimeout
		}
		// The page around this line already says that the mixer restarts
		// and must be left alone.
		e := Event{Stage: StageWait, Text: "Waiting for the mixer to restart."}
		if !since.IsZero() {
			e.Text = "The mixer is running again; waiting to be sure it has finished."
			if carried {
				e.Text = "The mixer is running again; waiting for it to confirm the fix."
			}
			e.Completed, e.Total = uint64(now.Sub(since)/time.Second), uint64(settle/time.Second)
		}
		if e.Text != lastText || now.Sub(lastAt) >= 5*time.Second {
			lastText, lastAt = e.Text, now
			r.event(e)
		}
		r.o.sleep(r.o.Poll)
	}
}

// takeReport reads the script's marker from its USB disk, writes the
// acknowledgement onto the disk and ejects it, which tells the script the
// marker arrived. Only all of that, and the disk then going away, counts as
// acknowledged. A disk that could not be read, or whose acknowledgement
// could not be written, is left alone: the script then offers it again or,
// at its limit, takes it back and writes the marker to the update drive. A
// disk that was read is acknowledged even when its marker is unusable, so
// the script finishes; that run then counts as having no result.
func (r *run) takeReport(v platform.Volume) *report {
	got := &report{}
	r.emit(StageReport, "Reading the result of the fix from the mixer")
	data, err := flash.ReadReport(r.p, v, carrier.MarkerName, maxMarker)
	if err != nil {
		got.problem = fmt.Sprintf("the result on the mixer's USB disk could not be read: %v", err)
		return got
	}
	if m, err := carrier.ParseMarker(data); err != nil {
		got.problem = fmt.Sprintf("the result on the mixer's USB disk is not usable: %v", err)
	} else {
		got.marker = &m
	}
	r.emit(StageReport, "Telling the mixer the result has arrived")
	if err := flash.WriteReport(r.p, v, carrier.AckName, []byte("ack=1\n")); err != nil {
		if got.problem == "" {
			got.problem = fmt.Sprintf("the mixer could not be told the result arrived: %v", err)
		}
		return got
	}
	if err := r.p.Eject(&v); err != nil {
		return got
	}
	deadline := r.o.now().Add(ejectLimit)
	for {
		present, err := r.p.MediaPresent(&v)
		if err == nil && !present {
			break
		}
		if !r.o.now().Before(deadline) {
			return got
		}
		r.o.sleep(r.o.Poll)
	}
	r.o.sleep(ackGrace)
	got.acked = true
	return got
}

// saveBackup saves the script's copy of sector 0 in dir and gives the folder
// and the copy to the person a privileged run works for, so they can open
// them. It returns the copy's path, or why there is none.
func saveBackup(dir string, sector0 []byte) (string, string) {
	if err := workDir(dir); err != nil {
		return "", err.Error()
	}
	path, err := fix.SaveSector0(dir, sector0)
	if err != nil {
		return "", err.Error()
	}
	f, err := archive.OpenRegular(path)
	if err == nil {
		err = archive.LocalOwner(f)
		f.Close()
	}
	if err != nil {
		return path, fmt.Sprintf("the copy was saved but could not be given to your user account: %v", err)
	}
	return path, ""
}

// present: the mixer is connected in normal mode. An inventory that fails
// while it restarts counts as absent.
func (r *run) present() bool {
	inventory, err := r.p.Inventory()
	if err != nil {
		return false
	}
	for _, m := range flash.Mixers(inventory) {
		if m.Serial == r.serial && !m.UpdateMode {
			return true
		}
	}
	return false
}

func (r *run) waitError(err error, res *Result) error {
	done := ""
	if res != nil {
		done = res.Summary + "\n\n"
	}
	if errors.Is(err, ErrCanceled) {
		if res != nil {
			return fmt.Errorf("%s%w. The mixer is finishing RØDE's update on its own; leave it on until it shows its normal screen", done, err)
		}
		return fmt.Errorf("%w while the mixer was restarting. It finishes on its own and is safe to use: it runs RØDE's firmware with a small one-time fix that removes itself. Start again later to finish", err)
	}
	return fmt.Errorf("%sthe mixer has not come back in normal mode within %s, so nothing more was sent to it. If it shows its normal screen, start again. If it does not start, Repair mixer in the app can check it in storage mode", done, minutes(r.o.ReturnLimit))
}

// readMarker switches the mixer to update mode and reads the script's
// marker from its drive. It leaves the mixer in update mode. The marker is
// nil when there is none or it is not usable; problem then says why.
func (r *run) readMarker() (*carrier.Marker, string, error) {
	r.safe = false
	opts := r.flashOptions()
	opts.Step = func(string, string) {
		r.emit(StageReadResult, "Switching the mixer to USB update mode to read the result")
	}
	if err := flash.EnterUpdateMode(r.p, r.model, r.serial, opts); err != nil {
		return nil, "", fmt.Errorf("%v.\n\nIf the mixer shows \"Updating via USB\", %s; nothing was flashed", strings.TrimSuffix(err.Error(), "."), powerCycleLower)
	}
	r.updateMode = true
	if err := r.waitUpdateDrive(StageReadResult); err != nil {
		return nil, "", err
	}
	v, err := flash.UpdateDrive(r.p, r.serial)
	if err != nil {
		return nil, "", inUpdateMode(err)
	}
	r.emit(StageReadResult, "Reading the result of the fix")
	data, err := flash.ReadFile(v.Path, carrier.MarkerName, maxMarker)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, "no result file on the mixer", nil
	}
	if err != nil {
		return nil, fmt.Sprintf("the result file could not be read: %v", err), nil
	}
	m, err := carrier.ParseMarker(data)
	if err != nil {
		return nil, fmt.Sprintf("the result file is not usable: %v", err), nil
	}
	return &m, "", nil
}

func minutes(d time.Duration) string {
	if d >= time.Minute && d%time.Minute == 0 {
		n := int(d / time.Minute)
		if n == 1 {
			return "1 minute"
		}
		return fmt.Sprintf("%d minutes", n)
	}
	return d.String()
}

// summary says what a run found, for a person. final: the last flash is
// Options.FinalDir rather than RØDE's plain archive.
func summary(res *Result, final bool) string {
	m := res.Marker
	again, onIt := "RØDE's firmware was installed again", "RØDE's firmware was installed on it again"
	if final {
		again, onIt = "the new firmware was installed", "the new firmware was installed on it"
	}
	switch res.Outcome {
	case OutcomeFixed:
		return fmt.Sprintf("The mixer is fixed: its first system slot was made larger, and %s, so it now has the safe layout.", onIt)
	case OutcomeAlreadyFixed:
		return fmt.Sprintf("The mixer already had the fix. Nothing on it was changed; %s.", again)
	case OutcomeUnrecognized:
		return fmt.Sprintf("The mixer's storage is not laid out the way this fix expects (%s), so it was not changed. %s.", m.Reason, capital(again))
	case OutcomeFailed:
		return fmt.Sprintf("The fix could not finish on the mixer (%s). %s.", m.Reason, capital(again))
	}
	why := ""
	if res.MarkerProblem != "" {
		why = " (" + res.MarkerProblem + ")"
	}
	return fmt.Sprintf("The fix did not report a result after %d attempts%s, so whether it ran is not known. %s and nothing harmful was done. Try again later; if it keeps happening, the mixer can be checked in storage mode.", carrierAttempts, why, capital(again))
}

func capital(s string) string {
	if s == "" || s[0] < 'a' || s[0] > 'z' {
		return s
	}
	return string(s[0]-'a'+'A') + s[1:]
}
