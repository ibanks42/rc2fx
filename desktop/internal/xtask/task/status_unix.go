//go:build unix

package task

import (
	"os"
	"strconv"
	"strings"
	"syscall"

	"golang.org/x/sys/unix"
)

func statusText(state *os.ProcessState) string {
	wait, ok := state.Sys().(syscall.WaitStatus)
	if !ok {
		return "exit status: " + strconv.Itoa(state.ExitCode())
	}
	switch {
	case wait.Exited():
		return "exit status: " + strconv.Itoa(wait.ExitStatus())
	case wait.Signaled():
		text := "signal: " + strconv.Itoa(int(wait.Signal()))
		if name := unix.SignalName(wait.Signal()); name != "" {
			text += " (" + name + ")"
		}
		if wait.CoreDump() {
			text += " (core dumped)"
		}
		return text
	case wait.Stopped():
		text := "stopped (not terminated) by signal: " + strconv.Itoa(int(wait.StopSignal()))
		if name := unix.SignalName(wait.StopSignal()); name != "" {
			text += " (" + name + ")"
		}
		return text
	case wait.Continued():
		return "continued (WIFCONTINUED)"
	}
	return "unrecognised wait status: " + strconv.Itoa(int(wait))
}

func errnoText(errno syscall.Errno) string {
	text := errno.Error()
	if text != "" {
		// glibc's strerror starts with a capital; Go's table does not.
		text = strings.ToUpper(text[:1]) + text[1:]
	}
	return text + " (os error " + strconv.Itoa(int(errno)) + ")"
}
