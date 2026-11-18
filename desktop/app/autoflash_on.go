//go:build rc2fxdev

package app

// The flash autopilot, development builds only (packaging refuses rc2fxdev):
// with RC2FX_DEV_AUTOFLASH=<result file>, the app opens Install RC2FX, grants
// access, picks the RC2FX built into it and starts the run exactly as Before
// you start's button does, so a flash can be repeated unattended through the
// app's own flow. It writes one line to the result file and quits when the
// run ends: "ok", "failed: <why>", "repaired: <files>", "damaged: <files>"
// or "timeout". A finished run's result page is dismissed as Done does.
// installer/helper starts its helper with `sudo -n` in these builds when
// RC2FX_DEV_HELPER_SUDO=1, since nobody is there for pkexec's prompt.
//
// RC2FX_DEV_AUTOFLASH_FIX chooses whether the run fixes the mixer's storage
// first: "off" never, "on" always (also on a mixer running RC2FX), unset as
// Install decides (only where RC2FX isn't running). The fix's outcome goes
// to stderr; the result line stays as above.

import (
	"fmt"
	"os"
	"strings"
	"time"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// autoFlashTimeout bounds one run, from start to its end;
// autoFlashStartLimit bounds the wait for a run to be able to start.
const (
	autoFlashTimeout    = 25 * time.Minute
	autoFlashStartLimit = 5 * time.Minute
	autoFlashSSHWait    = 4 * time.Minute
)

// autoFlashBlocked says what kept a run from starting.
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
	// The run's outcome, then developer SSH is enabled before quitting so
	// the mixer can be examined (a flash leaves it off).
	outcome   string
	outcomeAt time.Time
	sshSent   bool
	sshSentAt time.Time
	// traced is the last progress line written to stderr.
	traced string
}

// autoFlashTrace writes each new step of the run to stderr with the time,
// so a run's timing can be read afterwards. Byte counts and seconds are left
// out, so a step is one line.
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

// autoFlashPoll runs on the app's poll.
func (a *App) autoFlashPoll(cx *gk.Cx) {
	result := os.Getenv("RC2FX_DEV_AUTOFLASH")
	if result == "" || autoFlash.done {
		return
	}
	autoFlashTrace(a)
	if autoFlash.launched.IsZero() {
		autoFlash.launched = time.Now()
		// RC2FX_DEV_AUTOFLASH_SSH_ONLY: no flash, only turn developer SSH on
		// (after a run that ended before the mixer was back).
		if os.Getenv("RC2FX_DEV_AUTOFLASH_SSH_ONLY") == "1" {
			autoFlash.outcome = "ssh only"
		}
	}
	// A run that cannot start in this long won't: say why and stop.
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
	// RC2FX_DEV_AUTOFLASH_TASK=fix runs the storage fix on its own instead
	// of an install (mixer on RØDE's firmware).
	if os.Getenv("RC2FX_DEV_AUTOFLASH_TASK") == "fix" {
		a.autoFixPoll(cx)
		return
	}
	i := a.installer
	if i == nil {
		// Without RC2FX running (a stock mixer) the app never connects; the
		// installer is then the first screen's way in.
		if !autoFlash.opened && (a.session.Connected || time.Since(autoFlash.launched) > 30*time.Second) {
			autoFlash.opened = true
			a.openTask(cx, taskInstall)
		}
		return
	}
	m := i.model
	if !autoFlash.started {
		// Opened just after a flash, the task picks that run up and may show
		// its result; Done closes it, and the next poll opens a fresh one.
		if m.Finished() && !i.busy() {
			a.closeTask(cx, false)
			autoFlash.opened = false
			return
		}
		if !i.access.AllowsFlashing() {
			// As a person does: ask once the page has settled. The page's
			// own background requests hold the installer lock, and the
			// helper's first request would find it taken.
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
		// The result page stays until dismissed; dismiss it as Done does.
		a.closeTask(cx, true)
	case m.ImageCheckResult() != nil && m.ImageCheckResult().Status == core.ImageStatusRepaired:
		autoFlash.outcome = "repaired: " + strings.Join(m.ImageCheckResult().Repaired, " ")
	case m.ImageCheckResult() != nil && m.ImageCheckResult().Status == core.ImageStatusDamaged:
		autoFlash.outcome = "damaged: " + strings.Join(m.ImageCheckResult().Damaged, " ")
	case time.Since(autoFlash.startedAt) > autoFlashTimeout:
		autoFlash.outcome = "timeout"
	}
}

// autoFlashFinish turns developer SSH on (as the Developer page's switch
// does) while the mixer is connected, then records the outcome and quits.
func (a *App) autoFlashFinish(cx *gk.Cx, result string) {
	if !autoFlash.sshSent && a.session.Connected && !a.ssh.Change.Busy() {
		if key, err := installer.SSHPublicKey(); err == nil {
			autoFlash.sshSent, autoFlash.sshSentAt = true, time.Now()
			a.sendSSH(core.SSHRequest{Enabled: true, PublicKey: &key})
			return
		}
	}
	waiting := autoFlash.sshSent && a.ssh.Change.Sent() && time.Since(autoFlash.sshSentAt) < 30*time.Second
	// The mixer may still be restarting when the run ends; give it a few
	// minutes to connect so SSH can be turned on.
	if waiting || (!autoFlash.sshSent && time.Since(autoFlash.outcomeAt) < autoFlashSSHWait) {
		return
	}
	autoFlash.done = true
	_ = os.WriteFile(result, []byte(autoFlash.outcome+"\n"), 0o600)
	fmt.Fprintf(os.Stderr, "autoflash: %s (ssh sent %v)\n", autoFlash.outcome, autoFlash.sshSent)
	gk.Quit(cx)
}

// autoFixPoll drives the fix-only task as a person would: open it from the
// first screen, grant access, start, and record how it ended.
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
		// No SSH on RØDE's firmware: finish without it.
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
