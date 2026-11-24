//go:build windows

package task

import (
	"os"
	"strconv"
	"syscall"
)

func statusText(state *os.ProcessState) string {
	code := uint32(state.ExitCode())
	if code&0x80000000 != 0 {
		return "exit code: 0x" + strconv.FormatUint(uint64(code), 16)
	}
	return "exit code: " + strconv.FormatUint(uint64(code), 10)
}

func errnoText(errno syscall.Errno) string {
	return errno.Error() + " (os error " + strconv.Itoa(int(errno)) + ")"
}
