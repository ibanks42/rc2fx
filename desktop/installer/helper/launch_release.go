//go:build !rc2fxdev

package helper

import "os/exec"

func devLaunch(string) *exec.Cmd { return nil }
