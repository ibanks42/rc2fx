// Package helper runs a privileged installer subprocess via pkexec for raw USB,
// mounting, and ejection. Requests and replies are JSON lines over stdio; a
// "cancel" line stops an in-flight download like Engine.Cancel.
package helper

import (
	"bufio"
	_ "embed"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sync"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/platform"
)

const Flag = "--installer-helper"

// USBPermission is the error prefix returned on Linux when udev blocks access
// to the mixer's USB 1 control interface.
const USBPermission = platform.USBPermission

// USBRule is the Linux udev rule shipped in config/udev (verified identical by
// tests). Shown verbatim to the user before installation.
//
//go:embed 70-rc2fx.rules
var USBRule string

const USBRulePath = "/etc/udev/rules.d/70-rc2fx.rules"

// USBRuleFlag installs USBRule to USBRulePath, reloads udev, and exits.
const USBRuleFlag = "--install-usb-rule"

// Root argument selecting the compiled-in payload.
const Embedded = "-"

// Root argument selecting a download-only helper.
const Download = "--download-payload"

// privilegedLaunch prepares `pkexec <exe>` and returns a stagedAppDir guard
// that must stay alive until the child exits.
func privilegedLaunch() (*exec.Cmd, *stagedAppDir, error) {
	exe, err := os.Executable()
	if err != nil {
		return nil, nil, err
	}
	executable, guard, err := prepareLaunch(exe)
	if err != nil {
		return nil, nil, err
	}
	if cmd := devLaunch(executable); cmd != nil {
		return cmd, guard, nil
	}
	return exec.Command("pkexec", executable), guard, nil
}

// Wire message from helper to desktop: {"progress": ...} or {"done": {"Ok"|"Err": ...}}.
type message struct {
	Progress *installer.Progress
	Done     *done
}

type done struct {
	Reply *installer.Reply
	Error *string
}

func (m message) MarshalJSON() ([]byte, error) {
	if m.Progress != nil {
		return tagged("progress", m.Progress)
	}
	if m.Done.Reply != nil {
		inner, err := tagged("Ok", m.Done.Reply)
		if err != nil {
			return nil, err
		}
		return tagged("done", rawJSON(inner))
	}
	inner, err := tagged("Err", m.Done.Error)
	if err != nil {
		return nil, err
	}
	return tagged("done", rawJSON(inner))
}

func (m *message) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	switch {
	case variant == "progress" && payload != nil:
		var p installer.Progress
		if err := sjson.Unmarshal(payload, &p); err != nil {
			return err
		}
		*m = message{Progress: &p}
	case variant == "done" && payload != nil:
		result, inner, err := sjson.Tagged(payload)
		if err != nil {
			return err
		}
		switch {
		case result == "Ok" && inner != nil:
			var r installer.Reply
			if err := sjson.Unmarshal(inner, &r); err != nil {
				return err
			}
			*m = message{Done: &done{Reply: &r}}
		case result == "Err" && inner != nil:
			var text string
			if err := sjson.Unmarshal(inner, &text); err != nil {
				return err
			}
			*m = message{Done: &done{Error: &text}}
		default:
			return fmt.Errorf("unknown variant `%s`, expected `Ok` or `Err`", result)
		}
	default:
		return fmt.Errorf("unknown variant `%s`, expected `progress` or `done`", variant)
	}
	return nil
}

// Wire message from desktop to helper: {"request": ...} or "cancel".
type inbound struct {
	Request *installer.Request
	Cancel  bool
}

func (i inbound) MarshalJSON() ([]byte, error) {
	if i.Cancel {
		return []byte(`"cancel"`), nil
	}
	return tagged("request", i.Request)
}

func (i *inbound) UnmarshalJSON(data []byte) error {
	variant, payload, err := sjson.Tagged(data)
	if err != nil {
		return err
	}
	switch {
	case variant == "cancel" && payload == nil:
		*i = inbound{Cancel: true}
	case variant == "request" && payload != nil:
		var r installer.Request
		if err := sjson.Unmarshal(payload, &r); err != nil {
			return err
		}
		*i = inbound{Request: &r}
	default:
		return fmt.Errorf("unknown variant `%s`, expected `request` or `cancel`", variant)
	}
	return nil
}

type rawJSON []byte

func (r rawJSON) MarshalJSON() ([]byte, error) { return r, nil }

func tagged(variant string, payload any) ([]byte, error) {
	body, err := sjson.Marshal(payload)
	if err != nil {
		return nil, err
	}
	out := append([]byte{'{'}, sjson.String(variant)...)
	out = append(out, ':')
	out = append(out, body...)
	return append(out, '}'), nil
}

func writeLine(w io.Writer, value any) error {
	data, err := sjson.Marshal(value)
	if err != nil {
		return err
	}
	_, err = w.Write(append(data, '\n'))
	return err
}

// Serve reads JSON-line requests from stdin and writes progress/replies to stdout until EOF.
func Serve(root, work string) error {
	var stdout sync.Mutex
	out := bufio.NewWriter(os.Stdout)
	send := func(m message) error {
		stdout.Lock()
		defer stdout.Unlock()
		if err := writeLine(out, m); err != nil {
			return err
		}
		return out.Flush()
	}
	engine := engineFor(root, work)
	engine.Progress = func(p installer.Progress) { send(message{Progress: &p}) }
	// Read stdin concurrently so cancel lines can interrupt an active request.
	type parsed struct {
		request installer.Request
		err     error
	}
	requests := make(chan parsed)
	go func() {
		defer close(requests)
		lines := bufio.NewReader(os.Stdin)
		for {
			line, err := lines.ReadString('\n')
			if len(line) == 0 && err != nil {
				if err != io.EOF {
					requests <- parsed{err: err}
				}
				return
			}
			var in inbound
			if err := sjson.Unmarshal([]byte(trimNewline(line)), &in); err != nil {
				requests <- parsed{err: err}
				continue
			}
			if in.Cancel {
				engine.Cancel.Store(true)
				continue
			}
			requests <- parsed{request: *in.Request}
		}
	}()
	for p := range requests {
		if p.err != nil {
			return p.err
		}
		engine.Cancel.Store(false)
		reply, err := engine.Handle(p.request)
		m := message{Done: &done{Reply: &reply}}
		if err != nil {
			text := err.Error()
			m = message{Done: &done{Error: &text}}
		}
		if err := send(m); err != nil {
			return err
		}
	}
	return nil
}

func trimNewline(line string) string {
	if n := len(line); n > 0 && line[n-1] == '\n' {
		line = line[:n-1]
		if n := len(line); n > 0 && line[n-1] == '\r' {
			line = line[:n-1]
		}
	}
	return line
}

// ErrNotGranted indicates pkexec exited 126 or 127 (prompt dismissed or auth failed).
var ErrNotGranted error = installer.Error("administrator permission was not granted")

// ErrStopped indicates the helper process exited unexpectedly.
var ErrStopped error = installer.Error("the installer helper stopped; choose Try again or start again")

const exitGrace = 5 * time.Second

// Helper manages a running privileged installer subprocess.
type Helper struct {
	cmd   *exec.Cmd
	stdin *stdinBox
	// Own pipe rather than cmd.StdoutPipe so cmd.Wait doesn't race with reads.
	stdout *bufio.Reader
	output *os.File
	exited chan struct{}
	appdir *stagedAppDir
}

type stdinBox struct {
	mu sync.Mutex
	w  io.WriteCloser
}

// Canceler cancels an in-flight download on the helper from any goroutine.
type Canceler struct{ stdin *stdinBox }

func (c Canceler) Cancel() {
	c.stdin.mu.Lock()
	defer c.stdin.mu.Unlock()
	if c.stdin.w != nil {
		writeLine(c.stdin.w, inbound{Cancel: true})
	}
}

func StartDownloadOnly(work string) (*Helper, error) {
	return spawn(Download, work)
}

func spawn(root, work string) (*Helper, error) {
	cmd, appdir, err := privilegedLaunch()
	if err != nil {
		return nil, err
	}
	cmd.Args = append(cmd.Args, Flag, root, work)
	h, err := start(cmd, appdir)
	if err != nil {
		appdir.Remove()
		return nil, err
	}
	return h, nil
}

func start(cmd *exec.Cmd, appdir *stagedAppDir) (*Helper, error) {
	cmd.Stderr = os.Stderr
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	output, input, err := os.Pipe()
	if err != nil {
		stdin.Close()
		return nil, err
	}
	cmd.Stdout = input
	err = cmd.Start()
	// Close parent's write end so stdout sees EOF when the child exits.
	input.Close()
	if err != nil {
		output.Close()
		stdin.Close()
		return nil, installer.Error(fmt.Sprintf("could not ask for administrator permission (pkexec): %v", err))
	}
	h := &Helper{cmd: cmd, stdin: &stdinBox{w: stdin}, stdout: bufio.NewReader(output), output: output, exited: make(chan struct{}), appdir: appdir}
	go func() {
		cmd.Wait()
		close(h.exited)
	}()
	return h, nil
}

// Alive reports whether the helper subprocess is still running.
func (h *Helper) Alive() bool {
	select {
	case <-h.exited:
		return false
	default:
		return true
	}
}

func (h *Helper) Canceler() Canceler { return Canceler{h.stdin} }

// Wait briefly for the child's exit code to distinguish pkexec auth rejection
// (126/127) from other exits.
func (h *Helper) stopped(fallback error) error {
	select {
	case <-h.exited:
	case <-time.After(exitGrace):
		return fallback
	}
	if code := h.cmd.ProcessState.ExitCode(); code == 126 || code == 127 {
		return ErrNotGranted
	}
	return ErrStopped
}

// Handle sends a request to the helper and blocks until its reply arrives.
func (h *Helper) Handle(request installer.Request, progress func(installer.Progress)) (installer.Reply, error) {
	h.stdin.mu.Lock()
	if h.stdin.w == nil {
		h.stdin.mu.Unlock()
		return installer.Reply{}, installer.Error("the installer helper stopped")
	}
	err := writeLine(h.stdin.w, inbound{Request: &request})
	h.stdin.mu.Unlock()
	if err != nil {
		return installer.Reply{}, h.stopped(err)
	}
	for {
		line, err := h.stdout.ReadString('\n')
		if len(line) == 0 && err != nil {
			fallback := ErrStopped
			if !errors.Is(err, io.EOF) {
				fallback = err
			}
			return installer.Reply{}, h.stopped(fallback)
		}
		var m message
		if err := sjson.Unmarshal([]byte(trimNewline(line)), &m); err != nil {
			return installer.Reply{}, err
		}
		if m.Progress != nil {
			progress(*m.Progress)
			continue
		}
		if m.Done.Error != nil {
			return installer.Reply{}, installer.Error(*m.Done.Error)
		}
		return *m.Done.Reply, nil
	}
}

// Close closes stdin to shut down the helper cleanly and waits for exit.
func (h *Helper) Close() error {
	h.stdin.mu.Lock()
	if h.stdin.w != nil {
		h.stdin.w.Close()
		h.stdin.w = nil
	}
	h.stdin.mu.Unlock()
	<-h.exited
	h.output.Close()
	h.appdir.Remove()
	return nil
}

// RequestUSBRule invokes the app via pkexec with USBRuleFlag to install the udev rule.
func RequestUSBRule() error {
	cmd, appdir, err := privilegedLaunch()
	if err != nil {
		return err
	}
	defer appdir.Remove()
	cmd.Args = append(cmd.Args, USBRuleFlag)
	cmd.Stdin = nil
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	err = cmd.Run()
	var exit *exec.ExitError
	switch {
	case err == nil:
		return nil
	case errors.As(err, &exit):
		if code := exit.ExitCode(); code == 126 || code == 127 {
			return installer.Error("The password prompt was dismissed or the password wasn't accepted.")
		}
		return installer.Error(fmt.Sprintf("Installing the USB rule failed (%v).", exit.ProcessState))
	}
	return installer.Error(fmt.Sprintf("could not ask for administrator permission (pkexec): %v", err))
}
