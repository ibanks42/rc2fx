//go:build rc2fxdev

package helper

import (
	"os"
	"os/exec"
)

// In dev builds, RC2FX_DEV_HELPER_SUDO=1 uses non-interactive sudo instead of
// pkexec for unattended test runs.
func devLaunch(executable string) *exec.Cmd {
	if os.Getenv("RC2FX_DEV_HELPER_SUDO") != "1" {
		return nil
	}
	return exec.Command("sudo", "-n", executable)
}
