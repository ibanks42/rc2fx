//go:build !rc2fxdev

package helper

import "os/exec"

// devLaunch is launch_dev.go's; other builds always use pkexec.
func devLaunch(string) *exec.Cmd { return nil }
