//go:build rc2fxdev

package app

// Dev-only unattended flash runner (packaging rejects rc2fxdev builds).
// When RC2FX_DEV_AUTOFLASH=<path> is set, this drives the real Install RC2FX UI
// end-to-end using the bundled firmware, writes a single-line result ("ok",
// "failed: <why>", "repaired: <files>", "damaged: <files>", or "timeout") to
// <path>, and exits. Pair with RC2FX_DEV_HELPER_SUDO=1 so the helper uses
// `sudo -n` instead of prompting via pkexec.
//
// RC2FX_DEV_AUTOFLASH_FIX controls the pre-flash storage fix: "off" skips it,
// "on" forces it even on a mixer already running RC2FX, and unset lets the
// installer decide. The fix output goes to stderr.

import (
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/ibanks42/goui-kit/gk"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

const (
	autoFlashTimeout    = 25 * time.Minute
	autoFlashStartLimit = 5 * time.Minute
	autoFlashSSHWait    = 4 * time.Minute
)

func autoFlashBlocked(a *App) string {
	i := a.installer
	switch {
	case i == nil && !a.session.Connected:
		return "the mixer never connected and the installer did not open"
	case i == nil:
		return "the installer did not open"
	case !i.access.AllowsFlashing():
		return fmt.Sprintf("no administrator access (%v: %s)", i.access.Kind, i.access.Why)
	case i.model.Error != "":
		return i.model.Error
	case i.busy():
		return "the installer stayed busy"
	case i.model.Run != nil && i.model.Run.Status().Kind == installflow.RunWaiting:
		return "a run was still waiting: " + i.model.Run.Status().Why
	}
	m := i.model
	return fmt.Sprintf("the installer could not start: choice=%v mixer=%v options=%v run=%v finished=%v status=%v privileged=%v busy=%v recently=%v error=%q",
		choiceVersion(m.Choice), m.Mixer, m.MixerOptions(), m.Run != nil, m.Finished(), m.Status != nil,
		m.Privileged, m.Busy(), m.RecentlyFlashed(), m.Error)
}

var autoFlash struct {
	opened, started, done bool
	asks                  int
	askedAt               time.Time
	launched, startedAt   time.Time
	// Re-enable developer SSH after flashing (flashing resets it) before exiting.
	outcome   string
	outcomeAt time.Time
	sshSent   bool
	sshSentAt time.Time
	traced    string
}

// autoFlashTrace logs phase transitions to stderr with timestamps, omitting
// byte counts and timers so each step produces a single line.
func autoFlashTrace(a *App) {
	i := a.installer
	if i == nil || i.progress == nil {
		return
	}
	p := i.progress
	line := fmt.Sprintf("%s | %s | flash %d", p.Phase, p.Detail, p.Flash)
	if line != autoFlash.traced {
		autoFlash.traced = line
		fmt.Fprintf(os.Stderr, "%s progress: %s\n", time.Now().Format("15:04:05.000"), line)
	}
}

func (a *App) autoFlashPoll(cx *gk.Cx) {
	result := os.Getenv("RC2FX_DEV_AUTOFLASH")
	if result == "" || autoFlash.done {
		return
	}
	autoFlashTrace(a)
	if autoFlash.launched.IsZero() {
		autoFlash.launched = time.Now()
		// Skip flashing and just re-enable developer SSH (useful if a prior run exited before reboot).
		if os.Getenv("RC2FX_DEV_AUTOFLASH_SSH_ONLY") == "1" {
			autoFlash.outcome = "ssh only"
		}
	}
	if !autoFlash.started && autoFlash.outcome == "" && time.Since(autoFlash.launched) > autoFlashStartLimit {
		autoFlash.outcome = "not started: " + autoFlashBlocked(a)
	}
	if autoFlash.outcome != "" {
		if autoFlash.outcomeAt.IsZero() {
			autoFlash.outcomeAt = time.Now()
		}
		a.autoFlashFinish(cx, result)
		return
	}
	// Run the standalone storage fix flow instead of a full install.
	if os.Getenv("RC2FX_DEV_AUTOFLASH_TASK") == "fix" {
		a.autoFixPoll(cx)
		return
	}
	i := a.installer
	if i == nil {
		// A stock mixer won't connect over the RC2FX protocol, so fall back to opening
		// the installer after a short wait.
		if !autoFlash.opened && (a.session.Connected || time.Since(autoFlash.launched) > 30*time.Second) {
			autoFlash.opened = true
			a.openTask(cx, taskInstall)
		}
		return
	}
	m := i.model
	if !autoFlash.started {
		// If the task picked up a previous finished run, dismiss it so the next poll starts clean.
		if m.Finished() && !i.busy() {
			a.closeTask(cx, false)
			autoFlash.opened = false
			return
		}
		if !i.access.AllowsFlashing() {
			// Wait for initial background requests to release the installer lock before
			// starting the helper.
			if i.busy() || i.access.Kind == installflow.AccessAsking ||
				time.Since(autoFlash.askedAt) < 10*time.Second {
				return
			}
			if autoFlash.asks < 5 {
				autoFlash.asks++
				autoFlash.askedAt = time.Now()
				a.grantInstallerAccess(cx)
			}
			return
		}
		if a.bundledMixerSoftware != nil && (m.Mixer == nil || m.Mixer.Kind != installflow.MixerSourceBundled) {
			m.ChooseMixer(installflow.Bundled(*a.bundledMixerSoftware))
			a.notify(cx)
			return
		}
		if m.Mixer == nil || m.Mixer.Kind != installflow.MixerSourceBundled || !m.CanStart() || i.busy() {
			return
		}
		switch fix := os.Getenv("RC2FX_DEV_AUTOFLASH_FIX"); fix {
		case "off":
			m.FixPolicy = installflow.FixNever
		case "on":
			m.FixPolicy = installflow.FixAlways
		case "":
		default:
			autoFlash.outcome = fmt.Sprintf("not started: RC2FX_DEV_AUTOFLASH_FIX is %q, not on or off", fix)
			return
		}
		autoFlash.started, autoFlash.startedAt = true, time.Now()
		fmt.Fprintf(os.Stderr, "autoflash: starting RØDE %s with RC2FX %s, storage fix %v\n",
			choiceVersion(m.Choice), m.Mixer.Version(), m.WantsStorageFix())
		a.startTask(cx)
		return
	}
	switch {
	case m.Failed():
		autoFlash.outcome = "failed: " + m.Run.Status().Why
	case m.Finished():
		autoFlash.outcome = "ok"
		if last := m.LastRun(); last != nil && last.Fix != nil {
			fmt.Fprintf(os.Stderr, "autoflash: storage fix %+v (error %q)\n", last.Fix.Reply, last.Fix.Error)
		}
		a.closeTask(cx, true)
	case m.ImageCheckResult() != nil && m.ImageCheckResult().Status == core.ImageStatusRepaired:
		autoFlash.outcome = "repaired: " + strings.Join(m.ImageCheckResult().Repaired, " ")
	case m.ImageCheckResult() != nil && m.ImageCheckResult().Status == core.ImageStatusDamaged:
		autoFlash.outcome = "damaged: " + strings.Join(m.ImageCheckResult().Damaged, " ")
	case time.Since(autoFlash.startedAt) > autoFlashTimeout:
		autoFlash.outcome = "timeout"
	}
}

// autoFlashFinish re-enables developer SSH once the mixer reconnects, writes the
// result file, and quits the app.
func (a *App) autoFlashFinish(cx *gk.Cx, result string) {
	if !autoFlash.sshSent && a.session.Connected && !a.ssh.Change.Busy() {
		if key, err := installer.SSHPublicKey(); err == nil {
			autoFlash.sshSent, autoFlash.sshSentAt = true, time.Now()
			a.sendSSH(core.SSHRequest{Enabled: true, PublicKey: &key})
			return
		}
	}
	waiting := autoFlash.sshSent && a.ssh.Change.Sent() && time.Since(autoFlash.sshSentAt) < 30*time.Second
	// Give the mixer time to finish rebooting and reconnect so we can turn SSH back on.
	if waiting || (!autoFlash.sshSent && time.Since(autoFlash.outcomeAt) < autoFlashSSHWait) {
		return
	}
	autoFlash.done = true
	_ = os.WriteFile(result, []byte(autoFlash.outcome+"\n"), 0o600)
	fmt.Fprintf(os.Stderr, "autoflash: %s (ssh sent %v)\n", autoFlash.outcome, autoFlash.sshSent)
	gk.Quit(cx)
}

func (a *App) autoFixPoll(cx *gk.Cx) {
	i := a.installer
	if i == nil || i.fix == nil {
		if !autoFlash.opened && time.Since(autoFlash.launched) > 15*time.Second {
			autoFlash.opened = true
			a.openFix(cx)
		}
		return
	}
	if !autoFlash.started {
		if !i.access.AllowsFlashing() {
			if i.busy() || i.access.Kind == installflow.AccessAsking ||
				time.Since(autoFlash.askedAt) < 10*time.Second {
				return
			}
			if autoFlash.asks < 5 {
				autoFlash.asks++
				autoFlash.askedAt = time.Now()
				a.grantInstallerAccess(cx)
			}
			return
		}
		if i.busy() {
			return
		}
		autoFlash.started, autoFlash.startedAt = true, time.Now()
		fmt.Fprintf(os.Stderr, "autoflash: starting the storage fix on its own\n")
		a.startFix(cx)
		return
	}
	switch f := i.fix; {
	case f.Kind == installflow.FixEnded:
		autoFlash.outcome = fmt.Sprintf("fix ended: ending=%d error=%q", f.Ending(), f.Error)
		if f.Reply != nil {
			fmt.Fprintf(os.Stderr, "autoflash: storage fix %+v\n", *f.Reply)
		}
		// Stock RØDE firmware doesn't have SSH, so skip waiting for it.
		autoFlash.sshSent = true
		a.closeTask(cx, false)
	case time.Since(autoFlash.startedAt) > autoFlashTimeout:
		autoFlash.outcome = "timeout"
		autoFlash.sshSent = true
	}
}

func choiceVersion(c *installflow.Choice) string {
	if c == nil {
		return "?"
	}
	return c.Version
}
