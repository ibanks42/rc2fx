//go:build rc2fxdev

package helper

import (
	"os"
	"os/exec"
)

// devLaunch starts the helper with `sudo -n` instead of pkexec when
// RC2FX_DEV_HELPER_SUDO=1: unattended development runs (the app's flash
// autopilot) have nobody for the password prompt. Development builds only.
func devLaunch(executable string) *exec.Cmd {
	if os.Getenv("RC2FX_DEV_HELPER_SUDO") != "1" {
		return nil
	}
	return exec.Command("sudo", "-n", executable)
}
