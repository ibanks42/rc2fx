package autofix

import (
	"errors"
	"fmt"
	"io/fs"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/carrier"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/fix"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/flash"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

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
