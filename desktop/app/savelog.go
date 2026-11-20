package app

// Optional debug log enabled via RC2FX_SAVE_LOG=<path> for tracing save/live-edit
// revisions, echoes, and conflicts. No-op when unset.

import (
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
)

var saveLog = sync.OnceValue(func() *os.File {
	path := os.Getenv("RC2FX_SAVE_LOG")
	if path == "" {
		return nil
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		fmt.Fprintf(os.Stderr, "RC2FX_SAVE_LOG: %s\n", err)
		return nil
	}
	return f
})

func saveLogf(format string, args ...any) {
	f := saveLog()
	if f == nil {
		return
	}
	fmt.Fprintf(f, "%s "+format+"\n", append([]any{time.Now().Format("15:04:05.000")}, args...)...)
}

func saveLogging() bool { return saveLog() != nil }

func saveLogTargets(label string, targets []core.Target) {
	if !saveLogging() {
		return
	}
	b, err := json.Marshal(targets)
	if err != nil {
		saveLogf("%s: %s", label, err)
		return
	}
	saveLogf("%s: %s", label, b)
}

var heldReason string

func (a *App) logHeld(reason string) {
	if reason == heldReason || !saveLogging() {
		return
	}
	heldReason = reason
	if reason == "" {
		reason = "save no longer held"
	}
	saveLogf("%s", reason)
}

func (a *App) logReceive(state core.DeviceState, acknowledged, snapshot bool) {
	if !saveLogging() {
		return
	}
	saved := uint64(0)
	if a.session.Saved != nil {
		saved = a.session.Saved.Config.Revision
	}
	saveLogf("state: revision %d (saved %d) engine gen %d ack %v snapshot %v",
		state.Config.Revision, saved, state.Engine.Generation, acknowledged, snapshot)
	loading := a.presets.loadingRevision()
	if a.pending == nil && loading == nil {
		return
	}
	c := model.CheckEcho(&a.session, state, a.pending, loading)
	saveLogf("  echo %v: %+v", c.Own(), c)
	if c.ApplyPending && c.ApplyFollows && !c.ApplySameTargets {
		saveLogTargets("  sent", a.pending.Request.Targets)
		saveLogTargets("  echoed", state.Config.Targets)
	}
}
