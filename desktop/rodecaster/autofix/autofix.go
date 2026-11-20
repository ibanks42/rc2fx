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
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
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

	// The last flash goes onto the slot the mixer is not running. RØDE's
	// updater formats it without a block size, so it gets 4 KB blocks only
	// when it is at least 512 MiB: after the fix, or slot 2 on any factory
	// table. On anything else (the fix did not finish and the mixer runs
	// slot 2, or the script could not say) the flash could be damaged as
	// it lands, so nothing more is sent. In update mode a power cycle
	// brings the mixer back on the system it was running, unchanged.
	if ok, why := lastFlashSafe(marker); !ok {
		res.Flashes = r.flashes
		r.current = nil
		stop := fmt.Sprintf("The app stopped there: the next update would have gone onto storage that RØDE's update bug can damage (%s). Nothing more was sent to the mixer.", why)
		if r.updateMode {
			stop += " It is waiting in USB update mode: switch it off and on, and it starts again as it was."
		}
		res.Summary = notApplied(&res) + " " + stop
		return res, &NotApplied{Summary: res.Summary}
	}

	// The mixer is in update mode after the marker read, or in normal mode
	// after the USB report. RØDE's plain archive (or the final firmware)
	// goes onto the slot that is now safe to format, and is the clean way
	// out of update mode.
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
