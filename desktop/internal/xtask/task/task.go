// Package task provides shared helpers for xtask commands: locating the repo
// root, running subprocesses, hashing, and formatting exit statuses, I/O
// errors, and quoted strings to match the Rust stdlib output from before the
// Go port.
package task

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"unicode"
	"unicode/utf8"
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
		return Errorf("could not start %s: %s", Debug(program), IOError(err))
	}
	err := command.Wait()
	if err != nil {
		var exit *exec.ExitError
		if !errors.As(err, &exit) {
			return Errorf("could not start %s: %s", Debug(program), IOError(err))
		}
	}
	if command.ProcessState.Success() {
		return nil
	}
	return Errorf("%s failed (%s)", Debug(program), Status(command.ProcessState))
}

// Program returns the invoked command name (Args[0]) rather than the resolved path.
func Program(command *exec.Cmd) string {
	if len(command.Args) > 0 {
		return command.Args[0]
	}
	return command.Path
}

func Sha256(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
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

// Debug quotes text using Rust's `{:?}` escaping rules.
func Debug(text string) string {
	var out strings.Builder
	out.WriteByte('"')
	for len(text) > 0 {
		r, size := utf8.DecodeRuneInString(text)
		if r == utf8.RuneError && size == 1 {
			fmt.Fprintf(&out, "\\x%02X", text[0])
			text = text[1:]
			continue
		}
		text = text[size:]
		switch r {
		case '"':
			out.WriteString(`\"`)
		case '\\':
			out.WriteString(`\\`)
		case '\n':
			out.WriteString(`\n`)
		case '\r':
			out.WriteString(`\r`)
		case '\t':
			out.WriteString(`\t`)
		case 0:
			out.WriteString(`\0`)
		default:
			if r < 0x20 || r == 0x7f || (r >= 0x80 && !unicode.IsPrint(r) && r != ' ') {
				out.WriteString(`\u{` + strconv.FormatInt(int64(r), 16) + `}`)
			} else {
				out.WriteRune(r)
			}
		}
	}
	out.WriteByte('"')
	return out.String()
}

// ParseUint parses an unsigned base-10 integer (allowing an optional leading '+').
func ParseUint(text string, bits int) (uint64, bool) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" {
		return 0, false
	}
	for i := 0; i < len(digits); i++ {
		if digits[i] < '0' || digits[i] > '9' {
			return 0, false
		}
	}
	value, err := strconv.ParseUint(digits, 10, bits)
	return value, err == nil
}

func CRLFToLF(text string) string { return strings.ReplaceAll(text, "\r\n", "\n") }
