// Package task provides shared helpers for xtask commands: locating the repo
// root, running subprocesses, and formatting exit statuses and I/O errors.
package task

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
)

// Error is a user-facing error message printed by main after "error: ".
type Error string

func (e Error) Error() string { return string(e) }

func Errorf(format string, args ...any) error { return Error(fmt.Sprintf(format, args...)) }

const (
	MixerScriptsDir = "mixer/scripts"
	AssetsDir       = "desktop/packaging"
)

// RootVariable overrides the repository root (set by the ./xtask launcher).
const RootVariable = "RC2FX_XTASK_ROOT"

// Root returns the repository root from RC2FX_XTASK_ROOT, falling back to the
// checkout containing this source file when run via `go run` or `go test`.
func Root() string {
	if root := os.Getenv(RootVariable); root != "" {
		return root
	}
	_, file, _, ok := runtime.Caller(0)
	if !ok || !filepath.IsAbs(file) {
		wd, _ := os.Getwd()
		return wd
	}
	// Walk up from desktop/internal/xtask/task/task.go.
	return filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(file)))))
}

// Run executes command attached to this process's stdio and fails on a non-zero exit.
func Run(command *exec.Cmd) error {
	if command.Stdin == nil {
		command.Stdin = os.Stdin
	}
	if command.Stdout == nil {
		command.Stdout = os.Stdout
	}
	if command.Stderr == nil {
		command.Stderr = os.Stderr
	}
	program := Program(command)
	if err := command.Start(); err != nil {
		return Errorf("could not start %q: %s", program, IOError(err))
	}
	err := command.Wait()
	if err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			return Errorf("could not start %q: %s", program, IOError(err))
		}
	}
	if command.ProcessState.Success() {
		return nil
	}
	return Errorf("%q failed (%s)", program, Status(command.ProcessState))
}

// Program returns the invoked command name (Args[0]) rather than the resolved path.
func Program(command *exec.Cmd) string {
	if len(command.Args) > 0 {
		return command.Args[0]
	}
	return command.Path
}

// Status formats a finished process's exit status (`exit status: 1`,
// `signal: 9 (SIGKILL)`, or `exit code: 1` on Windows).
func Status(state *os.ProcessState) string {
	if state == nil {
		return "exit status: unknown"
	}
	return statusText(state)
}

// IOError formats OS errors as "<msg> (os error N)", stripping fs.PathError
// wrappers since callers include the path themselves.
func IOError(err error) string {
	if err == nil {
		return ""
	}
	if errors.Is(err, exec.ErrNotFound) {
		// Report missing executables as ENOENT to match execvp.
		return errnoText(syscall.ENOENT)
	}
	var errno syscall.Errno
	if errors.As(err, &errno) {
		return errnoText(errno)
	}
	var path *fs.PathError
	if errors.As(err, &path) {
		return path.Err.Error()
	}
	return err.Error()
}

func CRLFToLF(text string) string { return strings.ReplaceAll(text, "\r\n", "\n") }
