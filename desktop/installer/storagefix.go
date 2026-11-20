package installer

// The storage fix as a request (RequestStorageFix): rodecaster/autofix grows
// the mixer's first system slot through RØDE's own USB update, so every
// later update formats it with 4 KB blocks. It writes to the mixer exactly
// as a flash does, so it runs where flashes run (the helper, on Linux), and
// it reports autofix's events as progress.

import (
	"errors"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// StorageFixPhase starts the Phase of every progress event the storage fix
// reports; the rest is autofix's stage word (autofix.Stage*).
const StorageFixPhase = "fix-"

// StorageFixStage is the autofix stage of a progress event, ok false for
// an event that is not the storage fix's.
func StorageFixStage(p Progress) (string, bool) {
	stage, ok := strings.CutPrefix(p.Phase, StorageFixPhase)
	return stage, ok && stage != ""
}

// StorageFix is what RequestStorageFix did.
type StorageFix struct {
	_ struct{} `serde:"default"`
	// Outcome is autofix's (autofix.Outcome*); "" when the run stopped
	// before the script reported anything.
	Outcome string `json:"outcome"`
	// Reason is what the script said, for unrecognized and failed.
	Reason string `json:"reason"`
	// Backup is the saved copy of what the fix changed on the mixer; ""
	// when none was saved.
	Backup   string `json:"backup"`
	Firmware string `json:"firmware"`
	Flashes  int    `json:"flashes"`
	// Error is why the run stopped early, in autofix's words; "" when it
	// finished. Its Outcome may still be set: the script reported, and a
	// later step stopped.
	Error string `json:"error"`
	// Refused: the run would not start, because the mixer or its firmware
	// is not one the fix supports. Nothing was changed, and starting again
	// gives the same answer.
	Refused bool `json:"refused"`
	// Touched: the run sent the mixer a USB command, so it may no longer be
	// as it was.
	Touched bool `json:"touched"`
	// FinalSent: the mixer accepted the last flash (RØDE's plain archive,
	// or the prepared firmware with Final) and is installing it.
	FinalSent bool `json:"final_sent"`
	// Unsafe: the fix did not leave the slot the last flash would format
	// large enough for 4 KB blocks, so nothing more was sent
	// (autofix.NotApplied). Error says why and what to do; installing
	// anyway would format that slot with 1 KB blocks.
	Unsafe bool `json:"unsafe"`
}

// storageFixWork and storageFixBackups are the fix's folders in the work
// folder: its downloads (removed after a finished run), and the copies of
// what it changed, which stay.
const (
	storageFixWork    = "storage-fix"
	storageFixBackups = "storage-fix-backups"
)

// storageFix runs the fix on the one connected mixer. With final, the last
// flash is the prepared RC2FX firmware, and the carrier is RØDE's archive
// of the same version. autofix's own refusals and errors come back in the
// reply, never retried around; only an engine problem is an error.
// debug has the script keep its debug log on the mixer (development
// builds only; see fixDebugAllowed).
func (e *Engine) storageFix(final, debug bool) (*StorageFix, error) {
	var touched, finalSent bool
	o := autofix.Options{
		Platform:  e.Platform,
		Work:      filepath.Join(e.Work, storageFixWork),
		BackupDir: filepath.Join(e.Work, storageFixBackups),
		Client:    func() *http.Client { return e.client() },
		// Stopping counts only until the mixer is first told to switch to
		// update mode: after that the fix runs to its end, which leaves the
		// mixer on RØDE's (or the prepared) firmware.
		Cancel: func() error {
			if !touched && e.canceled() {
				return Error(Canceled)
			}
			return nil
		},
		Record:   e.commandLog,
		DebugLog: debug,
		Events: func(ev autofix.Event) {
			p := Progress{Phase: StorageFixPhase + ev.Stage, Completed: ev.Completed, Total: ev.Total, Detail: ev.Text}
			if ev.Flash != nil {
				p.Flash, p.Carrier = ev.Flash.Number, ev.Flash.Carrier
			}
			switch ev.Stage {
			case autofix.StageUpdateMode, autofix.StageCopy, autofix.StageFlash, autofix.StageReadResult:
				touched = true
			case autofix.StageWait, autofix.StageReport:
				touched = true
				finalSent = finalSent || (ev.Flash != nil && !ev.Flash.Carrier)
			}
			e.emit(p)
		},
	}
	if final {
		saved, err := e.load()
		if err != nil {
			return nil, err
		}
		p := saved.Prepared
		if p == nil || p.Stock {
			return nil, Error("prepare firmware with RC2FX before the storage fix")
		}
		if err := e.verifyPrepared(p); err != nil {
			return nil, err
		}
		o.FinalDir, o.FinalMD5, o.Version = e.Work, p.ArchiveMD5, p.Version
		// RØDE's archive that prepare downloaded is the carrier's base when
		// it is still the one for this version; otherwise autofix downloads
		// it again.
		if profile, err := e.profile(p.Profile); err == nil {
			src := filepath.Join(e.Work, "rode-original.tar.gz")
			if md, err := archive.FileHash(src, archive.MD5); err == nil && md == profile.ArchiveMD5 {
				digest := filepath.Join(e.Work, "rode-original.md5")
				if archive.AtomicFile(digest, []byte(md)) == nil {
					o.LocalArchive, o.LocalMD5 = src, digest
				}
			}
		}
	}
	res, err := autofix.Run(o)
	out := &StorageFix{
		Outcome:   string(res.Outcome),
		Backup:    res.Backup,
		Firmware:  res.Firmware,
		Flashes:   res.Flashes,
		Touched:   touched,
		FinalSent: finalSent,
	}
	if res.Marker != nil {
		out.Reason = res.Marker.Reason
	}
	if err != nil {
		if errors.Is(err, autofix.ErrCanceled) && !touched {
			return nil, Error(Canceled)
		}
		var refusal *autofix.Refusal
		var unsafe *autofix.NotApplied
		out.Error, out.Refused, out.Unsafe = err.Error(), errors.As(err, &refusal), errors.As(err, &unsafe)
	}
	return out, nil
}
