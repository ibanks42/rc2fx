package installer

// Engine integration for RequestStorageFix (rodecaster/autofix), which grows
// system slot 1 via RØDE's USB update so future updates format with 4 KB blocks.

import (
	"errors"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/autofix"
)

// StorageFixPhase prefixes Progress.Phase for storage-fix events (followed by autofix.Stage*).
const StorageFixPhase = "fix-"

func StorageFixStage(p Progress) (string, bool) {
	stage, ok := strings.CutPrefix(p.Phase, StorageFixPhase)
	return stage, ok && stage != ""
}

// StorageFix is the result of RequestStorageFix.
type StorageFix struct {
	_         struct{} `serde:"default"`
	Outcome   string   `json:"outcome"` // autofix.Outcome* or "" if stopped before reporting
	Reason    string   `json:"reason"`
	Backup    string   `json:"backup"`
	Firmware  string   `json:"firmware"`
	Flashes   int      `json:"flashes"`
	Error     string   `json:"error"`
	Refused   bool     `json:"refused"`    // unsupported mixer/firmware; nothing changed
	Touched   bool     `json:"touched"`    // at least one USB command was sent
	FinalSent bool     `json:"final_sent"` // final flash accepted by mixer
	// Unsafe is true when slot 1 is still too small for 4 KB blocks
	// (autofix.NotApplied), so the final flash was withheld.
	Unsafe bool `json:"unsafe"`
}

const (
	storageFixWork    = "storage-fix"
	storageFixBackups = "storage-fix-backups"
)

// storageFix runs autofix on the connected mixer. With final, the last flash
// uses the prepared RC2FX firmware instead of plain stock. Autofix refusals
// and errors are returned inside StorageFix rather than as an engine error.
func (e *Engine) storageFix(final, debug bool) (*StorageFix, error) {
	var touched, finalSent bool
	o := autofix.Options{
		Platform:  e.Platform,
		Work:      filepath.Join(e.Work, storageFixWork),
		BackupDir: filepath.Join(e.Work, storageFixBackups),
		Client:    func() *http.Client { return e.client() },
		// Only cancel before the first USB mode switch; once touched, run to completion.
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
		// Reuse the locally downloaded stock archive as the carrier base if it matches.
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
