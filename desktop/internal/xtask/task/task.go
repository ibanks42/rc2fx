// Package task is what every xtask command shares: the repository root,
// running a program to completion, SHA-256 in hex, and fixed wording for
// exit statuses, I/O errors and quoted strings: the Rust standard library's,
// as the xtask printed them before it was ported to Go.
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

// Error is a message for the user; main prints it after "error: ".
type Error string

func (e Error) Error() string { return string(e) }

// Errorf is fmt.Sprintf as an Error.
func Errorf(format string, args ...any) error { return Error(fmt.Sprintf(format, args...)) }

// The repository files this tool reads: the scripts that run on the mixer
// (mixer/scripts), and the package assets (desktop/packaging: launcher,
// desktop entry, icons, entitlements).
const (
	MixerScriptsDir = "mixer/scripts"
	AssetsDir       = "desktop/packaging"
)

// RootVariable names the repository root; the ./xtask launcher sets it.
const RootVariable = "RC2FX_XTASK_ROOT"

// Root is the repository root: RC2FX_XTASK_ROOT, or, when this program runs
// from source (go run, go test), the checkout this file is in.
func Root() string {
	if root := os.Getenv(RootVariable); root != "" {
		return root
	}
	_, file, _, ok := runtime.Caller(0)
	if !ok || !filepath.IsAbs(file) {
		wd, _ := os.Getwd()
		return wd
	}
	// desktop/internal/xtask/task/task.go
	return filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(file)))))
}

// Run runs a command to completion with this process's stdio, failing on a
// non-zero exit.
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

// Program is the program a command runs, as it was named, not the path it
// was found at.
func Program(command *exec.Cmd) string {
	if len(command.Args) > 0 {
		return command.Args[0]
	}
	return command.Path
}

// Sha256 is the lowercase hexadecimal SHA-256 of data.
func Sha256(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

// Status is a finished process's status: `exit status: 1`,
// `signal: 9 (SIGKILL)`, or on Windows `exit code: 1`.
func Status(state *os.ProcessState) string {
	if state == nil {
		return "exit status: unknown"
	}
	return statusText(state)
}

// IOError is an error's text: an operating system error is its description
// and `(os error N)`; anything else as it is. Path and syscall wrappers are
// dropped: the callers' messages name the path themselves.
func IOError(err error) string {
	if err == nil {
		return ""
	}
	if errors.Is(err, exec.ErrNotFound) {
		// A program not on PATH is reported as execvp's ENOENT.
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

// Debug quotes text as Rust's `{:?}` does for a str, Path or OsStr.
func Debug(text string) string {
	var out strings.Builder
	out.WriteByte('"')
	for len(text) > 0 {
		r, size := utf8.DecodeRuneInString(text)
		if r == utf8.RuneError && size == 1 {
			// An OsStr's invalid bytes.
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

// ParseUint is an unsigned integer of bits bits: an optional `+` and ASCII
// digits only, within range.
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

// CRLFToLF replaces every "\r\n" with "\n".
func CRLFToLF(text string) string { return strings.ReplaceAll(text, "\r\n", "\n") }

// ReadFile is os.ReadFile with the path and IOError's text in its error.
func ReadFile(path string) ([]byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, Errorf("%s: %s", path, IOError(err))
	}
	return data, nil
}
